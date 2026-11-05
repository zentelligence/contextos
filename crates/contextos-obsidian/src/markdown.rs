use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::link_scan::{
    Piece, ReferenceDefinition, ReferencePiece, Scanner, StandardPiece, WikilinkPiece, normalised_label,
};

/// How a link was written: a wikilink or a Markdown link, and whether it embeds its target.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LinkForm {
    /// `[[target]]`.
    Wikilink,
    /// `![[target]]`.
    WikilinkEmbed,
    /// `[text](path)` or `[text][label]`.
    Markdown,
    /// `![alt](path)` or `![alt][label]`.
    MarkdownEmbed,
}

impl LinkForm {
    /// The kebab-case name used on the wire and in stored records.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Wikilink => "wikilink",
            Self::WikilinkEmbed => "wikilink-embed",
            Self::Markdown => "markdown",
            Self::MarkdownEmbed => "markdown-embed",
        }
    }

    /// Whether the link embeds its target (`![[...]]` or `![...](...)`).
    #[must_use]
    pub const fn is_embed(self) -> bool {
        matches!(self, Self::WikilinkEmbed | Self::MarkdownEmbed)
    }

    /// Whether the link was written as a wikilink.
    #[must_use]
    pub const fn is_wikilink(self) -> bool {
        matches!(self, Self::Wikilink | Self::WikilinkEmbed)
    }
}

/// Whether `target`, written in a link of `form`, is a template placeholder and not a real destination: a wikilink
/// target holding a `{{...}}` field, or one written wholly in angle brackets (`[[<slug>]]`). In a Markdown link the
/// angle brackets around a path are syntax, so only the `{{...}}` field marks a placeholder there.
#[must_use]
pub fn is_placeholder_target(form: LinkForm, target: &str) -> bool {
    let target = target.trim();
    let field = target.find("{{").is_some_and(|open| {
        target
            .get(open.saturating_add(2)..)
            .is_some_and(|rest| rest.contains("}}"))
    });
    field || (form.is_wikilink() && target.len() > 1 && target.starts_with('<') && target.ends_with('>'))
}

/// One outgoing link read from a note, in any of the four forms.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObsidianLink {
    /// How the link was written.
    pub form: LinkForm,
    /// The destination without any heading or block reference: the wikilink's target, or the Markdown link's path with
    /// URL encoding decoded. Resolved by [`crate::resolve_link`].
    pub target: String,
    /// The display text of a wikilink (`[[target|display]]`) or the text of a Markdown link.
    pub display: Option<String>,
    /// The heading the link points into, when it has one.
    pub heading: Option<String>,
    /// The block the link points to, when it has one.
    pub block: Option<String>,
    /// The 1-based line the link is written on. A reference-style link is reported at the line that uses it.
    pub line: usize,
    /// The link exactly as written in the note, `[[...]]`, `![[...]]`, `[text](...)`, or `[text][label]`.
    pub raw: String,
}

impl ObsidianLink {
    /// Whether the link embeds its target.
    #[must_use]
    pub const fn embed(&self) -> bool {
        self.form.is_embed()
    }

    /// Whether the target is a template placeholder rather than a real destination; see [`is_placeholder_target`].
    #[must_use]
    pub fn is_placeholder(&self) -> bool {
        is_placeholder_target(self.form, &self.target)
    }
}

/// What a link written between `[[` and `]]` says: a destination, an optional anchor, and optional display text.
struct WikilinkText<'a> {
    destination: &'a str,
    display: Option<&'a str>,
}

impl<'a> From<&'a str> for WikilinkText<'a> {
    /// Splits at the first `|`. A `|` written `\|`, as a table row needs, ends the destination all the same, the
    /// backslash being the table's and not the link's.
    fn from(inner: &'a str) -> Self {
        match inner.split_once('|') {
            Some((before, display)) => Self {
                destination: before.strip_suffix('\\').unwrap_or(before).trim(),
                display: Some(display.trim()),
            },
            None => Self {
                destination: inner.trim(),
                display: None,
            },
        }
    }
}

/// A destination split into the file it names and where it points within it.
struct Anchored {
    target: String,
    heading: Option<String>,
    block: Option<String>,
}

impl Anchored {
    /// Splits `destination` at its first `#`. `^id` after it is a block reference and anything else a heading.
    fn of(destination: &str) -> Self {
        match destination.split_once('#') {
            Some((target, anchor)) if anchor.starts_with('^') => Self {
                target: target.trim().to_owned(),
                heading: None,
                block: Some(anchor.trim_start_matches('^').to_owned()),
            },
            Some((target, heading)) => Self {
                target: target.trim().to_owned(),
                heading: Some(heading.to_owned()),
                block: None,
            },
            None => Self {
                target: destination.trim().to_owned(),
                heading: None,
                block: None,
            },
        }
    }

