//! Updating an existing note's frontmatter and body in one write. The frontmatter is changed by an RFC 7386 merge
//! patch, the body is optionally replaced, and the note keeps what the server maintains: `created` stays as it is and
//! `updated` is stamped, so a patch may not name either. The title rules a new note is held to apply to whatever the
//! patch changes, and a replacement body is validated as Obsidian Markdown. Line endings are the write pipeline's:
//! every text write is stored with `\n` ones.
//! The frontmatter block is always written back from its parsed form, even when only the body changes, so YAML comments
//! and hand formatting in it are not kept.

use serde_json::{Map, Value};

use crate::{FrontmatterDocument, FrontmatterError, NoteError, ValidatedMarkdown};

/// Fields the server maintains on every note.
const MANAGED_FIELDS: [&str; 2] = ["created", "updated"];

/// Fields whose value may not repeat the title.
const SUMMARY_FIELDS: [&str; 3] = ["description", "summary", "excerpt"];

/// Inputs for one note update.
#[derive(Clone, Debug, PartialEq)]
pub struct NoteUpdateInput<'a> {
    /// The note as it stands.
    pub document: FrontmatterDocument,
    /// An RFC 7386 merge patch for the frontmatter; empty to leave it as it is.
    pub patch: Map<String, Value>,
    /// The replacement body, everything after the frontmatter's closing delimiter; `None` keeps the current body.
    pub body: Option<&'a str>,
    /// The `updated` stamp.
    pub timestamp: &'a str,
}

/// A validated, updated note, ready to be written in place of the one it was made from.
#[derive(Clone, Debug, PartialEq)]
pub struct NoteUpdate(FrontmatterDocument);

impl TryFrom<NoteUpdateInput<'_>> for NoteUpdate {
    type Error = NoteError;

    fn try_from(value: NoteUpdateInput<'_>) -> Result<Self, Self::Error> {
        if value.timestamp.trim().is_empty() {
            return Err(NoteError::EmptyTimestamp);
        }
        if let Some(field) = MANAGED_FIELDS
            .into_iter()
            .find(|field| value.patch.contains_key(*field))
        {
            return Err(NoteError::ManagedField { field });
        }
        match value.patch.get("title") {
            Some(Value::String(title)) if title.trim().is_empty() => return Err(NoteError::EmptyTitle),
            Some(Value::Null) => return Err(NoteError::EmptyTitle),
            Some(Value::String(_)) | None => {}
            Some(_) => return Err(NoteError::TitleNotText),
        }
        let touches_title = ["title"]
            .into_iter()
            .chain(SUMMARY_FIELDS)
            .any(|field| value.patch.contains_key(field));
        let mut document = value.document;
        if let Some(body) = value.body {
            ValidatedMarkdown::try_from(body)?;
            body.clone_into(&mut document.body);
        }
        document.apply_merge_patch(value.patch, value.timestamp);
        if touches_title
            && let Some(title) = document.frontmatter.get("title").and_then(Value::as_str)
            && let Some(field) = SUMMARY_FIELDS
                .into_iter()
                .find(|field| document.frontmatter.get(*field).and_then(Value::as_str) == Some(title))
        {
            return Err(NoteError::TitleCollision { field });
        }
        Ok(Self(document))
    }
}

impl TryFrom<NoteUpdate> for String {
    type Error = FrontmatterError;

    fn try_from(value: NoteUpdate) -> Result<Self, Self::Error> {
        Self::try_from(&value.0)
    }
}
