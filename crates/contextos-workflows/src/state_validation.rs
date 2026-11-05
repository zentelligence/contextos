//! Static validation results as `.contextos/jobs.toml` stores them, and the rules that keep one for a job: what a
//! result was made for, whether a job's recorded facts still apply to its version, and what keeping a result does to the
//! job's state.
//!
//! A child of [`crate::state`], so it reaches a job's stored fields directly, and every state change it makes still goes
//! through [`PackageJobsState::transition`] and the transition table.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::{JobState, PackageJobsState};
use crate::fingerprint::JobFingerprint;
use crate::library_lock::{LibraryLock, VersionBasis};
use crate::lifecycle::{Actor, Applied, JobLifecycle, Suspension, SuspensionCause, SuspensionReason, Transition};
use crate::release_state::{RandomIdentityIssuer, ReleaseKind, ReleaseKinds as _};
use crate::validation::{RULES_REVISION, ValidationCode, ValidationReport};

impl PackageJobsState {
    /// Keeps what static validation found for one version of a job. It records a fact and changes no state, so a failed
    /// validation is stored and shown without moving the job, and a pass is followed by the `Validated` transition.
    pub fn record_validation(&mut self, workflow_slug: &str, job_slug: &str, validation: StoredValidation) {
        self.job_entry(workflow_slug, job_slug).validation = Some(validation);
    }

    /// Whether a job whose facts were recorded for one version has since become another: a validated or registered job
    /// whose recorded version is not `current`. A draft has no recorded version, and an enabled or suspended job's new
    /// version is handled by probation, so neither is drift.
    ///
    /// A `current` of `None` means the job's files can no longer be read, which is a change of version too.
    #[must_use]
    pub fn version_drift(&self, workflow_slug: &str, job_slug: &str, current: Option<&JobFingerprint>) -> bool {
        self.job(workflow_slug, job_slug).is_some_and(|job| {
            matches!(job.lifecycle, JobLifecycle::Validated | JobLifecycle::Registered)
                && job.fingerprint.as_ref() != current
        })
    }

