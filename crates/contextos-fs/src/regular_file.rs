//! Opening vault files without ever waiting on them.
//!
//! A path that was a regular file when inspected can be replaced before it is opened, and a vault can hold a named
//! pipe or device under a note's name. Opening a named pipe waits until its other end is opened too, which would hold
//! a thread (and, for a mutation, the vault's write lock) for ever. So on Unix every open here passes `O_NONBLOCK`: a
//! pipe opens at once, or for writing with no reader fails at once, and anything that is not a regular file is then
//! refused as not a regular file before a byte is read or written.
//!
//! The flag stays set on the regular file. It does not change reading or writing a regular file, with one exception
//! on Linux: when another process holds a conflicting lease on the file (as a Samba or NFS server can), the open fails
//! at once with `WouldBlock` instead of waiting for the lease to be broken. Clearing the flag would need a raw `fcntl`
//! call, which this crate does not make.
//!
//! On Windows a named pipe lives only in the `\\.\pipe\` namespace, which a vault path cannot name, so the opens are
//! plain ones and the regular-file check still applies.

use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, Read};
use std::path::Path;

/// The marker an [`io::Error`] carries when the path is not a regular file. It names no path, so a caller that
/// quotes the error in a warning never exposes a host path.
#[derive(Debug)]
struct NotRegularFile;

impl std::fmt::Display for NotRegularFile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the path is not a regular file")
    }
}

impl std::error::Error for NotRegularFile {}

/// Opens `path` with `options`, without waiting when it names a pipe or device.
///
/// The handle is not checked: callers that need their own report for a file that is not regular check its metadata.
/// On Unix this sets the options' custom flags to `O_NONBLOCK`, replacing any custom flags already set.
///
/// # Errors
///
/// Returns the open's error. An open refused because the path is a special file (a pipe with no reader opened for
/// writing, a socket, or a device with no driver) is reported as not a regular file.
pub fn open_without_waiting(path: &Path, options: &mut OpenOptions) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    options.open(path).map_err(|error| {
        if names_a_special_file(&error) || names_a_directory(path, &error) {
            not_regular_file()
        } else {
            error
        }
    })
}

/// Opens the regular file at `path` with `options`, without waiting, and returns it with its handle's metadata.
///
/// # Errors
///
/// Returns the open's error, or an [`io::ErrorKind::InvalidInput`] error when the open handle is not a regular file,
/// which [`is_not_regular_file`] recognises.
pub fn open_regular_file(path: &Path, options: &mut OpenOptions) -> io::Result<(File, Metadata)> {
    let file = open_without_waiting(path, options)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(not_regular_file());
    }
    Ok((file, metadata))
}

/// Reads the whole of the regular file at `path`, as [`std::fs::read`] does but never waiting on a pipe or device.
///
/// # Errors
///
/// As [`open_regular_file`], or the read's error.
pub fn read_regular_file(path: &Path) -> io::Result<Vec<u8>> {
    let (mut file, metadata) = open_regular_file(path, OpenOptions::new().read(true))?;
    let mut bytes = buffer_for(metadata.len())?;
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// An empty buffer with room for a file of `size` bytes, or an [`io::ErrorKind::OutOfMemory`] error when no buffer
/// can hold it, as [`std::fs::read`] reports one: a sparse file can claim any size without using disk.
fn buffer_for(size: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(usize::try_from(size).unwrap_or(usize::MAX))
        .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;
    Ok(bytes)
}

/// Reads the regular file at `path` as UTF-8 text, as [`std::fs::read_to_string`] does but never waiting on a pipe or
/// device.
///
/// # Errors
///
/// As [`read_regular_file`], or an [`io::ErrorKind::InvalidData`] error when the content is not UTF-8.
pub fn read_regular_text(path: &Path) -> io::Result<String> {
    String::from_utf8(read_regular_file(path)?).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Whether `error` reports that the path is not a regular file, from one of the functions in this module.
#[must_use]
pub fn is_not_regular_file(error: &io::Error) -> bool {
    matches!(error.get_ref(), Some(inner) if inner.is::<NotRegularFile>())
}

/// An [`io::ErrorKind::InvalidInput`] error marking the path as not a regular file.
fn not_regular_file() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, NotRegularFile)
}

/// Whether `error` means the path names a directory: Windows refuses to open one as a file with access denied, which
/// would otherwise read as a permissions problem. Elsewhere a directory opens, and the handle's metadata tells.
fn names_a_directory(path: &Path, error: &io::Error) -> bool {
    cfg!(windows) && error.kind() == io::ErrorKind::PermissionDenied && path.is_dir()
}

/// Whether `error`, from an open with `O_NONBLOCK`, means the path names a special file that cannot be opened: a pipe
/// with no reader opened for writing, a socket, or a device with no driver.
fn names_a_special_file(error: &io::Error) -> bool {
    #[cfg(unix)]
    {
        // A socket is `ENXIO` on Linux and `EOPNOTSUPP` on macOS.
        matches!(error.raw_os_error(), Some(libc::ENXIO | libc::EOPNOTSUPP))
    }
    #[cfg(not(unix))]
    {
        let _ = error;
        false
    }
}

#[cfg(test)]
#[path = "regular_file_test.rs"]
mod tests;
