//! The mechanism behind a job's run claim, for a vault job and a shared job alike.
//!
//! A job's claim is an exclusive advisory lock on `{dir}/{job}.lock`, held for the whole run. The operating system
//! releases it when its holder dies, so a run that crashed no longer holds the claim and the next run takes it. The lock
//! file is never deleted, because removing a file another process may be about to lock is a race, and its existence
//! means nothing.
//!
//! Two small files sit beside the lock and say nothing a lock cannot be trusted over:
//!
//! - `{job}.run` is written by the holder once it knows its run number, so a page or another process can say which run is
//!   active. It counts only while the lock is held, so one a dead holder left behind is never reported.
//! - `{job}.cancel` is written by whoever asks for that run to stop, naming the run number, so a request meant for one
//!   run can never stop another. The holder polls it and cancels its own run.
//!
//! Locking is advisory and the directory must be on a local disk.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;

use crate::atomic_write::write_atomically;
use crate::bounded_read::read_text_within;

/// How many times a refused claim is tried, and how long to wait between tries.
const TAKE_ATTEMPTS: u32 = 10;
const TAKE_RETRY_DELAY: Duration = Duration::from_millis(20);

/// The largest descriptor or request read back. Each holds a run number and a time.
const MAX_DESCRIPTOR_BYTES: u64 = 4 * 1024;

/// Where one job's claim files live: a directory and the job's name within it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimSlot {
    directory: PathBuf,
    job: String,
}

/// A run that holds a job's claim.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ActiveRun {
    pub run_id: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub started: OffsetDateTime,
}

/// What a job's claim shows right now.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClaimState {
    /// Nobody holds the claim.
    Idle,
    /// A run holds the claim. Its number is `None` in the moment between taking the claim and publishing it.
    Held(Option<ActiveRun>),
}

#[derive(Debug, Error)]
pub enum RunClaimError {
    // The claim file is held in the host's data folder, so it is carried for diagnosis and never printed.
    #[error("the job {job:?} is already running")]
    Busy { job: String, path: PathBuf },
    #[error("{name:?} cannot name a claim: it must be a single plain path segment")]
    InvalidName { name: String },
    #[error("the claim could not be taken: {0}")]
    Io(#[from] io::Error),
}

#[derive(Debug, Error)]
pub enum CancelError {
    #[error("run {run_id} of the job is not active, so there is nothing to cancel")]
    NotActive { run_id: u64 },
    #[error("the cancel request could not be written: {0}")]
    Io(#[from] io::Error),
}

/// Polls for a cancel request while a run is active, and cancels the run's token when one names it. Dropping it stops the
/// polling, so it never outlives its run.
#[derive(Debug)]
pub struct CancelWatch {
    task: tokio::task::JoinHandle<()>,
}

impl Drop for CancelWatch {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A held claim, released when dropped, so success, failure, timeout, cancellation, and a panic all release it.
#[derive(Debug)]
pub struct RunClaim {
    file: Option<File>,
    slot: ClaimSlot,
}

impl ClaimSlot {
    /// The slot for `job` in `directory`.
    ///
    /// # Errors
    ///
    /// Returns [`RunClaimError::InvalidName`] when `job` could leave the directory.
    pub fn new(directory: PathBuf, job: &str) -> Result<Self, RunClaimError> {
        // A dot is refused too, so no job's claim files can be named like another job's bookkeeping files.
        if !is_plain_segment(job) || job.contains('.') {
            return Err(RunClaimError::InvalidName { name: job.to_owned() });
        }
        Ok(Self {
            directory,
            job: job.to_owned(),
        })
    }

    /// A file beside the claim, named for the job and ending `extension`.
    pub(crate) fn sibling(&self, extension: &str) -> PathBuf {
        self.directory.join(format!("{}.{extension}", self.job))
    }

    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    #[must_use]
    pub fn lock_path(&self) -> PathBuf {
        self.directory.join(format!("{}.lock", self.job))
    }

