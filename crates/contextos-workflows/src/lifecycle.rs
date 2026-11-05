//! A job's lifecycle: the six states, who may move a job between them, and what each state allows. Pure: no I/O and no
//! clock, so every transition can be driven deterministically.
//!
//! Anything that changes a job's state goes through [`apply`], with the actor making the change, so a route, a command,
//! or the scheduler cannot make a transition the table does not allow, or one the actor does not own.

use std::fmt;

use contextos_core::OwnerStanding;
use semver::Version;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::fingerprint::JobFingerprint;
use crate::logging::{CUT_MARKER, cut_to_bytes};
use crate::release_state::{ReleaseKind, ReplacedBy};
use crate::run_record::MAX_ERROR_BYTES;
use crate::schedule::{CronSchedule, CronScheduleError};
use crate::script::RunFailure;
use crate::state::{StoredValidation, SuspensionRecord};
use crate::validation::ValidationReport;

/// Where a job is in its lifecycle. A job nothing has been recorded for is [`JobLifecycle::Draft`].
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobLifecycle {
    /// Incomplete, or not yet validated for its current version.
    #[default]
    Draft,
    /// Static validation passed for its current version.
    Validated,
    /// Validated, and a dry run completed for its current version.
    Registered,
    /// Registered, with a schedule the operator set. The only state that runs on a schedule.
    Enabled,
    /// Stopped by the operator, or automatically with a recorded reason.
    Suspended,
    /// Taken out of use and kept, so it can be deleted or re-activated.
    Retired,
}

impl JobLifecycle {
    /// The word a state is stored and shown as.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Validated => "validated",
            Self::Registered => "registered",
            Self::Enabled => "enabled",
            Self::Suspended => "suspended",
            Self::Retired => "retired",
        }
    }

    /// Whether the scheduler may start a job in this state when its schedule is due.
    #[must_use]
    pub const fn runs_on_schedule(self) -> bool {
        matches!(self, Self::Enabled)
    }

    /// Whether an operator may run a job in this state by hand.
    #[must_use]
    pub const fn runs_by_hand(self) -> bool {
        matches!(self, Self::Registered | Self::Enabled | Self::Suspended)
    }
}

impl fmt::Display for JobLifecycle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl TryFrom<&str> for JobLifecycle {
    type Error = UnknownLifecycle;

    fn try_from(word: &str) -> Result<Self, UnknownLifecycle> {
        [
            Self::Draft,
            Self::Validated,
            Self::Registered,
            Self::Enabled,
            Self::Suspended,
            Self::Retired,
        ]
        .into_iter()
        .find(|state| state.as_str() == word)
        .ok_or_else(|| UnknownLifecycle(word.to_owned()))
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
#[error("{0:?} is not a job state")]
pub struct UnknownLifecycle(String);

/// Who is making a change. Each transition has the actors that own it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Actor {
    Operator,
    Validator,
    DryRun,
    System,
}

impl Actor {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Operator => "operator",
            Self::Validator => "validator",
            Self::DryRun => "dry run",
            Self::System => "system",
        }
    }
}

impl fmt::Display for Actor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A cron expression the scheduler can evaluate. The sole way a schedule enters the lifecycle, so a stored schedule is
/// always one that parsed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobSchedule(String);

impl JobSchedule {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for JobSchedule {
    type Error = CronScheduleError;

    fn try_from(expression: &str) -> Result<Self, CronScheduleError> {
        CronSchedule::parse(expression)?;
        Ok(Self(expression.to_owned()))
    }
}

/// Why a job was suspended. Never empty, so a suspended job always says why.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SuspensionReason(String);

impl SuspensionReason {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for SuspensionReason {
    type Error = EmptySuspensionReason;

