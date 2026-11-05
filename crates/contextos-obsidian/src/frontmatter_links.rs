//! Adding and removing wikilinks in a declared link field of a note's
//! frontmatter, as pure functions over the frontmatter map: the note is
//! read and written by the caller. Links are written as quoted wikilinks
//! with display text, `[[target|Display]]`, the form Obsidian renders as a
//! property link and keeps current when a note is renamed.

use serde_json::{Map, Value};
use thiserror::Error;

use crate::{LinkCollection, LinkField};

/// Characters that cannot appear in a wikilink's target (Obsidian forbids
/// them in note names for exactly this reason).
pub(crate) const TARGET_FORBIDDEN: &[char] = &['[', ']', '|', '#', '^', '\n', '\r'];
/// Characters that cannot appear in a wikilink's display text.
const DISPLAY_FORBIDDEN: &[char] = &['[', ']', '|', '\n', '\r'];

/// Typed link-editing failures, all with stable `link/` codes.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum LinkError {
    #[error("link field {field:?} is not declared by the note's base (declared: {})", .declared.join(", "))]
    UndeclaredField { field: String, declared: Vec<String> },
    #[error("no base whose scope contains this note declares link_fields in its sidecar")]
    NoLinkFields,
    #[error("more than one base of equal scope contains this note and declares link_fields: {}", .bases.join(", "))]
    AmbiguousBase { bases: Vec<String> },
    #[error("link target {target:?} was not found in this vault")]
    TargetNotFound { target: String },
    #[error("link target {target:?} matches more than one note: {}", .matches.join(", "))]
    TargetAmbiguous { target: String, matches: Vec<String> },
    #[error("a note cannot link to itself")]
    SelfReference,
    #[error("link field {field:?} holds a single link and is already set to {existing}")]
    SingleValued { field: String, existing: String },
    #[error("frontmatter field {field:?} does not hold a link or a list of links")]
    MalformedField { field: String },
    #[error("{text:?} cannot be written as a wikilink")]
    UnlinkableText { text: String },
}

impl LinkError {
    /// Returns the stable machine-readable error code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UndeclaredField { .. } => "link/undeclared-field",
            Self::NoLinkFields => "link/no-link-fields",
            Self::AmbiguousBase { .. } => "link/ambiguous-base",
            Self::TargetNotFound { .. } => "link/target-not-found",
            Self::TargetAmbiguous { .. } => "link/target-ambiguous",
            Self::SelfReference => "link/self-reference",
            Self::SingleValued { .. } => "link/single-valued",
            Self::MalformedField { .. } => "link/malformed-field",
            Self::UnlinkableText { .. } => "link/unlinkable-text",
        }
    }

    /// Returns an actionable correction for this failure.
    #[must_use]
    pub const fn remediation(&self) -> &'static str {
        match self {
            Self::UndeclaredField { .. } => {
                "Use one of the declared link fields, or declare the field under link_fields in the base's sidecar."
            }
            Self::NoLinkFields => "Declare link_fields in the sidecar of the base that governs this note, and retry.",
            Self::AmbiguousBase { .. } => {
                "Give the bases distinct scopes, or declare link_fields in only one of them, and retry."
            }
            Self::TargetNotFound { .. } => {
                "Pass the id of a record in this note's base, or the vault path of an existing note."
            }
            Self::TargetAmbiguous { .. } => "Pass the target's vault path instead of its id.",
            Self::SelfReference => "Choose a different target note.",
            Self::SingleValued { .. } => "Remove the existing link first, then add the new one.",
            Self::MalformedField { .. } => {
                "Correct the field to a wikilink string or a list of them with frontmatter_update, and retry."
            }
            Self::UnlinkableText { .. } => {
                "Rename the note so that its name and id avoid the characters [ ] | # ^ and line breaks, or link it by another route."
            }
        }
    }
}

/// A wikilink as written into a link field: `[[target|display]]`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WikiLink(String);

/// The parts of a wikilink to write.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WikiLinkInput<'a> {
    /// The link target: a note name, or a vault path without the extension.
    pub target: &'a str,
    /// The text shown for the link.
    pub display: &'a str,
}

fn unlinkable(text: &str) -> LinkError {
    LinkError::UnlinkableText { text: text.to_owned() }
}

impl TryFrom<WikiLinkInput<'_>> for WikiLink {
    type Error = LinkError;

    fn try_from(value: WikiLinkInput<'_>) -> Result<Self, Self::Error> {
        if value.target.is_empty() || value.target.contains(TARGET_FORBIDDEN) {
            return Err(unlinkable(value.target));
        }
        if value.display.is_empty() || value.display.contains(DISPLAY_FORBIDDEN) {
            return Err(unlinkable(value.display));
        }
        Ok(Self(format!("[[{}|{}]]", value.target, value.display)))
    }
}

