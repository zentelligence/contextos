use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::sync::Arc;

use contextos_core::{ContentHash, Origin, VaultPath, VaultSet};
use serde::Serialize;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::read_stage::{ReadHook, ReadStage};
use crate::regular_file::{is_not_regular_file, open_without_waiting};
use crate::{FileVersion, FsError, FsErrorInfo};

/// Limits applied to filesystem operations for a vault set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FsLimits {
    pub max_read_bytes: u64,
    pub max_batch_files: usize,
}

/// Trusted construction input for the native filesystem adapter.
#[derive(Clone, Debug)]
pub struct FilesystemConfig {
    pub roots: VaultSet,
    pub limits: Vec<FsLimits>,
    /// Path patterns hidden from every enumeration surface for this vault,
    /// one set per root, aligned by index with `roots`. Governs
    /// omission from listings only; a direct, explicit-path read is never
    /// affected. See [`crate::default_hidden_patterns`] for the baseline
    /// callers should layer their own patterns on top of.
    pub hidden: Vec<Vec<String>>,
    pub atomic_write_guard: Option<Arc<dyn GuardsAtomicWrites>>,
}

/// Controllable boundary immediately after temporary content is durably flushed.
pub trait GuardsAtomicWrites: std::fmt::Debug + Send + Sync {
    /// Allows persistence to continue or injects a pre-rename failure.
    ///
    /// # Errors
    ///
    /// Returns an I/O error to simulate interruption before atomic replacement.
    fn after_flush(&self, target: &Path) -> Result<(), std::io::Error>;
}

#[derive(Debug)]
struct AllowAtomicWrites;

impl GuardsAtomicWrites for AllowAtomicWrites {
    fn after_flush(&self, _target: &Path) -> Result<(), std::io::Error> {
        Ok(())
    }
}

/// Root-confined filesystem operations.
#[derive(Clone, Debug)]
pub struct Filesystem {
    pub(crate) roots: VaultSet,
    pub(crate) limits: Vec<FsLimits>,
    pub(crate) hidden: Vec<Vec<String>>,
    pub(crate) atomic_write_guard: Arc<dyn GuardsAtomicWrites>,
    pub(crate) server_owned: crate::server_owned::ServerOwnedFolders,
}

impl TryFrom<FilesystemConfig> for Filesystem {
    type Error = FsError;

    fn try_from(value: FilesystemConfig) -> Result<Self, Self::Error> {
        if value.roots.len() != value.limits.len() {
            return Err(FsError::LimitCountMismatch {
                root_count: value.roots.len(),
                limit_count: value.limits.len(),
            });
        }
        if value.roots.len() != value.hidden.len() {
            return Err(FsError::HiddenCountMismatch {
                root_count: value.roots.len(),
                hidden_count: value.hidden.len(),
            });
        }
        if let Some((root_index, _)) = value
            .limits
            .iter()
            .enumerate()
            .find(|(_, limits)| limits.max_read_bytes == 0 || limits.max_batch_files == 0)
        {
            return Err(FsError::InvalidLimits { root_index });
        }
        let atomic_write_guard = match value.atomic_write_guard {
            Some(guard) => guard,
            None => Arc::new(AllowAtomicWrites),
        };
        Ok(Self {
            roots: value.roots,
            limits: value.limits,
            hidden: value.hidden,
            atomic_write_guard,
            server_owned: crate::server_owned::ServerOwnedFolders::default(),
        })
    }
}

impl Filesystem {
    /// This adapter with `folders` owned by the server: a mutation whose origin is a tool is refused there, and one
    /// the server makes on its own account is not.
    #[must_use]
    pub fn with_server_owned_folders(mut self, folders: Vec<crate::ServerOwnedFolder>) -> Self {
        self.server_owned = folders.into();
        self
    }

    /// Refuses a change by `origin` to `path` when it falls in a server-owned folder, for a tool that reads and plans
    /// before it writes and so must be refused before it reports anything else about the path.
    ///
    /// # Errors
    ///
    /// Returns [`FsError::ServerOwned`] naming the vault-relative path and carrying the folder's own code.
    pub fn ensure_not_server_owned(&self, path: &VaultPath, origin: &Origin) -> Result<(), FsError> {
        self.server_owned.check(path, origin)
    }
}

/// A validated 1-based inclusive line range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LineRange {
    from: usize,
    to: usize,
}

impl TryFrom<(usize, usize)> for LineRange {
    type Error = FsError;

