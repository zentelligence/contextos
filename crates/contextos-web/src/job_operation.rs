//! The one operation an operator's request about a job goes through, for a vault job and a shared job alike.
//!
//! Every caller goes through [`perform`], so none can disagree with another about what is allowed. The operation holds
//! every rule an operator's request adds to the transition table of `contextos-workflows`, once: a schedule enables a
//! registered job and replaces the schedule of an enabled or suspended one, enabling is for the version the job has
//! now, and resuming validates the version the job has now first and goes ahead only when that passes. Where a job's
//! state lives, and how it is validated, are behind [`JobStore`], with an adapter for a vault's `.contextos/jobs.toml`
//! and one for the host's state file, so neither adapter holds a rule.
//!
//! The operator is the only actor here. A transition the table does not give the operator, or from a state it does not
//! apply to, is refused with the table's own message, which names the state and what would allow it, and nothing is
//! written.

use std::fmt::Write as _;
use std::future::Future;

use contextos_workflows::{
    Actor, Applied, Changed, JobFingerprint, JobLifecycle, JobSchedule, JobState, ReleaseFingerprintInput, StoreKind,
    StoredFailure, StoredValidation, SuspensionReason, Transition, TransitionError, apply,
};
use thiserror::Error;
use time::OffsetDateTime;

use crate::enabling_report::EnablingReport;
use crate::scheduler::{RunKind, run_allowed};
use crate::store_retention::RetentionWarning;

/// What an operator asks of one job.
#[derive(Clone, Debug)]
pub enum OperatorRequest {
    /// Validate the job now, whether or not its job file says it is ready, and record the result.
    Validate,
    /// Set the job's schedule: it enables a registered job, and replaces the schedule of an enabled or suspended one.
    Schedule(JobSchedule),
    /// Enable a registered successor with its predecessor's schedule.
    Enable,
    /// Clear the schedule of an enabled job, which returns it to `registered`.
    ClearSchedule,
    /// Stop an enabled job, and say why.
    Suspend(SuspensionReason),
    /// Resume a suspended job, once validation passes for the version it has now.
    Resume,
    /// Take an enabled or suspended job out of use.
    Retire,
    /// Bring a retired job back as a draft.
    Reactivate,
    /// Delete a retired job's state.
    Delete,
}

/// What an operator's request came to.
#[derive(Debug)]
pub enum OperationOutcome {
    /// Validation ran and its result was recorded. `lifecycle` is the job's state afterwards: a draft that passed is
    /// `validated`, and anything else is as it was.
    Validated {
        result: Box<StoredValidation>,
        lifecycle: JobLifecycle,
    },
    /// A transition was made.
    Changed(Applied),
    /// A job was enabled, which can retire its predecessor and move the workflow's `current` and `previous`.
    Enabled(Box<EnablingReport>),
}

/// Why an operator's request was not carried out. Nothing was written in any case.
#[derive(Debug, Error)]
pub enum OperationError {
    /// The transition table does not allow it. Its text names the state and what would allow it.
    #[error("{code}: {0}", code = .0.code())]
    Refused(#[from] TransitionError),
    /// Another writer changed the state since it was read. Repeating the request reads the new state.
    #[error("workflow/state-changed: the job state changed while this change was being made; try again")]
    Conflict,
    /// The state could not be read or written, or is malformed, or the server it lives behind cannot be reached.
    #[error("workflow/state-unavailable: the job state could not be read or written: {0}")]
    Unavailable(String),
    /// The request needs a version of the job, and its files could not all be read.
    #[error("workflow/job-unreadable: the job's files could not all be read, so there is no version of it to {0}")]
    Unreadable(&'static str),
    /// The request needs the job's files, and there are none by that name. The text says where they were looked for.
    #[error("workflow/job-not-found: {0}")]
    NotFound(String),
    /// The job's file could not be removed, or is one that cannot be used and so was left alone. The state is as it
    /// was.
    #[error("workflow/job-store-unavailable: {0}")]
    JobStoreUnavailable(String),
    /// Two job files share the job's key, so a request that would remove one of them cannot say which.
    #[error("workflow/job-key-taken: {0}")]
    KeyTaken(String),
    /// Validation did not pass for the version the job has now, so the job was not resumed.
    #[error("{}", not_valid_message(failures, unverifiable, *unreadable))]
    NotValid {
        failures: Vec<StoredFailure>,
        /// The checks that could not be made, each as `check: reason`.
        unverifiable: Vec<String>,
        /// Whether the job's files could not all be read, so it had no version to validate.
        unreadable: bool,
    },
}

impl OperationError {
    /// The stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Refused(refusal) => refusal.code(),
            Self::Conflict => "workflow/state-changed",
            Self::Unavailable(_) => "workflow/state-unavailable",
            Self::NotFound(_) => "workflow/job-not-found",
            Self::KeyTaken(_) => "workflow/job-key-taken",
            Self::JobStoreUnavailable(_) => "workflow/job-store-unavailable",
            Self::Unreadable(_) => "workflow/job-unreadable",
            Self::NotValid { .. } => "workflow/validation-failed",
        }
    }
}

