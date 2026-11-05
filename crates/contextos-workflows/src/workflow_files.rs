//! The files a workflow declares it ships beside its script, and the only way its script reads them.
//!
//! A workflow lists files and folders in `templates`, relative to its own root: the installed release of a shared
//! package, or a vault workflow's folder under `registry/workflows/`. A folder admits every regular file beneath it.
//! [`WorkflowFiles`] is what a run is given: it refuses a path that is not a plain relative path or is not declared
//! before anything is read, and leaves reading to a host-supplied [`WorkflowFileSource`]. Nothing here writes, so a
//! script still changes the world only through `call_mcp`.

use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use contextos_core::ContentHash;
use thiserror::Error;

use crate::caller::WorkflowFailureKind;

/// The largest file a script may read, in bytes. A workflow ships templates and rules, not data.
pub const MAX_WORKFLOW_FILE_BYTES: u64 = 1024 * 1024;

/// The most files one listing may return. A listing that would hold more is refused, never cut short.
pub const MAX_LISTED_WORKFLOW_FILES: usize = 1000;

/// A path within a workflow's own root: `/`-separated, relative, and made only of ordinary segments.
///
/// No segment may be empty, `.`, or `..`, and none may hold a backslash, which is a separator on Windows, a colon,
/// which names a drive or an alternate data stream there, or NUL. Nor may a segment end in a dot or a space, which
/// Windows drops, or be a name Windows reserves for a device (`CON`, `PRN`, `AUX`, `NUL`, `COM1` to `COM9`, `LPT1` to
/// `LPT9`, with or without an extension). So the path cannot leave the root it is joined to on any platform, and names
/// one file the same way on every platform.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorkflowFilePath(String);

/// Why a string is not a [`WorkflowFilePath`].
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{path:?} is not a plain relative path within the workflow: {reason}")]
pub struct WorkflowFilePathError {
    pub path: String,
    pub reason: &'static str,
}

impl TryFrom<&str> for WorkflowFilePath {
    type Error = WorkflowFilePathError;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        let refuse = |reason| WorkflowFilePathError {
            path: raw.to_owned(),
            reason,
        };
        if raw.is_empty() {
            return Err(refuse("it is empty"));
        }
        if raw.contains(['\\', ':', '\0']) {
            return Err(refuse("it holds a backslash, a colon, or NUL"));
        }
        if raw.starts_with('/') {
            return Err(refuse("it is absolute"));
        }
        for segment in raw.split('/') {
            match segment {
                "" => return Err(refuse("it has an empty segment")),
                "." | ".." => return Err(refuse("it has a `.` or `..` segment")),
                _ if segment.ends_with(['.', ' ']) => return Err(refuse("a segment ends in a dot or a space")),
                _ if is_reserved_device_name(segment) => {
                    return Err(refuse("a segment is a name Windows reserves for a device"));
                }
                _ => {}
            }
        }
        Ok(Self(raw.to_owned()))
    }
}

/// Whether `segment`, before its first dot, is a device name Windows reserves in every folder.
fn is_reserved_device_name(segment: &str) -> bool {
    let stem = segment.split('.').next().unwrap_or(segment).to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes().get(3), Some(b'1'..=b'9')))
}

impl WorkflowFilePath {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether `other` is this path or lies beneath it.
    #[must_use]
    pub fn holds(&self, other: &Self) -> bool {
        other
            .0
            .strip_prefix(self.0.as_str())
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    }

    /// The path's segments, in order.
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }
}

impl fmt::Display for WorkflowFilePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The files and folders a workflow declares in `templates`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeclaredFiles(Vec<WorkflowFilePath>);

impl TryFrom<Vec<String>> for DeclaredFiles {
    type Error = WorkflowFilePathError;

    fn try_from(raw: Vec<String>) -> Result<Self, Self::Error> {
        raw.iter()
            .map(|entry| WorkflowFilePath::try_from(entry.as_str()))
            .collect::<Result<_, _>>()
            .map(Self)
    }
}

impl DeclaredFiles {
    /// Each declared file or folder, in the order declared.
    #[must_use]
    pub fn entries(&self) -> &[WorkflowFilePath] {
        &self.0
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether `path` is a declared entry or lies beneath one.
    #[must_use]
    pub fn admits(&self, path: &WorkflowFilePath) -> bool {
        self.0.iter().any(|entry| entry.holds(path))
    }
}

/// One file's complete text and the SHA-256 of its bytes, as 64 lower-case hexadecimal characters: the hash
/// `contextos` reports as `content_hash`, so a script can compare a shipped file with one in a vault.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowFile {
    pub content: String,
    pub content_hash: ContentHash,
}

/// Why a script could not read or list its workflow's files. Shown to the script as `<code>: <message>`.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{}: {message}", kind.code())]
pub struct WorkflowFileFailure {
    pub kind: WorkflowFailureKind,
    pub message: String,
}

impl From<WorkflowFilePathError> for WorkflowFileFailure {
    fn from(error: WorkflowFilePathError) -> Self {
        Self::new(WorkflowFailureKind::FilePathInvalid, error.to_string())
    }
}

