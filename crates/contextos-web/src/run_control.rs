//! What a job page may ask of a job's runs: see whether one is active, start one, and ask an active one to stop.
//!
//! One trait, with a vault adapter and a shared adapter, so a vault page and a shared page differ only in how they build
//! one, as they do for [`crate::job_operation::JobStore`]. An adapter decides nothing a rule decides: the job's
//! state allows a run or not, a run of one job never overlaps another run of it, and a request to stop names the run it is
//! for, and each of those is enforced where the run is claimed.

use std::future::Future;

use thiserror::Error;

use crate::job_chain::RunHistory;
use crate::page_runs::{PageRunKind, PageRunsOfJob};
use crate::run_claim::{CancelError, ClaimSlot, ClaimState};

/// What a job's runs are doing now, as a page shows it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RunActivity {
    /// The job's claim, which shows a live run in any process.
    pub(crate) claim: ClaimState,
    /// What this process started from a page, which is how a dry run, which takes no claim, is seen.
    pub(crate) page: PageRunsOfJob,
}

impl Default for RunActivity {
    fn default() -> Self {
        Self {
            claim: ClaimState::Idle,
            page: PageRunsOfJob::default(),
        }
    }
}

/// A run a page started.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StartedRun {
    /// Its number, when it is known as the run starts. A shared run takes its number as it begins its record.
    pub(crate) run_id: Option<u64>,
    pub(crate) kind: PageRunKind,
}

impl StartedRun {
    /// Whether the run is a dry run.
    #[must_use]
    pub(crate) const fn is_dry(&self) -> bool {
        matches!(self.kind, PageRunKind::Dry)
    }
}

/// Why a page's request about a run was refused. Each message begins with its stable `workflow/` code when it has one.
#[derive(Debug, Error)]
pub(crate) enum RunRefusal {
    /// A run of the job is already active, so another cannot start.
    #[error("workflow/job-busy: a run of this job is active, so another cannot start until it ends")]
    Busy,
    /// The job's state does not allow the run. The text says which state, and what would allow it.
    #[error("{0}")]
    NotRunnable(String),
    /// No run of the job by this number is active, so there is nothing to stop.
    #[error("run R-{run_id} is not active, so there is nothing to stop")]
    NotActive { run_id: u64 },
    /// The run could not start, for a reason the text gives.
    #[error("{0}")]
    CouldNotStart(String),
    /// The host's claim, run number, or job state could not be used.
    #[error("the host could not be reached to {0}; see the log for the cause")]
    Unavailable(&'static str),
}

/// What a job's runs can be asked to do.
pub(crate) trait RunControl {
    /// What the job's runs are doing now.
    fn activity(&self) -> impl Future<Output = Result<RunActivity, RunRefusal>> + Send;

    /// The job's most recent runs, newest first, and those of the jobs it succeeded and was succeeded by.
    fn history(&self) -> impl Future<Output = Result<RunHistory, RunRefusal>> + Send;

    /// Starts a run of `kind` on a task of its own, and answers at once with the number it has, or why it could not start.
    /// A live run takes the job's claim. A dry run takes none.
    fn start(&self, kind: PageRunKind) -> impl Future<Output = Result<StartedRun, RunRefusal>> + Send;

    /// Asks the active run `run_id` to stop. It stops at its next step.
    fn cancel(&self, run_id: u64) -> impl Future<Output = Result<(), RunRefusal>> + Send;

    /// The name of the zone the job's schedule runs on the wall clock of: the operator's, as the host's clock
    /// reports it (its IANA name, else its POSIX rules).
    fn schedule_zone(&self) -> String;

    /// The address of the record of run `number`, when records have one a page can link to. A run whose record is on the
    /// host has none.
    fn record_link(&self, _number: u64) -> impl Future<Output = Option<String>> + Send {
        std::future::ready(None)
    }
}

/// A zone's name for a person reading it: its IANA name, else its POSIX rules.
pub(crate) fn zone_label(zone: &contextos_core::OperatorTimeZone) -> String {
    zone.name().or(zone.rules()).unwrap_or("this host's zone").to_owned()
}

/// What the job's claim shows, with what this process started from pages, without blocking the executor thread.
pub(crate) async fn observe(slot: &ClaimSlot, page: PageRunsOfJob) -> Result<RunActivity, RunRefusal> {
    let slot = slot.clone();
    match tokio::task::spawn_blocking(move || slot.state()).await {
        Ok(Ok(claim)) => Ok(RunActivity { claim, page }),
        Ok(Err(error)) => {
            tracing::warn!(%error, "a job's claim could not be examined");
            Err(RunRefusal::Unavailable("examine the job's runs"))
        }
        Err(error) => {
            tracing::warn!(%error, "examining a job's claim did not finish");
            Err(RunRefusal::Unavailable("examine the job's runs"))
        }
    }
}

/// Writes the request to stop run `run_id`, without blocking the executor thread. It is refused unless that run holds the
/// claim.
pub(crate) async fn ask_to_stop(slot: &ClaimSlot, run_id: u64) -> Result<(), RunRefusal> {
    let slot = slot.clone();
    match tokio::task::spawn_blocking(move || slot.request_cancel(run_id)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(CancelError::NotActive { run_id })) => Err(RunRefusal::NotActive { run_id }),
        Ok(Err(error)) => {
            tracing::warn!(%error, "a request to stop a run could not be written");
            Err(RunRefusal::Unavailable("ask the run to stop"))
        }
        Err(error) => {
            tracing::warn!(%error, "writing a request to stop a run did not finish");
            Err(RunRefusal::Unavailable("ask the run to stop"))
        }
    }
}
