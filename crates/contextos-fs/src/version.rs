//! The version token every read reports: a cheap identity of the file's state, taken from its metadata, that a
//! continuing read passes back as `expected_version` so a read in parts notices when the file changed between parts.

use std::fmt::Write as _;
use std::fs::Metadata;

use serde::Serialize;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::FsError;

/// The number of lowercase hexadecimal characters in a token.
const TOKEN_LENGTH: usize = 32;

/// An opaque token naming one version of a file, compared only for equality.
///
/// The token is the first 16 bytes of a SHA-256 over the file's size, its modification time in nanoseconds, and its
/// identity. On Unix the identity is the device, the inode, and the inode change time (seconds and nanoseconds),
/// which the kernel advances on every content or metadata change and which a caller cannot set, so a same-size
/// rewrite that restores the modification time is still noticed. Elsewhere the identity is the creation time (the
/// standard library has no stable file index or change time there), so a same-size change that keeps the
/// modification time can go unnoticed. The token is a cheap guard for continuing a read, not a proof of identical
/// content: only a whole read's `content_hash` is. Hashing the metadata keeps raw numbers out of the token and stops
/// clients parsing it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct FileVersion(String);

/// Accepts a token a caller passes back, refusing anything that is not 32 lowercase hexadecimal characters.
impl TryFrom<&str> for FileVersion {
    type Error = FsError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let well_formed =
            value.len() == TOKEN_LENGTH && value.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
        if well_formed {
            Ok(Self(value.to_owned()))
        } else {
            Err(FsError::InvalidVersion)
        }
    }
}

/// The facts about a file's state that its version token is derived from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileState {
    size: u64,
    /// The modification time in nanoseconds since the Unix epoch, zero where the platform does not report one.
    modified_nanos: i128,
    identity: Identity,
}

/// What identifies the file beyond its size and modification time: on Unix, its device, inode, and inode change time.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Identity {
    device: u64,
    inode: u64,
    changed_seconds: i64,
    changed_nanos: i64,
}

/// What identifies the file beyond its size and modification time: elsewhere, its creation time in nanoseconds since
/// the Unix epoch, zero where the platform does not report one.
#[cfg(not(unix))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Identity {
    created_nanos: i128,
}

/// Reads the facts the token covers from a file's metadata.
impl From<&Metadata> for FileState {
    fn from(metadata: &Metadata) -> Self {
        let modified_nanos = metadata
            .modified()
            .ok()
            .map(|time| OffsetDateTime::from(time).unix_timestamp_nanos())
            .unwrap_or_default();
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            Identity {
                device: metadata.dev(),
                inode: metadata.ino(),
                changed_seconds: metadata.ctime(),
                changed_nanos: metadata.ctime_nsec(),
            }
        };
        #[cfg(not(unix))]
        let identity = Identity {
            created_nanos: metadata
                .created()
                .ok()
                .map(|time| OffsetDateTime::from(time).unix_timestamp_nanos())
                .unwrap_or_default(),
        };
        Self {
            size: metadata.len(),
            modified_nanos,
            identity,
        }
    }
}

/// Derives the version token naming this file state.
impl From<&FileState> for FileVersion {
    fn from(state: &FileState) -> Self {
        let mut digest = Sha256::new();
        digest.update(state.size.to_le_bytes());
        digest.update(state.modified_nanos.to_le_bytes());
        #[cfg(unix)]
        {
            digest.update(state.identity.device.to_le_bytes());
            digest.update(state.identity.inode.to_le_bytes());
            digest.update(state.identity.changed_seconds.to_le_bytes());
            digest.update(state.identity.changed_nanos.to_le_bytes());
        }
        #[cfg(not(unix))]
        digest.update(state.identity.created_nanos.to_le_bytes());
        let token = digest
            .finalize()
            .iter()
            .take(TOKEN_LENGTH / 2)
            .fold(String::new(), |mut token, byte| {
                let _ = write!(token, "{byte:02x}");
                token
            });
        Self(token)
    }
}

/// Derives the version of the file whose metadata this is.
impl From<&Metadata> for FileVersion {
    fn from(metadata: &Metadata) -> Self {
        Self::from(&FileState::from(metadata))
    }
}

impl<'a> From<&'a FileVersion> for &'a str {
    fn from(value: &'a FileVersion) -> Self {
        &value.0
    }
}

#[cfg(test)]
#[path = "version_test.rs"]
mod tests;
