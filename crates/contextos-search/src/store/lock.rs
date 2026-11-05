//! A derived store's cross-process write lock, held for one write and waited for with a bound.
//!
//! Every `contextos` instance on a host may write a vault's derived stores. A store whose engine does not serialise
//! writers across processes on its own takes this lock around each write: an operating-system advisory lock on a file
//! beside the store, which every process resolving the same state directory shares. A writer that finds it held waits
//! for it, up to a bound, and then fails as `index/busy` having changed nothing. The operating system releases the
//! lock when its holder exits, so a crashed process never leaves a store locked.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{DerivedStore, StoreFailure};

/// How long a write waits for another instance's write to the same store: the same budget `SQLite`'s busy timeout
/// gives the link graph and vector stores, and tantivy's writer lock gives the text index.
pub const DEFAULT_WRITE_WAIT: Duration = Duration::from_secs(5);

/// How often a waiting writer tries the lock again.
const RETRY_INTERVAL: Duration = Duration::from_millis(10);

/// A store's write lock, held until it is dropped.
#[derive(Debug)]
pub struct StoreWriteLock {
    _file: File,
}

impl StoreWriteLock {
    /// Takes the exclusive lock on `lock_path` for `store`, creating the file when absent, trying again until `wait`
    /// has passed. A wait too long to represent waits without limit.
    ///
    /// # Errors
    ///
    /// Returns a busy failure when another holder still has the lock after `wait`, and an I/O failure when the lock
    /// file cannot be created, opened, or locked.
    pub fn acquire(store: DerivedStore, lock_path: &Path, wait: Duration) -> Result<Self, StoreFailure> {
        let file = open_lock_file(store, lock_path)?;
        let deadline = Instant::now().checked_add(wait);
        loop {
            match contextos_fs::try_lock_exclusive(&file) {
                Ok(true) => return Ok(Self { _file: file }),
                Ok(false) => {}
                Err(error) => {
                    return Err(StoreFailure::io(
                        store,
                        lock_path.display().to_string(),
                        format!("taking the write lock: {error}"),
                    ));
                }
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(StoreFailure::busy(
                    store,
                    lock_path.display().to_string(),
                    format!(
                        "another instance held the write lock for longer than the {} ms a write waits",
                        wait.as_millis()
                    ),
                ));
            }
            std::thread::sleep(RETRY_INTERVAL);
        }
    }
}

/// The write lock file beside a store file: `<file name>.lock`.
#[must_use]
pub fn lock_path_beside(file: &Path) -> PathBuf {
    let mut name = file.file_name().map(std::ffi::OsStr::to_os_string).unwrap_or_default();
    name.push(".lock");
    file.with_file_name(name)
}

fn open_lock_file(store: DerivedStore, lock_path: &Path) -> Result<File, StoreFailure> {
    let failed = |what: &str, error: std::io::Error| {
        StoreFailure::io(store, lock_path.display().to_string(), format!("{what}: {error}"))
    };
    if let Some(directory) = lock_path.parent() {
        std::fs::create_dir_all(directory).map_err(|error| failed("creating the lock file's directory", error))?;
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
        .map_err(|error| failed("opening the write lock", error))
}

#[cfg(test)]
#[path = "lock_test.rs"]
mod tests;
