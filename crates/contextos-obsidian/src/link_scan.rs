//! The scanner behind link expansion and the link graph: it reads a Markdown document into pieces, naming each link it
//! finds and everything else as text to copy, so the two uses cannot disagree about what is a link.
//!
//! A link is a wikilink (`[[target]]`), an embed of one, an inline Markdown link (`[text](path "title")`), a
//! reference-style link (`[text][label]` with a `[label]: path` definition), or an image form of either. Nothing inside
//! a fenced or indented code block, a code span, a `%%` comment, or after a backslash is a link.

use std::collections::HashMap;

use crate::link_blocks::{BlockState, InlineState, LineKind, fence_content, span_closes_later};

/// One piece of a document, in source order. Concatenating the text each piece covers gives the document back, so a
/// consumer that rewrites links copies every [`Piece::Text`] and substitutes for the rest.
pub(crate) enum Piece<'a> {
    /// Text that is not a link: copied as it is.
    Text(&'a str),
    /// `[[inner]]` or `![[inner]]`.
    Wikilink(WikilinkPiece<'a>),
    /// `[[` or `![[` with no closing `]]` on its line. `covered` is the single character the scanner moved past.
    UnclosedWikilink { embed: bool, line: usize, covered: &'a str },
    /// `[text](destination "title")` or its `!` embed.
    Standard(StandardPiece<'a>),
    /// `[text][label]`, `[text][]`, or `[label]`, which is a link only when the note defines the label. `covered` is the
    /// opening bracket (`[` or `![`) the scanner moved past: the text inside is scanned for links of its own, and the
    /// label that follows is not scanned at all.
    Reference(ReferencePiece<'a>),
    /// A link reference definition line, `[label]: destination "title"`.
    Definition(ReferenceDefinition<'a>),
}

pub(crate) struct WikilinkPiece<'a> {
    pub(crate) embed: bool,
    /// The text between the brackets.
    pub(crate) inner: &'a str,
    /// Whether the line is a table row, where a literal `|` is written `\|`.
    pub(crate) in_table: bool,
    pub(crate) line: usize,
    /// The link exactly as written.
    pub(crate) raw: &'a str,
}

pub(crate) struct StandardPiece<'a> {
    pub(crate) embed: bool,
    pub(crate) link: StandardLink<'a>,
    pub(crate) line: usize,
    /// The link exactly as written, `!` included for an embed.
    pub(crate) raw: &'a str,
}

pub(crate) struct ReferencePiece<'a> {
    pub(crate) embed: bool,
    /// The text between the first pair of brackets.
    pub(crate) text: &'a str,
    /// The label between the second pair, when the link has one and it is not empty. Otherwise the text is the label.
    pub(crate) label: Option<&'a str>,
    pub(crate) line: usize,
    /// The link exactly as written.
    pub(crate) raw: &'a str,
    pub(crate) covered: &'a str,
}

/// A link destination as written: bare, or between `<` and `>`.
pub(crate) struct Destination<'a> {
    pub(crate) text: &'a str,
    pub(crate) bracketed: bool,
}

impl Destination<'_> {
    /// The destination's path, as written, and its fragment from the `#` on (`""` when it has none).
    pub(crate) fn path_and_fragment(&self) -> (&str, &str) {
        self.text.find('#').map_or((self.text, ""), |at| {
            (
                self.text.get(..at).unwrap_or(self.text),
                self.text.get(at..).unwrap_or(""),
            )
        })
    }

    /// `path`, taken from this destination, with URL encoding decoded. A path between angle brackets is taken
    /// literally, and so is one whose encoding does not decode to text.
    pub(crate) fn decoded(&self, path: &str) -> String {
        if self.bracketed {
            path.to_owned()
        } else {
            percent_decoded(path).unwrap_or_else(|| path.to_owned())
        }
    }

    /// The vault path this destination names, decoded and without its fragment, or `None` when it names something
    /// other than a vault file.
    pub(crate) fn vault_target(&self) -> Option<String> {
        if is_external_target(self.text) {
            return None;
        }
        Some(self.decoded(self.path_and_fragment().0))
    }
}

