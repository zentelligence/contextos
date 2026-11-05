//! Paths inside a store: the store-relative paths the engine hands to [`crate::StoreIo`], the release-relative paths a
//! manifest lists, and the names of the entries at the top of a store.

use std::fmt;

use crate::error::PackagesError;
use crate::name::{OperationId, ReleaseKey, ReleaseName};

/// The file in each release folder that describes the release. Never listed in itself, and never part of the digest.
pub const MANIFEST_FILE_NAME: &str = "manifest.toml";

/// The file at the top of a store that records every release ever installed.
pub const LEDGER_FILE_NAME: &str = "ledger.toml";

/// The advisory lock file directly under a store's root, which serialises every process changing the store.
pub const LOCK_FILE_NAME: &str = ".lock";

/// The ticket queue folder beside the lock file, kept so waiting processes are served in turn.
pub const LOCK_QUEUE_NAME: &str = ".lock.queue";

/// The file beside the lock file that guards the queue.
pub const LOCK_QUEUE_GUARD_NAME: &str = ".lock.queue.lock";

/// The prefix of a staging folder's name, `_staging-{id}`, where an install or repair is written before one rename
/// makes it visible.
pub const STAGING_PREFIX: &str = "_staging-";

/// The prefix of a removal folder's name, `_removing-{id}`, where a release is renamed before it is deleted.
pub const REMOVING_PREFIX: &str = "_removing-";

/// A path relative to a store's root, `/`-separated, each segment one plain folder or file name with no traversal.
///
/// Built only by the engine, from validated parts: a release folder from a [`crate::ReleaseKey`], a file in it from a
/// [`ReleaseFilePath`], a staging or removal folder from an [`OperationId`], and the ledger. An adapter maps it onto
/// its store (a folder under the host store's root, or a [`contextos_core::VaultPath`] under `.contextos/packages/`)
/// and never builds one from text it has read.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StorePath(Vec<String>);

impl StorePath {
    /// The path's segments, top first. Empty for the store's root.
    #[must_use]
    pub fn segments(&self) -> &[String] {
        &self.0
    }
}

/// A path a manifest lists, relative to its release folder: `/`-separated, each segment non-empty, not `.` or `..`,
/// holding no `\`, `:`, or control character, and not ending in a dot or a space; never `manifest.toml` at the top.
///
/// Built with `TryFrom<&str>`; a path that fails is [`crate::PackagesError::InvalidManifest`] when a manifest lists
/// it, and an [`crate::IncompleteFinding::UnlistedEntry`] when it is found in a release folder. Orders segment by
/// segment, the order the digest takes files in, so a release hashes the same on every platform.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ReleaseFilePath(Vec<String>);

impl ReleaseFilePath {
    /// The path's segments, top first.
    #[must_use]
    pub fn segments(&self) -> &[String] {
        &self.0
    }
}

/// What one entry at the top of a store is, read from its name with `From<&str>`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoreEntryName {
    /// A package folder holding release folders.
    Package(ReleaseName),
    /// An install or repair's staging folder, `_staging-{id}`, left by a crash if seen outside an operation.
    Staging(OperationId),
    /// A release renamed aside for removal, `_removing-{id}`.
    Removing(OperationId),
    /// The store's ledger, [`LEDGER_FILE_NAME`].
    Ledger,
    /// The store's own lock: [`LOCK_FILE_NAME`], its queue folder [`LOCK_QUEUE_NAME`], or the queue's guard file
    /// [`LOCK_QUEUE_GUARD_NAME`]. Part of the store, never read as a release, and never changed by the engine.
    Lock,
    /// Any other entry. Never read as a release and never changed.
    Other(String),
}

impl StorePath {
    /// The store's root, which has no segments.
    #[must_use]
    pub fn root() -> Self {
        Self(Vec::new())
    }

    /// A staging folder, `_staging-{id}`, at the top of the store.
    #[must_use]
    pub fn staging_folder(id: &OperationId) -> Self {
        Self(vec![format!("{STAGING_PREFIX}{}", id.as_str())])
    }

