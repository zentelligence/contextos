//! Atomic replacement for a single file this crate owns exclusively (a
//! configuration file or a run record): write a
//! uniquely named temporary file in the target directory, then replace the
//! target with a single rename, so a reader never observes a partially
//! written file (`security.md`'s persistence discipline).
//!
//! A signature adapter over `contextos-fs`'s one atomic writer for files outside every vault, which
//! `contextos-service` calls the same way for the service definitions and process records it writes.

use std::io;
use std::path::Path;

use contextos_fs::host_file;

/// Writes `contents` to `path` via a uniquely named temporary file in the
/// same directory, replaced atomically. The directory must already exist:
/// callers such as run records rely on a missing directory being reported,
/// never recreated.
///
/// The implementation is `contextos_fs::host_file::replace_file_atomically`,
/// the one atomic writer for files outside every vault; this wrapper keeps
/// the [`io::Result`] signature the settings and service callers use.
///
/// # Errors
///
/// Returns an [`io::Error`] carrying the underlying cause (and its kind,
/// `NotFound` for a missing directory) when the temporary file cannot be
/// written or the rename onto `path` fails; `path` is left unchanged on
/// either failure.
pub fn write_atomically(path: &Path, contents: &[u8]) -> io::Result<()> {
    host_file::replace_file_atomically(path, contents).map_err(io::Error::from)
}

#[cfg(test)]
#[path = "atomic_write_test.rs"]
mod tests;