    /// Keeps `validation` for a job and applies what it decides about the job's state.
    ///
    /// A `validated` or `registered` job whose recorded version is not the one `validation` was made for has changed
    /// version, so the system first returns it to `draft`, as the validation pass would: whatever was validated or
    /// dry-run was another version. A `draft` whose result passed for a version then moves to `validated` for it.
    ///
    /// A result for the version the job holds that failed for a reason the job owns, a failure rather than a check
    /// that could not be made, withdraws what that version earned: a `validated` or `registered` job goes back to
    /// `draft`, and an `enabled` one is suspended with the failures as the reason. A check that could not be made, such
    /// as a server that could not be reached, is no fault of the job and changes nothing. An `enabled` job's new version
    /// is put on trial or suspended by the validation pass, and a `suspended` job is checked again by a resume, so
    /// neither is changed here otherwise.
    pub fn keep_validation(
        &mut self,
        workflow_slug: &str,
        job_slug: &str,
        validation: StoredValidation,
        now: OffsetDateTime,
    ) -> ValidationKept {
        let held = self
            .job(workflow_slug, job_slug)
            .and_then(JobState::fingerprint)
            .is_some_and(|recorded| Some(recorded) == validation.fingerprint.as_ref());
        let (validation, edited_record) = self.with_release_rules_applied(workflow_slug, job_slug, validation);
        let version = validation.fingerprint.clone();
        let passed = validation.passed;
        let basis = validation.made_from.clone().filter(|_| passed);
        let validation_basis = basis.clone();
        if let Some(basis) = &basis
            && self
                .fixed_lock(workflow_slug, basis)
                .is_some_and(|fixed| *fixed != basis.lock)
        {
            return ValidationKept {
                lock: LockOutcome::Conflict,
                ..ValidationKept::default()
            };
        }
        // A failure the job owns withdraws what its version earned. A job file edited away from what an enabled job's
        // record holds is one whatever its version, since it is not a new version of the same job.
        let owned_failure =
            ((held && !validation.failures.is_empty()) || edited_record).then(|| SuspensionReason::from(&validation));
        let mut kept = ValidationKept {
            returned_to_draft: self.version_drift(workflow_slug, job_slug, version.as_ref())
                && self
                    .transition(workflow_slug, job_slug, Actor::System, &Transition::VersionChanged, now)
                    .is_ok(),
            ..ValidationKept::default()
        };
        self.record_validation(workflow_slug, job_slug, validation);
        if let Some(reason) = owned_failure {
            match self.lifecycle_of(workflow_slug, job_slug) {
                JobLifecycle::Validated | JobLifecycle::Registered => {
                    kept.returned_to_draft = self
                        .transition(
                            workflow_slug,
                            job_slug,
                            Actor::System,
                            &Transition::ValidationFailed,
                            now,
                        )
                        .is_ok();
                }
                JobLifecycle::Enabled => {
                    let suspension = Suspension {
                        cause: SuspensionCause::ValidationFailed,
                        reason,
                    };
                    kept.suspended = self
                        .transition(
                            workflow_slug,
                            job_slug,
                            Actor::System,
                            &Transition::Suspend(suspension),
                            now,
                        )
                        .is_ok();
                }
                JobLifecycle::Draft | JobLifecycle::Suspended | JobLifecycle::Retired => {}
            }
        }
        if let Some(basis) = basis {
            kept.lock = self.fix_lock(workflow_slug, &basis);
        }
        kept.validated = match version {
            Some(version) if passed => self
                .transition(
                    workflow_slug,
                    job_slug,
                    Actor::Validator,
                    &Transition::Validated(version),
                    now,
                )
                .is_ok_and(|applied| applied == Applied::To(JobLifecycle::Validated)),
            _ => false,
        };
        if kept.validated {
            // A random identity colliding with one already held is refused and the job keeps none for now: the next
            // validation issues another, since nothing about the job depends on the identity before it is linked to.
            let _ = self.ensure_identity(workflow_slug, job_slug, &RandomIdentityIssuer);
            if let Some(basis) = validation_basis {
                if let Some(predecessor) = &basis.predecessor {
                    // The link was checked against this state before anything was kept, so recording it cannot refuse.
                    let _ = self.record_link(workflow_slug, job_slug, predecessor);
                }
                let job = self.job_entry(workflow_slug, job_slug);
                job.release = Some(basis.release);
                job.lock = Some(basis.lock);
            }
        }
        kept
    }

    /// `validation` with the rules about releases and the link applied to the job file it was made for, as every
    /// validation of a job applies them, for a result that passed or failed on its own checks. See
    /// [`Self::release_rule_findings`]. A rule that does not hold fails the result on `version` or `predecessor` and
    /// keeps it from passing, so no lock or link is recorded from it. A result carrying no release (the job's files could
    /// not be read) is returned as it was. The flag says the job's file was edited away from its record.
    #[must_use]
    pub fn with_release_rules_applied(
        &self,
        workflow_slug: &str,
        job_slug: &str,
        mut validation: StoredValidation,
    ) -> (StoredValidation, bool) {
        let Some(basis) = validation.made_from.clone() else {
            return (validation, false);
        };
        let findings = self.release_rule_findings(workflow_slug, job_slug, validation.fingerprint.as_ref(), &basis);
        if !findings.failures.is_empty() {
            validation.passed = false;
            validation.made_from = None;
            validation
                .failures
                .extend(findings.failures.into_iter().map(|found| StoredFailure {
                    code: ValidationCode::ArtefactInvalid.as_str().to_owned(),
                    artefact: found.artefact.to_owned(),
                    message: found.message,
                }));
        }
        (validation, findings.edited_record)
    }

