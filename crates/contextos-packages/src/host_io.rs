//! The host package store's I/O: [`crate::StoreIo`] over the store's folder under the platform app-data directory.
//!
//! The store's root comes from trusted configuration and is resolved once. Every path is formed from a
//! [`crate::StorePath`] below that root, every read refuses a symbolic link, junction, or other reparse point at any
//! level and reports one in a listing as [`crate::StoreEntryKind::Link`], every write is a temporary file flushed and
//! renamed into place, and the lock is the store's advisory lock file. Blocking work runs on a blocking thread, never
//! on an executor thread.

use std::fs::{self, File};
use std::io::{self, Read as _};
use std::path::PathBuf;
use std::time::Duration;

use contextos_fs::host_file::{create_file_atomically, replace_file_atomically};
use contextos_fs::{FsError, HostLockGuard, is_link_or_reparse_point, lock_host};

use crate::io::{StoreEntry, StoreEntryKind, StoreFailure, StoreFailureKind, StoreHome, StoreIo, StoreTreeEntry};
use crate::path::{LOCK_FILE_NAME, StorePath};

/// The host package store's I/O over a folder on the local disk.
#[derive(Clone, Debug)]
pub struct HostStoreIo {
    layout: HostLayout,
}

impl HostStoreIo {
    /// The store whose folder is `root`, taking its lock for no longer than `lock_wait`. `root` comes from trusted
    /// configuration, never from a request, and need not exist yet.
    #[must_use]
    pub fn new(root: PathBuf, lock_wait: Duration) -> Self {
        Self {
            layout: HostLayout { root, lock_wait },
        }
    }
}

impl StoreIo for HostStoreIo {
    type Lock = HostLockGuard;

    fn home(&self) -> StoreHome {
        StoreHome::Host
    }

    async fn lock(&self) -> Result<HostLockGuard, StoreFailure> {
        let layout = self.layout.clone();
        blocking(move || layout.lock()).await
    }

    async fn list(&self, path: &StorePath) -> Result<Vec<StoreEntry>, StoreFailure> {
        let layout = self.layout.clone();
        let path = path.segments().to_vec();
        blocking(move || layout.list(&path)).await
    }

    async fn list_tree(&self, path: &StorePath, max_entries: usize) -> Result<Vec<StoreTreeEntry>, StoreFailure> {
        let layout = self.layout.clone();
        let path = path.segments().to_vec();
        blocking(move || layout.list_tree(&path, max_entries)).await
    }

    async fn read(&self, path: &StorePath, max_bytes: u64) -> Result<Vec<u8>, StoreFailure> {
        let layout = self.layout.clone();
        let path = path.segments().to_vec();
        blocking(move || layout.read(&path, max_bytes)).await
    }

    async fn write_new(&self, path: &StorePath, contents: &[u8]) -> Result<(), StoreFailure> {
        let layout = self.layout.clone();
        let path = path.segments().to_vec();
        let contents = contents.to_vec();
        blocking(move || layout.write_new(&path, &contents)).await
    }

    async fn replace(&self, path: &StorePath, contents: &[u8]) -> Result<(), StoreFailure> {
        let layout = self.layout.clone();
        let path = path.segments().to_vec();
        let contents = contents.to_vec();
        blocking(move || layout.replace(&path, &contents)).await
    }

    async fn create_dir_all(&self, path: &StorePath) -> Result<(), StoreFailure> {
        let layout = self.layout.clone();
        let path = path.segments().to_vec();
        blocking(move || layout.create_dir_all(&path)).await
    }

    async fn rename(&self, from: &StorePath, to: &StorePath) -> Result<(), StoreFailure> {
        let layout = self.layout.clone();
        let from = from.segments().to_vec();
        let to = to.segments().to_vec();
        blocking(move || layout.rename(&from, &to)).await
    }

    async fn remove_tree(&self, path: &StorePath) -> Result<bool, StoreFailure> {
        let layout = self.layout.clone();
        let path = path.segments().to_vec();
        blocking(move || layout.remove_tree(&path)).await
    }
}