    fn try_from(value: (usize, usize)) -> Result<Self, Self::Error> {
        let (from, to) = value;
        if from == 0 || to == 0 || from > to {
            return Err(FsError::InvalidRange { from, to });
        }
        Ok(Self { from, to })
    }
}

/// Optional selection applied to a text read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadLimit {
    Head(usize),
    Tail(usize),
    Range(LineRange),
}

/// Application request for one text file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadTextRequest {
    pub path: VaultPath,
    pub limit: Option<ReadLimit>,
}

/// Result of a UTF-8 text read.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReadTextResult {
    pub content: String,
    pub line_count: usize,
    pub content_hash: ContentHash,
    pub truncated: bool,
    /// The file's size in bytes, from the inspection the read itself made: a caller that also wants the file's
    /// metadata need not inspect it, or read it, a second time.
    pub size: u64,
    /// When the file was created, where the platform records it.
    pub created: Option<OffsetDateTime>,
    /// When the file was last modified, where the platform records it.
    pub modified: Option<OffsetDateTime>,
    /// The version of the file the read opened, which a continuing read passes back as `expected_version`.
    pub version: FileVersion,
}

/// Request for one MCP-embeddable file capped independently at 10 MiB.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttachmentRequest {
    pub path: VaultPath,
}

/// Root-confined bytes and detected media type for an MCP attachment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Attachment {
    pub path: String,
    /// The validated vault path, used to build a `{name}://{relative-path}`
    /// URI addressing-compatible with the resource surface. This superseded
    /// an earlier scheme that addressed attachments by a plain `file://`
    /// path, so every filesystem-backed MCP content item now shares one URI
    /// form regardless of which tool produced it.
    pub vault_path: VaultPath,
    pub mime_type: String,
    pub bytes: Vec<u8>,
    pub text: bool,
    /// The hash of exactly `bytes`, in the same form every other read reports, so it can be supplied as
    /// `expected_hash` to overwrite the file. Taken from the bytes returned, not from a later read of the file.
    pub content_hash: ContentHash,
    /// The file's size in bytes, from the handle the bytes were read through.
    pub size: u64,
    /// The version of the file the attachment was read from.
    pub version: FileVersion,
}

/// Best-effort MIME type for a path by extension, via `mime_guess`'s
/// registry-backed table (the same data behind Apache's `mime.types` and
/// most browsers) rather than a hand-rolled, narrow match: the one
/// extension-based source both `read_attachment` and the resource
/// surface's listing use, so the same file is never described
/// with two different MIME types depending on which surface reports it.
/// Returns `None` for anything `mime_guess` doesn't recognise; callers
/// choose their own fallback (a listing has no content in hand and
/// defaults to a generic type; `read_attachment` content-sniffs first via
/// `infer` and only reaches this as its own fallback).
#[must_use]
pub fn mime_type_for_extension(path: &Path) -> Option<String> {
    mime_guess::from_path(path)
        .first()
        .map(|mime| mime.essence_str().to_owned())
}

/// Application request for isolated reads of several files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadManyRequest {
    pub paths: Vec<VaultPath>,
}

/// Per-file result for a batch read.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReadManyResult {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<ContentHash>,
    /// The file's size in bytes, present exactly when `content` is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// The version of the file read, present exactly when `content` is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<FileVersion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<FsErrorInfo>,
}

/// The fixed MCP attachment size cap: the shared 10 MiB file limit
/// ([`contextos_core::MAX_FILE_BYTES`]), used by `fs_attach_file`'s binary
/// read path, `fs_write_file`'s binary write path (`mutate.rs`), and
/// `contextos-mcp`'s own pre-decode `content_base64` size check (ahead of the
/// authoritative check here, not a replacement for it), so all three agree
/// with each other and with every request body limit derived from it.
pub const MAX_ATTACHMENT_BYTES: u64 = contextos_core::MAX_FILE_BYTES;

impl Filesystem {
    /// Reads a text or binary attachment with the fixed MCP 10 MiB cap.
    ///
    /// # Errors
    ///
    /// Returns a typed confinement, metadata, size, type, or read error.
    pub fn read_attachment(&self, request: &AttachmentRequest) -> Result<Attachment, FsError> {
        self.read_attachment_staged(request, MAX_ATTACHMENT_BYTES, &mut |_| {})
    }

