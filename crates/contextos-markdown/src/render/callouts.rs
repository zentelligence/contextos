//! Obsidian callouts: a blockquote whose first line is `[!type]`, with an optional fold marker and title.

use pulldown_cmark::{CowStr, Event, Tag, TagEnd};

use crate::escape::escape_attribute;
use crate::prepare::Spanned;

/// A callout's opening line.
struct Opening {
    kind: String,
    /// `None` when it does not fold, `Some(true)` when it starts open.
    fold: Option<bool>,
    /// The title text written after the marker on the same text run.
    title: String,
}

impl TryFrom<&str> for Opening {
    type Error = ();

    fn try_from(text: &str) -> Result<Self, Self::Error> {
        let rest = text.strip_prefix("[!").ok_or(())?;
        let (kind, after) = rest.split_once(']').ok_or(())?;
        if kind.is_empty() || !kind.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(());
        }
        let (fold, title) = match after.chars().next() {
            Some('+') => (Some(true), after.get(1..).unwrap_or_default()),
            Some('-') => (Some(false), after.get(1..).unwrap_or_default()),
            _ => (None, after),
        };
        Ok(Self {
            kind: kind.to_ascii_lowercase(),
            fold,
            title: title.trim_start().to_owned(),
        })
    }
}

/// The style an Obsidian callout type takes: its own, or the type it is an alias of.
fn style(kind: &str) -> &str {
    match kind {
        "summary" | "tldr" => "abstract",
        "hint" | "important" => "tip",
        "check" | "done" => "success",
        "help" | "faq" => "question",
        "caution" | "attention" => "warning",
        "fail" | "missing" => "failure",
        "error" => "danger",
        "cite" => "quote",
        other => other,
    }
}

fn default_title(kind: &str) -> String {
    let mut chars = kind.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// Each callout becomes its container; any other blockquote stays a blockquote.
pub(super) fn callouts(events: &[Spanned]) -> Vec<Spanned> {
    let mut out = Vec::with_capacity(events.len());
    let mut closings: Vec<Option<&'static str>> = Vec::new();
    let mut index = 0;
    while let Some((event, range)) = events.get(index) {
        match event {
            Event::Start(Tag::BlockQuote(_)) => {
                let opening = match (events.get(index.saturating_add(1)), events.get(index.saturating_add(2))) {
                    (Some((Event::Start(Tag::Paragraph), _)), Some((Event::Text(text), _))) => {
                        Opening::try_from(text.as_ref()).ok()
                    }
                    _ => None,
                };
                let Some(opening) = opening else {
                    closings.push(None);
                    out.push((event.clone(), range.clone()));
                    index = index.saturating_add(1);
                    continue;
                };
                let kind = escape_attribute(&opening.kind);
                let class = escape_attribute(style(&opening.kind));
                let (open, middle, close) = match opening.fold {
                    None => (
                        format!(
                            "<div class=\"callout callout-{class}\" data-callout=\"{kind}\"><div class=\"callout-title\">"
                        ),
                        "</div><div class=\"callout-body\">",
                        "</div></div>\n",
                    ),
                    Some(starts_open) => (
                        format!(
                            "<details class=\"callout callout-{class}\" data-callout=\"{kind}\"{}><summary class=\"callout-title\">",
                            if starts_open { " open" } else { "" }
                        ),
                        "</summary><div class=\"callout-body\">",
                        "</div></details>\n",
                    ),
                };
                out.push((Event::Html(CowStr::from(open)), range.clone()));
                let mut title_end = index.saturating_add(3);
                let mut title: Vec<Spanned> = Vec::new();
                if !opening.title.is_empty() {
                    title.push((Event::Text(CowStr::from(opening.title.clone())), range.clone()));
                }
                while let Some((inner, inner_range)) = events.get(title_end) {
                    if matches!(
                        inner,
                        Event::SoftBreak | Event::HardBreak | Event::End(TagEnd::Paragraph)
                    ) {
                        break;
                    }
                    title.push((inner.clone(), inner_range.clone()));
                    title_end = title_end.saturating_add(1);
                }
                if title.is_empty() {
                    title.push((Event::Text(CowStr::from(default_title(&opening.kind))), range.clone()));
                }
                out.extend(title);
                out.push((Event::Html(CowStr::from(middle)), range.clone()));
                closings.push(Some(close));
                match events.get(title_end) {
                    Some((Event::End(TagEnd::Paragraph), _)) => index = title_end.saturating_add(1),
                    Some((_, break_range)) => {
                        out.push((Event::Start(Tag::Paragraph), break_range.clone()));
                        index = title_end.saturating_add(1);
                    }
                    None => index = title_end,
                }
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                match closings.pop().flatten() {
                    Some(close) => out.push((Event::Html(CowStr::from(close)), range.clone())),
                    None => out.push((event.clone(), range.clone())),
                }
                index = index.saturating_add(1);
            }
            _ => {
                out.push((event.clone(), range.clone()));
                index = index.saturating_add(1);
            }
        }
    }
    out
}