fn not_valid_message(failures: &[StoredFailure], unverifiable: &[String], unreadable: bool) -> String {
    let mut text = String::from(
        "workflow/validation-failed: validation did not pass for the job's current version, so it was not resumed",
    );
    if unreadable {
        text.push_str("; the job's files could not all be read");
    }
    if !unverifiable.is_empty() {
        text.push_str("; some checks were unverifiable");
    }
    for failure in failures {
        let _ = write!(text, "\n  {}: {}: {}", failure.code, failure.artefact, failure.message);
    }
    for check in unverifiable {
        let _ = write!(text, "\n  {check} could not be checked");
    }
    text
}

/// Why a store could not do what the operation asked of it.
#[derive(Debug, Error)]
pub enum StoreError {
    /// Another writer changed the state since it was read.
    #[error("the job state changed while this change was being made")]
    Conflict,
    /// The state could not be read or written, or the server it lives behind cannot be reached.
    #[error("{0}")]
    Unavailable(String),
    /// The job's files could not be found.
    #[error("{0}")]
    NotFound(String),
}

impl From<StoreError> for OperationError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Conflict => Self::Conflict,
            StoreError::Unavailable(reason) => Self::Unavailable(reason),
            StoreError::NotFound(place) => Self::NotFound(place),
        }
    }
}

/// Where one job's state lives and how it is validated. An adapter holds no rule: the operation decides, and the table
/// of `contextos-workflows` checks, inside the adapter's own lock or hash check.
pub trait JobStore {
    /// What the store holds for the job, which a page shows. `None` when nothing is stored for it, so it is a draft.
    fn stored(&self) -> impl Future<Output = Result<Option<JobState>, StoreError>> + Send;

    /// Whether there is a job by this name: its files are there, or something is stored for it.
    fn exists(&self) -> impl Future<Output = Result<bool, StoreError>> + Send;

    /// The job's state now. A job nothing is stored for is a draft.
    fn lifecycle(&self) -> impl Future<Output = Result<JobLifecycle, StoreError>> + Send;

    /// The job's version as its files stand now, or `None` when they cannot all be read.
    fn version(&self) -> impl Future<Output = Result<Option<JobFingerprint>, StoreError>> + Send;

    /// Validates the job now and keeps the result as [`contextos_workflows::JobsState::keep_validation`] does: a
    /// `validated` or `registered` job whose version changed goes back to `draft` first, and a draft that passed moves
    /// to `validated` for the version validated. Returns what was recorded.
    fn validate(&self, now: OffsetDateTime) -> impl Future<Output = Result<StoredValidation, StoreError>> + Send;

    /// The stored schedule of the predecessor this job was linked to, which an enabling with none given takes.
    fn predecessor_schedule(&self) -> impl Future<Output = Result<Option<String>, StoreError>> + Send {
        async { Ok(None) }
    }

    /// Whether deleting the job also removes its files. A shared job's file goes with its state; a vault job's files
    /// are the vault's, so only its state is deleted.
    fn removes_files_on_delete(&self) -> bool {
        false
    }

    /// Removes the releases of the job's store that nothing keeps any longer. The operation has been made when this runs,
    /// so a failure is the operation's warning and never its failure.
    fn retain(&self) -> impl Future<Output = Result<(), RetentionWarning>> + Send {
        async { Ok(()) }
    }