    fn try_from(reason: &str) -> Result<Self, EmptySuspensionReason> {
        let reason = reason.trim();
        if reason.is_empty() {
            Err(EmptySuspensionReason)
        } else {
            Ok(Self(reason.to_owned()))
        }
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
#[error("a suspension needs a reason")]
pub struct EmptySuspensionReason;

impl From<&ValidationReport> for SuspensionReason {
    /// Why a job was suspended because its new version failed validation: every failure, with its code and the file it
    /// concerns, cut to the length a recorded error may have.
    fn from(report: &ValidationReport) -> Self {
        let found = report
            .failures
            .iter()
            .map(|failure| format!("{} ({}): {}", failure.code.as_str(), failure.artefact, failure.message))
            .collect::<Vec<_>>()
            .join("; ");
        let text = if found.is_empty() {
            String::from("validation of the new version failed")
        } else {
            format!("validation of the new version failed: {found}")
        };
        Self(cut_to_bytes(&text, MAX_ERROR_BYTES))
    }
}

impl From<&StoredValidation> for SuspensionReason {
    /// Why a job was suspended because validation of the version it holds failed: every stored failure, with its code
    /// and the file it concerns, cut to the length a recorded error may have.
    fn from(validation: &StoredValidation) -> Self {
        let found = validation
            .failures
            .iter()
            .map(|failure| format!("{} ({}): {}", failure.code, failure.artefact, failure.message))
            .collect::<Vec<_>>()
            .join("; ");
        let text = if found.is_empty() {
            String::from("validation failed")
        } else {
            format!("validation failed: {found}")
        };
        Self(cut_to_bytes(&text, MAX_ERROR_BYTES))
    }
}

/// The owner is a member of the team, so there is nothing to suspend the job for.
#[derive(Debug, Error, Eq, PartialEq)]
#[error("the job's owner is a member of the team")]
pub struct OwnerIsMember;

impl TryFrom<&OwnerStanding<'_>> for SuspensionReason {
    type Error = OwnerIsMember;

    /// Why a job was suspended because its owner is not a member: what the job file declares, never a member, cut to
    /// the length a recorded error may have.
    fn try_from(standing: &OwnerStanding<'_>) -> Result<Self, OwnerIsMember> {
        let text = match standing {
            OwnerStanding::Member(_) => return Err(OwnerIsMember),
            OwnerStanding::NotAMember(slug) => {
                format!(
                    "the job's owner {:?} is not a member of .contextos/team.toml",
                    slug.as_str()
                )
            }
            OwnerStanding::InvalidSlug(text) => {
                format!("the job's owner {text:?} is not a member of .contextos/team.toml")
            }
            OwnerStanding::Undeclared => "the job file declares no owner".to_owned(),
        };
        // The cut marker counts, so the whole reason stays within the length a recorded error may have.
        Ok(Self(cut_to_bytes(&text, MAX_ERROR_BYTES - CUT_MARKER.len())))
    }
}

impl From<&RunFailure> for SuspensionReason {
    /// Why a job was suspended because the first live run of its new version failed.
    fn from(failure: &RunFailure) -> Self {
        Self(cut_to_bytes(
            &format!("the first live run of this version failed: {failure}"),
            MAX_ERROR_BYTES,
        ))
    }
}

/// Why a job was suspended, as a structured cause with its own stable code. The code is never parsed back out of text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SuspensionCause {
    /// The operator stopped the job.
    Operator,
    /// The job's new version failed validation, or the rules tightened and an enabled job no longer passes them.
    ValidationFailed,
    /// The first live run of the version on trial failed.
    ProbationFailed {
        /// The run that failed, when its record could be started and so had a number.
        run: Option<u64>,
        /// The stable code the run failed with, when it had one.
        failure: Option<String>,
    },
    /// The job's owner is not a member of the team.
    OwnerUnresolved,
    /// A shared job's package is no longer available.
    PackageUnavailable,
}

impl SuspensionCause {
    /// The stable machine-readable code a suspension of this cause is stored and shown with.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Operator => "workflow/operator-suspended",
            Self::ValidationFailed => "workflow/validation-failed",
            Self::ProbationFailed { .. } => "workflow/probation-failed",
            Self::OwnerUnresolved => "workflow/owner-unresolved",
            Self::PackageUnavailable => "workflow/package-unavailable",
        }
    }

    /// Whether the operator, rather than the system, owns suspending for this cause.
    #[must_use]
    pub const fn is_operators(&self) -> bool {
        matches!(self, Self::Operator)
    }
}

/// A cause and the words that explain it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Suspension {
    pub cause: SuspensionCause,
    pub reason: SuspensionReason,
}

