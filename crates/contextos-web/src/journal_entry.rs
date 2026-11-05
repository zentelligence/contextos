//! A day's Journal entry as the Journal screen edits it: one tab per `##`
//! section of the entry itself, in the order written, each edited as the
//! Markdown it holds. Nothing here knows any section by name: the sections,
//! their headings, and the guidance shown with them all come from the entry
//! and the vault's configured template.
//!
//! A section's guidance is the template's own framing of it: the block quotes
//! and comments it opens with, and the `---` rule it closes with
//! ([`contextos_markdown::SectionSpan`]). Where the entry's section still opens
//! (or closes) with exactly the template's framing, that framing is shown as a
//! hint and kept out of the text being edited, but every byte of it stays in
//! the file. Where the entry differs from the template (or there is no
//! template), nothing is held back: the whole section is edited.
//!
//! Every change replaces one section's editable bytes and leaves every other
//! byte of the file as it was. A change is checked against the version of
//! that section the editor started from ([`SectionFingerprint`]), never the
//! whole file, so text another tool appended to a different section since is
//! not a reason to refuse it.

use std::fmt::Write as _;
use std::ops::Range;

use contextos_markdown::{NoteSections, SectionSpan};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Whether two headings name the same section: trimmed, ignoring case, as an
/// operator would read them.
#[must_use]
pub fn same_heading(left: &str, right: &str) -> bool {
    left.trim().to_lowercase() == right.trim().to_lowercase()
}

/// A section named in configuration, as the heading it makes reads: `Tasks
/// *new*` names the heading `## Tasks *new*` writes, whose text is `Tasks
/// new`. A configured name is matched this way, never as written, so a name
/// with inline Markdown in it finds the section it adds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeadingText(String);

impl From<&str> for HeadingText {
    fn from(name: &str) -> Self {
        let line = name.split_whitespace().collect::<Vec<_>>().join(" ");
        let heading = NoteSections::from(format!("## {line}\n").as_str())
            .sections
            .into_iter()
            .next()
            .map(|span| span.heading)
            .unwrap_or_default();
        Self(heading)
    }
}

impl HeadingText {
    /// Whether this is the text of `heading` (a section's heading as read).
    #[must_use]
    pub fn names(&self, heading: &str) -> bool {
        same_heading(&self.0, heading)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A day's entry text and the configured template's text (already rendered
/// for that day; empty when there is no usable template).
#[derive(Clone, Copy, Debug)]
pub struct EntryInput<'a> {
    pub text: &'a str,
    pub template: &'a str,
}

/// One tab of the day's entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntrySection {
    /// The section's heading text; empty for [`Self::whole_body`].
    pub heading: String,
    /// The template's framing of this section, held out of the editable
    /// content because the entry still matches it.
    pub guidance: Option<String>,
    /// Whether this stands for the whole body of an entry that has no `##`
    /// sections at all.
    pub whole_body: bool,
    /// The bytes an edit replaces.
    editable: Range<usize>,
    /// Where the section's closing `---` rules start, whether or not they are
    /// held out of the editable content: an appended item always goes above
    /// them.
    closing: usize,
}

/// A day's entry, divided into its sections.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalEntry {
    text: String,
    /// The template the entry was read with, to read a changed copy the same way.
    template: String,
    sections: Vec<EntrySection>,
}

/// The version of one section's content an editor started from: a SHA-256 of
/// the content as shown for editing, so blank lines around it, or line ending
/// style, are not a change.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(try_from = "String")]
pub struct SectionFingerprint(String);

/// A submitted fingerprint that is not 64 lowercase hexadecimal digits.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("a section version is 64 lowercase hexadecimal digits")]
pub struct MalformedFingerprint;

impl TryFrom<String> for SectionFingerprint {
    type Error = MalformedFingerprint;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let well_formed = value.len() == 64 && value.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
        if well_formed {
            Ok(Self(value))
        } else {
            Err(MalformedFingerprint)
        }
    }
}

impl SectionFingerprint {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A section's content as shown for editing.
struct ShownContent<'a>(&'a str);