    #[must_use]
    pub fn run_path(&self) -> PathBuf {
        self.directory.join(format!("{}.run", self.job))
    }

    #[must_use]
    pub fn cancel_path(&self) -> PathBuf {
        self.directory.join(format!("{}.cancel", self.job))
    }

    /// What the claim shows.
    ///
    /// Blocking: call it from a blocking thread.
    ///
    /// # Errors
    ///
    /// Returns the filesystem error when the claim cannot be examined.
    pub fn state(&self) -> io::Result<ClaimState> {
        let lock = match File::open(self.lock_path()) {
            Ok(lock) => lock,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(ClaimState::Idle),
            Err(error) => return Err(error),
        };
        // A shared lock, so two observers never mistake each other for a run. It is released at once, and a run that
        // tries for the claim in that moment retries (`TAKE_ATTEMPTS`).
        if contextos_fs::try_lock_shared(&lock)? {
            // Closing the file would release the lock too, so a failure to unlock changes nothing worth reporting.
            let _ = lock.unlock();
            return Ok(ClaimState::Idle);
        }
        Ok(ClaimState::Held(Self::read_descriptor::<ActiveRun>(&self.run_path())))
    }

    /// Whether a run holds the claim now, examined on a blocking thread.
    ///
    /// # Errors
    ///
    /// Returns the filesystem error when the claim cannot be examined, or the error of the blocking task that ended.
    pub async fn is_held(&self) -> io::Result<bool> {
        let slot = self.clone();
        let state = tokio::task::spawn_blocking(move || slot.state())
            .await
            .map_err(io::Error::other)??;
        Ok(matches!(state, ClaimState::Held(_)))
    }

    /// Says which run holds the claim. Only the holder calls it.
    fn write_descriptor(&self, run: &ActiveRun) -> io::Result<()> {
        // A request an earlier run left behind is for that run, so it goes before this one is announced.
        remove_if_present(&self.cancel_path())?;
        write_atomically(&self.run_path(), &serde_json::to_vec(run).map_err(io::Error::other)?)
    }

    /// Whether a cancel request names `run_id`.
    #[must_use]
    pub fn cancel_requested(&self, run_id: u64) -> bool {
        Self::read_descriptor::<CancelRequest>(&self.cancel_path()).is_some_and(|request| request.run_id == run_id)
    }

    fn read_descriptor<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
        read_text_within(path, MAX_DESCRIPTOR_BYTES)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
    }

    /// Asks the active run `run_id` to stop.
    ///
    /// Blocking: call it from a blocking thread.
    ///
    /// # Errors
    ///
    /// Returns [`CancelError::NotActive`] when no run holds the claim or the one that does is not `run_id`.
    pub fn request_cancel(&self, run_id: u64) -> Result<(), CancelError> {
        match self.state()? {
            ClaimState::Held(Some(active)) if active.run_id == run_id => {}
            ClaimState::Held(_) | ClaimState::Idle => return Err(CancelError::NotActive { run_id }),
        }
        let request = serde_json::to_vec(&CancelRequest { run_id }).map_err(io::Error::other)?;
        write_atomically(&self.cancel_path(), &request)?;
        Ok(())
    }
}

/// Starts watching `slot` for a request to cancel run `run_id`, looking every `every`.
#[must_use]
pub fn watch_for_cancel(
    slot: ClaimSlot,
    run_id: u64,
    token: tokio_util::sync::CancellationToken,
    every: Duration,
) -> CancelWatch {
    let task = tokio::spawn(async move {
        loop {
            tokio::time::sleep(every).await;
            if token.is_cancelled() {
                return;
            }
            let looked = slot.clone();
            let asked = tokio::task::spawn_blocking(move || looked.cancel_requested(run_id))
                .await
                .unwrap_or(false);
            if asked {
                token.cancel();
                return;
            }
        }
    });
    CancelWatch { task }
}

impl RunClaim {
    /// Takes the claim for `slot`. A lock file left by a run that has ended, or that died, holds nothing and is taken.
    ///
    /// # Errors
    ///
    /// Returns [`RunClaimError::Busy`] when a live run holds the claim, and [`RunClaimError::Io`] for any other failure.
    pub fn acquire(slot: &ClaimSlot) -> Result<Self, RunClaimError> {
        fs::create_dir_all(&slot.directory)?;
        let path = slot.lock_path();
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        // A look at the claim (`ClaimSlot::state`) holds it for an instant, so a refusal is tried again a few times
        // before it is believed. A live run holds it for far longer.
        for attempt in 1..=TAKE_ATTEMPTS {
            if contextos_fs::try_lock_exclusive(&file)? {
                // What a run that died left is nobody's, and must not be taken for this run's until it says which it is.
                slot.clear_run();
                return Ok(Self {
                    file: Some(file),
                    slot: slot.clone(),
                });
            }
            if attempt < TAKE_ATTEMPTS {
                std::thread::sleep(TAKE_RETRY_DELAY);
            }
        }
        Err(RunClaimError::Busy {
            job: slot.job.clone(),
            path,
        })
    }

