//! The job state of a vault's `.contextos/jobs.toml` and of the host's `shared-jobs.toml`: the per-job lifecycle,
//! schedule, and facts, held once for the whole store and keyed by package, workflow, and job.
//!
//! Each package's jobs are an ordinary [`PackageJobsState`], so a job is moved between states by exactly the transition
//! table every job is, with no second copy of any rule. Pure: reading and writing the file, and the lock that makes a
//! change by the scheduler and one by a command-line process safe together, belong to `contextos-web`.

use std::collections::{BTreeMap, BTreeSet};

use semver::Version;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::fingerprint::JobFingerprint;
use crate::job_release::PredecessorKey;
use crate::library_lock::{LibraryLock, VersionBasis};
use crate::lifecycle::{Actor, Applied, JobLifecycle, Transition, TransitionError};
use crate::release_state::{
    IdentityIssuer, JobIdentity, JobLink, PackageReleases, ReleaseKind, ReleaseKinds, ReleaseStateError,
    WorkflowVersions,
};
use crate::state::{
    ChainHold, Changed, EnableRequest, Enabled, JobScope, JobState, LinkRefusal, PackageJobsState, ReleaseContext,
    ReleaseRuleFindings, StoredValidation, ValidationKept, WorkflowJobsState,
};
use crate::workflow_table::WorkflowTable;

/// The state file's full contents, in either store.
///
/// In memory each package's [`PackageJobsState`] holds its own package's `current` and `previous` and release records
/// in its `packages`. In the file, one `[{package}.{workflow}]` table holds the workflow's `current` and `previous`, a
/// table for each release under its quoted version holds that release's lock and retired marker, and a table for each job
/// holds the job.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(from = "HostFile", into = "HostFile")]
pub struct JobsState {
    packages: BTreeMap<String, PackageJobsState>,
}

/// The state file as it is written: `[{package}.{workflow}]` tables, with no fixed words above them.
#[derive(Default, Deserialize, Serialize)]
#[serde(transparent)]
struct HostFile(BTreeMap<String, BTreeMap<String, WorkflowTable>>);

impl From<HostFile> for JobsState {
    fn from(file: HostFile) -> Self {
        let packages = file
            .0
            .into_iter()
            .map(|(name, workflows)| {
                let mut jobs = PackageJobsState::default();
                let mut releases = PackageReleases::default();
                for (workflow, table) in workflows {
                    if table.versions != WorkflowVersions::default() {
                        releases.workflows.insert(workflow.clone(), table.versions);
                    }
                    for (version, held) in table.releases {
                        releases
                            .releases
                            .entry(version)
                            .or_default()
                            .workflows
                            .insert(workflow.clone(), held);
                    }
                    if !table.jobs.is_empty() {
                        jobs.workflows.insert(workflow, WorkflowJobsState { jobs: table.jobs });
                    }
                }
                if releases != PackageReleases::default() {
                    jobs.packages.insert(name.clone(), releases);
                }
                (name, jobs)
            })
            .collect();
        Self { packages }
    }
}

impl From<JobsState> for HostFile {
    fn from(state: JobsState) -> Self {
        let packages = state
            .packages
            .into_iter()
            .map(|(name, jobs)| {
                let held = jobs.packages.get(&name).cloned().unwrap_or_default();
                let mut workflows: BTreeMap<String, WorkflowTable> = BTreeMap::new();
                for (workflow, versions) in held.workflows {
                    workflows.entry(workflow).or_default().versions = versions;
                }
                for (version, release) in held.releases {
                    for (workflow, kept) in release.workflows {
                        workflows
                            .entry(workflow)
                            .or_default()
                            .releases
                            .insert(version.clone(), kept);
                    }
                }
                for (workflow, held) in jobs.workflows {
                    workflows.entry(workflow).or_default().jobs = held.jobs;
                }
                (name, workflows)
            })
            .collect();
        Self(packages)
    }
}

