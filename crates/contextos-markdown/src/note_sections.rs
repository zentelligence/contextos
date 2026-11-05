//! A note's top-level `##` sections, by byte range, for editing a note one section at a time.
//!
//! Sections are found with the same parser and options the renderer uses, so a section boundary is exactly a heading
//! the rendered page shows: a `## ` line inside a code block, a `%%comment%%`, a block quote, or a list item is not
//! one, and neither is anything in the frontmatter. Only a second-level heading at the top level of the body starts a
//! section; a first-level or deeper heading after it is part of that section's content. Every range is a byte range
//! of the text as given, so a caller replaces one section's bytes and leaves every other byte as it was.

use std::ops::Range;

use contextos_obsidian::FrontmatterSpan;
use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};

use crate::comments::{CommentInput, CommentMasked};
use crate::prepare::{code_ranges, options};

/// A note's text divided into its top-level second-level sections.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NoteSections {
    /// Where the body starts, after any frontmatter. Text from here to the first section (a title, navigation links)
    /// belongs to no section.
    pub body_start: usize,
    /// Every section, in the order written.
    pub sections: Vec<SectionSpan>,
}

/// One `##` section.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SectionSpan {
    /// The heading's plain text: its words with inline formatting and closing `#`s left out.
    pub heading: String,
    /// From the start of the heading line to the start of the next section, or the end of the text.
    pub range: Range<usize>,
    /// Everything after the heading line.
    pub content: Range<usize>,
    /// The leading run of block quotes and HTML blocks (such as `<!-- comments -->`), with the blank lines before and
    /// between them: the part of a section a template uses to say what goes in it. Empty, at `content.start`, when the
    /// section opens with anything else.
    pub lead: Range<usize>,
    /// The trailing run of thematic breaks (`---`), with the blank lines between and after them: the rule a template
    /// uses to close a section. Empty, at `content.end`, when the section ends with anything else.
    pub trail: Range<usize>,
}

/// A top-level block of a section's content, as far as framing is concerned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlockKind {
    /// A block quote or an HTML block.
    Guidance,
    /// A thematic break.
    Rule,
    Other,
}

/// A section being read: its heading's start and text, and its content's top-level blocks so far.
struct OpenSection {
    start: usize,
    heading: String,
    content_start: usize,
    blocks: Vec<(BlockKind, Range<usize>)>,
}

impl From<&str> for NoteSections {
    fn from(text: &str) -> Self {
        let body_start = match FrontmatterSpan::from(text) {
            FrontmatterSpan::Closed { body_start, .. } => body_start,
            FrontmatterSpan::Absent | FrontmatterSpan::Unclosed => {
                if text.starts_with('\u{feff}') {
                    '\u{feff}'.len_utf8()
                } else {
                    0
                }
            }
        };
        let body = text.get(body_start..).unwrap_or_default();
        let code = code_ranges(body, true);
        let masked = String::from(CommentMasked::from(CommentInput {
            text: body,
            code: &code,
        }));

        let mut sections = Vec::new();
        let mut open: Option<OpenSection> = None;
        let mut in_heading: Option<String> = None;
        let mut depth = 0_usize;
        for (event, range) in Parser::new_ext(&masked, options()).into_offset_iter() {
            let range = range.start.saturating_add(body_start)..range.end.saturating_add(body_start);
            match event {
                Event::Start(Tag::Heading {
                    level: HeadingLevel::H2,
                    ..
                }) if depth == 0 => {
                    if let Some(finished) = open.take() {
                        sections.push(finished.close(text, range.start));
                    }
                    open = Some(OpenSection {
                        start: range.start,
                        heading: String::new(),
                        content_start: line_end(text, range.end),
                        blocks: Vec::new(),
                    });
                    in_heading = Some(String::new());
                    depth = depth.saturating_add(1);
                }
                Event::End(TagEnd::Heading(HeadingLevel::H2)) if depth == 1 && in_heading.is_some() => {
                    if let (Some(section), Some(words)) = (open.as_mut(), in_heading.take()) {
                        words.trim().clone_into(&mut section.heading);
                    }
                    depth = depth.saturating_sub(1);
                }
                Event::Start(tag) => {
                    if depth == 0
                        && let Some(section) = open.as_mut()
                    {
                        let kind = match tag {
                            Tag::BlockQuote(_) | Tag::HtmlBlock => BlockKind::Guidance,
                            _ => BlockKind::Other,
                        };
                        section.blocks.push((kind, range.clone()));
                    }
                    depth = depth.saturating_add(1);
                }
                Event::End(_) => depth = depth.saturating_sub(1),
                Event::Rule if depth == 0 => {
                    if let Some(section) = open.as_mut() {
                        section.blocks.push((BlockKind::Rule, range));
                    }
                }
                Event::Text(words) | Event::Code(words) => {
                    if let Some(heading) = in_heading.as_mut() {
                        heading.push_str(&words);
                    } else if depth == 0
                        && let Some(section) = open.as_mut()
                    {
                        section.blocks.push((BlockKind::Other, range));
                    }
                }
                _ => {
                    if depth == 0
                        && let Some(section) = open.as_mut()
                    {
                        section.blocks.push((BlockKind::Other, range));
                    }
                }
            }
        }
        if let Some(finished) = open.take() {
            sections.push(finished.close(text, text.len()));
        }
        Self { body_start, sections }
    }
}

impl OpenSection {
    fn close(self, text: &str, end: usize) -> SectionSpan {
        let content = self.content_start.min(end)..end;
        let leading = self
            .blocks
            .iter()
            .take_while(|(kind, _)| *kind == BlockKind::Guidance)
            .last()
            .map_or(content.start, |(_, range)| line_end(text, range.end).min(end));
        let trailing_rules = self
            .blocks
            .iter()
            .rev()
            .take_while(|(kind, _)| *kind == BlockKind::Rule)
            .last()
            .map_or(content.end, |(_, range)| line_start(text, range.start));
        let trail_start = trailing_rules.max(leading);
        SectionSpan {
            heading: self.heading,
            range: self.start..end,
            content: content.clone(),
            lead: content.start..leading,
            trail: trail_start..content.end,
        }
    }
}

/// `at`, moved on past the line break ending the line it is on, unless it already starts a line.
fn line_end(text: &str, at: usize) -> usize {
    if at == 0 || text.get(..at).is_some_and(|before| before.ends_with('\n')) {
        return at;
    }
    text.get(at..)
        .and_then(|rest| rest.find('\n'))
        .map_or(text.len(), |offset| at.saturating_add(offset).saturating_add(1))
}

/// The start of the line `at` is on.
fn line_start(text: &str, at: usize) -> usize {
    text.get(..at)
        .and_then(|before| before.rfind('\n'))
        .map_or(0, |offset| offset.saturating_add(1))
}

#[cfg(test)]
#[path = "note_sections_test.rs"]
mod tests;
