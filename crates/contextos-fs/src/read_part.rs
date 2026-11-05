//! Reading a byte part of a text file: the part a caller asks for by offset and length, read by seeking to it, so a
//! file of any size, or a single line longer than any part, can be read inline part by part.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use contextos_core::VaultPath;
use serde::Serialize;
use time::OffsetDateTime;

use crate::read::open_for_reading;
use crate::read_stage::{ReadHook, ReadStage};
use crate::{FileVersion, Filesystem, FsError};

/// A requested part: where it starts and the most bytes it may hold.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BytePart {
    offset: u64,
    length: u64,
}

/// The shortest part that can always make progress: the longest UTF-8 character is four bytes.
const MINIMUM_PART_BYTES: u64 = 4;

/// Builds a part from `(offset, length)`. A length shorter than the longest UTF-8 character is refused when the
/// part is read, after the file's version has been checked, so a stale `expected_version` is reported first.
impl From<(u64, u64)> for BytePart {
    fn from(value: (u64, u64)) -> Self {
        let (offset, length) = value;
        Self { offset, length }
    }
}

/// Application request for one byte part of a text file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadPartRequest {
    pub path: VaultPath,
    pub part: BytePart,
    /// The `version` an earlier read of this file returned; a file whose version now differs is refused.
    pub expected_version: Option<FileVersion>,
}

/// One byte part of a text file and the facts about the whole file that reading it established.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ReadPartResult {
    pub content: String,
    /// The whole file's size in bytes.
    pub size: u64,
    pub version: FileVersion,
    /// Where the part starts in the file.
    pub offset: u64,
    /// The part's length in bytes.
    pub length: u64,
    /// Where the next part starts, or -1 when this part reaches the end of the file.
    pub next_offset: i64,
    /// `true` unless the part is the whole file.
    pub truncated: bool,
    pub created: Option<OffsetDateTime>,
    pub modified: Option<OffsetDateTime>,
}

impl Filesystem {
    /// Reads one byte part of a text file by seeking to it, reading at most the part's length (capped at the
    /// configured read maximum), and ending on the last whole character. Only the part's own bytes are read and
    /// validated, so the cost is the part, never the file.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the path is unauthorised, missing, or not a regular file; when
    /// `expected_version` no longer matches (checked before anything else about the part) or the file changed while
    /// the part was read; when the offset lies beyond the end or inside a character; or when the part holds a NUL
    /// byte or invalid UTF-8.
    pub fn read_part(&self, request: &ReadPartRequest) -> Result<ReadPartResult, FsError> {
        self.read_part_staged(request, &mut |_| {})
    }

    /// [`Filesystem::read_part`] with a hook called at each [`ReadStage`], for tests that change the file mid-read.
    pub(crate) fn read_part_staged(
        &self,
        request: &ReadPartRequest,
        hook: ReadHook<'_>,
    ) -> Result<ReadPartResult, FsError> {
        let path: &Path = (&request.path).into();
        if !self.roots.authorises(&request.path) {
            return Err(FsError::OutsideRoot {
                path: path.to_path_buf(),
            });
        }
        let (mut file, metadata) = open_regular_file(path, hook)?;
        let version = FileVersion::from(&metadata);
        if let Some(expected) = &request.expected_version
            && *expected != version
        {
            return Err(FsError::VersionConflict {
                path: path.to_path_buf(),
                current_version: version,
            });
        }

        if request.part.length < MINIMUM_PART_BYTES {
            return Err(FsError::PartTooShort {
                length: request.part.length,
                minimum: MINIMUM_PART_BYTES,
            });
        }
        let size = metadata.len();
        let offset = request.part.offset;
        if offset > size {
            return Err(FsError::PartOutOfRange {
                path: path.to_path_buf(),
                offset,
                size,
            });
        }
        let maximum = self.limits(&request.path)?.max_read_bytes;
        let length = request.part.length.min(maximum);
        let content = read_whole_characters(&mut file, path, offset, length, size)?;
        hook(ReadStage::ContentRead);

        let after = file.metadata().map_err(|source| FsError::ReadMetadata {
            path: path.to_path_buf(),
            source,
        })?;
        let version_after = FileVersion::from(&after);
        if version_after != version {
            return Err(FsError::VersionConflict {
                path: path.to_path_buf(),
                current_version: version_after,
            });
        }

        let part_length = u64::try_from(content.len()).map_err(|_| FsError::ReadContent {
            path: path.to_path_buf(),
            source: std::io::Error::other("the part is longer than the largest representable length"),
        })?;
        let end_offset = offset.saturating_add(part_length);
        let next_offset = if end_offset >= size {
            -1
        } else {
            i64::try_from(end_offset).map_err(|_| FsError::ReadContent {
                path: path.to_path_buf(),
                source: std::io::Error::other("the part ends beyond the largest representable offset"),
            })?
        };
        Ok(ReadPartResult {
            content,
            size,
            version,
            offset,
            length: part_length,
            next_offset,
            truncated: !(offset == 0 && end_offset >= size),
            created: metadata.created().ok().map(OffsetDateTime::from),
            modified: metadata.modified().ok().map(OffsetDateTime::from),
        })
    }
}

