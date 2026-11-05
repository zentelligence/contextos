//! The host's state file for shared jobs, read and changed safely by several processes.
//!
//! The Web scheduler and a command-line process both change this one file, so a change is made under an exclusive
//! advisory lock on a file beside it, the same mechanism the shared claim uses and with the same requirement that the
//! directory is on a local disk. Inside the lock the current file is read, changed, and replaced atomically, so one
//! writer never loses another's change. The lock is held only for that read, change, and write, never across a run.
//!
//! A state file that does not parse is refused and left as it was, never repaired. The file holds lifecycle state, a
//! schedule, and recorded facts, and no input value, path, or credential.

use std::fs::{self, File};
use std::io;
use std::path::PathBuf;

use contextos_workflows::JobsState;
use thiserror::Error;

use crate::atomic_write::write_atomically;
use crate::bounded_read::{BoundedReadError, read_text_within};

/// The largest state file the store will read. It holds a few lines per job.
const MAX_STATE_BYTES: u64 = 4 * 1024 * 1024;

const STATE_FILE: &str = "shared-jobs.toml";
const LOCK_FILE: &str = "shared-jobs.lock";

#[derive(Debug, Error)]
pub enum SharedStateError {
    #[error(
        "the shared job state at {} is not valid and is not repaired: {reason}",
        crate::trailing_path::Trailing { path, count: 2 }
    )]
    Malformed { path: PathBuf, reason: String },
    #[error("the shared job state could not be read or written: {0}")]
    Io(#[from] io::Error),
    #[error("the shared job state could not be serialised: {0}")]
    Serialise(#[from] toml::ser::Error),
}

/// Where the host keeps its shared job state.
#[derive(Clone, Debug)]
pub struct SharedStateStore {
    directory: PathBuf,
}

impl SharedStateStore {
    /// A store whose file and lock live in `directory`, which is created on the first change.
    #[must_use]
    pub const fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    #[must_use]
    pub fn state_path(&self) -> PathBuf {
        self.directory.join(STATE_FILE)
    }

    #[must_use]
    pub fn lock_path(&self) -> PathBuf {
        self.directory.join(LOCK_FILE)
    }

    /// The current state. A store with no file yet is an empty state. The file is only ever replaced whole, so a read
    /// needs no lock and never sees a half-written one.
    ///
    /// Blocking: call it from a blocking thread.
    ///
    /// # Errors
    ///
    /// Returns [`SharedStateError::Malformed`] when the file does not parse, and [`SharedStateError::Io`] when it
    /// cannot be read.
    pub fn read(&self) -> Result<JobsState, SharedStateError> {
        self.read_file()
    }

    /// Applies `change` to the current state under the lock, and writes the result when `change` altered it. A change
    /// that alters nothing writes nothing, so a refused transition leaves the file, and even its existence, as it was.
    ///
    /// Blocking: waits for the lock, so call it from a blocking thread.
    ///
    /// # Errors
    ///
    /// Returns [`SharedStateError::Malformed`] when the file does not parse, and leaves it untouched, and
    /// [`SharedStateError::Io`] or [`SharedStateError::Serialise`] when it cannot be locked or written.
    pub fn update<R>(&self, change: impl FnOnce(&mut JobsState) -> R) -> Result<R, SharedStateError> {
        fs::create_dir_all(&self.directory)?;
        let lock = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.lock_path())?;
        lock.lock()?;
        let outcome = self.change_locked(change);
        // Closing the file would release the lock too, so a failure to unlock changes nothing worth reporting.
        let _ = lock.unlock();
        outcome
    }

    fn change_locked<R>(&self, change: impl FnOnce(&mut JobsState) -> R) -> Result<R, SharedStateError> {
        let before = self.read_file()?;
        let mut state = before.clone();
        let result = change(&mut state);
        if state != before {
            write_atomically(&self.state_path(), toml::to_string(&state)?.as_bytes())?;
        }
        Ok(result)
    }

    fn read_file(&self) -> Result<JobsState, SharedStateError> {
        let path = self.state_path();
        match read_text_within(&path, MAX_STATE_BYTES) {
            Ok(text) => toml::from_str(&text).map_err(|error| SharedStateError::Malformed {
                path: path.clone(),
                // The parser's message alone: its `Display` repeats the offending source line.
                reason: error.message().to_owned(),
            }),
            Err(BoundedReadError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(JobsState::default()),
            Err(BoundedReadError::Io(error)) => Err(error.into()),
            Err(BoundedReadError::TooLarge { .. }) => Err(SharedStateError::Malformed {
                path,
                reason: format!("the file is larger than the {MAX_STATE_BYTES} byte limit"),
            }),
            Err(BoundedReadError::NotText { .. }) => Err(SharedStateError::Malformed {
                path,
                reason: "the file is not UTF-8 text".to_owned(),
            }),
        }
    }
}

#[cfg(test)]
#[path = "shared_state_test.rs"]
mod tests;
