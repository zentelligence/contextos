//! Passes over a segment's inline text: merged text runs, block ids, heading anchors, highlights, and tags.

use pulldown_cmark::{CowStr, Event, Tag, TagEnd};

use crate::escape::{escape_attribute, escape_text};
use crate::prepare::Spanned;
use crate::slug::Anchors;

/// Adjacent text events joined into one, so a pass reading text sees a run whole however the parser split it.
pub(super) fn merged_text(events: Vec<Spanned>) -> Vec<Spanned> {
    let mut out: Vec<Spanned> = Vec::with_capacity(events.len());
    for (event, range) in events {
        if let Event::Text(next) = &event
            && let Some((Event::Text(previous), previous_range)) = out.last_mut()
        {
            *previous = CowStr::from(format!("{previous}{next}"));
            previous_range.end = range.end;
            continue;
        }
        out.push((event, range));
    }
    out
}

/// The `^id` that ends `text`, after whitespace or as the whole of it, and the text before it.
fn trailing_block_id(text: &str) -> Option<(&str, &str)> {
    let trimmed = text.trim_end();
    let caret = trimmed.rfind('^')?;
    let id = trimmed.get(caret.saturating_add(1)..)?;
    let before = trimmed.get(..caret)?;
    let valid = !id.is_empty() && id.chars().all(|c| c.is_alphanumeric() || c == '-');
    (valid && (before.is_empty() || before.ends_with(char::is_whitespace))).then(|| (before.trim_end(), id))
}

fn anchor_html(id: &str) -> String {
    format!("<span class=\"block-anchor\" id=\"^{}\"></span>", escape_attribute(id))
}

/// Each `^id` ending a paragraph, list item, or heading becomes an anchor in its place, and a paragraph holding only
/// one becomes an anchor on its own, for the block before it. Returns each id with the offset it was written at.
pub(super) fn block_ids(events: Vec<Spanned>) -> (Vec<Spanned>, Vec<(String, usize)>) {
    let mut out: Vec<Spanned> = Vec::with_capacity(events.len());
    let mut found = Vec::new();
    for (event, range) in events {
        let ends_block = matches!(event, Event::End(TagEnd::Paragraph | TagEnd::Item | TagEnd::Heading(_)));
        if !ends_block {
            out.push((event, range));
            continue;
        }
        let last_text = match out.last() {
            Some((Event::Text(text), text_range)) => {
                trailing_block_id(text).map(|(before, id)| (before.to_owned(), id.to_owned(), text_range.start))
            }
            _ => None,
        };
        let Some((before, id, offset)) = last_text else {
            out.push((event, range));
            continue;
        };
        found.push((id.clone(), offset));
        let alone = before.is_empty()
            && matches!(event, Event::End(TagEnd::Paragraph))
            && matches!(out.iter().rev().nth(1), Some((Event::Start(Tag::Paragraph), _)));
        let text = out.pop();
        if alone {
            let start = out.pop().map_or(range.clone(), |(_, start)| start);
            out.push((
                Event::Html(CowStr::from(format!("{}\n", anchor_html(&id)))),
                start.start..range.end,
            ));
            continue;
        }
        if let Some((_, text_range)) = text {
            if !before.is_empty() {
                out.push((Event::Text(CowStr::from(before)), text_range.clone()));
            }
            out.push((Event::Html(CowStr::from(anchor_html(&id))), text_range));
        }
        out.push((event, range));
    }
    (out, found)
}

/// A heading for the outline.
pub(super) struct HeadingFound {
    pub(super) level: u8,
    pub(super) text: String,
    pub(super) anchor: String,
    pub(super) offset: usize,
}

/// Every heading gains an anchor unique across the page. Returns each heading for the outline.
pub(super) fn heading_anchors(events: &[Spanned], anchors: &mut Anchors) -> (Vec<Spanned>, Vec<HeadingFound>) {
    let mut out = Vec::with_capacity(events.len());
    let mut found = Vec::new();
    let mut index = 0;
    while let Some((event, range)) = events.get(index) {
        if let Event::Start(Tag::Heading {
            level, classes, attrs, ..
        }) = event
        {
            let mut text = String::new();
            for (inner, _) in events.iter().skip(index.saturating_add(1)) {
                match inner {
                    Event::End(TagEnd::Heading(_)) => break,
                    Event::Text(part) | Event::Code(part) => text.push_str(part),
                    _ => {}
                }
            }
            let anchor = anchors.heading(&text);
            found.push(HeadingFound {
                level: u8::try_from(*level as usize).unwrap_or(6),
                text,
                anchor: anchor.clone(),
                offset: range.start,
            });
            out.push((
                Event::Start(Tag::Heading {
                    level: *level,
                    id: Some(CowStr::from(anchor)),
                    classes: classes.clone(),
                    attrs: attrs.clone(),
                }),
                range.clone(),
            ));
        } else {
            out.push((event.clone(), range.clone()));
        }
        index = index.saturating_add(1);
    }
    (out, found)
}

fn is_block_boundary(event: &Event<'_>) -> bool {
    match event {
        Event::Start(tag) => !is_inline_tag(tag),
        Event::End(end) => !is_inline_end(*end),
        Event::Rule | Event::Html(_) => true,
        _ => false,
    }
}

fn is_inline_tag(tag: &Tag<'_>) -> bool {
    matches!(
        tag,
        Tag::Emphasis
            | Tag::Strong
            | Tag::Strikethrough
            | Tag::Superscript
            | Tag::Subscript
            | Tag::Link { .. }
            | Tag::Image { .. }
    )
}

fn is_inline_end(end: TagEnd) -> bool {
    matches!(
        end,
        TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::Link
            | TagEnd::Image
    )
}