impl AsRef<str> for WikiLink {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// The target of a stored link field entry: the note name or path inside
/// its `[[...]]`, without any display text, heading, or block reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinkTarget(String);

impl LinkTarget {
    /// Returns the target text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for LinkTarget {
    type Error = LinkError;

    /// Reads the target of `entry`, which must be exactly one plain
    /// wikilink (surrounding whitespace aside).
    fn try_from(entry: &str) -> Result<Self, Self::Error> {
        let trimmed = entry.trim();
        let whole_link = trimmed.starts_with("[[") && trimmed.ends_with("]]") && trimmed.matches("[[").count() == 1;
        let collection = LinkCollection::try_from(trimmed).map_err(|_| unlinkable(entry))?;
        match collection.outgoing() {
            [link] if whole_link && !link.embed() && !link.target.is_empty() => Ok(Self(link.target.clone())),
            _ => Err(unlinkable(entry)),
        }
    }
}

/// What an add or remove did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkOutcome {
    Added,
    AlreadyPresent,
    Removed,
    Absent,
}

/// The result of one edit: what happened, and the field's new value to merge
/// into the frontmatter (`Value::Null` deletes the field), or `None` when
/// nothing changed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinkEdit {
    pub outcome: LinkOutcome,
    pub value: Option<Value>,
}

impl LinkEdit {
    const fn unchanged(outcome: LinkOutcome) -> Self {
        Self { outcome, value: None }
    }
}

impl LinkField {
    /// Returns the field's current entries: none when it is missing, null,
    /// or blank; the one link when it holds a single string; each element
    /// of a list of strings.
    ///
    /// # Errors
    ///
    /// Returns [`LinkError::MalformedField`] when the field holds anything
    /// else.
    pub fn entries(&self, frontmatter: &Map<String, Value>) -> Result<Vec<String>, LinkError> {
        let malformed = || LinkError::MalformedField {
            field: self.name().to_owned(),
        };
        match frontmatter.get(self.name()) {
            None | Some(Value::Null) => Ok(Vec::new()),
            Some(Value::String(text)) => Ok(if text.trim().is_empty() {
                Vec::new()
            } else {
                vec![text.clone()]
            }),
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| item.as_str().map(str::to_owned).ok_or_else(malformed))
                .collect(),
            Some(_) => Err(malformed()),
        }
    }

    /// Adds `link`, unless an entry for which `same` is true is already
    /// there, in which case nothing changes. Existing entries are kept
    /// exactly as written.
    ///
    /// # Errors
    ///
    /// Returns [`LinkError::MalformedField`] for an unreadable field, and
    /// [`LinkError::SingleValued`] when a single-valued field already holds
    /// a different link.
    pub fn with_link(
        &self,
        frontmatter: &Map<String, Value>,
        link: &WikiLink,
        same: &dyn Fn(&str) -> bool,
    ) -> Result<LinkEdit, LinkError> {
        let mut entries = self.entries(frontmatter)?;
        if entries.iter().any(|entry| same(entry)) {
            return Ok(LinkEdit::unchanged(LinkOutcome::AlreadyPresent));
        }
        if self.single() {
            if let Some(existing) = entries.into_iter().next() {
                return Err(LinkError::SingleValued {
                    field: self.name().to_owned(),
                    existing,
                });
            }
            return Ok(LinkEdit {
                outcome: LinkOutcome::Added,
                value: Some(Value::String(link.as_ref().to_owned())),
            });
        }
        entries.push(link.as_ref().to_owned());
        Ok(LinkEdit {
            outcome: LinkOutcome::Added,
            value: Some(Value::Array(entries.into_iter().map(Value::String).collect())),
        })
    }

    /// Removes every entry for which `same` is true. A list keeps its other
    /// entries (an emptied list stays as `[]`); a single-valued field is
    /// deleted. Removing what is not there changes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`LinkError::MalformedField`] for an unreadable field.
    pub fn without_links(
        &self,
        frontmatter: &Map<String, Value>,
        same: &dyn Fn(&str) -> bool,
    ) -> Result<LinkEdit, LinkError> {
        let entries = self.entries(frontmatter)?;
        let remaining: Vec<String> = entries.iter().filter(|entry| !same(entry)).cloned().collect();
        if remaining.len() == entries.len() {
            return Ok(LinkEdit::unchanged(LinkOutcome::Absent));
        }
        let value = if self.single() {
            remaining.into_iter().next().map_or(Value::Null, Value::String)
        } else {
            Value::Array(remaining.into_iter().map(Value::String).collect())
        };
        Ok(LinkEdit {
            outcome: LinkOutcome::Removed,
            value: Some(value),
        })
    }
}

#[cfg(test)]
#[path = "frontmatter_links_test.rs"]
mod tests;