impl Suspension {
    /// The operator stopping a job, and saying why.
    #[must_use]
    pub const fn operator(reason: SuspensionReason) -> Self {
        Self {
            cause: SuspensionCause::Operator,
            reason,
        }
    }
}

/// A change to a job's state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Transition {
    /// Static validation passed for this version of the job.
    Validated(JobFingerprint),
    /// A dry run completed for this version of the job.
    DryRunCompleted(JobFingerprint),
    /// The operator set a schedule on a registered job, which is what makes it enabled, for the version the job has now.
    /// The version must be the one that was validated and dry run, so a job whose files changed since is not enabled.
    Enable(JobSchedule, JobFingerprint),
    /// The operator replaced the schedule of an enabled or suspended job, which does not change its state.
    SetSchedule(JobSchedule),
    /// The operator cleared the schedule of an enabled job.
    ClearSchedule,
    /// The operator, or the system, stopped an enabled job, for a structured cause. The operator owns the `Operator`
    /// cause and the system owns every other.
    Suspend(Suspension),
    /// The operator resumed a suspended job, for the version the job has now. Validation must have passed for that
    /// version.
    Resume(JobFingerprint),
    /// The operator took a job out of use.
    Retire,
    /// The system retired a job because its successor was enabled, in the same state write as that enabling. The
    /// record names the successor.
    SuccessorEnabled(ReplacedBy),
    /// The operator brought a retired job back as a draft.
    Reactivate,
    /// The operator deleted a retired job.
    Delete,
    /// The job's version fingerprint changed under a job that is not running on a schedule.
    VersionChanged,
    /// Validation of the version a `validated` or `registered` job holds failed for a reason the job owns, not for a
    /// check that could not be made, so what was validated and dry-run no longer stands.
    ValidationFailed,
    /// An enabled job's new version passed validation, so it stays enabled with its first live run on trial. The dry
    /// run is not repeated.
    OnProbation(JobFingerprint),
    /// The first live run of the version on trial succeeded or degraded, which ends the trial. A suspended job's trial
    /// ends the same way, because a manual run confirms that it works.
    ProbationPassed(JobFingerprint),
    /// The first live run of the version on trial failed, which suspends the job and leaves the trial pending. The
    /// suspension names the run.
    ProbationFailed(JobFingerprint, Suspension),
}