impl From<ShownContent<'_>> for SectionFingerprint {
    fn from(content: ShownContent<'_>) -> Self {
        let digest = Sha256::digest(normalised(content.0).as_bytes());
        let mut hex = String::with_capacity(digest.len() * 2);
        for byte in digest {
            // Writing to a `String` cannot fail.
            let _ = write!(hex, "{byte:02x}");
        }
        Self(hex)
    }
}

/// A request to replace one section's content.
#[derive(Clone, Copy, Debug)]
pub struct SectionEdit<'a> {
    /// The section's place among the entry's sections.
    pub index: usize,
    /// The heading the editor showed at that place.
    pub heading: &'a str,
    /// The version of the content the editor started from.
    pub base: &'a SectionFingerprint,
    /// The new content.
    pub content: &'a str,
}

/// Why an edit was not applied.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SectionEditRefused {
    /// The section is no longer where the editor found it: sections were
    /// added, removed, or renamed since.
    #[error("the day's sections changed since this section was opened")]
    Moved,
    /// The section's content changed since the editor started from it.
    #[error("this section changed since it was opened")]
    Changed,
    /// The text would change how the rest of the entry reads: an unclosed
    /// code block or comment, say, that would swallow the sections after it.
    #[error("the text would change how the rest of the day's entry reads")]
    Unbalanced,
}

impl From<EntryInput<'_>> for JournalEntry {
    fn from(input: EntryInput<'_>) -> Self {
        let found = NoteSections::from(input.text);
        let template = NoteSections::from(input.template);
        let sections = if found.sections.is_empty() {
            vec![EntrySection {
                heading: String::new(),
                guidance: None,
                whole_body: true,
                editable: found.body_start..input.text.len(),
                closing: input.text.len(),
            }]
        } else {
            found
                .sections
                .iter()
                .map(|span| {
                    let model = template
                        .sections
                        .iter()
                        .find(|candidate| same_heading(&candidate.heading, &span.heading));
                    framed_section(input.text, span, model.map(|model| (input.template, model)))
                })
                .collect()
        };
        Self {
            text: input.text.to_owned(),
            template: input.template.to_owned(),
            sections,
        }
    }
}

/// `span` as a tab, the template's framing of its section of the same
/// heading held out wherever the entry still has it. The entry's leading
/// guidance may run on past the template's (content that opens with its own
/// quote), and its closing rules may start before the template's (content that
/// closes with its own rule), so the template's framing is matched line by
/// line from the outside in, never the entry's whole run at once.
fn framed_section(text: &str, span: &SectionSpan, model: Option<(&str, &SectionSpan)>) -> EntrySection {
    let (model_lead, model_trail) = model.map_or_else(Default::default, |(template, model_span)| {
        (
            framing(template, &model_span.lead),
            framing(template, &model_span.trail),
        )
    });
    let held_lead_end = (!model_lead.is_empty())
        .then(|| {
            line_ends(text, &span.lead)
                .into_iter()
                .find(|end| framing(text, &(span.lead.start..*end)) == model_lead)
        })
        .flatten();
    let held_trail_start = (!model_trail.is_empty())
        .then(|| {
            line_starts(text, &span.trail)
                .into_iter()
                .rev()
                .find(|start| framing(text, &(*start..span.trail.end)) == model_trail)
        })
        .flatten();
    let start = held_lead_end.unwrap_or(span.content.start);
    let end = held_trail_start.unwrap_or(span.content.end).max(start);
    let closing = if framing(text, &span.trail).is_empty() {
        end
    } else {
        span.trail.start.clamp(start, end)
    };
    EntrySection {
        heading: span.heading.clone(),
        guidance: held_lead_end.map(|_| model_lead),
        whole_body: false,
        editable: start..end,
        closing,
    }
}

/// The text of `range`, line endings made Unix ones, trimmed: framing as it
/// reads, whatever blank lines surround it.
fn framing(source: &str, range: &Range<usize>) -> String {
    normalised(source.get(range.clone()).unwrap_or_default())
        .trim()
        .to_owned()
}

/// Every offset in `range` just after a line break, then `range.end`.
fn line_ends(text: &str, range: &Range<usize>) -> Vec<usize> {
    let mut ends: Vec<usize> = text
        .get(range.clone())
        .unwrap_or_default()
        .match_indices('\n')
        .map(|(at, _)| range.start + at + 1)
        .collect();
    if ends.last() != Some(&range.end) {
        ends.push(range.end);
    }
    ends
}

