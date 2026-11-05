//! The block structure a document's lines sit in, which decides whether a line can hold a link: fenced and indented
//! code blocks, the frontmatter block, `%%` comments, and the quotes and list items that fences and code are measured
//! against. One classifier serves link expansion, the link graph, and callout validation, so they never disagree.

use crate::link_scan::ReferenceDefinition;

/// What a line is, once the blocks around it are known.
pub(crate) enum LineKind<'a> {
    /// A fence, a line inside a code block, a blank line, or a frontmatter delimiter: no links, copied as it is.
    Verbatim,
    /// A line of the frontmatter block: scanned for links, but `%%` there is YAML and not a comment.
    Frontmatter,
    Definition(ReferenceDefinition<'a>),
    /// Prose, scanned for links.
    Inline,
}

/// Whether `source` opens with a `---` line and closes that frontmatter block with a later `---` or `...` line. A lone
/// `---` at the top is a thematic break, and what follows is ordinary Markdown.
fn frontmatter_closes(source: &str) -> bool {
    let mut lines = source.lines();
    lines.next().is_some_and(|first| first.trim_end() == "---")
        && lines.any(|line| matches!(line.trim_end(), "---" | "..."))
}

/// Where the scanner is relative to the document's frontmatter block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Frontmatter {
    /// The document has none, or it is over.
    Absent,
    /// The document has one, which its first line opens.
    Opens,
    /// Inside it.
    Inside,
}

/// What the line before the current one was, which decides whether an indented line is code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Preceding {
    /// Ordinary text: an indented line continues its paragraph.
    Text,
    /// Nothing that a paragraph could continue: the start of the document, a blank line, a heading, or a closed fence.
    /// An indented line is code.
    Blank,
    /// A line of an indented code block: an indented line is more code.
    IndentedCode,
}

/// What the inline scan carries from one line to the next: whether a `%%` comment is open, and the length of the
/// backtick run of a code span that has not closed yet.
#[derive(Default)]
pub(crate) struct InlineState {
    pub(crate) in_comment: bool,
    pub(crate) open_span: Option<usize>,
}

pub(crate) struct BlockState {
    frontmatter: Frontmatter,
    fence: Option<Fence>,
    pub(crate) inline: InlineState,
    preceding: Preceding,
    /// How many `>` levels the previous line sat inside.
    quote_depth: usize,
    /// The content column of each list item the line sits inside, outermost first.
    items: Vec<usize>,
}

impl BlockState {
    pub(crate) fn new(source: &str) -> Self {
        Self {
            frontmatter: if frontmatter_closes(source) {
                Frontmatter::Opens
            } else {
                Frontmatter::Absent
            },
            fence: None,
            inline: InlineState::default(),
            preceding: Preceding::Blank,
            quote_depth: 0,
            items: Vec::new(),
        }
    }