    const fn has_anchor(&self) -> bool {
        matches!(&self.heading, Some(anchor) if !anchor.is_empty())
            || matches!(&self.block, Some(anchor) if !anchor.is_empty())
    }
}

/// The link, if any, that a wikilink piece makes. A wikilink to a URL or to a heading or block of its own note names no
/// other file, so it makes none.
struct WikilinkRead(Option<ObsidianLink>);

impl TryFrom<WikilinkPiece<'_>> for WikilinkRead {
    type Error = MarkdownError;

    fn try_from(piece: WikilinkPiece<'_>) -> Result<Self, Self::Error> {
        let text = WikilinkText::from(piece.inner);
        let anchored = Anchored::of(text.destination);
        if anchored.target.is_empty() && !anchored.has_anchor() {
            return Err(MarkdownError::EmptyLinkTarget { line: piece.line });
        }
        // A wikilink names a note, never an address, so a target that looks like a URL scheme (`[[Re: meeting]]`) is a
        // note that may not exist. Only a link to a heading or block of its own note names no other file.
        if anchored.target.is_empty() {
            return Ok(Self(None));
        }
        Ok(Self(Some(ObsidianLink {
            form: if piece.embed {
                LinkForm::WikilinkEmbed
            } else {
                LinkForm::Wikilink
            },
            target: anchored.target,
            display: text.display.map(str::to_owned),
            heading: anchored.heading,
            block: anchored.block,
            line: piece.line,
            raw: piece.raw.to_owned(),
        })))
    }
}

/// Where a reference-style link's label points, as its definition says.
struct ReferenceTarget {
    anchored: Option<Anchored>,
}

impl From<&ReferenceDefinition<'_>> for ReferenceTarget {
    fn from(definition: &ReferenceDefinition<'_>) -> Self {
        let destination = &definition.destination;
        Self {
            anchored: destination.vault_target().map(|target| Anchored {
                target,
                heading: destination
                    .path_and_fragment()
                    .1
                    .strip_prefix('#')
                    .filter(|anchor| !anchor.starts_with('^'))
                    .map(str::to_owned),
                block: destination.path_and_fragment().1.strip_prefix("#^").map(str::to_owned),
            }),
        }
    }
}

/// The link, if any, that an inline Markdown link makes.
struct StandardRead(Option<ObsidianLink>);

impl From<StandardPiece<'_>> for StandardRead {
    fn from(piece: StandardPiece<'_>) -> Self {
        let destination = &piece.link.destination;
        let Some(target) = destination.vault_target() else {
            return Self(None);
        };
        let fragment = destination.path_and_fragment().1;
        Self(Some(ObsidianLink {
            form: if piece.embed {
                LinkForm::MarkdownEmbed
            } else {
                LinkForm::Markdown
            },
            target,
            display: Some(piece.link.text.to_owned()),
            heading: fragment
                .strip_prefix('#')
                .filter(|anchor| !anchor.starts_with('^'))
                .map(str::to_owned),
            block: fragment.strip_prefix("#^").map(str::to_owned),
            line: piece.line,
            raw: piece.raw.to_owned(),
        }))
    }
}

/// A link met while reading, in source order, that may still wait for a definition later in the note.
enum Candidate<'a> {
    Link(ObsidianLink),
    Reference(ReferencePiece<'a>),
}

/// Outgoing links extracted from an Obsidian Markdown source document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinkCollection {
    outgoing: Vec<ObsidianLink>,
}

impl TryFrom<&str> for LinkCollection {
    type Error = MarkdownError;

    /// Reads every wikilink, embed, inline Markdown link, and reference-style link in `source`, in source order,
    /// frontmatter included. Text that merely resembles a link (an unterminated `[text](`, a `[text][label]` with no
    /// definition) is text. A wikilink left open, or empty, is an error: a note carrying one is refused rather than
    /// written with a link nothing can follow.
    fn try_from(source: &str) -> Result<Self, Self::Error> {
        Self::read(Scanner::from(source)).map(|(links, _)| links)
    }
}