/// Every offset in `range` at which a line starts: `range.start`, then each
/// offset just after a line break before `range.end`.
fn line_starts(text: &str, range: &Range<usize>) -> Vec<usize> {
    let mut starts = vec![range.start];
    starts.extend(
        text.get(range.clone())
            .unwrap_or_default()
            .match_indices('\n')
            .map(|(at, _)| range.start + at + 1)
            .filter(|start| *start < range.end),
    );
    starts
}

impl JournalEntry {
    /// The entry's full text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn sections(&self) -> &[EntrySection] {
        &self.sections
    }

    /// The content of the section at `index`, as shown for editing: its
    /// editable bytes without the blank lines around them.
    #[must_use]
    pub fn content(&self, index: usize) -> Option<&str> {
        let section = self.sections.get(index)?;
        Some(shown(self.text.get(section.editable.clone()).unwrap_or_default()))
    }

    /// The version of the section at `index`, for an editor to send back.
    #[must_use]
    pub fn fingerprint(&self, index: usize) -> Option<SectionFingerprint> {
        self.content(index)
            .map(|content| SectionFingerprint::from(ShownContent(content)))
    }

    /// The entry's text with `edit` applied.
    ///
    /// # Errors
    ///
    /// [`SectionEditRefused::Moved`] when no section headed `edit.heading` is
    /// at `edit.index`; [`SectionEditRefused::Changed`] when that section's
    /// content is no longer the version `edit.base` names.
    pub fn with_section_replaced(&self, edit: &SectionEdit<'_>) -> Result<String, SectionEditRefused> {
        let section = self
            .sections
            .get(edit.index)
            .filter(|section| section.heading == edit.heading)
            .ok_or(SectionEditRefused::Moved)?;
        if self.fingerprint(edit.index).as_ref() != Some(edit.base) {
            return Err(SectionEditRefused::Changed);
        }
        let submitted = normalised(edit.content);
        let text = self.with_region_written(&section.editable, shown(&submitted));
        self.checked(text, edit.index)
    }

    /// `text`, a copy of the entry with the section at `index` changed, when
    /// every other section still reads as it did: the same heading, guidance,
    /// and content, in the same order. The changed section may have become
    /// several (content with headings of its own); `index` at or past the last
    /// section's place stands for a section added at the end.
    ///
    /// # Errors
    ///
    /// [`SectionEditRefused::Unbalanced`] otherwise.
    fn checked(&self, text: String, index: usize) -> Result<String, SectionEditRefused> {
        let after = JournalEntry::from(EntryInput {
            text: &text,
            template: &self.template,
        });
        let before = self.sections.len();
        // An entry with no sections stands as one whole-body section; the
        // first heading added to it starts the sections rather than replacing
        // one, so there is no earlier section to keep.
        let whole_body = self.sections.first().is_some_and(|section| section.whole_body);
        let earlier = if whole_body { 0 } else { index.min(before) };
        let later = before.saturating_sub(index.saturating_add(1));
        let reads_the_same = |old: usize, new: usize| {
            self.sections
                .get(old)
                .map(|section| (&section.heading, &section.guidance))
                == after
                    .sections
                    .get(new)
                    .map(|section| (&section.heading, &section.guidance))
                && self.content(old) == after.content(new)
        };
        let count = after.sections.len();
        let kept = count > earlier + later
            && (0..earlier).all(|position| reads_the_same(position, position))
            && (1..=later).all(|offset| reads_the_same(before - offset, count - offset));
        if kept {
            Ok(text)
        } else {
            Err(SectionEditRefused::Unbalanced)
        }
    }

    /// The entry's text with `item` added as the last list item of the first
    /// section headed `heading`, in place of any empty list items (a
    /// template's placeholder) the section ends with. An entry with no such
    /// section gains one, at the end. Line breaks in `item` become spaces, so
    /// it stays one item. `heading` is matched as a configured name
    /// ([`HeadingText`]).
    ///
    /// # Errors
    ///
    /// [`SectionEditRefused::Unbalanced`] when the item would change how the
    /// rest of the entry reads.
    pub fn with_item_appended(&self, heading: &str, item: &str) -> Result<String, SectionEditRefused> {
        let item = item.split_whitespace().collect::<Vec<_>>().join(" ");
        let name = HeadingText::from(heading);
        let Some((index, section)) = self
            .sections
            .iter()
            .enumerate()
            .find(|(_, section)| !section.whole_body && name.names(&section.heading))
        else {
            let mut text = self.text.clone();
            if !text.is_empty() {
                if !text.ends_with('\n') {
                    text.push('\n');
                }
                if !text.ends_with("\n\n") {
                    text.push('\n');
                }
            }
            let _ = write!(text, "## {}\n\n- {item}\n", heading.trim());
            let text = if self.text.contains("\r\n") {
                let added = text.get(self.text.len()..).unwrap_or_default();
                format!("{}{}", self.text, normalised(added).replace('\n', "\r\n"))
            } else {
                text
            };
            return self.checked(text, self.sections.len());
        };
        let region = section.editable.start..section.closing;
        let current = normalised(shown(self.text.get(region.clone()).unwrap_or_default()));
        let mut kept: Vec<&str> = current.lines().collect();
        while kept
            .last()
            .is_some_and(|line| line.trim().is_empty() || is_empty_list_item(line))
        {
            kept.pop();
        }
        let mut content = kept.join("\n");
        if let Some(last) = kept.last() {
            content.push_str(if is_list_item(last) { "\n" } else { "\n\n" });
        }
        let _ = write!(content, "- {item}");
        let text = self.with_region_written(&region, &content);
        self.checked(text, index)
    }

    /// The entry's text with the bytes of `region` replaced by `content`,
    /// set off by a blank line before it, and one after when anything
    /// follows, so it never runs into the guidance above it or turns the rule
    /// below it into a heading. Content equal to what is there already
    /// changes nothing.
    fn with_region_written(&self, region: &Range<usize>, content: &str) -> String {
        let current = self.text.get(region.clone()).unwrap_or_default();
        if normalised(shown(current)) == content {
            return self.text.clone();
        }
        let followed = region.end < self.text.len();
        // Text running straight on from the line before (guidance at the very
        // end of a file with no final line break) is ended first.
        let before = self.text.get(..region.start).unwrap_or_default();
        let opening = if before.is_empty() || before.ends_with('\n') {
            "\n"
        } else {
            "\n\n"
        };
        let replacement = if content.is_empty() {
            opening.to_owned()
        } else if followed {
            format!("{opening}{content}\n\n")
        } else {
            format!("{opening}{content}\n")
        };
        let replacement = if self.text.contains("\r\n") {
            replacement.replace('\n', "\r\n")
        } else {
            replacement
        };
        let mut text = String::with_capacity(self.text.len() + replacement.len());
        text.push_str(self.text.get(..region.start).unwrap_or_default());
        text.push_str(&replacement);
        text.push_str(self.text.get(region.end..).unwrap_or_default());
        text
    }
}

