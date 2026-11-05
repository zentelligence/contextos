//! Atomic writes for files and folders outside every vault.
//!
//! Host projection and configuration writing put files where no vault write pipeline applies (an agent host's own
//! folders, a configuration file). They still need the persistence discipline vault writes follow: a reader never
//! observes a partial file, and an interrupted folder replacement leaves a state one recovery call can repair.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;

use crate::FsError;

/// Suffix of the sibling a replacement folder is built in.
const NEW_SUFFIX: &str = ".contextos-new";
/// Suffix of the sibling the previous folder is parked in while the new one is installed.
const OLD_SUFFIX: &str = ".contextos-old";

/// Writes `contents` to `path` as [`replace_file_atomically`] does, first creating the parent folder when missing.
///
/// # Errors
///
/// Returns [`FsError::CreateParent`] when `path` has no parent or the parent cannot be created, and otherwise the
/// errors of [`replace_file_atomically`]. `path` is unchanged on any failure.
pub fn write_file_atomically(path: &Path, contents: &[u8]) -> Result<(), FsError> {
    let parent = parent_of(path)?;
    fs::create_dir_all(parent).map_err(|source| FsError::CreateParent {
        path: parent.to_path_buf(),
        source,
    })?;
    replace_file_atomically(path, contents)
}

/// Writes `contents` to `path` in a folder that must already exist: a uniquely named temporary file in the same
/// folder, flushed, then atomically renamed over the target.
///
/// This is the one atomic writer for files outside every vault; it mirrors the discipline vault writes follow. It
/// never creates a folder, so a caller whose missing folder means something (a run record cleaned up, say) is told
/// rather than having the folder recreated. When the target already exists, the temporary file is given its
/// permissions before the rename, so the replacement is never visible with the temporary file's default mode.
///
/// # Errors
///
/// Returns [`FsError::CreateParent`] when `path` has no parent, and [`FsError::CreateTemporary`] (which carries
/// [`io::ErrorKind::NotFound`] when the folder does not exist), [`FsError::WriteTemporary`] (also when the target's
/// permissions cannot be applied to the temporary file), [`FsError::FlushTemporary`], or
/// [`FsError::PersistTemporary`] for the corresponding step. `path` is unchanged on any failure.
pub fn replace_file_atomically(path: &Path, contents: &[u8]) -> Result<(), FsError> {
    replace_file_observed(path, contents, &NoFileFaults)
}

/// A point in a file replacement, reported to a [`FileReplaceObserver`] once it completes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileReplaceStep {
    /// The temporary file holds the new content and the target's permissions, and the rename is next.
    TemporaryReady,
}

/// Observes a file replacement. Tests inspect or interrupt it here instead of relying on timing.
pub trait FileReplaceObserver {
    /// Called after `step` completes.
    ///
    /// # Errors
    ///
    /// Any error abandons the replacement before the rename: the target is unchanged and the temporary file removed.
    fn reached(&self, step: FileReplaceStep) -> io::Result<()>;
}

/// An observer that never interrupts.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoFileFaults;

impl FileReplaceObserver for NoFileFaults {
    fn reached(&self, _step: FileReplaceStep) -> io::Result<()> {
        Ok(())
    }
}

/// As [`replace_file_atomically`], reporting each step to `observer`.
///
/// # Errors
///
/// As [`replace_file_atomically`], and [`FsError::AtomicWriteInterrupted`] when `observer` fails a step.
pub fn replace_file_observed(path: &Path, contents: &[u8], observer: &dyn FileReplaceObserver) -> Result<(), FsError> {
    let parent = parent_of(path)?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(|source| FsError::CreateTemporary {
        path: path.to_path_buf(),
        source,
    })?;
    temporary
        .write_all(contents)
        .map_err(|source| FsError::WriteTemporary {
            path: path.to_path_buf(),
            source,
        })?;
    // Flushed before the rename, so a crash leaves the old file or the whole new one, never a short one.
    temporary
        .as_file()
        .sync_all()
        .map_err(|source| FsError::FlushTemporary {
            path: path.to_path_buf(),
            source,
        })?;
    // An existing target's permissions are applied here, before the rename, so no reader ever sees the replacement with
    // the temporary file's default mode. A target that does not exist yet keeps that default.
    if let Ok(existing) = fs::metadata(path)
        && existing.is_file()
    {
        temporary
            .as_file()
            .set_permissions(existing.permissions())
            .map_err(|source| FsError::WriteTemporary {
                path: path.to_path_buf(),
                source,
            })?;
    }
    observer
        .reached(FileReplaceStep::TemporaryReady)
        .map_err(|source| FsError::AtomicWriteInterrupted {
            path: path.to_path_buf(),
            source,
        })?;
    temporary.persist(path).map_err(|error| FsError::PersistTemporary {
        path: path.to_path_buf(),
        source: error.error,
    })?;
    sync_directory(parent);
    Ok(())
}