/// Whether `target` is something other than a vault file: empty, an anchor, protocol-relative, or a URI with a scheme.
pub(crate) fn is_external_target(target: &str) -> bool {
    if target.is_empty() || target.starts_with('#') || target.starts_with("//") {
        return true;
    }
    target.split_once(':').is_some_and(|(scheme, _)| {
        scheme.len() > 1
            && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    })
}

/// `label` as Markdown compares labels: ignoring case and runs of white space.
pub(crate) fn normalised_label(label: &str) -> String {
    label.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Reads a document line by line into [`Piece`]s, tracking what Markdown makes inert: fenced and indented code blocks,
/// code spans, `%%` comments, and the frontmatter block, whose indented lines are YAML and not code.
///
/// This is the one scanner behind link expansion and the link graph, so the two cannot disagree about what is a link.
pub(crate) struct Scanner<'a> {
    lines: std::iter::Enumerate<std::str::SplitInclusive<'a, char>>,
    blocks: BlockState,
    line: Option<LineScan<'a>>,
    prose: Option<Vec<(usize, &'a str)>>,
}

impl<'a> From<&'a str> for Scanner<'a> {
    fn from(source: &'a str) -> Self {
        Self {
            lines: source.split_inclusive('\n').enumerate(),
            blocks: BlockState::new(source),
            line: None,
            prose: None,
        }
    }
}

impl<'a> Iterator for Scanner<'a> {
    type Item = Piece<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(scan) = &mut self.line {
                let following = self.lines.clone().map(|(_, line)| line);
                if let Some(piece) = scan.next_piece(&mut self.blocks.inline, following) {
                    return Some(piece);
                }
                self.line = None;
            }
            let (index, line) = self.lines.next()?;
            let number = index.saturating_add(1);
            let commented = self.blocks.inline.in_comment;
            match self.blocks.classify(line, index == 0) {
                LineKind::Verbatim => return Some(Piece::Text(line)),
                LineKind::Definition(definition) => {
                    self.record_prose(number, line, commented);
                    return Some(Piece::Definition(definition));
                }
                LineKind::Frontmatter => self.line = Some(LineScan::new(line, number, false)),
                LineKind::Inline => {
                    self.record_prose(number, line, commented);
                    self.line = Some(LineScan::new(line, number, true));
                }
            }
        }
    }
}

impl<'a> Scanner<'a> {
    /// A scanner that also keeps every line of prose it reads, for [`Self::into_prose`].
    pub(crate) fn recording_prose(source: &'a str) -> Self {
        Self {
            prose: Some(Vec::new()),
            ..Self::from(source)
        }
    }

    /// The prose lines read so far, each with its 1-based number: every line that is not code, frontmatter, a blank
    /// line, or inside a comment. Empty unless the scanner was made by [`Self::recording_prose`].
    pub(crate) fn into_prose(self) -> Vec<(usize, &'a str)> {
        self.prose.unwrap_or_default()
    }

    fn record_prose(&mut self, number: usize, line: &'a str, commented: bool) {
        if !commented && let Some(prose) = &mut self.prose {
            prose.push((number, line));
        }
    }
}

/// A prose line being scanned for links.
struct LineScan<'a> {
    line: &'a str,
    number: usize,
    index: LineIndex,
    rest: &'a str,
    /// The span of the label of a reference-style link, which the link has already used: an opening bracket inside it
    /// does not start a link of its own.
    label: std::ops::Range<usize>,
    /// Whether `%%` opens a comment: not in frontmatter, where it is YAML.
    comments: bool,
}

impl<'a> LineScan<'a> {
    fn new(line: &'a str, number: usize, comments: bool) -> Self {
        Self {
            line,
            number,
            index: LineIndex::from(line),
            rest: line,
            label: 0..0,
            comments,
        }
    }

