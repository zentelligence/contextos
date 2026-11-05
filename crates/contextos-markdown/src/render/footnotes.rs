//! Footnotes, numbered in the order they are first referred to and listed at the end of their note.

use std::collections::HashMap;

use pulldown_cmark::{CowStr, Event, Tag, TagEnd};

use crate::escape::escape_text;

/// `events`, a whole note, with each reference to a defined footnote numbered and the definitions listed after it.
/// Ids carry `scope`, so a page and the notes embedded in it keep their footnotes apart.
pub(super) fn numbered(events: Vec<Event<'static>>, scope: usize) -> Vec<Event<'static>> {
    let mut flow = Vec::with_capacity(events.len());
    let mut definitions: Vec<(String, Vec<Event<'static>>)> = Vec::new();
    let mut current: Option<(String, Vec<Event<'static>>, usize)> = None;
    for event in events {
        if let Some((label, body, depth)) = &mut current {
            match &event {
                Event::Start(Tag::FootnoteDefinition(_)) => *depth = depth.saturating_add(1),
                Event::End(TagEnd::FootnoteDefinition) if *depth == 0 => {
                    if !definitions.iter().any(|(known, _)| known == label) {
                        definitions.push((label.clone(), std::mem::take(body)));
                    }
                    current = None;
                    continue;
                }
                Event::End(TagEnd::FootnoteDefinition) => *depth = depth.saturating_sub(1),
                _ => {}
            }
            body.push(event);
            continue;
        }
        if let Event::Start(Tag::FootnoteDefinition(label)) = &event {
            current = Some((label.to_lowercase(), Vec::new(), 0));
            continue;
        }
        flow.push(event);
    }

    let mut numbers: HashMap<String, usize> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut out = Vec::with_capacity(flow.len());
    for event in flow {
        let Event::FootnoteReference(label) = &event else {
            out.push(event);
            continue;
        };
        let key = label.to_lowercase();
        if !definitions.iter().any(|(known, _)| *known == key) {
            out.push(Event::Text(CowStr::from(format!("[^{label}]"))));
            continue;
        }
        let first = !numbers.contains_key(&key);
        let next = numbers.len().saturating_add(1);
        let number = *numbers.entry(key.clone()).or_insert(next);
        if first {
            order.push(key);
        }
        let id = if first {
            format!(" id=\"fnref-{scope}-{number}\"")
        } else {
            String::new()
        };
        out.push(Event::Html(CowStr::from(format!(
            "<sup class=\"footnote-ref\"><a href=\"#fn-{scope}-{number}\"{id}>{number}</a></sup>"
        ))));
    }
    if definitions.is_empty() {
        return out;
    }
    for (label, _) in &definitions {
        if !numbers.contains_key(label) {
            order.push(label.clone());
        }
    }
    out.push(Event::Html(CowStr::from("<section class=\"footnotes\"><ol>\n")));
    for (position, label) in order.iter().enumerate() {
        let number = position.saturating_add(1);
        let referred = numbers.contains_key(label);
        out.push(Event::Html(CowStr::from(format!("<li id=\"fn-{scope}-{number}\">"))));
        if let Some((_, body)) = definitions.iter_mut().find(|(known, _)| known == label) {
            out.append(body);
        }
        if referred {
            out.push(Event::Html(CowStr::from(format!(
                "<a class=\"footnote-back\" href=\"#fnref-{scope}-{number}\" aria-label=\"{}\">\u{21a9}</a>",
                escape_text("Back to the reference")
            ))));
        }
        out.push(Event::Html(CowStr::from("</li>\n")));
    }
    out.push(Event::Html(CowStr::from("</ol></section>\n")));
    out
}
