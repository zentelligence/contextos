//! The part of a note an embed names: a heading's section, or a block.

use std::ops::Range;

use pulldown_cmark::{Event, Parser, Tag, TagEnd};

use crate::Fragment;
use crate::prepare::options;
use crate::slug::slug;

/// A note's text and the place in it to find.
pub(crate) struct SectionInput<'a> {
    pub(crate) text: &'a str,
    pub(crate) fragment: &'a Fragment,
}

/// The bytes of the text the place spans.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Section(pub(crate) Range<usize>);

/// The note has no such heading or block.
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum SectionMissing {
    #[error("The embedded note has no heading {0}.")]
    Heading(String),
    #[error("The embedded note has no block ^{0}.")]
    Block(String),
}

impl TryFrom<SectionInput<'_>> for Section {
    type Error = SectionMissing;

    fn try_from(input: SectionInput<'_>) -> Result<Self, Self::Error> {
        match input.fragment {
            Fragment::Heading(heading) => heading_section(input.text, heading),
            Fragment::Block(id) => block_section(input.text, id),
        }
    }
}

/// From the heading whose anchor matches `heading` to the next heading of the same or a higher level.
fn heading_section(text: &str, heading: &str) -> Result<Section, SectionMissing> {
    let wanted = slug(heading);
    let mut headings: Vec<(u8, String, usize)> = Vec::new();
    let mut current: Option<(u8, String, usize)> = None;
    for (event, range) in Parser::new_ext(text, options()).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                current = Some((u8::try_from(level as usize).unwrap_or(6), String::new(), range.start));
            }
            Event::Text(part) | Event::Code(part) => {
                if let Some((_, words, _)) = &mut current {
                    words.push_str(&part);
                }
            }
            Event::End(TagEnd::Heading(_)) => headings.extend(current.take()),
            _ => {}
        }
    }
    let found = headings
        .iter()
        .position(|(_, words, _)| slug(words) == wanted)
        .ok_or_else(|| SectionMissing::Heading(heading.to_owned()))?;
    let (level, _, start) = headings.get(found).cloned().unwrap_or_default();
    let end = headings
        .iter()
        .skip(found.saturating_add(1))
        .find(|(other, _, _)| *other <= level)
        .map_or(text.len(), |(_, _, start)| *start);
    Ok(Section(start..end))
}

/// The block `^id` marks: the list item it ends, else the top-level block it ends, or, when it stands alone on its own
/// line, the top-level block before it.
fn block_section(text: &str, id: &str) -> Result<Section, SectionMissing> {
    found_block(text, id).map(|range| {
        let mut end = range.end;
        while text.get(range.start..end).is_some_and(|block| block.ends_with("\n\n")) {
            end = end.saturating_sub(1);
        }
        Section(range.start..end)
    })
}

fn found_block(text: &str, id: &str) -> Result<std::ops::Range<usize>, SectionMissing> {
    let mut open: Vec<(bool, std::ops::Range<usize>)> = Vec::new();
    let mut previous_top_level: Option<std::ops::Range<usize>> = None;
    for (event, range) in Parser::new_ext(text, options()).into_offset_iter() {
        match event {
            Event::Start(tag) => open.push((matches!(tag, Tag::Item), range)),
            Event::End(_) => {
                if let Some((_, closed)) = open.pop()
                    && open.is_empty()
                {
                    previous_top_level = Some(closed);
                }
            }
            Event::Text(part) => {
                let trimmed = part.trim_end();
                let Some(before) = trimmed.strip_suffix(id).and_then(|rest| rest.strip_suffix('^')) else {
                    continue;
                };
                if before.is_empty() {
                    if open.len() == 1 {
                        return previous_top_level.ok_or_else(|| SectionMissing::Block(id.to_owned()));
                    }
                } else if !before.ends_with(char::is_whitespace) {
                    continue;
                }
                let item = open
                    .iter()
                    .rev()
                    .find(|(is_item, _)| *is_item)
                    .map(|(_, range)| range.clone());
                let top = open.first().map(|(_, range)| range.clone());
                return item.or(top).ok_or_else(|| SectionMissing::Block(id.to_owned()));
            }
            _ => {}
        }
    }
    Err(SectionMissing::Block(id.to_owned()))
}

#[cfg(test)]
#[path = "section_test.rs"]
mod tests;