    pub(crate) fn classify<'a>(&mut self, line: &'a str, first: bool) -> LineKind<'a> {
        if self.inline.in_comment || self.inline.open_span.is_some() {
            return LineKind::Inline;
        }
        let delimiter = line.trim_end();
        match self.frontmatter {
            Frontmatter::Inside => {
                if delimiter == "---" || delimiter == "..." {
                    self.frontmatter = Frontmatter::Absent;
                    self.preceding = Preceding::Blank;
                    return LineKind::Verbatim;
                }
                return LineKind::Frontmatter;
            }
            Frontmatter::Opens if first => {
                self.frontmatter = Frontmatter::Inside;
                return LineKind::Verbatim;
            }
            Frontmatter::Opens | Frontmatter::Absent => {}
        }
        if self.inside_open_fence(line) {
            return LineKind::Verbatim;
        }
        let (depth, rest) = strip_quotes(line, usize::MAX);
        if depth != self.quote_depth {
            self.quote_depth = depth;
            self.items.clear();
        }
        if rest.trim().is_empty() {
            self.preceding = Preceding::Blank;
            return LineKind::Verbatim;
        }
        let indent = indentation(rest);
        while self.items.last().is_some_and(|content| indent < *content) {
            self.items.pop();
        }
        let (content, spare) = strip_columns(rest, self.items.last().copied().unwrap_or(0));
        let relative = indentation(content).saturating_add(spare);
        if relative >= 4 && self.preceding != Preceding::Text {
            self.preceding = Preceding::IndentedCode;
            return LineKind::Verbatim;
        }
        let (item_column, body) = self.open_item(content, relative);
        if let Some(open) = Fence::opened_by(body) {
            self.fence = Some(Fence {
                quote_depth: depth,
                content_column: item_column,
                ..open
            });
            self.preceding = Preceding::Text;
            return LineKind::Verbatim;
        }
        self.preceding = if is_atx_heading(body) {
            Preceding::Blank
        } else {
            Preceding::Text
        };
        match ReferenceDefinition::try_from(line) {
            Ok(definition) => LineKind::Definition(definition),
            Err(_) => LineKind::Inline,
        }
    }

    /// Whether `line` is inside the open fence, closing it when it is the closing line. A line outside the quote or the
    /// list item the fence opened in ends that container, and with it the fence, so the line is read afresh.
    fn inside_open_fence(&mut self, line: &str) -> bool {
        let Some(open) = &self.fence else {
            return false;
        };
        let (depth, rest) = strip_quotes(line, open.quote_depth);
        let blank = rest.trim().is_empty();
        if !blank && (depth < open.quote_depth || indentation(rest) < open.content_column) {
            self.fence = None;
            return false;
        }
        let (content, _) = strip_columns(rest, open.content_column);
        if !blank && open.is_closed_by(content) {
            self.fence = None;
            self.preceding = Preceding::Blank;
        }
        true
    }

    /// Notes that the line starts a list item, if it does, and returns the content column the line's content sits at and
    /// the content itself: what follows the marker for an item, and the line as it is otherwise.
    fn open_item<'a>(&mut self, content: &'a str, relative: usize) -> (usize, &'a str) {
        let base = self.items.last().copied().unwrap_or(0);
        let trimmed = content.trim_start();
        let Some(marker) = list_marker_width(trimmed) else {
            return (base, trimmed);
        };
        let after = trimmed.get(marker..).unwrap_or("");
        let spaces = after.len().saturating_sub(after.trim_start_matches([' ', '\t']).len());
        // Five or more spaces after the marker make the item's content indented code, which starts one space in.
        let gap = if spaces == 0 || spaces > 4 { 1 } else { spaces };
        let column = base.saturating_add(relative).saturating_add(marker).saturating_add(gap);
        self.items.push(column);
        let body = after.get(gap.min(after.len())..).unwrap_or("");
        (column, body)
    }
}

/// Splits the `>` markers that open `line` (up to `limit` of them, each after at most three spaces and followed by an
/// optional space) from the rest, returning how many it took.
fn strip_quotes(line: &str, limit: usize) -> (usize, &str) {
    let mut depth = 0_usize;
    let mut rest = line;
    while depth < limit {
        let trimmed = rest.trim_start_matches(' ');
        if rest.len().saturating_sub(trimmed.len()) > 3 {
            break;
        }
        let Some(after) = trimmed.strip_prefix('>') else {
            break;
        };
        rest = after.strip_prefix([' ', '\t']).unwrap_or(after);
        depth = depth.saturating_add(1);
    }
    (depth, rest)
}

/// The width of the white space that starts `line`, a tab counting to the next multiple of four.
pub(crate) fn indentation(line: &str) -> usize {
    let mut width = 0_usize;
    for character in line.chars() {
        match character {
            ' ' => width = width.saturating_add(1),
            '\t' => width = width.saturating_add(4).saturating_sub(width % 4),
            _ => break,
        }
    }
    width
}

/// `line` without its first `columns` columns of leading white space, and how many columns of a tab were left over
/// when the cut fell inside one.
fn strip_columns(line: &str, columns: usize) -> (&str, usize) {
    let mut width = 0_usize;
    for (at, character) in line.char_indices() {
        if width >= columns {
            return (line.get(at..).unwrap_or(""), width.saturating_sub(columns));
        }
        match character {
            ' ' => width = width.saturating_add(1),
            '\t' => width = width.saturating_add(4).saturating_sub(width % 4),
            _ => return (line.get(at..).unwrap_or(""), 0),
        }
    }
    ("", 0)
}

