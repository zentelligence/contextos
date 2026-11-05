//! One package's part of the job state, and the pure rule that decides whether a cron tick has already fired.
//!
//! The state file, `.contextos/jobs.toml` in a vault and `shared-jobs.toml` on the host, holds what an operator decides
//! about a job and what validation and a dry run found, and nothing a run writes: which run is active, run numbers, and
//! the last tick fired are the host's, kept beside each job's claim. Its layout is [`crate::host_state::JobsState`]'s,
//! which keeps one [`PackageJobsState`] for each package and so identifies a job by its package, its workflow, and its
//! own key, mirroring `registry/workflows/{package}/{workflow}/jobs/{job}.toml`.
//!
//! Reading and writing the file itself is `contextos-web`'s own concern (an ordinary `fs_read_text_file`/`fs_write_file`
//! MCP round trip): this module defines the schema of a job's entry, deriving `serde::{Serialize, Deserialize}` so the
//! file can be round-tripped through `toml`, and the decision function a scheduler tick is evaluated through, taking
//! plain values with no I/O of its own.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::fingerprint::JobFingerprint;
use crate::library_lock::LibraryLock;
use crate::lifecycle::{Actor, Applied, JobLifecycle, SuspensionCause, Transition, TransitionError, apply};
use crate::logging::{CUT_MARKER, cut_to_bytes};
use crate::release_state::{JobIdentity, JobLink, JobRetirement, PackageReleases, RetirementCause};
use crate::run_record::MAX_ERROR_BYTES;

#[path = "state_validation.rs"]
mod validation_results;

#[path = "state_release.rs"]
mod release_rules;

#[path = "state_enable.rs"]
mod enabling;

pub use enabling::{Changed, EnableRequest, Enabled, JobScope, ReleaseContext, ReleaseMove};
pub use release_rules::{ChainHold, ChainRelation, LinkRefusal};
pub use validation_results::{
    LockOutcome, ReleaseRuleFailure, ReleaseRuleFindings, StoredFailure, StoredValidation, ValidationKept,
    ValidationRecord,
};

/// One package's jobs and release records. The state file holds every package's, which [`crate::host_state::JobsState`]
/// reads and writes.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(test, derive(Serialize, Deserialize))]
pub struct PackageJobsState {
    #[cfg_attr(test, serde(default, skip_serializing_if = "BTreeMap::is_empty"))]
    pub workflows: BTreeMap<String, WorkflowJobsState>,
    /// The package's workflows' `current` and `previous` and its releases' markers and library locks, by package name.
    #[cfg_attr(test, serde(default, skip_serializing_if = "BTreeMap::is_empty"))]
    pub packages: BTreeMap<String, PackageReleases>,
}

impl PackageJobsState {
    /// The recorded state for one job nested under `workflow_slug`, or
    /// `None` when neither the workflow nor the job has ever recorded a
    /// scheduler fire or favourite.
    #[must_use]
    pub fn job(&self, workflow_slug: &str, job_slug: &str) -> Option<&JobState> {
        self.workflows.get(workflow_slug)?.jobs.get(job_slug)
    }

    /// The lifecycle state of a job. A job nothing has been recorded for is a draft.
    #[must_use]
    pub fn lifecycle_of(&self, workflow_slug: &str, job_slug: &str) -> JobLifecycle {
        self.job(workflow_slug, job_slug)
            .map_or_else(JobLifecycle::default, |job| job.lifecycle)
    }

    /// Whether a job has ever been `enabled`. A job nothing has been recorded for has not.
    #[must_use]
    pub fn ever_enabled(&self, workflow_slug: &str, job_slug: &str) -> bool {
        self.job(workflow_slug, job_slug).is_some_and(JobState::ever_enabled)
    }

    /// The cron expression the scheduler may act on for a job: its stored schedule, and only while the job is
    /// `enabled`. Every other state yields none, whatever schedule is stored.
    #[must_use]
    pub fn scheduled_expression(&self, workflow_slug: &str, job_slug: &str) -> Option<&str> {
        let job = self.job(workflow_slug, job_slug)?;
        job.lifecycle
            .runs_on_schedule()
            .then_some(job.schedule.as_deref())
            .flatten()
    }