    /// Applies `transition` as the operator, checked by the table against the state as it is when it is written.
    fn apply(
        &self,
        transition: Transition,
        now: OffsetDateTime,
    ) -> impl Future<Output = Result<Changed, OperationError>> + Send;
}

/// Carries out an operator's `request` against `store`.
///
/// # Errors
///
/// Returns [`OperationError::Refused`] when the table does not allow the request, [`OperationError::NotValid`] when a
/// resume's validation does not pass, [`OperationError::Conflict`] when another writer changed the state in between,
/// and [`OperationError::Unavailable`] when the state cannot be read or written.
pub async fn perform<S: JobStore + Sync>(
    store: &S,
    request: OperatorRequest,
    now: OffsetDateTime,
) -> Result<OperationOutcome, OperationError> {
    match request {
        OperatorRequest::Validate => {
            let lifecycle = store.lifecycle().await?;
            if let Some(refused) = validation_refusal(lifecycle) {
                return Err(refused);
            }
            let result = store.validate(now).await?;
            let lifecycle = store.lifecycle().await?;
            Ok(OperationOutcome::Validated {
                result: Box::new(result),
                lifecycle,
            })
        }
        OperatorRequest::Schedule(schedule) => schedule_job(store, schedule, now).await,
        OperatorRequest::Enable => {
            let inherited = store.predecessor_schedule().await?;
            let schedule = inherited
                .as_deref()
                .and_then(|stored| JobSchedule::try_from(stored).ok())
                .ok_or(OperationError::Refused(TransitionError::ScheduleRequired {
                    transition: "Enable",
                }))?;
            schedule_job(store, schedule, now).await
        }
        OperatorRequest::ClearSchedule => clear_schedule(store, now).await,
        OperatorRequest::Suspend(reason) => {
            change(
                store,
                Transition::Suspend(contextos_workflows::Suspension::operator(reason)),
                now,
            )
            .await
        }
        OperatorRequest::Resume => resume(store, now).await,
        OperatorRequest::Retire => change(store, Transition::Retire, now).await,
        OperatorRequest::Reactivate => change(store, Transition::Reactivate, now).await,
        OperatorRequest::Delete => change(store, Transition::Delete, now).await,
    }
}

/// Clears a job's schedule. A retired job has none to clear, and clearing it succeeds and leaves it retired, as it does
/// for any job that the schedule's owner has already taken out of use.
async fn clear_schedule<S: JobStore + Sync>(
    store: &S,
    now: OffsetDateTime,
) -> Result<OperationOutcome, OperationError> {
    if store.lifecycle().await? == JobLifecycle::Retired {
        return Ok(OperationOutcome::Changed(Applied::To(JobLifecycle::Retired)));
    }
    change(store, Transition::ClearSchedule, now).await
}

async fn change<S: JobStore + Sync>(
    store: &S,
    transition: Transition,
    now: OffsetDateTime,
) -> Result<OperationOutcome, OperationError> {
    let changed = store.apply(transition, now).await?;
    Ok(match changed.enabled {
        Some(enabled) => OperationOutcome::Enabled(Box::new(EnablingReport::from(&enabled))),
        None => OperationOutcome::Changed(changed.applied),
    })
}

/// The refusal the table gives `transition` made by the operator for a job in `lifecycle`, or `None` when the table
/// allows it. The table checks the actor and the state before it looks at a version, so a request a state cannot take
/// is refused with the table's own message, which names the state and what would allow it, without reading the job's
/// files, and a job whose files are gone is told what its state allows and not that its files are missing.
fn refusal(lifecycle: JobLifecycle, transition: &Transition) -> Option<OperationError> {
    apply(lifecycle, Actor::Operator, transition)
        .err()
        .map(OperationError::from)
}

/// The states a job may be validated in on request. A retired job does nothing until it is re-activated, so validating
/// it would only store a result against a job that is out of use.
const VALIDATABLE: &[JobLifecycle] = &[
    JobLifecycle::Draft,
    JobLifecycle::Validated,
    JobLifecycle::Registered,
    JobLifecycle::Enabled,
    JobLifecycle::Suspended,
];