impl Transition {
    /// A short stable name, used in messages.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Validated(_) => "Validated",
            Self::DryRunCompleted(_) => "DryRunCompleted",
            Self::Enable(..) => "Enable",
            Self::SetSchedule(_) => "SetSchedule",
            Self::ClearSchedule => "ClearSchedule",
            Self::Suspend(_) => "Suspend",
            Self::Resume(_) => "Resume",
            Self::Retire => "Retire",
            Self::SuccessorEnabled(_) => "SuccessorEnabled",
            Self::Reactivate => "Reactivate",
            Self::Delete => "Delete",
            Self::VersionChanged => "VersionChanged",
            Self::ValidationFailed => "ValidationFailed",
            Self::OnProbation(_) => "OnProbation",
            Self::ProbationPassed(_) => "ProbationPassed",
            Self::ProbationFailed(..) => "ProbationFailed",
        }
    }

    const fn owners(&self) -> &'static [Actor] {
        match self {
            Self::Suspend(suspension) => {
                if suspension.cause.is_operators() {
                    &[Actor::Operator]
                } else {
                    &[Actor::System]
                }
            }
            Self::Validated(_) | Self::OnProbation(_) => &[Actor::Validator],
            Self::DryRunCompleted(_) => &[Actor::DryRun],
            Self::VersionChanged
            | Self::ValidationFailed
            | Self::ProbationPassed(_)
            | Self::ProbationFailed(..)
            | Self::SuccessorEnabled(_) => &[Actor::System],
            Self::Enable(..)
            | Self::SetSchedule(_)
            | Self::ClearSchedule
            | Self::Resume(_)
            | Self::Retire
            | Self::Reactivate
            | Self::Delete => &[Actor::Operator],
        }
    }

    const fn allowed_from(&self) -> &'static [JobLifecycle] {
        match self {
            Self::Validated(_) => &[JobLifecycle::Draft],
            Self::DryRunCompleted(_) => &[JobLifecycle::Validated],
            Self::Enable(..) => &[JobLifecycle::Registered],
            Self::ClearSchedule | Self::Suspend(_) | Self::OnProbation(_) | Self::ProbationFailed(..) => {
                &[JobLifecycle::Enabled]
            }
            Self::Resume(_) => &[JobLifecycle::Suspended],
            Self::SetSchedule(_) | Self::ProbationPassed(_) => &[JobLifecycle::Enabled, JobLifecycle::Suspended],
            Self::Retire | Self::SuccessorEnabled(_) => &[
                JobLifecycle::Draft,
                JobLifecycle::Validated,
                JobLifecycle::Registered,
                JobLifecycle::Enabled,
                JobLifecycle::Suspended,
            ],
            Self::Reactivate | Self::Delete => &[JobLifecycle::Retired],
            Self::VersionChanged => &[JobLifecycle::Draft, JobLifecycle::Validated, JobLifecycle::Registered],
            Self::ValidationFailed => &[JobLifecycle::Validated, JobLifecycle::Registered],
        }
    }

    const fn result(&self, from: JobLifecycle) -> Applied {
        match self {
            Self::SetSchedule(_) | Self::OnProbation(_) | Self::ProbationPassed(_) => Applied::To(from),
            Self::Validated(_) => Applied::To(JobLifecycle::Validated),
            Self::DryRunCompleted(_) | Self::ClearSchedule => Applied::To(JobLifecycle::Registered),
            Self::Enable(..) | Self::Resume(_) => Applied::To(JobLifecycle::Enabled),
            Self::Suspend(_) | Self::ProbationFailed(..) => Applied::To(JobLifecycle::Suspended),
            Self::Retire | Self::SuccessorEnabled(_) => Applied::To(JobLifecycle::Retired),
            Self::Reactivate | Self::VersionChanged | Self::ValidationFailed => Applied::To(JobLifecycle::Draft),
            Self::Delete => Applied::Removed,
        }
    }
}

/// What a transition did to the job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Applied {
    /// The job is now in this state.
    To(JobLifecycle),
    /// The job is gone from the store.
    Removed,
}