    /// A removal folder, `_removing-{id}`, at the top of the store.
    #[must_use]
    pub fn removing_folder(id: &OperationId) -> Self {
        Self(vec![format!("{REMOVING_PREFIX}{}", id.as_str())])
    }

    /// The store's ledger file.
    #[must_use]
    pub fn ledger_file() -> Self {
        Self(vec![LEDGER_FILE_NAME.to_owned()])
    }

    /// The file a release lists, below this release or staging folder.
    #[must_use]
    pub fn within(&self, file: &ReleaseFilePath) -> Self {
        let mut segments = self.0.clone();
        segments.extend(file.0.iter().cloned());
        Self(segments)
    }

    /// The `manifest.toml` of the release or staging folder at this path.
    #[must_use]
    pub fn manifest(&self) -> Self {
        let mut segments = self.0.clone();
        segments.push(MANIFEST_FILE_NAME.to_owned());
        Self(segments)
    }

    /// This path without its last segment; none for the root.
    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        let (_, parent) = self.0.split_last()?;
        Some(Self(parent.to_vec()))
    }
}

impl From<&ReleaseName> for StorePath {
    /// A package's folder.
    fn from(name: &ReleaseName) -> Self {
        Self(vec![name.as_str().to_owned()])
    }
}

impl From<&ReleaseKey> for StorePath {
    /// A release's folder, `{package}/{version}`.
    fn from(key: &ReleaseKey) -> Self {
        Self(vec![key.name.as_str().to_owned(), key.version.to_string()])
    }
}

impl fmt::Display for StorePath {
    /// The path as store-relative text, for diagnostics.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0.join("/"))
    }
}

impl TryFrom<&str> for ReleaseFilePath {
    type Error = PackagesError;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        let refuse = |reason: &'static str| PackagesError::InvalidStorePath {
            raw: raw.to_owned(),
            reason,
        };
        if raw.is_empty() {
            return Err(refuse("a path is not empty"));
        }
        let mut segments = Vec::new();
        for segment in raw.split('/') {
            if segment.is_empty() {
                return Err(refuse("a path has no empty segment"));
            }
            if segment == "." || segment == ".." {
                return Err(refuse("a path has no `.` or `..` segment"));
            }
            if segment.contains(['\\', ':']) || segment.chars().any(char::is_control) {
                return Err(refuse("a path has no backslash, colon, or control character"));
            }
            if segment.ends_with(['.', ' ']) {
                return Err(refuse("a path segment does not end in a dot or a space"));
            }
            segments.push(segment.to_owned());
        }
        if segments.len() == 1 && segments[0] == MANIFEST_FILE_NAME {
            return Err(refuse("the manifest is not a listed file"));
        }
        Ok(Self(segments))
    }
}

impl fmt::Display for ReleaseFilePath {
    /// The path with `/` separators, as a manifest lists it.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0.join("/"))
    }
}

impl From<&str> for StoreEntryName {
    fn from(raw: &str) -> Self {
        if raw == LEDGER_FILE_NAME {
            return Self::Ledger;
        }
        if [LOCK_FILE_NAME, LOCK_QUEUE_NAME, LOCK_QUEUE_GUARD_NAME].contains(&raw) {
            return Self::Lock;
        }
        if let Some(id) = raw.strip_prefix(STAGING_PREFIX) {
            return OperationId::try_from(id).map_or_else(|_| Self::Other(raw.to_owned()), Self::Staging);
        }
        if let Some(id) = raw.strip_prefix(REMOVING_PREFIX) {
            return OperationId::try_from(id).map_or_else(|_| Self::Other(raw.to_owned()), Self::Removing);
        }
        ReleaseName::try_from(raw).map_or_else(|_| Self::Other(raw.to_owned()), Self::Package)
    }
}

#[cfg(test)]
#[path = "path_test.rs"]
mod tests;