impl JobsState {
    /// The recorded state of one job, or `None` when nothing has been recorded for it.
    #[must_use]
    pub fn job(&self, package: &str, workflow: &str, job: &str) -> Option<&JobState> {
        self.packages.get(package)?.job(workflow, job)
    }

    /// A mutable handle to one job's entry, inserting an empty one (at every level) when none exists yet.
    pub fn job_entry(&mut self, package: &str, workflow: &str, job: &str) -> &mut JobState {
        self.packages
            .entry(package.to_owned())
            .or_default()
            .job_entry(workflow, job)
    }

    /// The package, workflow, and job of every job something is recorded for, in that order.
    #[must_use]
    pub fn recorded_jobs(&self) -> Vec<(&str, &str, &str)> {
        let mut recorded = Vec::new();
        for (package, jobs) in &self.packages {
            for (workflow, state) in &jobs.workflows {
                for job in state.jobs.keys() {
                    recorded.push((package.as_str(), workflow.as_str(), job.as_str()));
                }
            }
        }
        recorded
    }

    /// The name of every package something is recorded for.
    #[must_use]
    pub fn recorded_packages(&self) -> Vec<&str> {
        self.packages.keys().map(String::as_str).collect()
    }

    /// The `current` and `previous` versions and release records of `package`, when anything is recorded for it.
    #[must_use]
    pub fn package_releases(&self, package: &str) -> Option<&PackageReleases> {
        self.packages.get(package)?.package_releases(package)
    }

    /// The library lock fixed for `workflow` in `release` of `package`, once a job on it has passed validation. See
    /// [`PackageJobsState::library_lock`].
    #[must_use]
    pub fn library_lock(&self, package: &str, workflow: &str, release: &semver::Version) -> Option<&LibraryLock> {
        self.packages.get(package)?.library_lock(package, workflow, release)
    }

    /// The lifecycle state of a job. A job nothing has been recorded for is a draft.
    #[must_use]
    pub fn lifecycle_of(&self, package: &str, workflow: &str, job: &str) -> JobLifecycle {
        self.job(package, workflow, job)
            .map_or_else(JobLifecycle::default, JobState::lifecycle)
    }

    /// Whether a job has ever been `enabled`, as [`PackageJobsState::ever_enabled`] gives it.
    #[must_use]
    pub fn ever_enabled(&self, package: &str, workflow: &str, job: &str) -> bool {
        self.job(package, workflow, job).is_some_and(JobState::ever_enabled)
    }

    /// The cron expression the scheduler may act on for a job: its stored schedule, and only while it is `enabled`.
    #[must_use]
    pub fn scheduled_expression(&self, package: &str, workflow: &str, job: &str) -> Option<&str> {
        self.packages.get(package)?.scheduled_expression(workflow, job)
    }

    /// The schedule of the predecessor a successor was linked to, as [`PackageJobsState::predecessor_schedule`] gives it.
    #[must_use]
    pub fn predecessor_schedule(&self, package: &str, workflow: &str, job: &str) -> Option<&str> {
        self.packages.get(package)?.predecessor_schedule(workflow, job)
    }

    /// The moment a schedule tick last fired for a job, which keeps one tick from firing twice across a restart.
    #[must_use]
    pub fn last_scheduled_fire(&self, package: &str, workflow: &str, job: &str) -> Option<OffsetDateTime> {
        self.job(package, workflow, job)?.last_scheduled_fire
    }

    /// Records that the tick due at `at` has fired, whether it ran or was skipped.
    pub fn record_fire(&mut self, package: &str, workflow: &str, job: &str, at: OffsetDateTime) {
        self.within(package, |jobs| {
            jobs.job_entry(workflow, job).last_scheduled_fire = Some(at);
        });
    }