impl WorkflowFileFailure {
    #[must_use]
    pub fn new(kind: WorkflowFailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

/// Reads a workflow's files from wherever its host keeps them. Only ever asked about a path [`WorkflowFiles`] has
/// already found declared.
///
/// Boxed and pinned rather than an `async fn`, so it can be held as `Arc<dyn WorkflowFileSource>`, as
/// [`crate::McpCaller`] is.
pub trait WorkflowFileSource: Send + Sync {
    /// Reads the regular file at `path`. Refuses a missing path or a folder (`FileNotFound`), a link
    /// (`FileUnreadable`), content that is not UTF-8 (`FileNotText`), and a file over [`MAX_WORKFLOW_FILE_BYTES`]
    /// (`FileTooLarge`) before reading it whole.
    fn read<'a>(
        &'a self,
        path: &'a WorkflowFilePath,
    ) -> Pin<Box<dyn Future<Output = Result<WorkflowFile, WorkflowFileFailure>> + Send + 'a>>;

    /// Every regular file at or beneath `entry`: `entry` itself when it is a file. Refuses a missing `entry`
    /// (`FileNotFound`) and more than `limit` files (`FileTooLarge`).
    fn list<'a>(
        &'a self,
        entry: &'a WorkflowFilePath,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<WorkflowFilePath>, WorkflowFileFailure>> + Send + 'a>>;
}

/// The files one run's script may read: its workflow's declaration and where they are kept.
#[derive(Clone, Default)]
pub struct WorkflowFiles {
    declared: DeclaredFiles,
    source: Option<Arc<dyn WorkflowFileSource>>,
}

impl fmt::Debug for WorkflowFiles {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkflowFiles")
            .field("declared", &self.declared)
            .finish_non_exhaustive()
    }
}

impl WorkflowFiles {
    /// Files for a run whose workflow declares nothing, so every read is refused as undeclared.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Files declared by `declared`, read through `source`.
    #[must_use]
    pub fn new(declared: DeclaredFiles, source: Arc<dyn WorkflowFileSource>) -> Self {
        Self {
            declared,
            source: Some(source),
        }
    }

    /// Reads the declared file at `raw`.
    ///
    /// # Errors
    ///
    /// Returns `FilePathInvalid` for a path that is not plain and relative, `FileUndeclared` for one that is not
    /// declared, and whatever the source refuses.
    pub async fn read(&self, raw: &str) -> Result<WorkflowFile, WorkflowFileFailure> {
        let path = WorkflowFilePath::try_from(raw)?;
        let source = match &self.source {
            Some(source) if self.declared.admits(&path) => source,
            _ => return Err(undeclared(&path)),
        };
        let file = source.read(&path).await?;
        // Checked again here, so a source that read past the limit still cannot hand an oversized file to a script.
        if u64::try_from(file.content.len()).map_or(true, |length| length > MAX_WORKFLOW_FILE_BYTES) {
            return Err(WorkflowFileFailure::new(
                WorkflowFailureKind::FileTooLarge,
                format!("{path} is larger than the {MAX_WORKFLOW_FILE_BYTES} byte limit"),
            ));
        }
        Ok(file)
    }

    /// Lists every declared file at or beneath `raw_prefix`, sorted; an empty prefix lists them all.
    ///
    /// # Errors
    ///
    /// Returns `FilePathInvalid` for a prefix that is not plain and relative, `FileUndeclared` for one that overlaps no
    /// declared entry, `FileTooLarge` past [`MAX_LISTED_WORKFLOW_FILES`], and whatever the source refuses.
    pub async fn list(&self, raw_prefix: &str) -> Result<Vec<String>, WorkflowFileFailure> {
        let prefix = match raw_prefix {
            "" => None,
            raw => Some(WorkflowFilePath::try_from(raw)?),
        };
        let Some(source) = &self.source else {
            return match prefix {
                None => Ok(Vec::new()),
                Some(prefix) => Err(undeclared(&prefix)),
            };
        };
        // Only the entries the prefix overlaps are listed: one beneath the prefix, or one holding it.
        let overlapping: Vec<&WorkflowFilePath> = self
            .declared
            .entries()
            .iter()
            .filter(|entry| {
                prefix
                    .as_ref()
                    .is_none_or(|prefix| prefix.holds(entry) || entry.holds(prefix))
            })
            .collect();
        if let (Some(prefix), true) = (&prefix, overlapping.is_empty()) {
            return Err(undeclared(prefix));
        }
        let mut listed = BTreeSet::new();
        for entry in overlapping {
            // A prefix narrower than a declared folder is listed itself, so a large folder can be listed in parts.
            let asked = match &prefix {
                Some(prefix) if entry.holds(prefix) => prefix,
                _ => entry,
            };
            for file in source.list(asked, MAX_LISTED_WORKFLOW_FILES).await? {
                if prefix.as_ref().is_none_or(|prefix| prefix.holds(&file)) {
                    listed.insert(file);
                }
            }
            if listed.len() > MAX_LISTED_WORKFLOW_FILES {
                return Err(WorkflowFileFailure::new(
                    WorkflowFailureKind::FileTooLarge,
                    format!("the listing holds more than {MAX_LISTED_WORKFLOW_FILES} files; list a narrower prefix"),
                ));
            }
        }
        Ok(listed.into_iter().map(|file| file.0).collect())
    }
}

fn undeclared(path: &WorkflowFilePath) -> WorkflowFileFailure {
    WorkflowFileFailure::new(
        WorkflowFailureKind::FileUndeclared,
        format!("{path} is not a file or folder the workflow declares in `templates`"),
    )
}

#[cfg(test)]
#[path = "workflow_files_test.rs"]
mod tests;
