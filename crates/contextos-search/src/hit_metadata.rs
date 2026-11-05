//! Descriptive metadata for search hits, read from each hit's source file.
//!
//! The text index does not store frontmatter and the vector store holds only
//! vectors, so a hit's description, summary, status and the like come from
//! the note itself, and only for the results actually returned. This mirrors
//! how semantic search re-reads chunk text at query time, and it needs no
//! index schema change.
//!
//! Reading is best effort by design: a note that has vanished, moved outside
//! the vault, or cannot be parsed yields empty metadata, never an error, so
//! one unreadable hit cannot fail a whole result page.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use contextos_core::{VaultPath, VaultPathInput, VaultRoot, VaultSet};
use serde_json::{Map, Value};
use time::OffsetDateTime;

use crate::{DocumentSource, IndexedDocument};

/// The most bytes read from the start of a note when looking for
/// frontmatter. A frontmatter block that does not close inside this limit is
/// treated as absent, so a huge or hostile file cannot exhaust memory.
pub const METADATA_READ_LIMIT: usize = 64 * 1024;

/// Descriptive fields for one hit. Every field is optional: a note simply
/// may not declare it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HitMetadata {
    /// The note's title as the text index derives it: frontmatter `title`,
    /// then the first heading, then the file name.
    pub title: Option<String>,
    pub description: Option<String>,
    pub summary: Option<String>,
    pub excerpt: Option<String>,
    pub status: Option<String>,
    /// The frontmatter `type` field.
    pub note_type: Option<String>,
    /// The frontmatter `updated` value exactly as written.
    pub updated: Option<String>,
    /// The file's modification time.
    pub modified: Option<OffsetDateTime>,
}

/// Port for reading a hit's descriptive metadata.
pub trait ReadsHitMetadata: Send + Sync {
    /// Reads metadata for the note at the forward-slash relative `path`.
    ///
    /// Never fails: an unreadable, missing, or out-of-vault note yields
    /// [`HitMetadata::default`].
    fn read(&self, path: &str) -> HitMetadata;
}

/// Reads hit metadata from the vault's files, confined to its root.
///
/// Construction cannot fail. The root is resolved on first successful use, and
/// while it cannot be resolved every read yields empty metadata.
pub struct FilesystemHitMetadata {
    root: PathBuf,
    roots: OnceLock<VaultSet>,
}

impl From<PathBuf> for FilesystemHitMetadata {
    fn from(root: PathBuf) -> Self {
        Self {
            root,
            roots: OnceLock::new(),
        }
    }
}

impl FilesystemHitMetadata {
    /// The vault's root set, resolved on first successful use. A failure is
    /// not remembered, so a vault root that is missing now can be read once it
    /// appears.
    fn resolved_roots(&self) -> Option<&VaultSet> {
        if let Some(roots) = self.roots.get() {
            return Some(roots);
        }
        let vault_root = VaultRoot::try_from(self.root.clone()).ok()?;
        let roots = VaultSet::try_from(vec![vault_root]).ok()?;
        Some(self.roots.get_or_init(|| roots))
    }
}

impl ReadsHitMetadata for FilesystemHitMetadata {
    fn read(&self, path: &str) -> HitMetadata {
        // `VaultPath` construction is the confinement boundary: traversal,
        // absolute paths, and symlinks resolving outside the root are all
        // rejected here, and a rejection is just "no metadata".
        let Some(roots) = self.resolved_roots() else {
            return HitMetadata::default();
        };
        let Ok(vault_path) = VaultPath::try_from(VaultPathInput { roots, raw: path }) else {
            return HitMetadata::default();
        };
        let absolute: &Path = (&vault_path).into();
        // Only regular files: opening a FIFO or device node named like a note
        // could block the query indefinitely.
        if !std::fs::metadata(absolute).is_ok_and(|metadata| metadata.is_file()) {
            return HitMetadata::default();
        }
        // Checked again on the open handle, without waiting, in case the path was replaced since.
        let Ok((file, metadata)) = contextos_fs::open_regular_file(absolute, std::fs::OpenOptions::new().read(true))
        else {
            return HitMetadata::default();
        };
        let modified = metadata.modified().ok().map(OffsetDateTime::from);

        let mut leading = Vec::new();
        let limit = u64::try_from(METADATA_READ_LIMIT).unwrap_or(u64::MAX);
        if file.take(limit).read_to_end(&mut leading).is_err() {
            return HitMetadata::default();
        }
        let content = String::from_utf8_lossy(&leading);

        let document = IndexedDocument::from(DocumentSource {
            path: &vault_path,
            content: &content,
            modified: modified.unwrap_or(OffsetDateTime::UNIX_EPOCH),
        });
        let frontmatter = document.frontmatter();
        HitMetadata {
            title: Some(document.title().to_owned()).filter(|title| !title.is_empty()),
            description: text_field(frontmatter, "description"),
            summary: text_field(frontmatter, "summary"),
            excerpt: text_field(frontmatter, "excerpt"),
            status: text_field(frontmatter, "status"),
            note_type: text_field(frontmatter, "type"),
            updated: text_field(frontmatter, "updated"),
            modified,
        }
    }
}

/// Returns a frontmatter value only when it is a non-blank string, so a
/// number, list, or empty value is reported as absent rather than repaired.
fn text_field(frontmatter: &Map<String, Value>, key: &str) -> Option<String> {
    frontmatter
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}