    /// The identity of a shared job, given by `issuer` when its key first enters state and kept from then on, as
    /// [`PackageJobsState::ensure_identity`] does. No two packages' jobs share an identity.
    ///
    /// # Errors
    ///
    /// Returns [`ReleaseStateError::IdentityReused`] when the issuer gives an identity any package already holds.
    pub fn ensure_identity(
        &mut self,
        package: &str,
        workflow: &str,
        job: &str,
        issuer: &dyn IdentityIssuer,
    ) -> Result<JobIdentity, ReleaseStateError> {
        let elsewhere: BTreeSet<JobIdentity> = self
            .packages
            .iter()
            .filter(|(name, _)| name.as_str() != package)
            .flat_map(|(_, jobs)| jobs.identities_held().into_iter().cloned())
            .collect();
        self.within(package, |jobs| {
            jobs.ensure_identity_avoiding(workflow, job, issuer, &elsewhere)
                .cloned()
        })
    }

    /// Keeps what static validation found for one version of a job, as [`PackageJobsState::record_validation`] does.
    pub fn record_validation(&mut self, package: &str, workflow: &str, job: &str, validation: StoredValidation) {
        self.within(package, |jobs| jobs.record_validation(workflow, job, validation));
    }

    /// Keeps a validation result for one job and applies what it decides about the job's state, as
    /// [`PackageJobsState::keep_validation`] does.
    pub fn keep_validation(
        &mut self,
        package: &str,
        workflow: &str,
        job: &str,
        validation: StoredValidation,
        now: OffsetDateTime,
    ) -> ValidationKept {
        self.within(package, |jobs| jobs.keep_validation(workflow, job, validation, now))
    }

    /// What the rules about releases and the link say about a job file, as [`PackageJobsState::release_rule_findings`]
    /// gives it. A package with nothing recorded has only the rules about a job nothing is recorded for.
    #[must_use]
    pub fn release_rule_findings(
        &self,
        package: &str,
        (workflow, job): (&str, &str),
        version: Option<&JobFingerprint>,
        basis: &VersionBasis,
    ) -> ReleaseRuleFindings {
        match self.packages.get(package) {
            Some(jobs) => jobs.release_rule_findings(workflow, job, version, basis),
            None => PackageJobsState::default().release_rule_findings(workflow, job, version, basis),
        }
    }

    /// What a job file naming `named` says against the job's recorded release, as [`PackageJobsState::release_edit`]
    /// gives it.
    #[must_use]
    pub fn release_edit(
        &self,
        package: &str,
        (workflow, job): (&str, &str),
        named: &semver::Version,
    ) -> Option<String> {
        self.packages.get(package)?.release_edit(workflow, job, named)
    }

    /// Whether a validated or registered job's recorded version is no longer its current one. See
    /// [`PackageJobsState::version_drift`].
    #[must_use]
    pub fn version_drift(&self, package: &str, workflow: &str, job: &str, current: Option<&JobFingerprint>) -> bool {
        self.packages
            .get(package)
            .is_some_and(|jobs| jobs.version_drift(workflow, job, current))
    }

    /// Whether a ready draft is due static validation. See [`PackageJobsState::validation_due`].
    #[must_use]
    pub fn validation_due(
        &self,
        package: &str,
        workflow: &str,
        job: &str,
        ready: bool,
        current: Option<&JobFingerprint>,
    ) -> bool {
        self.packages
            .get(package)
            .map_or(ready, |jobs| jobs.validation_due(workflow, job, ready, current))
    }

    /// Whether a job's stored validation was made under older rules than the current ones. See
    /// [`PackageJobsState::rules_outdated`].
    #[must_use]
    pub fn rules_outdated(&self, package: &str, workflow: &str, job: &str) -> bool {
        self.packages
            .get(package)
            .is_some_and(|jobs| jobs.rules_outdated(workflow, job))
    }

    /// Whether an enabled job's recorded version is not its current one, so it is due the review that puts it on
    /// probation. See [`PackageJobsState::new_version_under_enabled_job`].
    #[must_use]
    pub fn new_version_under_enabled_job(
        &self,
        package: &str,
        workflow: &str,
        job: &str,
        current: Option<&JobFingerprint>,
    ) -> bool {
        self.packages
            .get(package)
            .is_some_and(|jobs| jobs.new_version_under_enabled_job(workflow, job, current))
    }