    /// Moves past `length` bytes of the line and returns them.
    fn advance(&mut self, length: usize) -> &'a str {
        let (taken, rest) = split_after(self.rest, length);
        self.rest = rest;
        taken
    }

    /// The next piece of the line. `following` are the lines after this one, searched only to see whether a code span
    /// that this line leaves open closes in a later line of its paragraph.
    fn next_piece(&mut self, state: &mut InlineState, following: impl Iterator<Item = &'a str>) -> Option<Piece<'a>> {
        if self.rest.is_empty() {
            return None;
        }
        let rest = self.rest;
        if let Some(length) = state.open_span {
            let end = self
                .index
                .closing_run(self.index.position_of(rest), length)
                .map(|at| at.saturating_add(length));
            let taken = end.unwrap_or(rest.len()).saturating_sub(self.index.position_of(rest));
            if end.is_some() {
                state.open_span = None;
            }
            return Some(Piece::Text(self.advance(taken.max(1))));
        }
        if state.in_comment {
            let length = rest.find("%%").map_or(rest.len(), |at| {
                state.in_comment = false;
                at.saturating_add(2)
            });
            return Some(Piece::Text(self.advance(length)));
        }
        let first = rest.chars().next()?;
        let length = if self.comments && rest.starts_with("%%") {
            state.in_comment = true;
            2
        } else {
            match first {
                '\\' => rest.chars().take(2).map(char::len_utf8).sum(),
                '`' => self.code_span_length(rest, state, following),
                '!' | '[' if !self.label.contains(&self.index.position_of(rest)) => {
                    if let Some(piece) = self.link_at(rest) {
                        return Some(piece);
                    }
                    first.len_utf8()
                }
                _ => first.len_utf8(),
            }
        };
        Some(Piece::Text(self.advance(length)))
    }

    /// The length of the code span the backtick run at the start of `rest` opens: to its closing run on this line, or
    /// the whole of the line when the span closes in a later line of the paragraph, or the run alone when it does not
    /// close, since a run with no closer is text.
    fn code_span_length(&self, rest: &str, state: &mut InlineState, following: impl Iterator<Item = &'a str>) -> usize {
        let run = rest.chars().take_while(|c| *c == '`').count();
        if let Some(length) = self.index.code_span_length(rest) {
            return length;
        }
        if span_closes_later(following, run) {
            state.open_span = Some(run);
            return rest.len();
        }
        run
    }

    /// The link that starts at `rest`, moving past what it covers, or `None` when the text there is not one.
    fn link_at(&mut self, rest: &'a str) -> Option<Piece<'a>> {
        let (embed, body) = match rest.strip_prefix('!') {
            Some(body) => (true, body),
            None => (false, rest),
        };
        let prefix = usize::from(embed);
        if let Some(wiki) = body.strip_prefix("[[") {
            return self.wikilink_at(embed, wiki);
        }
        let start = self.index.position_of(rest);
        let opener = start.saturating_add(prefix);
        if let Ok(standard) = StandardLink::try_from(StandardLinkInput {
            body,
            text_end: self
                .index
                .brackets
                .get(&opener)
                .map(|closer| closer.saturating_sub(opener)),
        }) {
            let raw = self.advance(prefix.saturating_add(standard.length));
            return Some(Piece::Standard(StandardPiece {
                embed,
                link: standard,
                line: self.number,
                raw,
            }));
        }
        self.reference_at(embed, start, opener)
    }

    fn wikilink_at(&mut self, embed: bool, wiki: &'a str) -> Option<Piece<'a>> {
        let prefix = usize::from(embed);
        // The link text ends at the first bracket, so a failed search stops there and not at the end of the line.
        let closed = wiki
            .find(['[', ']', '\n'])
            .filter(|end| wiki.get(*end..).is_some_and(|tail| tail.starts_with("]]")));
        let Some(end) = closed else {
            // A `[[` with a `]]` somewhere later on the line is text that merely looks like a link. One with none is a
            // link left open, which a writer must be told about.
            let wiki_start = self.index.length.saturating_sub(wiki.len());
            if self.index.last_wikilink_close.is_some_and(|close| close >= wiki_start) {
                return None;
            }
            let covered = self.advance(1);
            return Some(Piece::UnclosedWikilink {
                embed,
                line: self.number,
                covered,
            });
        };
        let inner = wiki.get(..end)?;
        let raw = self.advance(prefix.saturating_add(2).saturating_add(end).saturating_add(2));
        Some(Piece::Wikilink(WikilinkPiece {
            embed,
            inner,
            in_table: self.index.in_table,
            line: self.number,
            raw,
        }))
    }

    fn reference_at(&mut self, embed: bool, start: usize, opener: usize) -> Option<Piece<'a>> {
        let closer = *self.index.brackets.get(&opener)?;
        let text = self.line.get(opener.saturating_add(1)..closer)?;
        if text.trim().is_empty() || text.starts_with('^') {
            return None;
        }
        let after = closer.saturating_add(1);
        let label = self
            .index
            .brackets
            .get(&after)
            .filter(|_| self.line.get(after..).is_some_and(|tail| tail.starts_with('[')))
            .and_then(|label_closer| {
                self.line
                    .get(after.saturating_add(1)..*label_closer)
                    .map(|label| (label, label_closer.saturating_add(1)))
            })
            .filter(|(label, _)| !label.contains(['[', ']']));
        let (label, end) = match label {
            Some((label, end)) => {
                self.label = after..end;
                ((!label.trim().is_empty()).then_some(label), end)
            }
            None => (None, after),
        };
        let raw = self.line.get(start..end)?;
        let covered = self.advance(usize::from(embed).saturating_add(1));
        Some(Piece::Reference(ReferencePiece {
            embed,
            text,
            label,
            line: self.number,
            raw,
            covered,
        }))
    }
}

/// Splits `text` after `at` bytes, or leaves it whole when `at` is not a boundary inside it.
pub(crate) fn split_after(text: &str, at: usize) -> (&str, &str) {
    text.split_at_checked(at).unwrap_or((text, ""))
}

/// What one pass over a line works out in advance, so that the scanner never searches ahead of an opener for what the
/// line already tells it. Without this, a long line of unclosed openers would be searched to its end once per opener,
/// which is quadratic in the length of a hostile note.
struct LineIndex {
    length: usize,
    /// Where the last `]]` on the line starts, so a `[[` can tell without searching ahead whether anything closes it.
    last_wikilink_close: Option<usize>,
    /// Whether the line is a table row, where a literal `|` is written `\|`.
    pub(crate) in_table: bool,
    /// The position of each `[` that has a matching `]`, mapped to that `]`.
    pub(crate) brackets: HashMap<usize, usize>,
    /// The start of every run of backticks, by the length of the run.
    backtick_runs: HashMap<usize, Vec<usize>>,
}

impl From<&str> for LineIndex {
    fn from(line: &str) -> Self {
        let mut open: Vec<usize> = Vec::new();
        let mut brackets = HashMap::new();
        let mut backtick_runs: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut bytes = line.bytes().enumerate().peekable();
        while let Some((at, byte)) = bytes.next() {
            match byte {
                b'\\' => {
                    // A backslash escapes the character after it, but not inside a code span, where it is literal.
                    // So an escaped backtick opens no span, yet can close one: it is recorded as a run all the same.
                    if let Some((escaped_at, _)) = bytes.next_if(|(_, next)| *next == b'`') {
                        let mut length = 1_usize;
                        while bytes.next_if(|(_, next)| *next == b'`').is_some() {
                            length = length.saturating_add(1);
                        }
                        backtick_runs.entry(length).or_default().push(escaped_at);
                        if length > 1 {
                            backtick_runs
                                .entry(length.saturating_sub(1))
                                .or_default()
                                .push(escaped_at.saturating_add(1));
                        }
                    } else {
                        bytes.next();
                    }
                }
                b'[' => open.push(at),
                b']' => {
                    if let Some(start) = open.pop() {
                        brackets.insert(start, at);
                    }
                }
                b'`' => {
                    let mut length = 1_usize;
                    while bytes.next_if(|(_, next)| *next == b'`').is_some() {
                        length = length.saturating_add(1);
                    }
                    backtick_runs.entry(length).or_default().push(at);
                }
                _ => {}
            }
        }
        Self {
            length: line.len(),
            last_wikilink_close: line.rfind("]]"),
            in_table: line.trim_start().starts_with('|'),
            brackets,
            backtick_runs,
        }
    }
}

impl LineIndex {
    /// Where `rest`, a suffix of the line, begins in it.
    pub(crate) fn position_of(&self, rest: &str) -> usize {
        self.length.saturating_sub(rest.len())
    }

    /// The length of the code span opened by the backtick run at the start of `rest`, or `None` when no run of the same
    /// length follows it on the line.
    pub(crate) fn code_span_length(&self, rest: &str) -> Option<usize> {
        let run = rest.chars().take_while(|c| *c == '`').count();
        let start = self.position_of(rest);
        self.closing_run(start.saturating_add(run), run)
            .map(|closer| closer.saturating_add(run).saturating_sub(start))
    }

    /// Where the first backtick run of exactly `length` that starts at or after `from` begins.
    pub(crate) fn closing_run(&self, from: usize, length: usize) -> Option<usize> {
        let starts = self.backtick_runs.get(&length)?;
        starts
            .get(starts.partition_point(|candidate| *candidate < from))
            .copied()
    }
}

/// The text is not a well-formed `[text](destination)` link.
#[derive(Clone, Copy, Debug)]
pub(crate) struct NotAStandardLink;

/// A `[text](destination "title")` link, located within the text that starts at its `[`.
pub(crate) struct StandardLink<'a> {
    /// Everything from `[` to the closing `)`.
    pub(crate) length: usize,
    pub(crate) text: &'a str,
    pub(crate) lead: &'a str,
    pub(crate) destination: Destination<'a>,
    pub(crate) tail: &'a str,
}