    /// The `current` and `previous` versions and release records of `package`, when anything is recorded for it.
    #[must_use]
    pub fn package_releases(&self, package: &str) -> Option<&PackageReleases> {
        self.packages.get(package)
    }

    /// Applies `transition`, made by `actor`, to a job. A refused transition changes nothing, not even by recording
    /// an entry for a job that had none. A deletion removes the job's entry.
    ///
    /// # Errors
    ///
    /// Returns the refusal when the actor does not own the transition or the job is not in a state it applies to.
    pub fn transition(
        &mut self,
        workflow_slug: &str,
        job_slug: &str,
        actor: Actor,
        transition: &Transition,
        now: OffsetDateTime,
    ) -> Result<Applied, TransitionError> {
        if matches!(
            transition,
            Transition::Enable(..) | Transition::Resume(_) | Transition::Reactivate
        ) && apply(self.lifecycle_of(workflow_slug, job_slug), actor, transition).is_ok()
            && self
                .job(workflow_slug, job_slug)
                .is_some_and(|job| job.release.is_some())
        {
            return Err(TransitionError::ReleaseRulesApply {
                transition: transition.name(),
            });
        }
        self.apply_transition(workflow_slug, job_slug, actor, transition, now)
    }

    /// [`Self::transition`] without the check that sends a change on a recorded release to the operations that apply the
    /// rules of its release.
    fn apply_transition(
        &mut self,
        workflow_slug: &str,
        job_slug: &str,
        actor: Actor,
        transition: &Transition,
        now: OffsetDateTime,
    ) -> Result<Applied, TransitionError> {
        let applied = apply(self.lifecycle_of(workflow_slug, job_slug), actor, transition)
            .map_err(|refused| self.with_suspension(workflow_slug, job_slug, refused))?;
        if let Transition::DryRunCompleted(version)
        | Transition::Enable(_, version)
        | Transition::ProbationPassed(version)
        | Transition::ProbationFailed(version, _) = transition
            && self
                .job(workflow_slug, job_slug)
                .and_then(|job| job.fingerprint.as_ref())
                != Some(version)
        {
            return Err(TransitionError::WrongVersion {
                transition: transition.name(),
            });
        }
        if let Transition::Resume(version) = transition
            && !self
                .job(workflow_slug, job_slug)
                .and_then(JobState::validation)
                .is_some_and(|stored| stored.passed && stored.fingerprint.as_ref() == Some(version))
        {
            return Err(TransitionError::NotValidated {
                transition: transition.name(),
            });
        }
        if matches!(
            transition,
            Transition::ProbationPassed(_) | Transition::ProbationFailed(..)
        ) && !self
            .job(workflow_slug, job_slug)
            .is_some_and(JobState::first_run_pending)
        {
            return Err(TransitionError::NotOnProbation {
                transition: transition.name(),
            });
        }
        match applied {
            Applied::Removed => {
                if let Some(workflow) = self.workflows.get_mut(workflow_slug) {
                    workflow.jobs.remove(job_slug);
                    if workflow.jobs.is_empty() {
                        self.workflows.remove(workflow_slug);
                    }
                }
            }
            Applied::To(next) => self
                .job_entry(workflow_slug, job_slug)
                .record(next, actor, transition, now),
        }
        Ok(applied)
    }

    /// `refused`, with the job's suspension record added when it names a suspended job, so the refusal says why.
    #[must_use]
    pub fn with_suspension(&self, workflow_slug: &str, job_slug: &str, refused: TransitionError) -> TransitionError {
        match refused {
            TransitionError::WrongState {
                transition,
                from: from @ JobLifecycle::Suspended,
                allowed,
                suspension: None,
            } => TransitionError::WrongState {
                transition,
                from,
                allowed,
                suspension: self
                    .job(workflow_slug, job_slug)
                    .and_then(JobState::suspension)
                    .cloned()
                    .map(Box::new),
            },
            other => other,
        }
    }