    #[must_use]
    pub const fn slot(&self) -> &ClaimSlot {
        &self.slot
    }

    /// Says which run holds the claim, so it can be seen and asked to stop.
    ///
    /// # Errors
    ///
    /// Returns the filesystem error when the descriptor cannot be written.
    pub fn publish(&self, run: &ActiveRun) -> io::Result<()> {
        self.slot.write_descriptor(run)
    }

    /// [`Self::publish`] without blocking the caller's executor thread.
    ///
    /// # Errors
    ///
    /// Returns the filesystem error when the descriptor cannot be written.
    pub async fn announce(&self, run: ActiveRun) -> io::Result<()> {
        let slot = self.slot.clone();
        tokio::task::spawn_blocking(move || slot.write_descriptor(&run))
            .await
            .map_err(io::Error::other)?
    }

    /// Whether a cancel request names `run_id`.
    #[must_use]
    pub fn cancel_requested(&self, run_id: u64) -> bool {
        self.slot.cancel_requested(run_id)
    }

    /// Releases the claim without blocking the caller's executor thread. Dropping a claim releases it too.
    ///
    /// # Errors
    ///
    /// Returns the filesystem error when the lock could not be released. Closing the file releases it regardless.
    pub async fn release(mut self) -> io::Result<()> {
        let Some(file) = self.file.take() else {
            return Ok(());
        };
        let slot = self.slot.clone();
        tokio::task::spawn_blocking(move || {
            slot.clear_run();
            file.unlock()
        })
        .await
        .map_err(io::Error::other)?
    }
}

impl Drop for RunClaim {
    fn drop(&mut self) {
        // The descriptor and request go before the lock does, so a run that takes the claim next is never overwritten.
        // The lock is released when the file closes.
        if self.file.is_some() {
            self.slot.clear_run();
        }
    }
}

impl ClaimSlot {
    /// Removes what a run published and what asked it to stop. A failure leaves a file that is reported only while the
    /// lock is held, and the next run clears.
    fn clear_run(&self) {
        for path in [self.run_path(), self.cancel_path()] {
            if let Err(error) = remove_if_present(&path) {
                tracing::warn!(%error, "a run's claim files could not be removed");
            }
        }
    }
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// What a cancel request holds: the run it is for.
#[derive(Deserialize, Serialize)]
struct CancelRequest {
    run_id: u64,
}

/// A name that is one ordinary path segment: not empty, not `.` or `..`, and holding no separator. A colon is refused
/// too, because on Windows it makes the rest of the name an alternate data stream.
pub(crate) fn is_plain_segment(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0', ':'])
}

#[cfg(test)]
#[path = "run_claim_test.rs"]
mod tests;