impl LinkCollection {
    /// Reads the links `scanner` finds, and gives the scanner back so that what it learned of the document's blocks can
    /// be used too.
    fn read(mut scanner: Scanner<'_>) -> Result<(Self, Scanner<'_>), MarkdownError> {
        let mut candidates = Vec::new();
        let mut definitions: HashMap<String, ReferenceTarget> = HashMap::new();
        for piece in scanner.by_ref() {
            match piece {
                Piece::Text(_) => {}
                Piece::UnclosedWikilink { embed, line, .. } => return Err(MarkdownError::UnclosedLink { line, embed }),
                Piece::Wikilink(wikilink) => {
                    if let WikilinkRead(Some(link)) = WikilinkRead::try_from(wikilink)? {
                        candidates.push(Candidate::Link(link));
                    }
                }
                Piece::Standard(standard) => {
                    if let StandardRead(Some(link)) = StandardRead::from(standard) {
                        candidates.push(Candidate::Link(link));
                    }
                }
                Piece::Reference(reference) => candidates.push(Candidate::Reference(reference)),
                Piece::Definition(definition) => {
                    // The first definition of a label is the one that counts.
                    definitions
                        .entry(normalised_label(definition.label))
                        .or_insert_with(|| ReferenceTarget::from(&definition));
                }
            }
        }
        let outgoing = candidates
            .into_iter()
            .filter_map(|candidate| match candidate {
                Candidate::Link(link) => Some(link),
                Candidate::Reference(reference) => reference_link(&reference, &definitions),
            })
            .collect();
        Ok((Self { outgoing }, scanner))
    }
}

/// The link a reference-style piece makes, when the note defines its label to name a vault file.
fn reference_link(
    reference: &ReferencePiece<'_>,
    definitions: &HashMap<String, ReferenceTarget>,
) -> Option<ObsidianLink> {
    let label = reference.label.unwrap_or(reference.text);
    let anchored = definitions.get(&normalised_label(label))?.anchored.as_ref()?;
    Some(ObsidianLink {
        form: if reference.embed {
            LinkForm::MarkdownEmbed
        } else {
            LinkForm::Markdown
        },
        target: anchored.target.clone(),
        display: Some(reference.text.to_owned()),
        heading: anchored.heading.clone(),
        block: anchored.block.clone(),
        line: reference.line,
        raw: reference.raw.to_owned(),
    })
}

impl TryFrom<String> for LinkCollection {
    type Error = MarkdownError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_from(value.as_str())
    }
}

impl LinkCollection {
    /// Returns outgoing links in source order.
    #[must_use]
    pub fn outgoing(&self) -> &[ObsidianLink] {
        &self.outgoing
    }
}

/// Obsidian Markdown proven to contain well-formed links, embeds, and callouts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedMarkdown {
    links: LinkCollection,
}

impl TryFrom<&str> for ValidatedMarkdown {
    type Error = MarkdownError;

    fn try_from(source: &str) -> Result<Self, Self::Error> {
        let (links, scanner) = LinkCollection::read(Scanner::recording_prose(source))?;
        // Callouts are looked for only in the lines the scanner reads as prose, so code, comments, and frontmatter are
        // judged exactly as they are when links are read.
        for (line_number, line) in scanner.into_prose() {
            validate_callout(line.trim_end_matches(['\n', '\r']), line_number)?;
        }
        Ok(Self { links })
    }
}

impl TryFrom<String> for ValidatedMarkdown {
    type Error = MarkdownError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_from(value.as_str())
    }
}

impl ValidatedMarkdown {
    /// Returns the outgoing links discovered during validation.
    #[must_use]
    pub const fn links(&self) -> &LinkCollection {
        &self.links
    }
}

fn validate_callout(line: &str, line_number: usize) -> Result<(), MarkdownError> {
    let mut remaining = line.trim_start();
    while let Some(nested) = remaining.strip_prefix('>') {
        remaining = nested.trim_start();
    }
    let Some(declaration) = remaining.strip_prefix("[!") else {
        return Ok(());
    };
    let Some(closing) = declaration.find(']') else {
        return Err(MarkdownError::InvalidCallout { line: line_number });
    };
    let callout_type = &declaration[..closing];
    if callout_type.is_empty()
        || !callout_type
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(MarkdownError::InvalidCallout { line: line_number });
    }
    Ok(())
}

/// Typed Obsidian Markdown validation failures.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum MarkdownError {
    #[error("wikilink on line {line} has no closing brackets")]
    UnclosedLink { line: usize, embed: bool },
    #[error("wikilink on line {line} has no target, heading, or block")]
    EmptyLinkTarget { line: usize },
    #[error("callout declaration on line {line} is malformed")]
    InvalidCallout { line: usize },
}

impl MarkdownError {
    /// Returns the stable machine-readable error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        "format/markdown"
    }

    /// Returns an actionable correction for invalid Obsidian syntax.
    #[must_use]
    pub const fn remediation(&self) -> &'static str {
        "Correct the Obsidian wikilink or embed syntax and retry."
    }
}

#[cfg(test)]
#[path = "markdown_test.rs"]
mod tests;
