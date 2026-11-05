//! What a vault job's runs need the host to remember between runs: the next run number, the last cron tick that fired,
//! and the highest run already examined for a run-failed task. All are written as runs happen, so they live beside the
//! job's claim on the host and never in the vault's `.contextos/jobs.toml`, which holds only what an operator decided.
//!
//! One short exclusive lock, `{job}.bookkeeping.lock`, guards the files and is held only for a read and a write, so a
//! process that is starting a run, a skipped tick, and a dry run never take the same number or move the tick backwards.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use contextos_workflows::cut_to_bytes;
use thiserror::Error;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::atomic_write::write_atomically;
use crate::run_claim::ClaimSlot;
use crate::trailing_path::Trailing;

const COUNTER_EXTENSION: &str = "next-run-id";
const LAST_FIRE_EXTENSION: &str = "last-fire";
const RUN_FAILURE_MARK_EXTENSION: &str = "run-failed-examined";
const LOCK_EXTENSION: &str = "bookkeeping.lock";

/// How much of an unreadable file an error repeats.
const MAX_ECHO_BYTES: usize = 32;

#[derive(Debug, Error)]
pub enum BookkeepingError {
    /// The message names the file by its job folder and name, never the host folder it is in.
    #[error(
        "the run counter at {path} holds {value:?}, which is not a number; delete it to start again",
        path = Trailing { path, count: 2 }
    )]
    Counter { path: PathBuf, value: String },
    /// The message names the file by its job folder and name, never the host folder it is in.
    #[error(
        "the last tick at {path} holds {value:?}, which is not a time; delete it to start again",
        path = Trailing { path, count: 2 }
    )]
    LastFire { path: PathBuf, value: String },
    /// The message names the file by its job folder and name, never the host folder it is in.
    #[error(
        "the run-failed mark at {path} holds {value:?}, which is not a number; delete it to start again",
        path = Trailing { path, count: 2 }
    )]
    RunFailureMark { path: PathBuf, value: String },
    #[error("the run bookkeeping could not be read or written: {0}")]
    Io(#[from] io::Error),
}

impl ClaimSlot {
    /// Takes the job's next run number. `highest_recorded` is the highest number among the job's run records, so a
    /// counter that was lost, or is behind the records, never gives a number a record already has.
    ///
    /// Blocking: waits for the lock, so call it from a blocking thread.
    ///
    /// # Errors
    ///
    /// Returns [`BookkeepingError::Counter`] when the counter is not a number, and [`BookkeepingError::Io`] when it
    /// cannot be locked, read, or written. No number is taken in either case.
    pub fn take_run_number(&self, highest_recorded: u64) -> Result<u64, BookkeepingError> {
        self.locked(|| {
            let path = self.sibling(COUNTER_EXTENSION);
            let counter = match read_text(&path)? {
                Some(text) => text.trim().parse::<u64>().map_err(|_| BookkeepingError::Counter {
                    path: path.clone(),
                    value: cut_to_bytes(&text, MAX_ECHO_BYTES),
                })?,
                None => 1,
            };
            let number = counter.max(highest_recorded.saturating_add(1));
            write_atomically(&path, number.saturating_add(1).to_string().as_bytes())?;
            Ok(number)
        })
    }

    /// The number the job's next run will take, without taking it: the counter as it stands, or one when no run has
    /// taken a number. It moves whenever a run, a skipped tick, or a dry run takes a number, so it tells a caller whether
    /// anything has happened since it last looked. No lock is taken, because the counter is replaced atomically.
    ///
    /// Blocking: call it from a blocking thread.
    ///
    /// # Errors
    ///
    /// Returns [`BookkeepingError::Counter`] when the counter is not a number, and [`BookkeepingError::Io`] when it
    /// cannot be read.
    pub fn peek_run_number(&self) -> Result<u64, BookkeepingError> {
        let path = self.sibling(COUNTER_EXTENSION);
        match read_text(&path)? {
            Some(text) => text.trim().parse::<u64>().map_err(|_| BookkeepingError::Counter {
                path: path.clone(),
                value: cut_to_bytes(&text, MAX_ECHO_BYTES),
            }),
            None => Ok(1),
        }
    }