    /// [`Filesystem::read_attachment`] with the cap as a parameter and a hook called at each [`ReadStage`], for
    /// tests that change the file mid-read.
    pub(crate) fn read_attachment_staged(
        &self,
        request: &AttachmentRequest,
        maximum: u64,
        hook: ReadHook<'_>,
    ) -> Result<Attachment, FsError> {
        let path: &Path = (&request.path).into();
        if !self.roots.authorises(&request.path) {
            return Err(FsError::OutsideRoot {
                path: path.to_path_buf(),
            });
        }
        let metadata = match path.metadata() {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Err(FsError::NotFound {
                    path: path.to_path_buf(),
                });
            }
            Err(source) => {
                return Err(FsError::ReadMetadata {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        if !metadata.is_file() {
            return Err(FsError::NotFile {
                path: path.to_path_buf(),
            });
        }
        if metadata.len() > maximum {
            return Err(FsError::TooLarge {
                path: path.to_path_buf(),
                size: metadata.len(),
                maximum,
            });
        }
        hook(ReadStage::Inspected);
        let mut file = open_for_reading(path)?;
        // The size and version come from the open handle, so they describe the file whose bytes are returned.
        let handle_metadata = file.metadata().map_err(|source| FsError::ReadMetadata {
            path: path.to_path_buf(),
            source,
        })?;
        // The path may have been replaced between the inspection and the open, so the handle is checked again.
        if !handle_metadata.is_file() {
            return Err(FsError::NotFile {
                path: path.to_path_buf(),
            });
        }
        if handle_metadata.len() > maximum {
            return Err(FsError::TooLarge {
                path: path.to_path_buf(),
                size: handle_metadata.len(),
                maximum,
            });
        }
        hook(ReadStage::Opened);
        // One byte beyond the cap shows a file that grew past it after the size check.
        let mut bytes = Vec::new();
        (&mut file)
            .take(maximum.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|source| FsError::ReadContent {
                path: path.to_path_buf(),
                source,
            })?;
        if u64::try_from(bytes.len()).is_ok_and(|read| read > maximum) {
            return Err(FsError::TooLarge {
                path: path.to_path_buf(),
                size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                maximum,
            });
        }
        let text = !bytes.contains(&0) && std::str::from_utf8(&bytes).is_ok();
        // Content-sniffing (`infer`, magic bytes) is authoritative when it
        // recognises the bytes; an extension can lie, content cannot.
        // Most plain-text formats (JSON, YAML, Markdown, CSV, plain text)
        // have no magic-byte signature, so `infer` correctly returns
        // `None` for those and extension-based `mime_guess` fills the
        // gap; the text/binary heuristic is the last resort for
        // anything neither recognises.
        let mime_type = infer::get(&bytes)
            .map(|kind| kind.mime_type().to_owned())
            .or_else(|| mime_type_for_extension(path))
            .unwrap_or_else(|| if text { "text/plain" } else { "application/octet-stream" }.to_owned());
        let content_hash = ContentHash::from(<[u8; 32]>::from(Sha256::digest(&bytes)));
        Ok(Attachment {
            path: request.path.portable_relative(),
            vault_path: request.path.clone(),
            mime_type,
            bytes,
            text,
            content_hash,
            size: handle_metadata.len(),
            version: FileVersion::from(&handle_metadata),
        })
    }

    /// Returns the largest batch that can be valid across all configured roots.
    #[must_use]
    pub fn batch_capacity(&self) -> usize {
        self.limits.iter().fold(0_usize, |capacity, limits| {
            capacity.saturating_add(limits.max_batch_files)
        })
    }

    /// Reads UTF-8 text while streaming hashes, validation, and line selection.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the path is unauthorised, missing, not a
    /// regular file, too large without a limiter, unreadable, or binary.
    pub fn read_text(&self, request: &ReadTextRequest) -> Result<ReadTextResult, FsError> {
        self.read_text_checked(request, None)
    }

    /// Reads UTF-8 text as [`Filesystem::read_text`] does, refusing a file whose version is no longer
    /// `expected_version`. The version is checked before the size limit, so a file that changed is reported as
    /// changed rather than as too large.
    ///
    /// # Errors
    ///
    /// Returns [`FsError::VersionConflict`] when the file's version differs, and otherwise the errors of
    /// [`Filesystem::read_text`].
    pub fn read_text_if_unchanged(
        &self,
        request: &ReadTextRequest,
        expected_version: &FileVersion,
    ) -> Result<ReadTextResult, FsError> {
        self.read_text_checked(request, Some(expected_version))
    }

    fn read_text_checked(
        &self,
        request: &ReadTextRequest,
        expected_version: Option<&FileVersion>,
    ) -> Result<ReadTextResult, FsError> {
        self.read_text_staged(request, expected_version, &mut |_| {})
    }

    /// The whole or line read with a hook called at each [`ReadStage`], for tests that change the file mid-read.
    pub(crate) fn read_text_staged(
        &self,
        request: &ReadTextRequest,
        expected_version: Option<&FileVersion>,
        hook: ReadHook<'_>,
    ) -> Result<ReadTextResult, FsError> {
        let path: &std::path::Path = (&request.path).into();
        if !self.roots.authorises(&request.path) {
            return Err(FsError::OutsideRoot {
                path: path.to_path_buf(),
            });
        }
        let inspected = match path.metadata() {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Err(FsError::NotFound {
                    path: path.to_path_buf(),
                });
            }
            Err(source) => {
                return Err(FsError::ReadMetadata {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        if !inspected.is_file() {
            return Err(FsError::NotFile {
                path: path.to_path_buf(),
            });
        }
        hook(ReadStage::Inspected);
        let file = open_for_reading(path)?;
        // The size and version come from the open handle, so they describe the file this read goes on to read.
        let metadata = file.metadata().map_err(|source| FsError::ReadMetadata {
            path: path.to_path_buf(),
            source,
        })?;
        if !metadata.is_file() {
            return Err(FsError::NotFile {
                path: path.to_path_buf(),
            });
        }
        let version = FileVersion::from(&metadata);
        if let Some(expected) = expected_version
            && *expected != version
        {
            return Err(FsError::VersionConflict {
                path: path.to_path_buf(),
                current_version: version,
            });
        }
        let limits = self.limits(&request.path)?;
        if metadata.len() > limits.max_read_bytes && request.limit.is_none() {
            return Err(FsError::TooLarge {
                path: path.to_path_buf(),
                size: metadata.len(),
                maximum: limits.max_read_bytes,
            });
        }

        let mut reader = BufReader::new(file);
        let mut hasher = Sha256::new();
        let mut buffer = Vec::<u8>::new();
        let mut accumulator = ReadAccumulator::from(request.limit);

        loop {
            buffer.clear();
            let bytes_read = reader
                .read_until(b'\n', &mut buffer)
                .map_err(|source| FsError::ReadContent {
                    path: path.to_path_buf(),
                    source,
                })?;
            if bytes_read == 0 {
                break;
            }
            hasher.update(&buffer);
            if buffer.contains(&0) {
                return Err(FsError::Binary {
                    path: path.to_path_buf(),
                });
            }
            let line = std::str::from_utf8(&buffer).map_err(|_| FsError::Binary {
                path: path.to_path_buf(),
            })?;
            accumulator.observe(line);
        }

        let (content, line_count, truncated) = accumulator.finish();

        Ok(ReadTextResult {
            content,
            line_count,
            content_hash: ContentHash::from(<[u8; 32]>::from(hasher.finalize())),
            truncated,
            size: metadata.len(),
            created: metadata.created().ok().map(OffsetDateTime::from),
            modified: metadata.modified().ok().map(OffsetDateTime::from),
            version,
        })
    }

    /// Reads a bounded batch without promoting item failures to batch failure.
    ///
    /// # Errors
    ///
    /// Returns [`FsError::BatchTooLarge`] when the request exceeds the
    /// configured item limit. Individual read errors remain in their item.
    pub fn read_many(&self, request: ReadManyRequest) -> Result<Vec<ReadManyResult>, FsError> {
        let mut counts = vec![0_usize; self.limits.len()];
        for path in &request.paths {
            if !self.roots.authorises(path) {
                continue;
            }
            let absolute: &std::path::Path = path.into();
            let root_index = usize::try_from(path.root_id()).map_err(|source| FsError::PathValidation {
                path: absolute.to_path_buf(),
                source,
            })?;
            let count = counts.get_mut(root_index).ok_or(FsError::LimitCountMismatch {
                root_count: self.roots.len(),
                limit_count: self.limits.len(),
            })?;
            *count += 1;
        }
        for (root_index, count) in counts.into_iter().enumerate() {
            let limits = self
                .limits
                .get(root_index)
                .copied()
                .ok_or(FsError::LimitCountMismatch {
                    root_count: self.roots.len(),
                    limit_count: self.limits.len(),
                })?;
            if count > limits.max_batch_files {
                return Err(FsError::BatchTooLarge {
                    count,
                    maximum: limits.max_batch_files,
                });
            }
        }

        Ok(request
            .paths
            .into_iter()
            .map(|path| {
                let display_path = path.portable_relative();
                match self.read_text(&ReadTextRequest { path, limit: None }) {
                    Ok(result) => ReadManyResult {
                        path: display_path,
                        content: Some(result.content),
                        content_hash: Some(result.content_hash),
                        size: Some(result.size),
                        version: Some(result.version),
                        error: None,
                    },
                    Err(error) => ReadManyResult {
                        path: display_path,
                        content: None,
                        content_hash: None,
                        size: None,
                        version: None,
                        error: Some(FsErrorInfo::from(&error)),
                    },
                }
            })
            .collect())
    }

    pub(crate) fn limits(&self, path: &VaultPath) -> Result<FsLimits, FsError> {
        let absolute: &std::path::Path = path.into();
        let root_index = usize::try_from(path.root_id()).map_err(|source| FsError::PathValidation {
            path: absolute.to_path_buf(),
            source,
        })?;
        self.limits.get(root_index).copied().ok_or(FsError::LimitCountMismatch {
            root_count: self.roots.len(),
            limit_count: self.limits.len(),
        })
    }

    /// Returns the hidden path patterns configured for `path`'s vault root.
    /// Applied by enumeration surfaces only (`list_directory`,
    /// `list_directory_with_sizes`, `directory_tree`, `search_files`); a
    /// direct, explicit-path read never consults this.
    pub(crate) fn hidden(&self, path: &VaultPath) -> Result<&[String], FsError> {
        let absolute: &Path = path.into();
        let root_index = usize::try_from(path.root_id()).map_err(|source| FsError::PathValidation {
            path: absolute.to_path_buf(),
            source,
        })?;
        self.hidden
            .get(root_index)
            .map(Vec::as_slice)
            .ok_or(FsError::HiddenCountMismatch {
                root_count: self.roots.len(),
                hidden_count: self.hidden.len(),
            })
    }
}

struct ReadAccumulator {
    limit: Option<ReadLimit>,
    selected: String,
    tail: VecDeque<String>,
    line_count: usize,
    selected_count: usize,
}

impl From<Option<ReadLimit>> for ReadAccumulator {
    fn from(value: Option<ReadLimit>) -> Self {
        Self {
            limit: value,
            selected: String::new(),
            tail: VecDeque::new(),
            line_count: 0,
            selected_count: 0,
        }
    }
}

impl ReadAccumulator {
    fn observe(&mut self, line: &str) {
        self.line_count += 1;
        match self.limit {
            None => self.select(line),
            Some(ReadLimit::Head(maximum)) if self.line_count <= maximum => self.select(line),
            Some(ReadLimit::Tail(maximum)) if maximum > 0 => {
                self.tail.push_back(line.to_owned());
                if self.tail.len() > maximum {
                    self.tail.pop_front();
                }
            }
            Some(ReadLimit::Range(range)) if self.line_count >= range.from && self.line_count <= range.to => {
                self.select(line);
            }
            Some(ReadLimit::Head(_) | ReadLimit::Tail(_) | ReadLimit::Range(_)) => {}
        }
    }

    fn select(&mut self, line: &str) {
        self.selected.push_str(line);
        self.selected_count += 1;
    }

    fn finish(mut self) -> (String, usize, bool) {
        if matches!(self.limit, Some(ReadLimit::Tail(_))) {
            self.selected_count = self.tail.len();
            for line in self.tail {
                self.selected.push_str(&line);
            }
        }
        let truncated = self.selected_count != self.line_count;
        (self.selected, self.line_count, truncated)
    }
}

/// Opens `path` for reading without waiting on it, refusing a pipe, socket, or device as not a file: the path can be
/// replaced after a read inspected it, and opening a pipe the usual way would wait for a writer for ever.
pub(crate) fn open_for_reading(path: &Path) -> Result<std::fs::File, FsError> {
    open_without_waiting(path, std::fs::OpenOptions::new().read(true)).map_err(|source| {
        if is_not_regular_file(&source) {
            FsError::NotFile {
                path: path.to_path_buf(),
            }
        } else {
            FsError::OpenRead {
                path: path.to_path_buf(),
                source,
            }
        }
    })
}

#[cfg(test)]
#[path = "read_test.rs"]
mod tests;