/// The refusal of a validation request for a job in `lifecycle`, or `None` when it may be validated. Validation is not
/// a transition, so it is not in the table, and this is its one rule: [`perform`] refuses by it and [`offered`] offers
/// by it.
fn validation_refusal(lifecycle: JobLifecycle) -> Option<OperationError> {
    (!VALIDATABLE.contains(&lifecycle)).then_some(OperationError::Refused(TransitionError::WrongState {
        transition: "Validate",
        from: lifecycle,
        allowed: VALIDATABLE,
        suspension: None,
    }))
}

/// A version to build a transition with when only the table's refusal of it is wanted. It is never stored: [`refusal`]
/// is asked only about the actor and the state.
fn stand_in() -> JobFingerprint {
    JobFingerprint::from(&ReleaseFingerprintInput {
        store: StoreKind::Vault,
        package: "",
        release: "0.0.0",
        digest: "",
        job_toml: "",
        release_manifest: "",
        libraries: &[],
    })
}

/// One thing an operator may ask of a job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Validate,
    /// Set a schedule, which enables a registered job and replaces the schedule of an enabled or suspended one.
    Schedule,
    ClearSchedule,
    Suspend,
    Resume,
    Retire,
    Reactivate,
    Delete,
    /// Start a live run now, outside the schedule.
    Run,
    /// Start a run against the workflow's scenarios, which reads and never writes.
    DryRun,
}

/// What the operator may ask of a job in a given state, which a page shows as its actions.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Offered {
    actions: Vec<Action>,
}

impl Offered {
    /// Whether `action` is offered.
    #[must_use]
    pub fn offers(&self, action: Action) -> bool {
        self.actions.contains(&action)
    }

    #[must_use]
    pub fn validate(&self) -> bool {
        self.offers(Action::Validate)
    }

    #[must_use]
    pub fn schedule(&self) -> bool {
        self.offers(Action::Schedule)
    }

    #[must_use]
    pub fn clear_schedule(&self) -> bool {
        self.offers(Action::ClearSchedule)
    }

    #[must_use]
    pub fn suspend(&self) -> bool {
        self.offers(Action::Suspend)
    }

    #[must_use]
    pub fn resume(&self) -> bool {
        self.offers(Action::Resume)
    }

    #[must_use]
    pub fn retire(&self) -> bool {
        self.offers(Action::Retire)
    }

    #[must_use]
    pub fn reactivate(&self) -> bool {
        self.offers(Action::Reactivate)
    }

    #[must_use]
    pub fn delete(&self) -> bool {
        self.offers(Action::Delete)
    }

    #[must_use]
    pub fn run(&self) -> bool {
        self.offers(Action::Run)
    }

    #[must_use]
    pub fn dry_run(&self) -> bool {
        self.offers(Action::DryRun)
    }
}

impl From<&[Action]> for Offered {
    fn from(actions: &[Action]) -> Self {
        Self {
            actions: actions.to_vec(),
        }
    }
}

/// What a job in `lifecycle` offers the operator. Every action but validation and the runs is offered exactly when the
/// transition table allows it, so a page never offers what [`perform`] would refuse and holds no state rule of its own.
/// A job is validated on demand in any state but `retired`, which does nothing until it is re-activated. A run is
/// offered exactly when a run of that kind would be allowed.
#[must_use]
pub fn offered(lifecycle: JobLifecycle) -> Offered {
    let allowed = |transition: Transition| refusal(lifecycle, &transition).is_none();
    let schedule = JobSchedule::try_from("0 0 * * *").ok();
    let suspend = SuspensionReason::try_from("stand-in").ok();
    let candidates = [
        (validation_refusal(lifecycle).is_none(), Action::Validate),
        (
            schedule.is_some_and(|schedule| {
                allowed(Transition::Enable(schedule.clone(), stand_in())) || allowed(Transition::SetSchedule(schedule))
            }),
            Action::Schedule,
        ),
        (allowed(Transition::ClearSchedule), Action::ClearSchedule),
        (
            suspend
                .is_some_and(|reason| allowed(Transition::Suspend(contextos_workflows::Suspension::operator(reason)))),
            Action::Suspend,
        ),
        (allowed(Transition::Resume(stand_in())), Action::Resume),
        (allowed(Transition::Retire), Action::Retire),
        (allowed(Transition::Reactivate), Action::Reactivate),
        (allowed(Transition::Delete), Action::Delete),
        (run_allowed(lifecycle, RunKind::Live), Action::Run),
        (run_allowed(lifecycle, RunKind::Dry), Action::DryRun),
    ];
    Offered {
        actions: candidates
            .into_iter()
            .filter_map(|(offered, action)| offered.then_some(action))
            .collect(),
    }
}