    /// A mutable handle to `workflow_slug`/`job_slug`'s own entry,
    /// inserting a default one (both levels) if neither exists yet.
    pub fn job_entry(&mut self, workflow_slug: &str, job_slug: &str) -> &mut JobState {
        self.workflows
            .entry(workflow_slug.to_owned())
            .or_default()
            .jobs
            .entry(job_slug.to_owned())
            .or_default()
    }
}

/// Every one of a workflow's own saved jobs' scheduler bookkeeping.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(test, derive(Serialize, Deserialize))]
pub struct WorkflowJobsState {
    #[cfg_attr(test, serde(default, skip_serializing_if = "BTreeMap::is_empty"))]
    pub jobs: BTreeMap<String, JobState>,
}

/// `[jobs.{{slug}}]`: per-job scheduler bookkeeping.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(from = "JobStateRaw")]
pub struct JobState {
    /// The dedup guard preventing the same cron tick from firing twice
    /// across a restart near a tick boundary.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub last_scheduled_fire: Option<OffsetDateTime>,
    /// Whether this job is favourited: surfaced in the Command Centre header's shared Favourites
    /// panel ("Favourite jobs"), the apps-side equivalent of `.contextos/apps.toml`'s own
    /// `[apps.<slug>] favourite` flag. Read and written by `contextos-web`'s own job routes, never
    /// by the scheduler itself (which only reads `last_scheduled_fire` as a schedule baseline).
    #[serde(default)]
    pub favourite: bool,
    /// Where the job is in its lifecycle. Private, and written only by [`PackageJobsState::transition`], so no state is ever
    /// set except through the transition table.
    #[serde(default)]
    lifecycle: JobLifecycle,
    /// The schedule the operator set, kept while the job is `enabled` or `suspended`. The scheduler reads it only
    /// through [`PackageJobsState::scheduled_expression`], which yields it only while `enabled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    schedule: Option<String>,
    /// When the lifecycle state last changed.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    lifecycle_changed: Option<OffsetDateTime>,
    /// Who made that change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lifecycle_actor: Option<Actor>,
    /// The structured record of why the job is suspended, while it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    suspension: Option<SuspensionRecord>,
    /// The version of the job that its validation, and its dry run, were made for. Set when it is validated, and void
    /// when the version changes or the job is re-activated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fingerprint: Option<JobFingerprint>,
    /// Whether the next live run of the recorded version is its first, and so is on trial: set when the job is enabled
    /// or a new version of it passes validation, and cleared when that run succeeds or degrades. A failed run suspends
    /// the job and leaves it set. Only a job that is `enabled` or `suspended` has it.
    #[serde(default, skip_serializing_if = "is_false")]
    first_run: bool,
    /// What the last validation found, whether it passed or not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    validation: Option<StoredValidation>,
    /// When a dry run last completed for the recorded version.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    dry_run_at: Option<OffsetDateTime>,
    /// The identity the state file gave the job when its key first entered state, never reused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    identity: Option<JobIdentity>,
    /// The release its validated version was made from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    release: Option<semver::Version>,
    /// The library lock its validated version was made from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lock: Option<LibraryLock>,
    /// A successor's link to its predecessor, recorded at the successor's first passing validation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    link: Option<JobLink>,
    /// The record of the job's retirement by its successor's enabling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retirement: Option<JobRetirement>,
    /// Whether the job has ever been `enabled`: set when it is enabled and kept through suspension and retirement. A
    /// job retired without it ends its chain, so it cannot be a predecessor.
    #[serde(default, skip_serializing_if = "is_false")]
    ever_enabled: bool,
}

/// `serde`'s `skip_serializing_if` hands the field by reference, so this takes one, whatever its size.
#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_false(flag: &bool) -> bool {
    !*flag
}