/// The text of a possible standard link, and where its link text ends.
pub(crate) struct StandardLinkInput<'a> {
    /// The text from the `[`.
    pub(crate) body: &'a str,
    /// The offset in `body` of the `]` matching that `[`, when the line has one.
    pub(crate) text_end: Option<usize>,
}

impl<'a> TryFrom<StandardLinkInput<'a>> for StandardLink<'a> {
    type Error = NotAStandardLink;

    fn try_from(input: StandardLinkInput<'a>) -> Result<Self, Self::Error> {
        let inside = input.body.strip_prefix('[').ok_or(NotAStandardLink)?;
        let text_end = input
            .text_end
            .and_then(|closer| closer.checked_sub(1))
            .ok_or(NotAStandardLink)?;
        let text = inside.get(..text_end).ok_or(NotAStandardLink)?;
        let after_text = inside.get(text_end.saturating_add(1)..).ok_or(NotAStandardLink)?;
        let parenthesised = after_text.strip_prefix('(').ok_or(NotAStandardLink)?;

        let lead_length = parenthesised.len().saturating_sub(parenthesised.trim_start().len());
        let (lead, after_lead) = split_after(parenthesised, lead_length);
        let (destination, bracketed, after_destination) = if let Some(bracketed) = after_lead.strip_prefix('<') {
            let end = bracketed.find(['>', '\n', '[']).ok_or(NotAStandardLink)?;
            if !bracketed.get(end..).is_some_and(|tail| tail.starts_with('>')) {
                return Err(NotAStandardLink);
            }
            let destination = bracketed.get(..end).ok_or(NotAStandardLink)?;
            (
                destination,
                true,
                bracketed.get(end.saturating_add(1)..).ok_or(NotAStandardLink)?,
            )
        } else {
            let mut open = 0_usize;
            let mut end = after_lead.len();
            for (at, c) in after_lead.char_indices() {
                match c {
                    '(' => open = open.saturating_add(1),
                    ')' if open == 0 => {
                        end = at;
                        break;
                    }
                    ')' => open = open.saturating_sub(1),
                    '[' => return Err(NotAStandardLink),
                    c if c.is_whitespace() => {
                        end = at;
                        break;
                    }
                    _ => {}
                }
            }
            let (destination, rest) = split_after(after_lead, end);
            (destination, false, rest)
        };
        if destination.is_empty() {
            return Err(NotAStandardLink);
        }
        let tail_length = closing_parenthesis(after_destination).ok_or(NotAStandardLink)?;
        let tail = after_destination.get(..tail_length).ok_or(NotAStandardLink)?;
        let length = 1_usize
            .saturating_add(text.len())
            .saturating_add(2)
            .saturating_add(lead.len())
            .saturating_add(destination.len())
            .saturating_add(if bracketed { 2 } else { 0 })
            .saturating_add(tail.len())
            .saturating_add(1);
        Ok(Self {
            length,
            text,
            lead,
            destination: Destination {
                text: destination,
                bracketed,
            },
            tail,
        })
    }
}

