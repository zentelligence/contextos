//! Helpers the I/O cases share.

use std::path::Path;

use contextos_packages::{ReleaseFilePath, StoreFailure, StoreFailureKind, StorePath};

use super::worlds::BoxError;

/// The store path `text` names; the store's root for the empty string.
pub(super) fn path(text: &str) -> Result<StorePath, BoxError> {
    if text.is_empty() {
        return Ok(StorePath::root());
    }
    Ok(StorePath::root().within(&ReleaseFilePath::try_from(text)?))
}

pub(super) fn show(failure: &StoreFailure) -> String {
    format!("{:?}: {}", failure.kind, failure.detail)
}

/// The kind of failure a call ended in; a call that succeeded is an error of the test.
pub(super) fn kind_of<T>(result: Result<T, StoreFailure>) -> Result<StoreFailureKind, BoxError> {
    match result {
        Ok(_) => Err("expected a failure, but the call succeeded".into()),
        Err(failure) => Ok(failure.kind),
    }
}

/// Lays `bytes` down at `relative` below `store`, making the folders above it.
pub(super) fn put(store: &Path, relative: &str, bytes: &[u8]) -> Result<(), BoxError> {
    let target = store.join(relative);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(target, bytes)?;
    Ok(())
}

/// Makes a link at `at` to `target`, a folder or a file.
#[cfg(unix)]
pub(super) fn link(target: &Path, at: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, at)
}