    /// Applies `transition`, made by `actor`, to a job, as [`PackageJobsState::transition`] does. A refused transition changes
    /// nothing, and a package left with no job is removed.
    ///
    /// # Errors
    ///
    /// Returns the refusal when the actor does not own the transition or the job is not in a state it applies to.
    pub fn transition(
        &mut self,
        package: &str,
        workflow: &str,
        job: &str,
        actor: Actor,
        transition: &Transition,
        now: OffsetDateTime,
    ) -> Result<Applied, TransitionError> {
        self.within(package, |jobs| jobs.transition(workflow, job, actor, transition, now))
    }

    /// Applies `transition`, made by `actor`, to a shared job, as [`PackageJobsState::change`] does: with a `release` context an
    /// operator's enabling, resuming, or re-activating goes by whether the job's release is current, previous, or future. A refused change leaves no
    /// entry behind.
    ///
    /// # Errors
    ///
    /// Returns the refusal [`PackageJobsState::change`] gives.
    pub fn change(
        &mut self,
        scope: JobScope<'_>,
        actor: Actor,
        transition: &Transition,
        release: Option<&ReleaseContext>,
        now: OffsetDateTime,
    ) -> Result<Applied, TransitionError> {
        self.within(scope.package, |jobs| {
            jobs.change(scope.workflow, scope.job, actor, transition, release, now)
        })
    }

    /// Applies `transition` as [`Self::change`] does, and says what an enabling did beyond the job.
    ///
    /// # Errors
    ///
    /// Returns the refusal [`PackageJobsState::change_reporting`] gives.
    pub fn change_reporting(
        &mut self,
        scope: JobScope<'_>,
        actor: Actor,
        transition: &Transition,
        release: Option<&ReleaseContext>,
        now: OffsetDateTime,
    ) -> Result<Changed, TransitionError> {
        self.within(scope.package, |jobs| {
            jobs.change_reporting(scope.workflow, scope.job, actor, transition, release, now)
        })
    }

    /// Enables a `registered` shared job by whether its release is current, previous, or future, as [`PackageJobsState::enable`] does: the job, its
    /// predecessor, and the package's `current` and `previous` change together, or none does.
    ///
    /// # Errors
    ///
    /// Returns the refusal [`PackageJobsState::enable`] gives.
    pub fn enable(
        &mut self,
        scope: JobScope<'_>,
        request: EnableRequest<'_>,
        now: OffsetDateTime,
    ) -> Result<Enabled, TransitionError> {
        self.within(scope.package, |jobs| jobs.enable(scope, request, now))
    }

    /// Resumes a `suspended` shared job when its release allows it, as [`PackageJobsState::resume`] does.
    ///
    /// # Errors
    ///
    /// Returns the refusal [`PackageJobsState::resume`] gives.
    pub fn resume(
        &mut self,
        scope: JobScope<'_>,
        version: JobFingerprint,
        now: OffsetDateTime,
    ) -> Result<Applied, TransitionError> {
        self.within(scope.package, |jobs| jobs.resume(scope, version, now))
    }

    /// Brings a `retired` shared job back as a draft when its release allows it, as
    /// [`PackageJobsState::reactivate`] does.
    ///
    /// # Errors
    ///
    /// Returns the refusal [`PackageJobsState::reactivate`] gives.
    pub fn reactivate(&mut self, scope: JobScope<'_>, now: OffsetDateTime) -> Result<Applied, TransitionError> {
        self.within(scope.package, |jobs| jobs.reactivate(scope, now))
    }

    /// Marks an installed release older than `current` as superseded, as [`PackageJobsState::note_installed_release`] does.
    pub fn note_installed_release(
        &mut self,
        package: &str,
        workflow: &str,
        release: &Version,
        now: OffsetDateTime,
    ) -> bool {
        self.within(package, |jobs| {
            jobs.note_installed_release(package, workflow, release, now)
        })
    }