/// A job's entry as it is read: the stored fields, and the free-text `suspended_reason` an older file holds, which is
/// folded into a suspension record and never written again.
#[derive(Deserialize)]
struct JobStateRaw {
    #[serde(default, with = "time::serde::rfc3339::option")]
    last_scheduled_fire: Option<OffsetDateTime>,
    #[serde(default)]
    favourite: bool,
    #[serde(default)]
    lifecycle: JobLifecycle,
    #[serde(default)]
    schedule: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    lifecycle_changed: Option<OffsetDateTime>,
    #[serde(default)]
    lifecycle_actor: Option<Actor>,
    #[serde(default)]
    suspended_reason: Option<String>,
    #[serde(default)]
    suspension: Option<SuspensionRecord>,
    #[serde(default)]
    fingerprint: Option<JobFingerprint>,
    #[serde(default)]
    first_run: bool,
    #[serde(default)]
    validation: Option<StoredValidation>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    dry_run_at: Option<OffsetDateTime>,
    #[serde(default)]
    identity: Option<JobIdentity>,
    #[serde(default)]
    release: Option<semver::Version>,
    #[serde(default)]
    lock: Option<LibraryLock>,
    #[serde(default)]
    link: Option<JobLink>,
    #[serde(default)]
    retirement: Option<JobRetirement>,
    #[serde(default)]
    ever_enabled: bool,
}

impl From<JobStateRaw> for JobState {
    fn from(raw: JobStateRaw) -> Self {
        let suspension = raw.suspension.or_else(|| {
            raw.suspended_reason.map(|reason| SuspensionRecord {
                code: LEGACY_SUSPENSION_CODE.to_owned(),
                // What the older file never recorded stays unknown: neither is invented, and neither is written back.
                by: raw.lifecycle_actor,
                at: raw.lifecycle_changed,
                reason: cut_stored_text(&reason),
                run: None,
                failure: None,
            })
        });
        Self {
            last_scheduled_fire: raw.last_scheduled_fire,
            favourite: raw.favourite,
            lifecycle: raw.lifecycle,
            schedule: raw.schedule,
            lifecycle_changed: raw.lifecycle_changed,
            lifecycle_actor: raw.lifecycle_actor,
            suspension,
            fingerprint: raw.fingerprint,
            first_run: raw.first_run,
            validation: raw.validation,
            dry_run_at: raw.dry_run_at,
            identity: raw.identity,
            release: raw.release,
            lock: raw.lock,
            link: raw.link,
            ever_enabled: raw.ever_enabled
                || raw.retirement.is_some()
                || matches!(raw.lifecycle, JobLifecycle::Enabled | JobLifecycle::Suspended),
            retirement: raw.retirement,
        }
    }
}

/// The code a suspension stored before suspensions were structured reads as: its cause was never recorded.
const LEGACY_SUSPENSION_CODE: &str = "workflow/suspended";

/// Why a job is suspended, as it is stored: a stable code, who suspended it and when, and the words that explain it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SuspensionRecord {
    #[serde(deserialize_with = "cut_text")]
    pub code: String,
    /// Who suspended it. Absent when an older file never recorded it, which is not the same as the system.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<Actor>,
    /// When it was suspended. Absent when an older file never recorded it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub at: Option<OffsetDateTime>,
    #[serde(deserialize_with = "cut_text")]
    pub reason: String,
    /// The run that failed, for a suspension made by a failed first run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<u64>,
    /// The stable code that run failed with.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "cut_optional_text"
    )]
    pub failure: Option<String>,
}

/// Text read from the state file, cut to the length a recorded error may have, marker included. A hand-edited file can
/// hold any length, and what is read here reaches refusals, pages, and tasks, so it is bounded when it is read.
fn cut_stored_text(text: &str) -> String {
    cut_to_bytes(text, MAX_ERROR_BYTES - CUT_MARKER.len())
}

fn cut_text<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    String::deserialize(deserializer).map(|text| cut_stored_text(&text))
}

fn cut_optional_text<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer).map(|text| text.map(|text| cut_stored_text(&text)))
}