    /// The last tick that fired for the job, if any has.
    ///
    /// Blocking: call it from a blocking thread.
    ///
    /// # Errors
    ///
    /// Returns [`BookkeepingError::LastFire`] when the file is not a time, and [`BookkeepingError::Io`] when it cannot
    /// be read.
    pub fn last_fire(&self) -> Result<Option<OffsetDateTime>, BookkeepingError> {
        let path = self.sibling(LAST_FIRE_EXTENSION);
        let Some(text) = read_text(&path)? else {
            return Ok(None);
        };
        OffsetDateTime::parse(text.trim(), &Rfc3339)
            .map(Some)
            .map_err(|_| BookkeepingError::LastFire {
                path,
                value: cut_to_bytes(&text, MAX_ECHO_BYTES),
            })
    }

    /// Records that the tick due at `tick` fired, and says whether that moved the last fire forward. A tick that is not
    /// later than the last fire is left out and reported `false`, so two processes never fire one tick twice.
    ///
    /// Blocking: waits for the lock, so call it from a blocking thread.
    ///
    /// # Errors
    ///
    /// Returns [`BookkeepingError::LastFire`] when the file is not a time, and [`BookkeepingError::Io`] when it cannot
    /// be locked, read, or written.
    pub fn record_fire(&self, tick: OffsetDateTime) -> Result<bool, BookkeepingError> {
        self.locked(|| {
            if self.last_fire()?.is_some_and(|fired| fired >= tick) {
                return Ok(false);
            }
            let text = tick.format(&Rfc3339).map_err(io::Error::other)?;
            write_atomically(&self.sibling(LAST_FIRE_EXTENSION), text.as_bytes())?;
            Ok(true)
        })
    }

    /// The highest run number already examined for a run-failed task, if any has been. A run at or below it has been
    /// decided, whether it raised a task, was folded into an open one, or was not a failure. No lock is taken, because
    /// the mark is replaced atomically.
    ///
    /// Blocking: call it from a blocking thread.
    ///
    /// # Errors
    ///
    /// Returns [`BookkeepingError::RunFailureMark`] when the file is not a number, and [`BookkeepingError::Io`] when it
    /// cannot be read.
    pub fn run_failure_mark(&self) -> Result<Option<u64>, BookkeepingError> {
        let path = self.sibling(RUN_FAILURE_MARK_EXTENSION);
        let Some(text) = read_text(&path)? else {
            return Ok(None);
        };
        text.trim()
            .parse::<u64>()
            .map(Some)
            .map_err(|_| BookkeepingError::RunFailureMark {
                path,
                value: cut_to_bytes(&text, MAX_ECHO_BYTES),
            })
    }

    /// Records that run `number` has been examined for a run-failed task, and says whether that moved the mark forward.
    /// A number that is not above the mark is left out and reported `false`, so the mark never moves back.
    ///
    /// Blocking: waits for the lock, so call it from a blocking thread.
    ///
    /// # Errors
    ///
    /// Returns [`BookkeepingError::RunFailureMark`] when the file is not a number, and [`BookkeepingError::Io`] when it
    /// cannot be locked, read, or written. A mark that cannot be read is left as it is.
    pub fn advance_run_failure_mark(&self, number: u64) -> Result<bool, BookkeepingError> {
        self.locked(|| {
            if self.run_failure_mark()?.is_some_and(|mark| mark >= number) {
                return Ok(false);
            }
            write_atomically(&self.sibling(RUN_FAILURE_MARK_EXTENSION), number.to_string().as_bytes())?;
            Ok(true)
        })
    }

    /// Runs `body` under the job's bookkeeping lock.
    fn locked<R>(&self, body: impl FnOnce() -> Result<R, BookkeepingError>) -> Result<R, BookkeepingError> {
        fs::create_dir_all(self.directory())?;
        let lock = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.sibling(LOCK_EXTENSION))?;
        lock.lock()?;
        let result = body();
        // Closing the file would release the lock too, so a failure to unlock changes nothing worth reporting.
        let _ = lock.unlock();
        result
    }
}

/// The text of `path`, or `None` when there is no such file.
fn read_text(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
#[path = "run_bookkeeping_test.rs"]
mod tests;