    /// Records the `future` release of `workflow` in `package` from the releases of that workflow installed now, as
    /// [`PackageReleases::record_future`] does, and returns whether the recorded value changed. An install and a removal
    /// call it in the same state change as the store's, so a reader never finds `future` out of step.
    pub fn record_future(&mut self, package: &str, workflow: &str, installed: &[Version]) -> bool {
        self.within(package, |jobs| {
            let held = jobs.packages.entry(package.to_owned()).or_default();
            let changed = held.record_future(workflow, installed);
            if *held == PackageReleases::default() {
                jobs.packages.remove(package);
            }
            changed
        })
    }

    /// Fixes a workflow release's library lock when it allows, as [`PackageJobsState::fix_release_lock`] does.
    ///
    /// # Errors
    ///
    /// Returns the refusal [`PackageJobsState::fix_release_lock`] gives.
    pub fn fix_release_lock(
        &mut self,
        package: &str,
        workflow: &str,
        release: &Version,
        lock: LibraryLock,
    ) -> Result<bool, TransitionError> {
        self.within(package, |jobs| jobs.fix_release_lock(package, workflow, release, lock))
    }

    /// Checks the link a shared successor would have to `predecessor`, as [`PackageJobsState::check_link`] does.
    ///
    /// # Errors
    ///
    /// Returns the refusal [`PackageJobsState::check_link`] gives.
    pub fn check_link(
        &self,
        package: &str,
        workflow: &str,
        job: &str,
        predecessor: &PredecessorKey,
    ) -> Result<JobLink, LinkRefusal> {
        self.packages.get(package).map_or_else(
            || {
                Err(LinkRefusal::PredecessorMissing {
                    predecessor: predecessor.as_str().to_owned(),
                })
            },
            |jobs| jobs.check_link(workflow, job, predecessor),
        )
    }

    /// Records the link a shared successor has to `predecessor` at its first passing validation, as
    /// [`PackageJobsState::record_link`] does.
    ///
    /// # Errors
    ///
    /// Returns the refusal [`PackageJobsState::record_link`] gives, and records nothing.
    pub fn record_link(
        &mut self,
        package: &str,
        workflow: &str,
        job: &str,
        predecessor: &PredecessorKey,
    ) -> Result<JobLink, LinkRefusal> {
        self.within(package, |jobs| jobs.record_link(workflow, job, predecessor).cloned())
    }

    /// The job that keeps `job` from running live by hand, as [`PackageJobsState::chain_hold_on_live_run`] gives it.
    #[must_use]
    pub fn chain_hold_on_live_run(&self, package: &str, workflow: &str, job: &str) -> Option<ChainHold> {
        self.packages.get(package)?.chain_hold_on_live_run(workflow, job)
    }

    /// The key of the job that succeeds `job` and has not been retired, when there is one.
    #[must_use]
    pub fn standing_successor_of(&self, package: &str, workflow: &str, job: &str) -> Option<&str> {
        self.packages.get(package)?.standing_successor_of(workflow, job)
    }

    /// Runs `change` on the package's own state, which is created for it when it has none and dropped again when
    /// nothing is left in it, so a refused change leaves no entry behind.
    fn within<R>(&mut self, package: &str, change: impl FnOnce(&mut PackageJobsState) -> R) -> R {
        let mut jobs = self.packages.remove(package).unwrap_or_default();
        let result = change(&mut jobs);
        if !jobs.workflows.is_empty() || !jobs.packages.is_empty() {
            self.packages.insert(package.to_owned(), jobs);
        }
        result
    }
}

impl ReleaseKinds for JobsState {
    fn release_kind(&self, package: &str, workflow: &str, release: &Version) -> ReleaseKind {
        self.package_releases(package)
            .map_or(ReleaseKind::Future, |releases| releases.kind_of(workflow, release))
    }
}

#[cfg(test)]
#[path = "host_state_test.rs"]
mod tests;
