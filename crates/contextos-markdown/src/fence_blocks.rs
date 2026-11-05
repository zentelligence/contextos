//! Splits a body into Markdown segments and the triple-colon fence containers around them.
//!
//! Fences are found line by line, outside code, before any Markdown is parsed: a `:::name` line with no blank line
//! around it would otherwise become paragraph text whose extent cannot be recovered.

use std::ops::Range;

use crate::fence_attributes::{AttributeInput, FenceAttributes};
use crate::fence_vocabulary::{FenceSpec, FenceVocabulary};

/// A run of Markdown, or a fence holding more blocks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Block {
    /// A byte range of the body, parsed as Markdown on its own.
    Markdown(Range<usize>),
    Fence(Fence),
}

/// One fence container.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Fence {
    /// The name as written.
    pub(crate) name: String,
    /// What the vocabulary says it is, or `None` for a fence it does not know.
    pub(crate) spec: Option<FenceSpec>,
    pub(crate) attributes: FenceAttributes,
    /// Problems with the opening line itself, beyond its attributes.
    pub(crate) problems: Vec<String>,
    /// The bytes of the body the fence spans, from its opening line to the end of its closing line.
    pub(crate) range: Range<usize>,
    pub(crate) children: Vec<Block>,
}

/// The body to split.
pub(crate) struct FenceInput<'a> {
    pub(crate) text: &'a str,
    /// The byte ranges of code blocks in `text`, where no line is a fence line.
    pub(crate) code: &'a [Range<usize>],
    pub(crate) vocabulary: &'a FenceVocabulary,
}

/// The body as blocks.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct FenceTree(pub(crate) Vec<Block>);

/// What a line of the body is, to the fence grammar.
#[derive(Clone, Debug)]
enum LineKind<'a> {
    Open(OpenLine<'a>),
    /// A line of three or more colons and nothing else.
    Close {
        colons: usize,
    },
    Other,
}

#[derive(Clone, Debug)]
struct OpenLine<'a> {
    name: &'a str,
    colons: usize,
    /// The text between the braces, when there are braces.
    attributes: Option<&'a str>,
    problems: Vec<String>,
}

struct Line<'a> {
    range: Range<usize>,
    kind: LineKind<'a>,
}

impl<'a> From<&'a str> for LineKind<'a> {
    fn from(line: &'a str) -> Self {
        let content = line.trim_start_matches(' ');
        if line.len().saturating_sub(content.len()) > 3 {
            return Self::Other;
        }
        let content = content.trim_end();
        let colons = content.chars().take_while(|c| *c == ':').count();
        if colons < 3 {
            return Self::Other;
        }
        let after = content.get(colons..).unwrap_or_default();
        if after.is_empty() {
            return Self::Close { colons };
        }
        let name_length = after
            .find(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'))
            .unwrap_or(after.len());
        let (name, rest) = after.split_at(name_length);
        if name.is_empty() {
            return Self::Other;
        }
        let mut problems = Vec::new();
        let attributes = if let Some(braced) = rest.strip_prefix('{') {
            let (inside, trailing) = split_at_closing_brace(braced);
            if trailing.is_none() {
                problems.push("the attribute list is not closed with }".to_owned());
            }
            if trailing.is_some_and(|text| !text.trim().is_empty()) {
                problems.push("text after the attribute list is not part of the fence and is not shown".to_owned());
            }
            Some(inside)
        } else if rest.starts_with(char::is_whitespace) {
            problems.push(
                "text after the fence name is not part of the fence and is not shown; give a title as {title=\"...\"}"
                    .to_owned(),
            );
            None
        } else if rest.is_empty() {
            None
        } else {
            return Self::Other;
        };
        Self::Open(OpenLine {
            name,
            colons,
            attributes,
            problems,
        })
    }
}

/// The text before the `}` that closes an attribute list (outside a quoted value) and the text after it, or the whole
/// text and `None` when nothing closes it.
fn split_at_closing_brace(braced: &str) -> (&str, Option<&str>) {
    let mut quoted = false;
    let mut escaped = false;
    for (at, c) in braced.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            '}' if !quoted => return (braced.get(..at).unwrap_or(braced), braced.get(at.saturating_add(1)..)),
            _ => {}
        }
    }
    (braced, None)
}