impl fmt::Display for SuspensionRecord {
    /// The code, who and when when they were recorded, the run and the failure when there are any, and the reason.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.code)?;
        if let Some(by) = self.by {
            write!(formatter, " by {}", by.as_str())?;
        }
        if let Some(at) = self.at.and_then(|at| at.format(&Rfc3339).ok()) {
            write!(formatter, " at {at}")?;
        }
        if let Some(run) = self.run {
            write!(formatter, ", run R-{run}")?;
        }
        if let Some(failure) = &self.failure {
            write!(formatter, ", failure {failure}")?;
        }
        if !self.reason.is_empty() {
            write!(formatter, ": {}", self.reason)?;
        }
        Ok(())
    }
}

impl JobState {
    /// The structured record of why the job is suspended, while it is.
    #[must_use]
    pub const fn suspension(&self) -> Option<&SuspensionRecord> {
        self.suspension.as_ref()
    }

    #[must_use]
    pub const fn lifecycle(&self) -> JobLifecycle {
        self.lifecycle
    }

    /// Whether the job has ever been `enabled`, whatever became of it since.
    #[must_use]
    pub const fn ever_enabled(&self) -> bool {
        self.ever_enabled
    }

    /// The stored schedule, whatever the state. The scheduler uses [`PackageJobsState::scheduled_expression`] instead.
    #[must_use]
    pub fn schedule(&self) -> Option<&str> {
        self.schedule.as_deref()
    }

    #[must_use]
    pub const fn lifecycle_changed(&self) -> Option<OffsetDateTime> {
        self.lifecycle_changed
    }

    #[must_use]
    pub const fn lifecycle_actor(&self) -> Option<Actor> {
        self.lifecycle_actor
    }

    #[must_use]
    pub fn suspended_reason(&self) -> Option<&str> {
        self.suspension.as_ref().map(|suspension| suspension.reason.as_str())
    }

    /// The version of the job that its validation was made for.
    #[must_use]
    pub const fn fingerprint(&self) -> Option<&JobFingerprint> {
        self.fingerprint.as_ref()
    }

    /// Whether the job's next live run is the first of its version, and so on trial.
    #[must_use]
    pub const fn first_run_pending(&self) -> bool {
        self.first_run
    }

    #[must_use]
    pub const fn validation(&self) -> Option<&StoredValidation> {
        self.validation.as_ref()
    }

    #[must_use]
    pub const fn dry_run_at(&self) -> Option<OffsetDateTime> {
        self.dry_run_at
    }

    /// The identity the state file gave the job when its key first entered state.
    #[must_use]
    pub const fn identity(&self) -> Option<&JobIdentity> {
        self.identity.as_ref()
    }

    /// The release the job's validated version was made from.
    #[must_use]
    pub const fn release(&self) -> Option<&semver::Version> {
        self.release.as_ref()
    }

    /// The library lock the job's validated version was made from.
    #[must_use]
    pub const fn lock(&self) -> Option<&LibraryLock> {
        self.lock.as_ref()
    }

    /// A successor's link to its predecessor.
    #[must_use]
    pub const fn link(&self) -> Option<&JobLink> {
        self.link.as_ref()
    }

    /// The record of the job's retirement by its successor's enabling.
    #[must_use]
    pub const fn retirement(&self) -> Option<&JobRetirement> {
        self.retirement.as_ref()
    }

