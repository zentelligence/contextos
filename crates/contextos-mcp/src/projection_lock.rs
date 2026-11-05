//! The host-level projection lock: one advisory lock in the state root that every `contextos` process on the host
//! takes around each change to an agent host's folders.
//!
//! Each process projects into the same few folders under the operator's home (`~/.claude`, `~/.codex`, `~/.agents`),
//! whichever vaults it serves, so the vault write locks cannot keep them apart. This lock does, with the same
//! first-come queue and bounded wait as the vault locks ([`contextos_fs::lock_host`]).
//!
//! Lock order: a vault's write lock first, then this one, never the reverse. A tool write already holds its vault
//! locks when the router calls projection, and projection never takes a vault lock, so the two cannot deadlock. A
//! change that waits here holds no other lock a projecting process needs.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use contextos_core::{OperationEvent, OperationWarning, ProjectsDefinitions};

use contextos_fs::{DEFAULT_HOST_WAIT, lock_host};

/// How long a write-time projection waits for another process before reporting `projection/busy`. A tool write holds
/// its vault locks while it waits, so the bound is short; start-up refresh, doctor, and `doctor_resolve` hold nothing a
/// writer needs and wait as long as a vault write does.
pub(crate) const EVENT_WAIT: Duration = Duration::from_secs(5);

/// The name of the projection lock file in the state root.
pub(crate) const PROJECTION_LOCK: &str = "projection.lock";

/// What a [`LockedProjection`] wraps and where its lock lives.
pub(crate) struct LockedProjectionConfig<P> {
    /// The projection to run under the lock.
    pub(crate) inner: P,
    /// The lock file.
    pub(crate) lock: PathBuf,
    /// How long start-up refresh, doctor, and `doctor_resolve` wait for another process before reporting
    /// `projection/busy`. A write-time projection waits at most [`EVENT_WAIT`], or less when this is shorter.
    pub(crate) wait: Duration,
}

impl<P> LockedProjectionConfig<P> {
    /// The lock at its usual place beneath `state_root`, with the usual waits.
    pub(crate) fn beneath(inner: P, state_root: &std::path::Path) -> Self {
        Self {
            inner,
            lock: state_root.join(PROJECTION_LOCK),
            wait: DEFAULT_HOST_WAIT,
        }
    }
}

/// Runs every projection of `P` while holding the host-level projection lock.
///
/// A wait beyond the bound changes nothing and yields one `projection/busy` warning.
#[derive(Debug)]
pub(crate) struct LockedProjection<P> {
    inner: P,
    lock: PathBuf,
    /// The maintenance bound in milliseconds. Atomic so a test can shorten it on a projection every clone shares.
    wait_millis: AtomicU64,
}

impl<P> From<LockedProjectionConfig<P>> for LockedProjection<P> {
    fn from(value: LockedProjectionConfig<P>) -> Self {
        Self {
            inner: value.inner,
            lock: value.lock,
            wait_millis: AtomicU64::new(u64::try_from(value.wait.as_millis()).unwrap_or(u64::MAX)),
        }
    }
}

/// Why the projection lock could not be taken.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum ProjectionLockError {
    /// Another contextos process held the lock beyond the bound.
    #[error("another contextos process held the agent host projection lock for more than {seconds} seconds")]
    Busy {
        /// How long this process waited.
        seconds: u64,
    },
    /// The lock file could not be opened or locked.
    #[error("the agent host projection lock could not be opened")]
    Failed,
}

impl From<&contextos_fs::FsError> for ProjectionLockError {
    fn from(value: &contextos_fs::FsError) -> Self {
        match value {
            contextos_fs::FsError::HostLockBusy { seconds, .. } => Self::Busy { seconds: *seconds },
            _ => Self::Failed,
        }
    }
}

/// The warning that stands for a lock that could not be taken. Names no path, so nothing under the home directory
/// reaches a tool result or a log.
impl From<ProjectionLockError> for OperationWarning {
    fn from(value: ProjectionLockError) -> Self {
        match value {
            ProjectionLockError::Busy { seconds } => Self {
                code: "projection/busy".to_owned(),
                message: format!(
                    "Another contextos process held the agent host projection lock for more than {seconds} seconds, so \
                     nothing was projected. The vault write is unaffected. Retry the write, or restart this server to \
                     refresh every projection; if it persists, another contextos process is stuck mid-projection: stop it."
                ),
            },
            ProjectionLockError::Failed => Self {
                code: "projection/lock-failed".to_owned(),
                message: "The agent host projection lock could not be opened, so nothing was projected. The vault write is \
                          unaffected. Check that the server's state folder under the home directory is on a local disk it can \
                          write, then retry."
                    .to_owned(),
            },
        }
    }
}

impl<P> LockedProjection<P> {
    /// How long a write-time projection waits for another process before reporting `projection/busy`.
    pub(crate) fn event_wait(&self) -> Duration {
        self.maintenance_wait().min(EVENT_WAIT)
    }

    /// How long start-up refresh, doctor, and `doctor_resolve` wait for another process.
    pub(crate) fn maintenance_wait(&self) -> Duration {
        Duration::from_millis(self.wait_millis.load(Ordering::Relaxed))
    }

    /// The wrapped projection, for a test seam that configures it without taking the lock.
    pub(crate) const fn inner(&self) -> &P {
        &self.inner
    }

    /// Changes how long a maintenance pass waits for another process, for every holder of this projection. A
    /// write-time projection waits no longer than that either.
    pub(crate) fn set_wait(&self, wait: Duration) {
        self.wait_millis
            .store(u64::try_from(wait.as_millis()).unwrap_or(u64::MAX), Ordering::Relaxed);
    }

    /// Runs `body` on the wrapped projection while holding the lock.
    ///
    /// # Errors
    ///
    /// Returns the [`ProjectionLockError`] when the lock could not be taken, without running `body`.
    pub(crate) fn run_locked<R>(&self, body: impl FnOnce(&P) -> R) -> Result<R, ProjectionLockError> {
        self.run_locked_within(self.maintenance_wait(), body)
    }

    fn run_locked_within<R>(&self, wait: Duration, body: impl FnOnce(&P) -> R) -> Result<R, ProjectionLockError> {
        let _held = lock_host(&self.lock, wait).map_err(|error| ProjectionLockError::from(&error))?;
        Ok(body(&self.inner))
    }

    /// As [`Self::run_locked`], reporting a lock that could not be taken as the single warning to show.
    ///
    /// # Errors
    ///
    /// Returns the single warning to report, `projection/busy` when another process held the lock beyond the bound,
    /// without running `body`.
    pub(crate) fn with_lock<R>(&self, body: impl FnOnce(&P) -> R) -> Result<R, OperationWarning> {
        self.run_locked(body).map_err(OperationWarning::from)
    }
}

impl<P: ProjectsDefinitions> ProjectsDefinitions for LockedProjection<P> {
    fn project(&self, event: &OperationEvent) -> Vec<OperationWarning> {
        self.run_locked_within(self.event_wait(), |inner| inner.project(event))
            .map_err(OperationWarning::from)
            .unwrap_or_else(|warning| vec![warning])
    }
}

#[cfg(test)]
#[path = "projection_lock_test.rs"]
mod tests;
