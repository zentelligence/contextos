//! The one operation that changes a vault job's stored lifecycle state.
//!
//! Every route, command, and scheduler pass that moves a job between states calls [`change_lifecycle`], so the transition
//! table of `contextos-workflows` is enforced in one place. The state is read with its content hash and written back with
//! it, so a change another process made in between is rejected and never overwritten, and a refused transition writes
//! nothing.

use std::fmt;

use contextos_workflows::{Actor, Applied, Changed, JobScope, ReleaseContext, Transition, TransitionError};
use thiserror::Error;
use time::OffsetDateTime;

use crate::mcp_client::McpClient;
use crate::scheduler::{read_state, write_state};

/// Why a completed dry run of a saved job did not register it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotRegistered {
    DryRunNotCompleted,
    ScenarioCallsUnused,
    NotValidated,
    VersionChanged,
    InputsOverridden,
    StateUnavailable,
}

impl fmt::Display for NotRegistered {
    /// The reason as a phrase, such as `inputs overridden`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.word().replace('_', " "))
    }
}

impl From<&TransitionError> for NotRegistered {
    /// Why the lifecycle refused to register a job after its dry run completed.
    fn from(error: &TransitionError) -> Self {
        match error {
            TransitionError::WrongVersion { .. } | TransitionError::NotValidated { .. } => Self::VersionChanged,
            TransitionError::WrongActor { .. }
            | TransitionError::WrongState { .. }
            | TransitionError::NotOnProbation { .. }
            | TransitionError::KindRefused { .. }
            | TransitionError::StandingSuccessor { .. }
            | TransitionError::ReleaseRulesApply { .. }
            | TransitionError::PredecessorChanged { .. }
            | TransitionError::PredecessorUnconfirmed { .. }
            | TransitionError::FileDiffersFromRecord { .. }
            | TransitionError::PredecessorBusy { .. }
            | TransitionError::ScheduleRequired { .. }
            | TransitionError::IdentityUnrecorded { .. } => Self::NotValidated,
        }
    }
}

impl NotRegistered {
    #[must_use]
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::DryRunNotCompleted => "dry_run_not_completed",
            Self::ScenarioCallsUnused => "scenario_calls_unused",
            Self::NotValidated => "not_validated",
            Self::VersionChanged => "version_changed",
            Self::InputsOverridden => "inputs_overridden",
            Self::StateUnavailable => "state_unavailable",
        }
    }
}

/// One requested change to one job's state.
pub struct LifecycleChange<'a> {
    pub package: &'a str,
    pub workflow_slug: &'a str,
    pub job_slug: &'a str,
    /// Who is making the change, which decides whether the transition is theirs to make.
    pub actor: Actor,
    pub transition: Transition,
    pub now: OffsetDateTime,
}

#[derive(Debug, Error)]
pub enum LifecycleError {
    #[error("{code}: {source}", code = source.code())]
    Refused {
        #[from]
        source: TransitionError,
    },
    /// Another writer changed the state file since it was read, so nothing was written. Repeating the request reads the
    /// new state and applies the transition to it.
    #[error("workflow/state-changed: the job state changed while this change was being made; try again")]
    Conflict,
    /// The state file could not be read or written, or is malformed. Its own text says which.
    #[error("the job state could not be read or written: {0}")]
    Unavailable(String),
}

impl LifecycleError {
    /// The stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Refused { source } => source.code(),
            Self::Conflict => "workflow/state-changed",
            Self::Unavailable(_) => "workflow/state-unavailable",
        }
    }
}

/// Applies `change` to the job's entry in the vault's `.contextos/jobs.toml`.
///
/// # Errors
///
/// Returns [`LifecycleError::Refused`] when the actor does not own the transition or the job is not in a state it applies
/// to, with nothing written, [`LifecycleError::Conflict`] when another writer changed the file since it was read, and
/// [`LifecycleError::Unavailable`] when the file cannot be read, is malformed, or cannot be written.
pub async fn change_lifecycle(
    client: &McpClient,
    vault_name: &str,
    change: LifecycleChange<'_>,
) -> Result<Applied, LifecycleError> {
    change_lifecycle_in_release(client, vault_name, change, None).await
}

/// Applies `change` as [`change_lifecycle`] does, and with a `release` context applies an operator's enabling, resuming,
/// or re-activating by whether the job's release is current, previous, or future: the job, its predecessor, and the workflow's
/// `current` and `previous` are written
/// together in the one state write. A job whose state records a release is refused these changes without a context.
///
/// # Errors
///
/// As [`change_lifecycle`].
pub async fn change_lifecycle_in_release(
    client: &McpClient,
    vault_name: &str,
    change: LifecycleChange<'_>,
    release: Option<&ReleaseContext>,
) -> Result<Applied, LifecycleError> {
    change_lifecycle_reporting(client, vault_name, change, release)
        .await
        .map(|changed| changed.applied)
}

/// Applies `change` as [`change_lifecycle_in_release`] does, and says what an enabling did beyond the job: the
/// predecessor it retired and how `current` and `previous` moved.
///
/// # Errors
///
/// As [`change_lifecycle`].
pub async fn change_lifecycle_reporting(
    client: &McpClient,
    vault_name: &str,
    change: LifecycleChange<'_>,
    release: Option<&ReleaseContext>,
) -> Result<Changed, LifecycleError> {
    let mut snapshot = read_state(client, vault_name)
        .await
        .map_err(|error| LifecycleError::Unavailable(error.to_string()))?;
    let changed = snapshot.value.change_reporting(
        JobScope {
            package: change.package,
            workflow: change.workflow_slug,
            job: change.job_slug,
        },
        change.actor,
        &change.transition,
        release,
        change.now,
    )?;
    write_state(client, vault_name, &snapshot).await.map_err(|error| {
        if error.is_conflict() {
            LifecycleError::Conflict
        } else {
            LifecycleError::Unavailable(error.to_string())
        }
    })?;
    Ok(changed)
}