    fn record(&mut self, next: JobLifecycle, actor: Actor, transition: &Transition, now: OffsetDateTime) {
        // A trial begun or settled, or a schedule replaced, is not a change of state, so it does not say the state
        // changed.
        if !matches!(
            transition,
            Transition::OnProbation(_) | Transition::ProbationPassed(_) | Transition::SetSchedule(_)
        ) {
            self.lifecycle_changed = Some(now);
            self.lifecycle_actor = Some(actor);
        }
        self.lifecycle = next;
        match transition {
            Transition::Enable(schedule, _) | Transition::SetSchedule(schedule) => {
                if matches!(transition, Transition::Enable(..)) {
                    self.first_run = true;
                    self.ever_enabled = true;
                }
                self.schedule = Some(schedule.as_str().to_owned());
                // A schedule counts from now, so a tick that fell due while the job was not running, or under an
                // earlier schedule, is not run late.
                self.last_scheduled_fire = Some(now);
            }
            Transition::Resume(version) => {
                // A version that was not the one on record is a new version, so its first live run is on trial.
                if self.fingerprint.as_ref() != Some(version) {
                    self.fingerprint = Some(version.clone());
                    self.first_run = true;
                }
                self.last_scheduled_fire = Some(now);
            }
            Transition::Suspend(suspension) | Transition::ProbationFailed(_, suspension) => {
                let (run, failure) = match &suspension.cause {
                    SuspensionCause::ProbationFailed { run, failure } => (*run, failure.clone()),
                    _ => (None, None),
                };
                self.suspension = Some(SuspensionRecord {
                    code: suspension.cause.code().to_owned(),
                    by: Some(actor),
                    at: Some(now),
                    reason: suspension.reason.as_str().to_owned(),
                    run,
                    failure,
                });
            }
            Transition::ClearSchedule | Transition::Retire => self.schedule = None,
            Transition::SuccessorEnabled(replaced_by) => {
                self.schedule = None;
                self.retirement = Some(JobRetirement {
                    by: actor,
                    cause: RetirementCause::SuccessorEnabled,
                    at: now,
                    replaced_by: replaced_by.clone(),
                });
            }
            Transition::Reactivate | Transition::VersionChanged | Transition::ValidationFailed => {
                self.schedule = None;
                self.fingerprint = None;
                self.dry_run_at = None;
            }
            Transition::Validated(version) => self.fingerprint = Some(version.clone()),
            Transition::OnProbation(version) => {
                self.fingerprint = Some(version.clone());
                self.first_run = true;
            }
            Transition::ProbationPassed(_) => self.first_run = false,
            Transition::DryRunCompleted(_) => self.dry_run_at = Some(now),
            Transition::Delete => {}
        }
        if next != JobLifecycle::Suspended {
            self.suspension = None;
        }
        if !matches!(next, JobLifecycle::Enabled | JobLifecycle::Suspended) {
            self.first_run = false;
        }
        if next != JobLifecycle::Retired {
            self.retirement = None;
        }
    }
}

/// What a scheduler should do about a cron tick.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TickDecision {
    /// This tick has not fired: take the job's claim and start a run, or record the tick skipped when the claim is held.
    Start,
    /// This tick, or a later one, already fired: a restart landed near the tick boundary. Not recorded as a run at all,
    /// since nothing was skipped that should have run.
    AlreadyFired,
}

/// Decides what a scheduler should do about the tick due at `scheduled_for`, given the last tick that fired. Pure: no
/// I/O and no clock read, so callers can drive every branch deterministically. Ticks only move forward, so one older
/// than the last fired is never started.
#[must_use]
pub fn evaluate_tick(last_fired: Option<OffsetDateTime>, scheduled_for: OffsetDateTime) -> TickDecision {
    if last_fired.is_some_and(|fired| fired >= scheduled_for) {
        return TickDecision::AlreadyFired;
    }
    TickDecision::Start
}

#[cfg(test)]
#[path = "state_test.rs"]
mod tests;

#[cfg(test)]
#[path = "state_probation_test.rs"]
mod probation_tests;

#[cfg(test)]
#[path = "state_operator_test.rs"]
mod operator_tests;

#[cfg(test)]
#[path = "state_suspension_test.rs"]
mod suspension_tests;

#[cfg(test)]
#[path = "state_keep_validation_test.rs"]
mod keep_validation_tests;

#[cfg(test)]
#[path = "state_release_test.rs"]
mod release_tests;

#[cfg(test)]
#[path = "state_enable_test.rs"]
mod enable_tests;

#[cfg(test)]
#[path = "state_release_properties_test.rs"]
mod release_property_tests;