/// Whether a job in `lifecycle` is flagged as needing the operator's attention: a draft has not been taken forward, and
/// a suspended job has stopped running.
#[must_use]
pub const fn needs_attention(lifecycle: JobLifecycle) -> bool {
    matches!(lifecycle, JobLifecycle::Draft | JobLifecycle::Suspended)
}

/// What an operator's request came to, and what went wrong afterwards without undoing it.
#[derive(Debug)]
pub struct Performed {
    pub outcome: OperationOutcome,
    /// Retention could not finish after the request. The request was carried out all the same.
    pub warnings: Vec<RetentionWarning>,
}

/// Carries out an operator's `request` as [`perform`] does, then removes the releases nothing keeps any longer when the
/// request could have left some: a job enabled, retired, or deleted, or one whose validation passed, which can fix its
/// release's library lock. A retention that fails is a warning, and the request still succeeded; the next change runs it
/// again.
///
/// # Errors
///
/// As [`perform`]. A refused request runs no retention.
pub async fn perform_retaining<S: JobStore + Sync>(
    store: &S,
    request: OperatorRequest,
    now: OffsetDateTime,
) -> Result<Performed, OperationError> {
    let leaves_releases_unkept = matches!(
        request,
        OperatorRequest::Schedule(_) | OperatorRequest::Enable | OperatorRequest::Retire | OperatorRequest::Delete
    );
    let outcome = perform(store, request, now).await?;
    let retain =
        leaves_releases_unkept || matches!(&outcome, OperationOutcome::Validated { result, .. } if result.passed);
    let warnings = if retain {
        store.retain().await.err().into_iter().collect()
    } else {
        Vec::new()
    };
    Ok(Performed { outcome, warnings })
}

/// An enabled or suspended job's schedule is replaced. Any other job is asked to be enabled, so a refusal names what
/// enabling needs, which is a registered job, and not what replacing a schedule needs.
async fn schedule_job<S: JobStore + Sync>(
    store: &S,
    schedule: JobSchedule,
    now: OffsetDateTime,
) -> Result<OperationOutcome, OperationError> {
    let lifecycle = store.lifecycle().await?;
    if matches!(lifecycle, JobLifecycle::Enabled | JobLifecycle::Suspended) {
        return change(store, Transition::SetSchedule(schedule), now).await;
    }
    if let Some(refused) = refusal(lifecycle, &Transition::Enable(schedule.clone(), stand_in())) {
        return Err(refused);
    }
    let Some(version) = store.version().await? else {
        return Err(OperationError::Unreadable("enable"));
    };
    change(store, Transition::Enable(schedule, version), now).await
}

/// A suspended job is validated for the version it has now, and resumed for the version that validation was made for,
/// so the two cannot differ. A job that is not suspended is refused before anything is validated.
async fn resume<S: JobStore + Sync>(store: &S, now: OffsetDateTime) -> Result<OperationOutcome, OperationError> {
    let lifecycle = store.lifecycle().await?;
    if let Some(refused) = refusal(lifecycle, &Transition::Resume(stand_in())) {
        return Err(refused);
    }
    let result = store.validate(now).await?;
    let Some(version) = result.fingerprint.clone().filter(|_| result.passed) else {
        return Err(OperationError::NotValid {
            unreadable: result.fingerprint.is_none(),
            failures: result.failures,
            unverifiable: result.unverifiable,
        });
    };
    change(store, Transition::Resume(version), now).await
}

#[cfg(test)]
#[path = "job_operation_test.rs"]
mod tests;