/// Creates `path` with exactly `contents`, and never replaces a file that is already there: a uniquely named temporary
/// file in the same folder, flushed, then moved into place with a rename that fails when the target exists. On Unix the
/// file takes the permissions a hand-placed file would (the umask default).
///
/// The no-replace rule is enforced by the filesystem at the moment of the move, not by an earlier existence check, so
/// two writers racing for one name leave exactly one winner. A link at `path`, dangling or not, counts as existing and
/// is never written through. It never creates a folder.
///
/// # Errors
///
/// Returns [`FsError::CreateParent`] when `path` has no parent, [`FsError::CreateTemporary`] (carrying
/// [`io::ErrorKind::NotFound`] when the folder does not exist), [`FsError::WriteTemporary`],
/// [`FsError::FlushTemporary`], and [`FsError::PersistTemporary`] for the corresponding step, which carries
/// [`io::ErrorKind::AlreadyExists`] when something is already at `path`. `path` is unchanged on any failure and no
/// temporary file is left behind.
pub fn create_file_atomically(path: &Path, contents: &[u8]) -> Result<(), FsError> {
    create_file_observed(path, contents, &NoFileFaults)
}

/// As [`create_file_atomically`], reporting each step to `observer`.
///
/// # Errors
///
/// As [`create_file_atomically`], and [`FsError::AtomicWriteInterrupted`] when `observer` fails a step.
pub fn create_file_observed(path: &Path, contents: &[u8], observer: &dyn FileReplaceObserver) -> Result<(), FsError> {
    let parent = parent_of(path)?;
    let mut temporary = new_shareable_temporary(parent).map_err(|source| FsError::CreateTemporary {
        path: path.to_path_buf(),
        source,
    })?;
    temporary
        .write_all(contents)
        .map_err(|source| FsError::WriteTemporary {
            path: path.to_path_buf(),
            source,
        })?;
    // Flushed before the move, so a crash leaves no file or the whole one, never a short one.
    temporary
        .as_file()
        .sync_all()
        .map_err(|source| FsError::FlushTemporary {
            path: path.to_path_buf(),
            source,
        })?;
    observer
        .reached(FileReplaceStep::TemporaryReady)
        .map_err(|source| FsError::AtomicWriteInterrupted {
            path: path.to_path_buf(),
            source,
        })?;
    // The no-replace move: the filesystem refuses when anything, a dangling link included, is at `path`, and the
    // temporary file is removed when the error is dropped.
    temporary
        .persist_noclobber(path)
        .map_err(|error| FsError::PersistTemporary {
            path: path.to_path_buf(),
            source: error.error,
        })?;
    sync_directory(parent);
    Ok(())
}

/// A temporary file in `parent` that takes the permissions a hand-placed file would: on Unix the mode is requested as
/// `0o666` and the process umask narrows it, instead of the private `0o600` a bare temporary file gets.
fn new_shareable_temporary(parent: &Path) -> io::Result<NamedTempFile> {
    #[cfg(unix)]
    {
        tempfile::Builder::new()
            .permissions(std::os::unix::fs::PermissionsExt::from_mode(0o666))
            .tempfile_in(parent)
    }
    #[cfg(not(unix))]
    {
        tempfile::Builder::new().tempfile_in(parent)
    }
}