/// Opens the regular file at `path` and returns it with the metadata of the open handle, so the size and version
/// describe the file the part is read from.
fn open_regular_file(path: &Path, hook: ReadHook<'_>) -> Result<(File, std::fs::Metadata), FsError> {
    // Inspect the path first so a pipe or device is refused before it is ever opened.
    let inspected = path.metadata().map_err(|source| match source.kind() {
        std::io::ErrorKind::NotFound => FsError::NotFound {
            path: path.to_path_buf(),
        },
        _ => FsError::ReadMetadata {
            path: path.to_path_buf(),
            source,
        },
    })?;
    if !inspected.is_file() {
        return Err(FsError::NotFile {
            path: path.to_path_buf(),
        });
    }
    hook(ReadStage::Inspected);
    let file = open_for_reading(path)?;
    let metadata = file.metadata().map_err(|source| FsError::ReadMetadata {
        path: path.to_path_buf(),
        source,
    })?;
    // The path was a regular file when inspected; the opened handle must be one too, in case it was replaced since.
    if !metadata.is_file() {
        return Err(FsError::NotFile {
            path: path.to_path_buf(),
        });
    }
    Ok((file, metadata))
}

/// Seeks to `offset` and reads at most `length` bytes, and validates only those bytes as text. A part that ends
/// inside a character before the end of the file drops that incomplete character (at most three bytes); any other
/// invalid byte, a NUL, or a part that could not advance is refused as binary.
fn read_whole_characters(file: &mut File, path: &Path, offset: u64, length: u64, size: u64) -> Result<String, FsError> {
    let wanted = length.min(size - offset);
    let read_error = |source| FsError::ReadContent {
        path: path.to_path_buf(),
        source,
    };
    file.seek(SeekFrom::Start(offset)).map_err(read_error)?;
    let mut bytes = Vec::new();
    file.take(wanted).read_to_end(&mut bytes).map_err(read_error)?;

    if bytes.first().is_some_and(|byte| is_continuation(*byte)) {
        return Err(FsError::PartInsideCharacter {
            path: path.to_path_buf(),
            offset,
        });
    }
    let binary = || FsError::Binary {
        path: path.to_path_buf(),
    };
    if bytes.contains(&0) {
        return Err(binary());
    }
    let reaches_end = offset.saturating_add(wanted) >= size;
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => {
            let cut = error.utf8_error();
            // Only a character cut short by the part's own end is dropped; an invalid sequence, or an incomplete
            // character at the end of the file, is not text.
            if cut.error_len().is_some() || reaches_end {
                return Err(binary());
            }
            let mut bytes = error.into_bytes();
            bytes.truncate(cut.valid_up_to());
            String::from_utf8(bytes).map_err(|_| binary())?
        }
    };
    if text.is_empty() && !reaches_end {
        return Err(binary());
    }
    Ok(text)
}

/// Whether the byte continues a multi-byte UTF-8 character rather than starting one.
const fn is_continuation(byte: u8) -> bool {
    byte & 0xC0 == 0x80
}

#[cfg(test)]
#[path = "read_part_test.rs"]
mod tests;
