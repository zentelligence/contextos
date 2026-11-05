//! What a run of a vault job checks and records about the code it reads when it starts.
//!
//! A run reads the workflow's files and every library it reaches into one snapshot. When the job is `enabled`, the
//! snapshot's version must be the one that was reviewed when the scheduler starts the run, so a library edited after the
//! review is never run on a schedule before it has been: the run does not start and the change is reviewed on the next
//! pass. A run by hand is not held back. What the run read is recorded and
//! logged by library and file, never by content.

use contextos_workflows::{JobFingerprint, RunFailure, WorkflowFailureKind};

use crate::jobs::RegisteredJob;
use crate::mcp_client::McpClient;
use crate::run_records::{RunLibrary, RunRelease, Trigger};
use crate::scheduler::{StateError, read_state};
use crate::vault_state::FavouriteError;

/// Whether a run's code must be the reviewed version before the run may start.
///
/// Only a scheduled run is held to it, because the review is what puts a new version on probation and so lets a failed
/// run suspend the job. A run by hand starts on the code as it stands, and a dry run never settles anything.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReviewGate {
    /// The snapshot's version must be the reviewed one.
    Required,
    /// The run starts on whatever it read.
    NotRequired,
}

impl From<Trigger> for ReviewGate {
    fn from(trigger: Trigger) -> Self {
        match trigger {
            Trigger::Scheduled => Self::Required,
            Trigger::Manual => Self::NotRequired,
        }
    }
}

/// The version of the release a run read, once the run may start.
///
/// With [`ReviewGate::Required`] this is [`reviewed_version`]. Otherwise the job state is not read at all and the version
/// is the one the release gave, or `None` when it could not be worked out, in which case the run settles nothing.
///
/// # Errors
///
/// As [`reviewed_version`], and never with [`ReviewGate::NotRequired`].
pub(crate) async fn version_for_run(
    client: &McpClient,
    vault_name: &str,
    job: &RegisteredJob,
    version: Option<JobFingerprint>,
    gate: ReviewGate,
) -> Result<Option<JobFingerprint>, RunFailure> {
    match gate {
        ReviewGate::Required => reviewed_version(client, vault_name, job, version).await,
        ReviewGate::NotRequired => Ok(version),
    }
}

/// What a run that is not started because its code was not reviewed says.
pub(crate) fn unreviewed_code() -> RunFailure {
    let kind = WorkflowFailureKind::JobNotRunnable;
    RunFailure {
        kind: Some(kind),
        message: format!(
            "{}: the job's code (its files, or a library it reaches) changed since it was reviewed, so this run did not \
             start; the change is reviewed on the next pass",
            kind.code()
        ),
    }
}

/// Whether a run of `job`, whose release gave `version`, may start, and the version of what it read.
///
/// The code is reviewed, unreviewed, or unknown, and only reviewed code starts. A job that is not `enabled` has no review
/// that puts it on probation, and a job with no version on record has nothing the release could differ from, so neither
/// is refused. A state that cannot be read or parsed leaves the code unknown: the run does not start and says why, as an
/// outage and never as a change to the code.
///
/// # Errors
///
/// Returns the failure the run ends with: `workflow/job-not-runnable` for unreviewed code, and
/// `workflow/state-unavailable` for a job state that cannot be read or parsed for any reason, including a dropped
/// connection.
pub(crate) async fn reviewed_version(
    client: &McpClient,
    vault_name: &str,
    job: &RegisteredJob,
    version: Option<JobFingerprint>,
) -> Result<Option<JobFingerprint>, RunFailure> {
    let state = read_state(client, vault_name)
        .await
        .map_err(|error| unknown_state(&error))?;
    let recorded = state
        .value
        .job(&job.package, &job.workflow_slug, &job.slug)
        .and_then(contextos_workflows::JobState::fingerprint)
        .is_some();
    if recorded
        && state
            .value
            .new_version_under_enabled_job(&job.package, &job.workflow_slug, &job.slug, version.as_ref())
    {
        return Err(unreviewed_code());
    }
    Ok(version)
}

/// What a run says when the state that records the reviewed version could not be used, whatever the reason: it was
/// refused, it could not be read, the connection dropped while it was read, or it could not be parsed.
fn unknown_state(error: &StateError) -> RunFailure {
    let kind = WorkflowFailureKind::StateUnavailable;
    let why = match error {
        StateError::Persistence(FavouriteError::Malformed) => {
            "the job state could not be parsed; correct .contextos/jobs.toml"
        }
        StateError::Mcp(_) | StateError::Persistence(FavouriteError::Unreachable) => {
            "the job state could not be read because the server could not be asked"
        }
        _ => "the job state could not be read",
    };
    RunFailure {
        kind: Some(kind),
        message: format!(
            "{}: {why}, so whether the job's code was reviewed is unknown and this run did not start",
            kind.code()
        ),
    }
}

/// What a run read of the job's code: the libraries it reached, and the version of the whole snapshot, which a live run
/// has worked out and a run that ended before reading has not.
#[derive(Default)]
pub(crate) struct CodeRead {
    pub(crate) release: Option<RunRelease>,
    pub(crate) libraries: Vec<RunLibrary>,
    pub(crate) version: Option<contextos_workflows::JobFingerprint>,
}

/// Logs each library a run uses at INFO: its name and version, never their content.
pub(crate) fn log_libraries(vault_name: &str, job: &RegisteredJob, run_id: u64, libraries: &[RunLibrary]) {
    for library in libraries {
        tracing::info!(
            vault = vault_name,
            job = %job.slug,
            run_id,
            library = %library.name,
            version = library.release.as_ref().map_or("source", |release| release.version.as_str()),
            "run uses a vault library"
        );
    }
}

#[cfg(test)]
#[path = "run_snapshot_test.rs"]
mod tests;