/// Runs `work` on a blocking thread. A task that was cancelled or panicked is a failure of the store, not a result.
async fn blocking<T, F>(work: F) -> Result<T, StoreFailure>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, StoreFailure> + Send + 'static,
{
    tokio::task::spawn_blocking(work).await.unwrap_or_else(|_| {
        Err(StoreFailure {
            kind: StoreFailureKind::Failed,
            detail: "the store's blocking task did not finish".to_owned(),
        })
    })
}

/// The synchronous half of the adapter: every operation as plain blocking filesystem work.
#[derive(Clone, Debug)]
struct HostLayout {
    root: PathBuf,
    lock_wait: Duration,
}

/// An I/O error and the store-relative path it happened at, for [`StoreFailure::from`].
struct IoFault<'a> {
    at: &'a [String],
    error: &'a io::Error,
}

impl From<IoFault<'_>> for StoreFailure {
    fn from(fault: IoFault<'_>) -> Self {
        let kind = match fault.error.kind() {
            io::ErrorKind::NotFound => StoreFailureKind::NotFound,
            io::ErrorKind::AlreadyExists => StoreFailureKind::AlreadyExists,
            _ => StoreFailureKind::Failed,
        };
        Self {
            kind,
            detail: format!("{}: {}", Shown(fault.at), fault.error.kind()),
        }
    }
}

/// A store path as text for a failure's description: `/`-separated, with `.` for the store's root.
struct Shown<'a>(&'a [String]);

impl std::fmt::Display for Shown<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.is_empty() {
            f.write_str(".")
        } else {
            f.write_str(&self.0.join("/"))
        }
    }
}

/// What stands at one place, looked at without following a link.
enum Standing {
    Nothing,
    Link,
    Entry(fs::Metadata),
}

impl HostLayout {
    fn refused(at: &[String], why: &str) -> StoreFailure {
        StoreFailure {
            kind: StoreFailureKind::Refused,
            detail: format!("{}: {why}", Shown(at)),
        }
    }

    fn failed(at: &[String], error: &io::Error) -> StoreFailure {
        StoreFailure::from(IoFault { at, error })
    }

    /// The host path of `path`, refusing a segment that is not one plain name. The engine builds only valid paths, so
    /// this guards the adapter against a path built some other way.
    fn host_path(&self, path: &[String]) -> Result<PathBuf, StoreFailure> {
        let mut host = self.root.clone();
        for segment in path {
            let plain =
                !segment.is_empty() && segment != "." && segment != ".." && !segment.contains(['/', '\\', ':', '\0']);
            if !plain {
                return Err(Self::refused(path, "not a plain store path"));
            }
            host.push(segment);
        }
        Ok(host)
    }

    /// Looks at the place `path` names without following a link.
    fn stand(&self, path: &[String]) -> Result<Standing, StoreFailure> {
        match fs::symlink_metadata(self.host_path(path)?) {
            Ok(metadata) if is_link_or_reparse_point(&metadata) => Ok(Standing::Link),
            Ok(metadata) => Ok(Standing::Entry(metadata)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Standing::Nothing),
            Err(error) => Err(Self::failed(path, &error)),
        }
    }

    /// Refuses when any folder above the last segment of `path` is a link or reparse point, or a file. A folder that
    /// is missing is not refused; the operation reports it.
    fn check_parents(&self, path: &[String]) -> Result<(), StoreFailure> {
        let segments = path;
        for depth in 1..segments.len() {
            let above = &segments[..depth];
            match self.stand(above)? {
                Standing::Nothing => return Ok(()),
                Standing::Link => return Err(Self::refused(above, "a link or reparse point on the way")),
                Standing::Entry(metadata) if !metadata.is_dir() => {
                    return Err(Self::refused(above, "not a folder"));
                }
                Standing::Entry(_) => {}
            }
        }
        Ok(())
    }

    /// The host path of `path` once its parents are known to be plain folders.
    fn confined(&self, path: &[String]) -> Result<PathBuf, StoreFailure> {
        self.check_parents(path)?;
        self.host_path(path)
    }