/// The line is not a link reference definition.
#[derive(Clone, Copy, Debug)]
pub(crate) struct NotAReferenceDefinition;

/// A link reference definition, `[label]: destination "title"`, on one line. A destination on the line after its
/// label, which Markdown allows, is not recognised.
pub(crate) struct ReferenceDefinition<'a> {
    /// The label between the first brackets, as written.
    pub(crate) label: &'a str,
    /// Everything up to the destination: the indent, the label, the colon, and the space after it.
    pub(crate) lead: &'a str,
    pub(crate) destination: Destination<'a>,
    /// Everything after the destination, the title and the line ending included.
    pub(crate) tail: &'a str,
}

impl<'a> TryFrom<&'a str> for ReferenceDefinition<'a> {
    type Error = NotAReferenceDefinition;

    fn try_from(line: &'a str) -> Result<Self, Self::Error> {
        let content = fence_content(line).ok_or(NotAReferenceDefinition)?;
        let label = content.strip_prefix('[').ok_or(NotAReferenceDefinition)?;
        // A footnote definition (`[^1]:`) is not a link.
        if label.starts_with('^') {
            return Err(NotAReferenceDefinition);
        }
        let mut label_end = None;
        let mut escaped = false;
        for (at, c) in label.char_indices() {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '[' => return Err(NotAReferenceDefinition),
                ']' => {
                    label_end = Some(at);
                    break;
                }
                _ => {}
            }
        }
        let label_end = label_end.filter(|end| label.get(..*end).is_some_and(|text| !text.trim().is_empty()));
        let label_text = label_end
            .and_then(|end| label.get(..end))
            .ok_or(NotAReferenceDefinition)?;
        let after_label = label_end
            .and_then(|end| label.get(end.saturating_add(1)..))
            .and_then(|rest| rest.strip_prefix(':'))
            .ok_or(NotAReferenceDefinition)?;
        let spaced = after_label.trim_start_matches([' ', '\t']);
        let (text, bracketed, tail) = if let Some(bracketed) = spaced.strip_prefix('<') {
            let end = bracketed.find(['>', '<', '\n']).ok_or(NotAReferenceDefinition)?;
            if !bracketed.get(end..).is_some_and(|rest| rest.starts_with('>')) {
                return Err(NotAReferenceDefinition);
            }
            (
                bracketed.get(..end).ok_or(NotAReferenceDefinition)?,
                true,
                bracketed.get(end.saturating_add(1)..).ok_or(NotAReferenceDefinition)?,
            )
        } else {
            let end = spaced.find(char::is_whitespace).unwrap_or(spaced.len());
            let (text, tail) = split_after(spaced, end);
            (text, false, tail)
        };
        if text.is_empty() && !bracketed {
            return Err(NotAReferenceDefinition);
        }
        let destination_start = line.len().saturating_sub(spaced.len());
        Ok(Self {
            label: label_text,
            lead: line.get(..destination_start).ok_or(NotAReferenceDefinition)?,
            destination: Destination { text, bracketed },
            tail,
        })
    }
}