/// The folder `path` would be written in, refusing a bare file name with no folder.
fn parent_of(path: &Path) -> Result<&Path, FsError> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| FsError::CreateParent {
            path: path.to_path_buf(),
            source: io::Error::new(io::ErrorKind::InvalidInput, "target has no parent directory"),
        })
}

/// Flushes the folder entry the rename wrote, where the platform lets a folder be opened. A failure is ignored: the
/// file itself is already flushed, and this only narrows the window in which the rename could be lost.
fn sync_directory(directory: &Path) {
    #[cfg(unix)]
    if let Ok(handle) = fs::File::open(directory) {
        let _ = handle.sync_all();
    }
    #[cfg(not(unix))]
    let _ = directory;
}

/// A point in a folder replacement, reported to a [`FolderReplaceObserver`] once it completes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplaceStep {
    /// The new folder is fully built beside the target.
    NewFolderBuilt,
    /// The previous target was renamed aside.
    PreviousMovedAside,
    /// The new folder was renamed into the target position.
    NewFolderInstalled,
    /// The previous folder was removed.
    PreviousRemoved,
}

/// Observes a folder replacement. Tests inject faults here instead of relying on timing.
pub trait FolderReplaceObserver {
    /// Called after `step` completes.
    ///
    /// # Errors
    ///
    /// Any error abandons the replacement exactly where it stands, with no cleanup, which models an interruption.
    fn reached(&self, step: ReplaceStep) -> io::Result<()>;
}

/// An observer that never interrupts.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoFolderFaults;

impl FolderReplaceObserver for NoFolderFaults {
    fn reached(&self, _step: ReplaceStep) -> io::Result<()> {
        Ok(())
    }
}

/// What [`recover_folder`] repaired.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FolderRecovery {
    /// The previous folder was renamed back into the target position.
    pub restored_previous: bool,
    /// An incomplete new folder was removed.
    pub removed_incomplete: bool,
    /// A superseded previous folder was removed.
    pub removed_previous: bool,
}

/// Replaces the folder at `target` with one `build` fills.
///
/// The new folder is built at the fixed sibling `.<name>.contextos-new`; an existing target is renamed to
/// `.<name>.contextos-old`, the new folder is renamed into place, and the old one is removed. The fixed names are safe
/// only while the caller serialises replacements of the same target (the host-level projection lock). Any earlier
/// interruption is repaired first by [`recover_folder`]. A failure of `build` or of a rename restores the previous
/// state on a best-effort basis; an interruption injected through `observer` is deliberately left as is.
///
/// # Errors
///
/// Returns [`FsError::ReplaceDirectory`] naming the step that failed, [`FsError::ReplaceCleanup`] when the new folder
/// is in place but the previous one could not be removed, or [`FsError::CreateParent`] when the target's
/// parent cannot be created.
pub fn replace_folder(
    target: &Path,
    build: impl FnOnce(&Path) -> io::Result<()>,
    observer: &dyn FolderReplaceObserver,
) -> Result<(), FsError> {
    let fail = |source: io::Error| FsError::ReplaceDirectory {
        path: target.to_path_buf(),
        source,
    };
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| FsError::CreateParent {
            path: target.to_path_buf(),
            source: io::Error::new(io::ErrorKind::InvalidInput, "target has no parent directory"),
        })?;
    fs::create_dir_all(parent).map_err(|source| FsError::CreateParent {
        path: parent.to_path_buf(),
        source,
    })?;
    recover_folder(target)?;

    let new = sibling(target, NEW_SUFFIX)?;
    let old = sibling(target, OLD_SUFFIX)?;
    let staged = fs::create_dir(&new).and_then(|()| build(&new));
    if let Err(source) = staged {
        let _ = remove_path(&new);
        return Err(fail(source));
    }
    observer.reached(ReplaceStep::NewFolderBuilt).map_err(fail)?;

    let had_previous = target.symlink_metadata().is_ok();
    if had_previous {
        if let Err(source) = fs::rename(target, &old) {
            let _ = remove_path(&new);
            return Err(fail(source));
        }
        observer.reached(ReplaceStep::PreviousMovedAside).map_err(fail)?;
    }
    if let Err(source) = fs::rename(&new, target) {
        let _ = recover_folder(target);
        return Err(fail(source));
    }
    sync_directory(parent);
    // From here the new folder is in place, so a failure leaves only a leftover for the next recovery to remove.
    let cleanup = |source: io::Error| FsError::ReplaceCleanup {
        path: target.to_path_buf(),
        source,
    };
    observer.reached(ReplaceStep::NewFolderInstalled).map_err(cleanup)?;
    if had_previous {
        remove_path(&old).map_err(cleanup)?;
    }
    observer.reached(ReplaceStep::PreviousRemoved).map_err(fail)?;
    Ok(())
}