    fn lock(&self) -> Result<HostLockGuard, StoreFailure> {
        let lock_path = [LOCK_FILE_NAME.to_owned()];
        self.check_parents(&lock_path)?;
        match fs::symlink_metadata(&self.root) {
            Ok(metadata) if !metadata.is_dir() && !metadata.file_type().is_symlink() => {
                return Err(Self::refused(&[], "the store's root is not a folder"));
            }
            _ => {}
        }
        match self.stand(&lock_path)? {
            Standing::Link => return Err(Self::refused(&lock_path, "a link or reparse point")),
            Standing::Entry(_) | Standing::Nothing => {}
        }
        lock_host(&self.host_path(&lock_path)?, self.lock_wait).map_err(|error| match error {
            FsError::HostLockBusy { .. } => StoreFailure {
                kind: StoreFailureKind::LockBusy,
                detail: "another operation holds the store's lock".to_owned(),
            },
            other => StoreFailure {
                kind: StoreFailureKind::Failed,
                detail: format!("the store's lock could not be taken: {}", io::Error::from(other).kind()),
            },
        })
    }

    fn list(&self, path: &[String]) -> Result<Vec<StoreEntry>, StoreFailure> {
        match self.stand(path)? {
            Standing::Nothing => return Err(Self::failed(path, &io::ErrorKind::NotFound.into())),
            Standing::Link => return Err(Self::refused(path, "a link or reparse point")),
            Standing::Entry(metadata) if !metadata.is_dir() => return Err(Self::refused(path, "not a folder")),
            Standing::Entry(_) => {}
        }
        self.check_parents(path)?;
        let host = self.host_path(path)?;
        let mut entries = Vec::new();
        for entry in fs::read_dir(&host).map_err(|error| Self::failed(path, &error))? {
            let entry = entry.map_err(|error| Self::failed(path, &error))?;
            let metadata = entry.metadata().map_err(|error| Self::failed(path, &error))?;
            entries.push(StoreEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                kind: StoreEntryKind::from(&metadata),
            });
        }
        Ok(entries)
    }

    fn list_tree(&self, path: &[String], max_entries: usize) -> Result<Vec<StoreTreeEntry>, StoreFailure> {
        let mut found = Vec::new();
        // Folders still to read, each with its path relative to `path`.
        let mut pending = vec![String::new()];
        while let Some(relative) = pending.pop() {
            let mut here = path.to_vec();
            here.extend(relative.split('/').filter(|part| !part.is_empty()).map(str::to_owned));
            for entry in self.list(&here)? {
                let entry_path = if relative.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{relative}/{}", entry.name)
                };
                if found.len() >= max_entries {
                    return Err(StoreFailure {
                        kind: StoreFailureKind::TooLarge,
                        detail: format!("{}: more than {max_entries} entries", Shown(path)),
                    });
                }
                if entry.kind == StoreEntryKind::Directory {
                    pending.push(entry_path.clone());
                }
                found.push(StoreTreeEntry {
                    path: entry_path,
                    kind: entry.kind,
                });
            }
        }
        Ok(found)
    }

    fn read(&self, path: &[String], max_bytes: u64) -> Result<Vec<u8>, StoreFailure> {
        let host = self.confined(path)?;
        match self.stand(path)? {
            Standing::Nothing => return Err(Self::failed(path, &io::ErrorKind::NotFound.into())),
            Standing::Link => return Err(Self::refused(path, "a link or reparse point")),
            Standing::Entry(metadata) if !metadata.is_file() => return Err(Self::refused(path, "not a regular file")),
            Standing::Entry(metadata) if metadata.len() > max_bytes => return Err(Self::too_large(path, max_bytes)),
            Standing::Entry(_) => {}
        }
        let file = File::open(&host).map_err(|error| Self::failed(path, &error))?;
        // The file may have grown since it was measured, so the read itself is bounded too.
        let mut contents = Vec::new();
        file.take(max_bytes.saturating_add(1))
            .read_to_end(&mut contents)
            .map_err(|error| Self::failed(path, &error))?;
        if u64::try_from(contents.len()).map_or(true, |length| length > max_bytes) {
            return Err(Self::too_large(path, max_bytes));
        }
        Ok(contents)
    }

    fn too_large(at: &[String], max_bytes: u64) -> StoreFailure {
        StoreFailure {
            kind: StoreFailureKind::TooLarge,
            detail: format!("{}: more than {max_bytes} bytes", Shown(at)),
        }
    }

    fn write_new(&self, path: &[String], contents: &[u8]) -> Result<(), StoreFailure> {
        let host = self.confined(path)?;
        create_file_atomically(&host, contents).map_err(|error| Self::failed(path, &io::Error::from(error)))
    }

    fn replace(&self, path: &[String], contents: &[u8]) -> Result<(), StoreFailure> {
        let host = self.confined(path)?;
        match self.stand(path)? {
            Standing::Link => return Err(Self::refused(path, "a link or reparse point")),
            Standing::Entry(metadata) if !metadata.is_file() => return Err(Self::refused(path, "not a regular file")),
            Standing::Entry(_) | Standing::Nothing => {}
        }
        // The file is created when absent, and so is the folder it sits in, the store's root included.
        self.create_dir_all(&path[..path.len().saturating_sub(1)])?;
        replace_file_atomically(&host, contents).map_err(|error| Self::failed(path, &io::Error::from(error)))
    }

    fn create_dir_all(&self, path: &[String]) -> Result<(), StoreFailure> {
        // Every segment is checked before any folder is made, so a bad path leaves nothing behind.
        self.host_path(path)?;
        // The store's root is trusted configuration and may be missing with its own parents.
        fs::create_dir_all(&self.root).map_err(|error| Self::failed(&[], &error))?;
        let segments = path;
        for depth in 1..=segments.len() {
            let level = &segments[..depth];
            match self.stand(level)? {
                Standing::Link => return Err(Self::refused(level, "a link or reparse point on the way")),
                Standing::Entry(metadata) if metadata.is_dir() => {}
                Standing::Entry(_) => {
                    return Err(StoreFailure {
                        kind: StoreFailureKind::AlreadyExists,
                        detail: format!("{}: not a folder", Shown(level)),
                    });
                }
                Standing::Nothing => match fs::create_dir(self.host_path(level)?) {
                    Ok(()) => {}
                    // Another process may create the same folder first; a folder is what is wanted.
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists && self.is_plain_dir(level)? => {}
                    Err(error) => return Err(Self::failed(level, &error)),
                },
            }
        }
        Ok(())
    }

    fn is_plain_dir(&self, path: &[String]) -> Result<bool, StoreFailure> {
        Ok(matches!(self.stand(path)?, Standing::Entry(metadata) if metadata.is_dir()))
    }

    fn rename(&self, from: &[String], to: &[String]) -> Result<(), StoreFailure> {
        let from_host = self.confined(from)?;
        let to_host = self.confined(to)?;
        if matches!(self.stand(from)?, Standing::Nothing) {
            return Err(Self::failed(from, &io::ErrorKind::NotFound.into()));
        }
        // Held under the store's lock, so nothing else creates `to` between this look and the rename: the platform
        // rename replaces a file, or an empty folder, which a store must never do.
        if !matches!(self.stand(to)?, Standing::Nothing) {
            return Err(Self::failed(to, &io::ErrorKind::AlreadyExists.into()));
        }
        fs::rename(&from_host, &to_host).map_err(|error| Self::failed(from, &error))
    }

    fn remove_tree(&self, path: &[String]) -> Result<bool, StoreFailure> {
        let host = self.confined(path)?;
        match self.stand(path)? {
            Standing::Nothing => Ok(false),
            // The link itself goes; what it points at is not the store's to delete.
            Standing::Link => fs::remove_file(&host)
                .or_else(|_| fs::remove_dir(&host))
                .map(|()| true)
                .map_err(|error| Self::failed(path, &error)),
            Standing::Entry(metadata) if metadata.is_dir() => {
                fs::remove_dir_all(&host).map_err(|error| Self::failed(path, &error))?;
                Ok(true)
            }
            Standing::Entry(_) => fs::remove_file(&host)
                .map(|()| true)
                .map_err(|error| Self::failed(path, &error)),
        }
    }
}

impl From<&fs::Metadata> for StoreEntryKind {
    fn from(metadata: &fs::Metadata) -> Self {
        if is_link_or_reparse_point(metadata) {
            Self::Link
        } else if metadata.is_dir() {
            Self::Directory
        } else if metadata.is_file() {
            Self::File { size: metadata.len() }
        } else {
            Self::Other
        }
    }
}

#[cfg(test)]
#[path = "host_io_test.rs"]
mod tests;