/// How each fence line was matched.
#[derive(Clone, Copy, Debug)]
enum Matched {
    /// An open line and the line that closes it.
    Container { close: usize },
    /// A self-closing fence, and the close line after it that belongs to it, if any.
    SelfClosing { close: Option<usize> },
}

impl From<FenceInput<'_>> for FenceTree {
    fn from(input: FenceInput<'_>) -> Self {
        let mut lines = Vec::new();
        let mut start = 0_usize;
        for text in input.text.split_inclusive('\n') {
            let end = start.saturating_add(text.len());
            let in_code = input.code.iter().any(|code| code.contains(&start));
            let kind = if in_code { LineKind::Other } else { LineKind::from(text) };
            lines.push(Line {
                range: start..end,
                kind,
            });
            start = end;
        }

        let mut matched: Vec<Option<Matched>> = vec![None; lines.len()];
        let mut open: Vec<(usize, usize)> = Vec::new();
        let mut index = 0;
        while let Some(line) = lines.get(index) {
            match &line.kind {
                LineKind::Open(opening) => {
                    let self_closing = input
                        .vocabulary
                        .fence(opening.name)
                        .is_some_and(|spec| spec.self_closing);
                    if self_closing {
                        let next = index.saturating_add(1);
                        let close = lines
                            .get(next)
                            .is_some_and(|line| matches!(line.kind, LineKind::Close { .. }))
                            .then_some(next);
                        if let Some(slot) = matched.get_mut(index) {
                            *slot = Some(Matched::SelfClosing { close });
                        }
                        index = close.unwrap_or(index).saturating_add(1);
                        continue;
                    }
                    open.push((index, opening.colons));
                }
                LineKind::Close { colons } => {
                    if let Some(&(opened, opened_colons)) = open.last()
                        && opened_colons <= *colons
                    {
                        open.pop();
                        if let Some(slot) = matched.get_mut(opened) {
                            *slot = Some(Matched::Container { close: index });
                        }
                    }
                }
                LineKind::Other => {}
            }
            index = index.saturating_add(1);
        }

        let builder = TreeBuilder {
            input: &input,
            lines: &lines,
            matched: &matched,
        };
        Self(builder.blocks(0, lines.len()))
    }
}

struct TreeBuilder<'a> {
    input: &'a FenceInput<'a>,
    lines: &'a [Line<'a>],
    matched: &'a [Option<Matched>],
}

impl TreeBuilder<'_> {
    /// The blocks of lines `from..to`.
    fn blocks(&self, from: usize, to: usize) -> Vec<Block> {
        let mut blocks = Vec::new();
        let mut markdown: Option<Range<usize>> = None;
        let mut index = from;
        while index < to {
            let Some(line) = self.lines.get(index) else {
                break;
            };
            let found = match (&line.kind, self.matched.get(index).copied().flatten()) {
                (LineKind::Open(opening), Some(how)) => Some((opening, how)),
                _ => None,
            };
            let Some((opening, how)) = found else {
                markdown = Some(markdown.map_or(line.range.clone(), |run| run.start..line.range.end));
                index = index.saturating_add(1);
                continue;
            };
            if let Some(run) = markdown.take() {
                blocks.push(Block::Markdown(run));
            }
            let (children, last) = match how {
                Matched::Container { close } => (self.blocks(index.saturating_add(1), close), close),
                Matched::SelfClosing { close } => (Vec::new(), close.unwrap_or(index)),
            };
            let end = self.lines.get(last).map_or(line.range.end, |closing| closing.range.end);
            blocks.push(Block::Fence(self.fence(opening, line.range.start..end, children)));
            let next = last.saturating_add(1);
            index = next;
        }
        if let Some(run) = markdown {
            blocks.push(Block::Markdown(run));
        }
        blocks
    }

    fn fence(&self, opening: &OpenLine<'_>, range: Range<usize>, children: Vec<Block>) -> Fence {
        let attributes = opening.attributes.map_or_else(FenceAttributes::default, |raw| {
            FenceAttributes::from(AttributeInput {
                raw,
                vocabulary: self.input.vocabulary,
            })
        });
        Fence {
            name: opening.name.to_owned(),
            spec: self.input.vocabulary.fence(opening.name).cloned(),
            attributes,
            problems: opening.problems.clone(),
            range,
            children,
        }
    }
}

#[cfg(test)]
#[path = "fence_blocks_test.rs"]
mod tests;