/// One `==` in a text event.
#[derive(Clone, Copy)]
struct Marker {
    event: usize,
    at: usize,
    depth: usize,
    block: usize,
}

/// `==text==` becomes `<mark>`, across inline markup but only between markers at the same depth in the same block,
/// and never in code. A marker that pairs with nothing stays as written.
pub(super) fn highlights(events: Vec<Spanned>) -> Vec<Spanned> {
    let mut markers = Vec::new();
    let mut depth = 0_usize;
    let mut block = 0_usize;
    let mut in_code = false;
    for (index, (event, _)) in events.iter().enumerate() {
        match event {
            Event::Start(Tag::CodeBlock(_)) => in_code = true,
            Event::End(TagEnd::CodeBlock) => in_code = false,
            Event::Start(tag) if is_inline_tag(tag) => depth = depth.saturating_add(1),
            Event::End(end) if is_inline_end(*end) => depth = depth.saturating_sub(1),
            Event::Text(text) if !in_code => {
                markers.extend(text.match_indices("==").map(|(at, _)| Marker {
                    event: index,
                    at,
                    depth,
                    block,
                }));
            }
            _ => {}
        }
        if is_block_boundary(event) {
            block = block.saturating_add(1);
            depth = 0;
        }
    }
    let mut paired: Vec<(usize, usize, &'static str)> = Vec::new();
    let mut open: Option<Marker> = None;
    for marker in markers {
        match open {
            Some(start) if start.block == marker.block && start.depth == marker.depth => {
                let adjacent = start.event == marker.event && start.at.saturating_add(2) == marker.at;
                if !adjacent {
                    paired.push((start.event, start.at, "<mark>"));
                    paired.push((marker.event, marker.at, "</mark>"));
                }
                open = None;
            }
            _ => open = Some(marker),
        }
    }
    if paired.is_empty() {
        return events;
    }
    let mut out = Vec::with_capacity(events.len().saturating_add(paired.len().saturating_mul(2)));
    for (index, (event, range)) in events.into_iter().enumerate() {
        let here: Vec<(usize, &str)> = paired
            .iter()
            .filter(|(event, _, _)| *event == index)
            .map(|(_, at, html)| (*at, *html))
            .collect();
        let Event::Text(text) = &event else {
            out.push((event, range));
            continue;
        };
        if here.is_empty() {
            out.push((event, range));
            continue;
        }
        let mut copied = 0;
        for (at, html) in here {
            if let Some(piece) = text.get(copied..at).filter(|piece| !piece.is_empty()) {
                out.push((Event::Text(CowStr::from(piece.to_owned())), range.clone()));
            }
            out.push((Event::Html(CowStr::from(html)), range.clone()));
            copied = at.saturating_add(2);
        }
        if let Some(piece) = text.get(copied..).filter(|piece| !piece.is_empty()) {
            out.push((Event::Text(CowStr::from(piece.to_owned())), range.clone()));
        }
    }
    out
}

fn is_tag_body(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '/')
}

fn is_tag_boundary(previous: Option<char>) -> bool {
    previous.is_none_or(|c| c.is_whitespace() || matches!(c, '(' | '[' | '"' | '\''))
}

/// `#tag` becomes a tag, where Obsidian reads one: after a space, an opening bracket, or a quote, outside code and
/// links, and with at least one letter or `_`. Returns each tag with the offset of its text.
pub(super) fn tags(events: Vec<Spanned>) -> (Vec<Spanned>, Vec<(String, usize)>) {
    let mut out = Vec::with_capacity(events.len());
    let mut found = Vec::new();
    let mut previous: Option<char> = None;
    let mut in_code = false;
    let mut in_link = 0_usize;
    for (event, range) in events {
        match &event {
            Event::Start(Tag::CodeBlock(_)) => in_code = true,
            Event::End(TagEnd::CodeBlock) => in_code = false,
            Event::Start(Tag::Link { .. } | Tag::Image { .. }) => in_link = in_link.saturating_add(1),
            Event::End(TagEnd::Link | TagEnd::Image) => in_link = in_link.saturating_sub(1),
            Event::Code(code) => previous = code.chars().last(),
            Event::SoftBreak | Event::HardBreak => previous = None,
            Event::Start(tag) if !is_inline_tag(tag) => previous = None,
            _ => {}
        }
        let Event::Text(text) = &event else {
            out.push((event, range));
            continue;
        };
        if in_code || in_link > 0 || !text.contains('#') {
            previous = text.chars().last().or(previous);
            out.push((event, range));
            continue;
        }
        let mut plain = String::new();
        let mut chars = text.char_indices().peekable();
        while let Some((at, c)) = chars.next() {
            if c == '#' && is_tag_boundary(previous) {
                let rest = text.get(at.saturating_add(1)..).unwrap_or_default();
                let length: usize = rest.chars().take_while(|c| is_tag_body(*c)).map(char::len_utf8).sum();
                let name = rest.get(..length).unwrap_or_default();
                if name.chars().any(|c| c.is_alphabetic() || c == '_') {
                    if !plain.is_empty() {
                        out.push((Event::Text(CowStr::from(std::mem::take(&mut plain))), range.clone()));
                    }
                    found.push((name.to_owned(), range.start));
                    out.push((
                        Event::Html(CowStr::from(format!(
                            "<span class=\"tag\">#{}</span>",
                            escape_text(name)
                        ))),
                        range.clone(),
                    ));
                    while chars.next_if(|(next, _)| *next <= at.saturating_add(length)).is_some() {}
                    previous = name.chars().last();
                    continue;
                }
            }
            plain.push(c);
            previous = Some(c);
        }
        if !plain.is_empty() {
            out.push((Event::Text(CowStr::from(plain)), range));
        }
    }
    (out, found)
}

#[cfg(test)]
#[path = "inline_test.rs"]
mod tests;