    /// What the rules about releases and the link say about a job file that names `basis`'s release and predecessor and
    /// has `version`:
    ///
    /// - the link the job file names to a predecessor holds ([`Self::check_link`]);
    /// - a `draft`, or a `validated` or `registered` job on a version it has not been validated for, does not name a
    ///   release that is `previous` or retired, except a return successor naming `previous`; and
    /// - an `enabled`, `suspended`, or `retired` job's file names the release and the predecessor key its state records,
    ///   since a job changes release only through `jobs successor`. For a suspended job the failure blocks its resume, and
    ///   for a retired one its re-activation; neither changes the stage.
    #[must_use]
    pub fn release_rule_findings(
        &self,
        workflow_slug: &str,
        job_slug: &str,
        version: Option<&JobFingerprint>,
        basis: &VersionBasis,
    ) -> ReleaseRuleFindings {
        let mut findings = ReleaseRuleFindings::default();
        let mut fail = |artefact: &'static str, message: String| {
            findings.failures.push(ReleaseRuleFailure { artefact, message });
        };
        let mut predecessor_failed = false;
        if let Some(predecessor) = &basis.predecessor
            && let Err(refusal) = self.check_link(workflow_slug, job_slug, predecessor)
        {
            predecessor_failed = true;
            fail("predecessor", refusal.to_string());
        }
        let entry = self.job(workflow_slug, job_slug);
        let lifecycle = entry.map_or_else(JobLifecycle::default, |job| job.lifecycle);
        let new_version = entry
            .and_then(JobState::fingerprint)
            .is_none_or(|recorded| Some(recorded) != version);
        let closed_to_new_jobs = match lifecycle {
            JobLifecycle::Draft => true,
            JobLifecycle::Validated | JobLifecycle::Registered => new_version,
            JobLifecycle::Enabled | JobLifecycle::Suspended | JobLifecycle::Retired => false,
        };
        if closed_to_new_jobs && let Some(message) = self.closed_release(workflow_slug, basis) {
            fail("version", message);
        }
        let mut edited_record = false;
        if matches!(
            lifecycle,
            JobLifecycle::Enabled | JobLifecycle::Suspended | JobLifecycle::Retired
        ) && let Some(job) = entry
        {
            if let Some(message) = self.release_edit(workflow_slug, job_slug, &basis.release) {
                edited_record = true;
                fail("version", message);
            }
            let recorded_key = job.link.as_ref().and_then(|link| link.predecessor.as_deref());
            let predecessor_edited = match (&job.link, &basis.predecessor) {
                (None, None) => false,
                (Some(_), Some(named)) => recorded_key.is_some_and(|key| key != named.as_str()),
                (None, Some(_)) | (Some(_), None) => true,
            };
            if predecessor_edited {
                edited_record = true;
                // A predecessor the link check has already refused is reported once, in its own words.
                if !predecessor_failed {
                    fail(
                        "predecessor",
                        "the job file's predecessor differs from the one the job was enabled with; a job changes its predecessor through `jobs successor`"
                            .to_owned(),
                    );
                }
            }
        }
        findings.edited_record = edited_record;
        findings
    }

    /// What an `enabled`, `suspended`, or `retired` job's file naming `named` says against its record: the job was
    /// enabled on another release, and a job changes release only through `jobs successor`. `None` when the job is in
    /// none of those stages, has no release recorded, or `named` is the recorded release. It needs no installed release,
    /// so it holds for a file naming one that is not there.
    #[must_use]
    pub fn release_edit(&self, workflow_slug: &str, job_slug: &str, named: &semver::Version) -> Option<String> {
        let job = self.job(workflow_slug, job_slug)?;
        if !matches!(
            job.lifecycle,
            JobLifecycle::Enabled | JobLifecycle::Suspended | JobLifecycle::Retired
        ) {
            return None;
        }
        let recorded = job.release.as_ref().filter(|recorded| *recorded != named)?;
        Some(format!(
            "the job was enabled on release {recorded} and its file now names {named}; a job changes release through `jobs successor`"
        ))
    }

    /// Why a job may not name the release `basis` holds, when that release is `previous` or retired and the job is not a
    /// return successor naming `previous`: a successor whose predecessor is on `current`.
    fn closed_release(&self, workflow_slug: &str, basis: &VersionBasis) -> Option<String> {
        let kind = self.release_kind(&basis.package, workflow_slug, &basis.release);
        let return_successor = || {
            let current = self
                .package_releases(&basis.package)
                .and_then(|releases| releases.workflows.get(workflow_slug))
                .and_then(|versions| versions.current.as_ref());
            // A predecessor with no release recorded cannot be told from one on `current`; enabling checks it again
            // against the release it is recorded on, so it is not turned away here.
            basis.predecessor.as_ref().is_some_and(|predecessor| {
                self.job(workflow_slug, predecessor.as_str())
                    .is_some_and(|job| job.release.as_ref().is_none_or(|release| Some(release) == current))
            })
        };
        match kind {
            ReleaseKind::Current | ReleaseKind::Future => None,
            ReleaseKind::Previous if return_successor() => None,
            ReleaseKind::Previous | ReleaseKind::Retired => Some(format!(
                "release {} is a {kind} release, which a job may not newly name; name the current or a future release with `jobs replace`",
                basis.release
            )),
        }
    }

    /// The library lock fixed for `workflow_slug` in `release` of `package`, once a job on it has passed validation. A
    /// validation, version, dry run, or run reads it and resolves nothing.
    #[must_use]
    pub fn library_lock(&self, package: &str, workflow_slug: &str, release: &semver::Version) -> Option<&LibraryLock> {
        self.package_releases(package)?
            .workflow_release(release, workflow_slug)?
            .lock
            .as_ref()
    }

    /// The library lock already fixed for the workflow release `basis` names, when one is.
    fn fixed_lock(&self, workflow_slug: &str, basis: &VersionBasis) -> Option<&LibraryLock> {
        self.library_lock(&basis.package, workflow_slug, &basis.release)
    }

    /// Fixes the library lock of the workflow release `basis` names, unless one is already fixed: the first passing
    /// validation on a release fixes it, and a later one never changes it.
    fn fix_lock(&mut self, workflow_slug: &str, basis: &VersionBasis) -> LockOutcome {
        let release = self
            .packages
            .entry(basis.package.clone())
            .or_default()
            .releases
            .entry(basis.release.clone())
            .or_default()
            .workflows
            .entry(workflow_slug.to_owned())
            .or_default();
        if release.lock.is_none() {
            release.lock = Some(basis.lock.clone());
            LockOutcome::Fixed
        } else {
            LockOutcome::Agreed
        }
    }

    /// Whether a `draft` job that says it is `ready` is due static validation: its `current` version has not already been
    /// validated and failed, because a failed result stands until the version changes. A `current` of `None` means the
    /// job's files cannot be read, which is its own version for this purpose, so such a failure is recorded once.
    #[must_use]
    pub fn validation_due(
        &self,
        workflow_slug: &str,
        job_slug: &str,
        ready: bool,
        current: Option<&JobFingerprint>,
    ) -> bool {
        self.lifecycle_of(workflow_slug, job_slug) == JobLifecycle::Draft
            && ready
            && !self
                .job(workflow_slug, job_slug)
                .and_then(JobState::validation)
                .is_some_and(|stored| {
                    stored.fingerprint.as_ref() == current && !stored.passed && stored.rules >= RULES_REVISION
                })
    }

    /// Whether a job's stored validation was made under an earlier revision of the validation rules than the current
    /// one, so it is due again whether or not its version changed. A job with no stored validation is not.
    #[must_use]
    pub fn rules_outdated(&self, workflow_slug: &str, job_slug: &str) -> bool {
        self.job(workflow_slug, job_slug)
            .and_then(JobState::validation)
            .is_some_and(|stored| stored.rules < RULES_REVISION)
    }

    /// Whether an `enabled` job's version is not the one recorded for it, so its new version is due the validation that
    /// puts it on probation. A suspended job is not: it stays suspended until the operator resumes it, and a validated
    /// or registered job's new version is [`PackageJobsState::version_drift`].
    ///
    /// A `current` of `None` means the job's files can no longer be read, which is a change of version too.
    #[must_use]
    pub fn new_version_under_enabled_job(
        &self,
        workflow_slug: &str,
        job_slug: &str,
        current: Option<&JobFingerprint>,
    ) -> bool {
        self.job(workflow_slug, job_slug)
            .is_some_and(|job| job.lifecycle == JobLifecycle::Enabled && job.fingerprint.as_ref() != current)
    }
}