/// The bytes before the `)` that closes a link, allowing an optional quoted or parenthesised title before it.
fn closing_parenthesis(after_destination: &str) -> Option<usize> {
    let trimmed = after_destination.trim_start();
    let lead = after_destination.len().saturating_sub(trimmed.len());
    let closer = match trimmed.chars().next()? {
        ')' => return Some(lead),
        '"' => '"',
        '\'' => '\'',
        '(' => ')',
        _ => return None,
    };
    let title = trimmed.get(1..)?;
    // A title is not searched past the next `[`, which would make a run of unclosed titles quadratic.
    let title_end = title.get(..title.find('[').unwrap_or(title.len()))?.find(closer)?;
    let after_title = title.get(title_end.saturating_add(closer.len_utf8())..)?;
    let spaces = after_title.len().saturating_sub(after_title.trim_start().len());
    after_title.get(spaces..)?.starts_with(')').then(|| {
        after_destination
            .len()
            .saturating_sub(after_title.len())
            .saturating_add(spaces)
    })
}

/// `text` with each `%XX` sequence replaced by its byte, or `None` when the result is not valid UTF-8.
pub(crate) fn percent_decoded(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while let Some(byte) = bytes.get(at) {
        let escaped = (*byte == b'%')
            .then(|| bytes.get(at.saturating_add(1)..at.saturating_add(3)))
            .flatten()
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        if let Some(value) = escaped {
            decoded.push(value);
            at = at.saturating_add(3);
        } else {
            decoded.push(*byte);
            at = at.saturating_add(1);
        }
    }
    String::from_utf8(decoded).ok()
}

#[cfg(test)]
#[path = "link_scan_test.rs"]
mod tests;
