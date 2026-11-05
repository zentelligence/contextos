//! The body `note_create` writes: either text the caller supplies, or the body of another vault file that the server
//! copies verbatim, so content that only needs copying (a source filed into `memory/raw/`, say) never passes through
//! the caller and cannot be altered on the way.

use std::path::PathBuf;

use contextos_core::{ContentHash, VaultPath, VaultPathInput, VaultSet};
use contextos_fs::{Filesystem, FsError, ReadTextRequest};
use contextos_obsidian::FrontmatterDocument;

use crate::tool_error::ToolError;
use crate::tools::fs_expand::Expandable;
use crate::tools::obsidian_types::NoteContentSourceInput;

/// The unvalidated choice of body a `note_create` call made.
pub(crate) struct NoteBodyInput<'a> {
    pub(crate) roots: &'a VaultSet,
    pub(crate) content: Option<String>,
    pub(crate) content_from: Option<NoteContentSourceInput>,
}

/// Where a new note's body comes from: exactly one of the caller's text or a confined vault file.
pub(crate) enum NoteBody {
    Supplied(String),
    CopiedFrom {
        path: VaultPath,
        expected_hash: Option<ContentHash>,
    },
}

impl TryFrom<NoteBodyInput<'_>> for NoteBody {
    type Error = ToolError;

    fn try_from(value: NoteBodyInput<'_>) -> Result<Self, Self::Error> {
        match (value.content, value.content_from) {
            (Some(content), None) => Ok(Self::Supplied(content)),
            (None, Some(source)) => Ok(Self::CopiedFrom {
                path: VaultPath::try_from(VaultPathInput {
                    roots: value.roots,
                    raw: &source.path,
                })?,
                expected_hash: source.expected_hash.map(ContentHash::try_from).transpose()?,
            }),
            (Some(_), Some(_)) => Err(ToolError::Invalid("give content or content_from, not both")),
            (None, None) => Err(ToolError::Invalid("note_create needs content or content_from")),
        }
    }
}

impl NoteBody {
    /// Produces the body text. A copied source is read whole, within its vault's read limit, and refused if its hash
    /// differs from the one the caller expected. A Markdown source's frontmatter is left behind, since the note's own
    /// properties come from the call; any other text file has no frontmatter, so it is copied whole even when its
    /// first line is `---`.
    pub(crate) fn resolve(self, filesystem: &Filesystem) -> Result<String, ToolError> {
        let (path, expected_hash) = match self {
            Self::Supplied(content) => return Ok(content),
            Self::CopiedFrom { path, expected_hash } => (path, expected_hash),
        };
        let source = filesystem.read_text(&ReadTextRequest {
            path: path.clone(),
            limit: None,
        })?;
        if let Some(expected_hash) = expected_hash
            && source.content_hash != expected_hash
        {
            return Err(FsError::Conflict {
                current_hash: Some(source.content_hash),
                path: PathBuf::from(path),
            }
            .into());
        }
        // The same rule every frontmatter tool uses to recognise a note, so a `.MD` source is a note here too.
        if !matches!(Expandable::try_from(&path), Ok(Expandable::Note)) {
            return Ok(source.content);
        }
        Ok(FrontmatterDocument::try_from(source.content.as_str())?
            .body()
            .to_owned())
    }
}