/// The width of the list marker `content`, a line without its indentation, starts with, or `None` when it starts no
/// list item: a bullet or a number and a delimiter, then white space.
fn list_marker_width(content: &str) -> Option<usize> {
    let mut characters = content.chars();
    let first = characters.next()?;
    let marker = if matches!(first, '-' | '*' | '+') {
        first.len_utf8()
    } else if first.is_ascii_digit() {
        let digits = content
            .len()
            .saturating_sub(content.trim_start_matches(|c: char| c.is_ascii_digit()).len());
        let delimiter = content
            .get(digits..)?
            .chars()
            .next()
            .filter(|c| matches!(c, '.' | ')'))?;
        digits.saturating_add(delimiter.len_utf8())
    } else {
        return None;
    };
    let after = content.get(marker..)?;
    (after.is_empty() || after.starts_with([' ', '\t', '\n', '\r'])).then_some(marker)
}

/// Whether `content`, a line without its indentation, starts a list item.
pub(crate) fn starts_list_item(content: &str) -> bool {
    list_marker_width(content).is_some()
}

/// Whether `content`, a line without its indentation, is an ATX heading: one to six `#` and then white space or the end.
fn is_atx_heading(content: &str) -> bool {
    let hashes = content.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&hashes)
        && content
            .get(hashes..)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\t', '\n', '\r']))
}

/// An open fenced code block: its marker character, the length of the run that opened it, and the quote and list item
/// it sits in.
pub(crate) struct Fence {
    marker: char,
    length: usize,
    quote_depth: usize,
    content_column: usize,
}

impl Fence {
    /// The fence `line` opens: at most three spaces of indent, then three or more backticks or tildes, and for a
    /// backtick fence an info string without a backtick, since a line like ```` ```rust``` ```` is inline code.
    pub(crate) fn opened_by(line: &str) -> Option<Self> {
        let content = fence_content(line)?;
        let marker = content.chars().next().filter(|c| matches!(c, '`' | '~'))?;
        let length = content.chars().take_while(|c| *c == marker).count();
        let info = content.get(length..)?;
        (length >= 3 && !(marker == '`' && info.contains('`'))).then_some(Self {
            marker,
            length,
            quote_depth: 0,
            content_column: 0,
        })
    }

    /// Whether `line` closes this fence: the same character, at least as many times, and nothing else.
    fn is_closed_by(&self, line: &str) -> bool {
        let Some(content) = fence_content(line) else {
            return false;
        };
        let run = content.chars().take_while(|c| *c == self.marker).count();
        run >= self.length && content.get(run..).is_some_and(|rest| rest.trim().is_empty())
    }
}

/// `line` without up to three leading spaces, or `None` when it is indented further, which is code and not a fence.
pub(crate) fn fence_content(line: &str) -> Option<&str> {
    let content = line.trim_start_matches(' ');
    (line.len().saturating_sub(content.len()) <= 3).then_some(content)
}

/// The most lines after an unclosed backtick run that are searched for the run that closes it. A code span can run on
/// over several lines of a paragraph, but a bound keeps a hostile document of unclosed runs from being searched to its
/// end once per run.
const MAX_SPAN_LINES: usize = 50;

/// Whether a backtick run of exactly `length` appears in one of the lines after the current one that continue its
/// paragraph, so that a run of that length opening a code span on the current line closes there.
pub(crate) fn span_closes_later<'a>(following: impl Iterator<Item = &'a str>, length: usize) -> bool {
    for line in following.take(MAX_SPAN_LINES) {
        let (_, rest) = strip_quotes(line, usize::MAX);
        let content = rest.trim_start();
        if content.is_empty()
            || Fence::opened_by(content).is_some()
            || is_atx_heading(content)
            || starts_list_item(content)
        {
            return false;
        }
        if backtick_run_lengths(line).any(|run| run == length) {
            return true;
        }
    }
    false
}

/// The length of each maximal run of backticks in `line`.
fn backtick_run_lengths(line: &str) -> impl Iterator<Item = usize> + '_ {
    line.split(|c| c != '`').map(str::len).filter(|length| *length > 0)
}

#[cfg(test)]
#[path = "link_blocks_test.rs"]
mod tests;