/// `region` without its leading blank lines and trailing whitespace.
fn shown(region: &str) -> &str {
    let trimmed = region.trim_end();
    let mut start = 0;
    for line in trimmed.split_inclusive('\n') {
        if !line.trim().is_empty() {
            break;
        }
        start += line.len();
    }
    trimmed.get(start..).unwrap_or_default()
}

/// `text` with Windows line endings made Unix ones.
fn normalised(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// The marker a list item line opens with (`-`, `*`, `+`, `1.`, `1)`), and
/// what follows it, or `None` for any other line.
fn list_item_rest(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let rest = if let Some(rest) = trimmed.strip_prefix(['-', '*', '+']) {
        rest
    } else {
        let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        trimmed.get(digits..)?.strip_prefix(['.', ')'])?
    };
    (rest.is_empty() || rest.starts_with(char::is_whitespace)).then_some(rest)
}

fn is_list_item(line: &str) -> bool {
    list_item_rest(line).is_some()
}

/// A list item with nothing in it, a task box at most: a template's
/// placeholder for an item yet to be written.
fn is_empty_list_item(line: &str) -> bool {
    list_item_rest(line).is_some_and(|rest| {
        let rest = rest.trim();
        rest.is_empty() || matches!(rest, "[ ]" | "[x]" | "[X]" | "[]")
    })
}

#[cfg(test)]
#[path = "journal_entry_test.rs"]
mod tests;