/// Restores a consistent state for `target` after an interrupted [`replace_folder`], whatever step it stopped at.
///
/// A previous folder parked aside while the target is missing is renamed back; an incomplete new folder is removed;
/// a superseded previous folder is removed once the target is present. Calling it with nothing to repair does nothing.
///
/// # Errors
///
/// Returns [`FsError::ReplaceDirectory`] when a repair step fails.
pub fn recover_folder(target: &Path) -> Result<FolderRecovery, FsError> {
    let fail = |source: io::Error| FsError::ReplaceDirectory {
        path: target.to_path_buf(),
        source,
    };
    let new = sibling(target, NEW_SUFFIX)?;
    let old = sibling(target, OLD_SUFFIX)?;
    let mut recovery = FolderRecovery::default();
    let present = |path: &Path| path.symlink_metadata().is_ok();

    if present(&old) && !present(target) {
        fs::rename(&old, target).map_err(fail)?;
        recovery.restored_previous = true;
    }
    if present(&new) {
        remove_path(&new).map_err(fail)?;
        recovery.removed_incomplete = true;
    }
    if present(&old) {
        remove_path(&old).map_err(fail)?;
        recovery.removed_previous = true;
    }
    Ok(recovery)
}

/// Removes the file, folder tree, or link at `path` without following a link, and reports whether anything was there.
///
/// # Errors
///
/// Returns [`FsError::DeletePath`] when the entry exists but cannot be removed.
pub fn remove_entry(path: &Path) -> Result<bool, FsError> {
    match remove_path(path) {
        Ok(()) => {
            if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
                sync_directory(parent);
            }
            Ok(true)
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(FsError::DeletePath {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// A point in a file removal, reported to a [`FileRemoveObserver`] once it completes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileRemoveStep {
    /// The file's directory entry is gone.
    Removed,
    /// The folder that held the entry was flushed, so the removal survives a crash.
    ParentSynced,
}

/// Observes a file removal. Tests inspect the order of its steps here instead of relying on timing.
pub trait FileRemoveObserver {
    /// Called after `step` completes.
    fn reached(&self, step: FileRemoveStep);
}

/// Removes the file at `path` without following a link, and reports whether a file was there. The folder that held it
/// is flushed before this returns, so a caller that writes state afterwards never has the state outlive the removal.
///
/// # Errors
///
/// Returns [`FsError::DeletePath`] when the entry exists but cannot be removed.
pub fn remove_file_observed(path: &Path, observer: &dyn FileRemoveObserver) -> Result<bool, FsError> {
    match fs::remove_file(path) {
        Ok(()) => {
            observer.reached(FileRemoveStep::Removed);
            if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
                sync_directory(parent);
            }
            observer.reached(FileRemoveStep::ParentSynced);
            Ok(true)
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(FsError::DeletePath {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// The fixed sibling of `target` named `.<name><suffix>`.
fn sibling(target: &Path, suffix: &str) -> Result<PathBuf, FsError> {
    let name = target.file_name().ok_or_else(|| FsError::ReplaceDirectory {
        path: target.to_path_buf(),
        source: io::Error::new(io::ErrorKind::InvalidInput, "target has no final component"),
    })?;
    let mut sibling_name = OsString::from(".");
    sibling_name.push(name);
    sibling_name.push(suffix);
    Ok(target.with_file_name(sibling_name))
}

/// Removes a folder tree, a file, or a link itself without following it.
fn remove_path(path: &Path) -> io::Result<()> {
    if path.symlink_metadata()?.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

#[cfg(test)]
#[path = "host_file_test.rs"]
mod tests;