fn validator() -> Actor {
    Actor::Validator
}

/// One rule about releases or the link that a job file breaks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseRuleFailure {
    /// The job file key it concerns: `version` or `predecessor`.
    pub artefact: &'static str,
    pub message: String,
}

/// What the rules about releases and the link found in a job file. See [`PackageJobsState::release_rule_findings`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReleaseRuleFindings {
    /// Each rule the file breaks, to be reported as `validation/artefact-invalid` on its key.
    pub failures: Vec<ReleaseRuleFailure>,
    /// The job's file names another release or predecessor than its state records. An `enabled` job is then suspended at
    /// once, whatever its version, since it is another job and not a new version of this one; a `suspended` or `retired`
    /// job stays as it is and cannot be resumed or re-activated until the file is restored.
    pub edited_record: bool,
}

/// What static validation found for one version of a job, as it is stored.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StoredValidation {
    /// Who asked for it: the validation pass (the validator), or the operator by hand. Absent in an older file, which
    /// reads as the validator's.
    #[serde(default = "validator")]
    pub by: Actor,
    /// The revision of the validation rules it was made under. Absent in an older file, which reads as revision zero.
    #[serde(default)]
    pub rules: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// The version the result is for. Absent when the job's files could not all be read, so it had none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<JobFingerprint>,
    pub passed: bool,
    pub failures: Vec<StoredFailure>,
    /// The checks that could not be made.
    pub unverifiable: Vec<String>,
    /// What the version the result is for was made from: the release the job names and the library lock it ran with.
    /// Carried from the validation to the state change that keeps the result, so a first passing validation's lock is
    /// recorded in the same write that records the job `validated`. Never written to, or read from, the state file.
    #[serde(skip)]
    pub made_from: Option<VersionBasis>,
}

