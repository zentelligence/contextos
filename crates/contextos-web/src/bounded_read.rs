//! Reading a host file with a size limit enforced before its content is held in memory (`mcp-contracts.md`).

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum BoundedReadError {
    #[error("{path} is larger than the {limit} byte limit")]
    TooLarge { path: String, limit: u64 },
    #[error("{path} is not UTF-8 text")]
    NotText { path: String },
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Reads `path` as UTF-8 text, refusing a file larger than `limit` bytes. At most `limit + 1` bytes are ever read.
///
/// # Errors
///
/// Returns [`BoundedReadError::TooLarge`] over the limit, [`BoundedReadError::NotText`] for invalid UTF-8, and
/// [`BoundedReadError::Io`] for any filesystem failure, which keeps its [`io::ErrorKind`].
pub fn read_text_within(path: &Path, limit: u64) -> Result<String, BoundedReadError> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).map_or(true, |length| length > limit) {
        return Err(BoundedReadError::TooLarge {
            path: crate::trailing_path::Trailing { path, count: 2 }.to_string(),
            limit,
        });
    }
    String::from_utf8(bytes).map_err(|_| BoundedReadError::NotText {
        path: crate::trailing_path::Trailing { path, count: 2 }.to_string(),
    })
}

#[cfg(test)]
#[path = "bounded_read_test.rs"]
mod tests;