/// A transition the table does not allow. Nothing is changed.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum TransitionError {
    #[error("{transition} is made by the {owners}, not by the {actor}", owners = list(.allowed))]
    WrongActor {
        transition: &'static str,
        actor: Actor,
        allowed: &'static [Actor],
    },
    #[error(
        "{transition} is not allowed for a job that is {from}{why}; it applies to a job that is {states}",
        states = list(.allowed),
        why = SuspendedBecause(.suspension.as_deref())
    )]
    WrongState {
        transition: &'static str,
        from: JobLifecycle,
        allowed: &'static [JobLifecycle],
        /// Why the job is suspended, when it is and the reason is on record.
        suspension: Option<Box<SuspensionRecord>>,
    },
    /// The fact being recorded is about a version of the job that is not the one on record.
    #[error("{transition} was made for a version of the job that is not the one that was validated; validate it again")]
    WrongVersion { transition: &'static str },
    /// Validation has not passed for the version the job has now, so it cannot be resumed.
    #[error(
        "{transition} needs validation to have passed for the job's current version; validate the job, and resume it once it passes"
    )]
    NotValidated { transition: &'static str },
    /// The job has no first live run on trial, so there is no outcome to settle.
    #[error("{transition} applies to a job whose first live run is on trial, and this job has none")]
    NotOnProbation { transition: &'static str },
    /// The release the job names is one that does not allow the change.
    #[error("{transition} is not allowed for a job on release {release}, which is {kind}: {way_on}")]
    KindRefused {
        transition: &'static str,
        kind: ReleaseKind,
        release: Version,
        /// What to do instead.
        way_on: &'static str,
    },
    /// The job has a successor that has not been retired, so it neither runs nor comes back.
    #[error(
        "{transition} is not allowed for {job}, which has the standing successor {successor}; retire or delete {successor} first"
    )]
    StandingSuccessor {
        transition: &'static str,
        job: String,
        successor: String,
    },
    /// The change belongs to the operation that applies the rules of the job's release, which this call does not.
    #[error(
        "{transition} for a job on a recorded release is made through the release rules, not as a plain transition"
    )]
    ReleaseRulesApply { transition: &'static str },
    /// The predecessor's key is held by a job of another identity than the one the successor was linked to.
    #[error(
        "{successor} succeeds a job of identity {recorded}, but {predecessor} is now a job of identity {found}; \
         retire or delete {predecessor}, or make a new successor"
    )]
    PredecessorChanged {
        successor: String,
        predecessor: String,
        recorded: String,
        found: String,
    },
    /// The job file and the state disagree about the job's predecessor: one names a predecessor the other does not, or the
    /// file names another key than the one the link was recorded for.
    #[error(
        "{successor} and its job file disagree about its predecessor{}; restore the file's predecessor, or validate it again",
        HeldBy(holder.as_deref())
    )]
    PredecessorUnconfirmed {
        successor: String,
        /// The job that still holds the identity the link recorded, when one does: the recorded predecessor, renamed.
        holder: Option<String>,
    },
    /// The job file names another release or predecessor than the job was enabled with, so the job it describes is not
    /// the one the state holds.
    #[error(
        "{transition} is not allowed for {job}: its file's {key} is {named}, but the job was enabled with {recorded}; \
         restore the file, or use `jobs successor` to move to another"
    )]
    FileDiffersFromRecord {
        transition: &'static str,
        job: String,
        /// The job file key that differs: `version` or `predecessor`.
        key: &'static str,
        recorded: String,
        named: String,
    },
    /// Enabling needs an expression, and neither the request nor a predecessor gives one.
    #[error("{transition} needs a schedule, and no predecessor has one to carry over; give a cron expression")]
    ScheduleRequired { transition: &'static str },
    /// Enabling the successor would retire a predecessor that a run is using.
    #[error(
        "{successor} cannot be enabled while a run of its predecessor {predecessor} is active, because enabling it retires \
         {predecessor}; enable it when that run has ended"
    )]
    PredecessorBusy { successor: String, predecessor: String },
    /// The job's entry holds no identity, so a link to it could not be told from another job's.
    #[error("{job} has no recorded identity; validate it again")]
    IdentityUnrecorded { job: String },
}

/// What a refusal says of the job that still holds the identity a link recorded.
struct HeldBy<'a>(Option<&'a str>);

impl fmt::Display for HeldBy<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.map_or(Ok(()), |holder| {
            write!(formatter, " (the recorded predecessor is now the job {holder})")
        })
    }
}

/// What a refusal says of a suspended job: the whole record, so the operator need not look it up to see why.
struct SuspendedBecause<'a>(Option<&'a SuspensionRecord>);

impl fmt::Display for SuspendedBecause<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.map_or(Ok(()), |record| write!(formatter, " ({record})"))
    }
}

impl TransitionError {
    /// The stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        if matches!(self, Self::PredecessorBusy { .. }) {
            "workflow/job-busy"
        } else {
            "workflow/transition-refused"
        }
    }
}

fn list<T: fmt::Display>(items: &[T]) -> String {
    items.iter().map(ToString::to_string).collect::<Vec<_>>().join(" or ")
}

/// Applies `transition`, made by `actor`, to a job in state `from`.
///
/// # Errors
///
/// Returns [`TransitionError::WrongActor`] when the actor does not own the transition, and
/// [`TransitionError::WrongState`] when the job is not in a state the transition applies to. The actor is checked
/// first.
pub fn apply(from: JobLifecycle, actor: Actor, transition: &Transition) -> Result<Applied, TransitionError> {
    if !transition.owners().contains(&actor) {
        return Err(TransitionError::WrongActor {
            transition: transition.name(),
            actor,
            allowed: transition.owners(),
        });
    }
    if !transition.allowed_from().contains(&from) {
        return Err(TransitionError::WrongState {
            transition: transition.name(),
            from,
            allowed: transition.allowed_from(),
            suspension: None,
        });
    }
    Ok(transition.result(from))
}

#[cfg(test)]
#[path = "lifecycle_test.rs"]
mod tests;