/// One validation failure, as it is stored.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StoredFailure {
    pub code: String,
    pub artefact: String,
    pub message: String,
}

/// A validation report with the version and time it was made for, ready to be stored.
pub struct ValidationRecord<'a> {
    pub report: &'a ValidationReport,
    pub fingerprint: Option<JobFingerprint>,
    pub at: OffsetDateTime,
}

impl From<ValidationRecord<'_>> for StoredValidation {
    fn from(record: ValidationRecord<'_>) -> Self {
        Self {
            by: Actor::Validator,
            rules: RULES_REVISION,
            at: record.at,
            fingerprint: record.fingerprint,
            passed: record.report.passed(),
            failures: record
                .report
                .failures
                .iter()
                .map(|failure| StoredFailure {
                    code: failure.code.as_str().to_owned(),
                    artefact: failure.artefact.clone(),
                    message: failure.message.clone(),
                })
                .collect(),
            unverifiable: record
                .report
                .unverifiable
                .iter()
                .map(|check| format!("{}: {}", check.check, check.reason))
                .collect(),
            made_from: None,
        }
    }
}

impl StoredValidation {
    /// This result as asked for by `by`, rather than by the validation pass.
    #[must_use]
    pub const fn made_by(mut self, by: Actor) -> Self {
        self.by = by;
        self
    }
}

/// What keeping a validation result did to the job's state. See [`PackageJobsState::keep_validation`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ValidationKept {
    /// The job was `validated` or `registered` and went back to `draft`: the result was for another version than the one
    /// it holds, or it failed for a reason the job owns.
    pub returned_to_draft: bool,
    /// The result passed, so the job, a `draft` by then, moved to `validated` for the version it was made for.
    pub validated: bool,
    /// The job was `enabled`, and the result for the version it holds failed for a reason the job owns, so the system
    /// suspended it.
    pub suspended: bool,
    /// What keeping the result did about the workflow release's library lock.
    pub lock: LockOutcome,
}

/// What keeping a validation result did about the library lock of the workflow release it was made for.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LockOutcome {
    /// The result carried no lock to keep, or did not pass, so no lock was involved.
    #[default]
    Untouched,
    /// The result passed first on its workflow release, so its lock is now the release's.
    Fixed,
    /// The result passed against the lock already fixed for its workflow release.
    Agreed,
    /// The result passed against other library releases than the lock already fixed, so it was made for the wrong
    /// libraries: nothing was kept, and the validation is to be repeated against the recorded lock.
    Conflict,
}
