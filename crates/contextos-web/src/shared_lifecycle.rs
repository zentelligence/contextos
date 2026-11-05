//! The lifecycle of shared jobs: the one operation that changes a job's stored state, the pass that
//! validates new and changed jobs, and the settling of a first live run's trial.
//!
//! The rules are those of a vault job, from `contextos-workflows`, applied to the host state file instead of a vault's.
//! Every change is made under the state file's lock and checked against the state as it is then, so the scheduler and a
//! command-line process can both change it and neither loses the other's change. A pass looks at the jobs with the
//! state it read, runs validation outside the lock, and then applies what it found to the state as it is at that
//! moment, so a change another process made while validation ran is never overwritten.

use std::path::Path;

use contextos_fs::FsError;
use contextos_fs::host_file::{FileRemoveObserver, FileRemoveStep, remove_file_observed};
use contextos_fs::is_link_or_reparse_point;
use contextos_workflows::{
    Actor, Applied, Changed, FirstRunVerdict, JobFingerprint, JobLifecycle, JobScope, JobState, JobsState, LockOutcome,
    NewVersionVerdict, ReleaseContext, RunOutcome, StoreKind, StoredValidation, Suspension, SuspensionCause,
    Transition, TransitionError,
};
use thiserror::Error;
use time::OffsetDateTime;

use crate::job_lifecycle::NotRegistered;
use crate::job_probation::Settlement;
use crate::job_validation::{DryRunRecorded, JobValidation, PassReport, names_ready};
use crate::package_retention::RetentionError;
use crate::saved_jobs::SavedJobs;
use crate::shared_claim::SharedJobKey;
use crate::shared_jobs::{
    SharedJobFile, UnusableReason, find_job_files, list_shared_job_entries, list_shared_job_files,
    without_clashing_keys,
};
use crate::shared_state::{SharedStateError, SharedStateStore};
use crate::shared_validation::{SharedValidationHost, changed_on_host, validate_shared_job_against};
use crate::shared_version::{SharedVersion, recorded_lock, shared_job_state_blocking};
use crate::store_retention::{RetentionWarning, retain_host};

/// One requested change to one shared job's state.
pub struct SharedJobChange<'a> {
    pub key: &'a SharedJobKey,
    /// Who is making the change, which decides whether the transition is theirs to make.
    pub actor: Actor,
    pub transition: Transition,
    pub now: OffsetDateTime,
}

#[derive(Debug, Error)]
pub enum SharedLifecycleError {
    #[error("{code}: {source}", code = source.code())]
    Refused {
        #[from]
        source: TransitionError,
    },
    /// The state file could not be read or written, or is malformed. Its own text says which.
    #[error("the job state could not be read or written: {0}")]
    Unavailable(#[from] SharedStateError),
    #[error("the state change did not complete: {0}")]
    Interrupted(#[from] tokio::task::JoinError),
    /// Two job files share the job's key, so removing "the" file would pick one of them arbitrarily.
    #[error("workflow/job-key-taken: the job is the key of {files}, so neither was removed")]
    KeyTaken { files: String },
    /// The job's file could not be removed, so its state was left as it was.
    #[error("the job file could not be removed ({kind}), so the state was left as it was")]
    FileNotRemoved { kind: std::io::ErrorKind },
    /// The job's file is there but cannot be used, so it was left alone and the state was left as it was.
    #[error(
        "the job file is {reason}, so it was not removed and the state was left as it was; deal with the file by hand"
    )]
    FileUnusable { reason: UnusableReason },
    /// A link, a reparse point, or a non-folder stands where a job folder or the job file belongs, so nothing was
    /// removed through it and the state was left as it was.
    #[error(
        "a link or the wrong kind of entry stands where a job folder or file belongs, so nothing was removed and the state was left as it was"
    )]
    LinkInPlace,
}

impl SharedLifecycleError {
    /// The stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Refused { source } => source.code(),
            Self::Unavailable(_) | Self::Interrupted(_) => "workflow/state-unavailable",
            Self::FileNotRemoved { .. } | Self::FileUnusable { .. } | Self::LinkInPlace => {
                "workflow/job-store-unavailable"
            }
            Self::KeyTaken { .. } => "workflow/job-key-taken",
        }
    }
}

/// Applies `change` to the job's entry in the host state file, under its lock.
///
/// # Errors
///
/// Returns [`SharedLifecycleError::Refused`] when the actor does not own the transition or the job is not in a state it
/// applies to, with nothing written, and [`SharedLifecycleError::Unavailable`] when the file cannot be read, is
/// malformed, or cannot be written.
pub async fn change_shared_lifecycle(
    state: &SharedStateStore,
    change: SharedJobChange<'_>,
) -> Result<Applied, SharedLifecycleError> {
    change_shared_lifecycle_in_release(state, change, None).await
}

/// Applies `change` as [`change_shared_lifecycle`] does, and with a `release` context applies an operator's enabling,
/// resuming, or re-activating by whether the job's release is current, previous, or future, the job, its predecessor, and the package's `current` and `previous`
/// changing together in the one write under the state lock. A job whose state records a release is refused these
/// changes without a context.
///
/// # Errors
///
/// As [`change_shared_lifecycle`].
pub async fn change_shared_lifecycle_in_release(
    state: &SharedStateStore,
    change: SharedJobChange<'_>,
    release: Option<&ReleaseContext>,
) -> Result<Applied, SharedLifecycleError> {
    change_shared_lifecycle_reporting(state, change, release)
        .await
        .map(|changed| changed.applied)
}

/// Applies `change` as [`change_shared_lifecycle_in_release`] does, and says what an enabling did beyond the job: the
/// predecessor it retired and how `current` and `previous` moved.
///
/// # Errors
///
/// As [`change_shared_lifecycle`].
pub async fn change_shared_lifecycle_reporting(
    state: &SharedStateStore,
    change: SharedJobChange<'_>,
    release: Option<&ReleaseContext>,
) -> Result<Changed, SharedLifecycleError> {
    let release = release.cloned();
    let store = state.clone();
    let key = change.key.clone();
    let SharedJobChange {
        actor, transition, now, ..
    } = change;
    let applied = tokio::task::spawn_blocking(move || {
        store.update(|jobs| {
            jobs.change_reporting(
                JobScope {
                    package: &key.package,
                    workflow: &key.workflow,
                    job: &key.job,
                },
                actor,
                &transition,
                release.as_ref(),
                now,
            )
        })
    })
    .await??;
    Ok(applied?)
}

/// Deletes a retired shared job: its file, then its state, both inside the state file's lock.
///
/// The transition is checked against the state as it is under the lock, and only when it is allowed is the job's file,
/// found by the key it is tracked under, removed. The file goes before the state is written, so a failed state write
/// leaves a retired job with no file, which a delete handles, and never a file that returns as a draft. A job whose
/// file is already gone has its state removed alone.
///
/// # Errors
///
/// Returns [`SharedLifecycleError::Refused`] when the job is not retired, with nothing removed,
/// [`SharedLifecycleError::KeyTaken`] when two files share the job's key, and [`SharedLifecycleError::FileNotRemoved`]
/// or [`SharedLifecycleError::FileUnusable`] when the file cannot be removed or is one that cannot be used, and
/// [`SharedLifecycleError::Unavailable`] when the state cannot be made. Whenever it fails, the state is as it was.
pub async fn delete_shared_job(
    state: &SharedStateStore,
    jobs_root: &Path,
    key: &SharedJobKey,
    now: OffsetDateTime,
) -> Result<Applied, SharedLifecycleError> {
    let store = state.clone();
    let jobs_root = jobs_root.to_path_buf();
    let key = key.clone();
    let named = key.clone();
    let applied = tokio::task::spawn_blocking(move || {
        store.update(|jobs| {
            let before = jobs.clone();
            let applied = jobs.transition(
                &key.package,
                &key.workflow,
                &key.job,
                Actor::Operator,
                &Transition::Delete,
                now,
            )?;
            if let Err(error) = remove_job_file(&jobs_root, &key, &NoRemoveSteps) {
                *jobs = before;
                return Err(error);
            }
            Ok(applied)
        })
    })
    .await???;
    tracing::info!(package = %named.package, workflow = %named.workflow, job = %named.job, "shared job deleted");
    Ok(applied)
}

/// A removal nobody is watching.
struct NoRemoveSteps;

impl FileRemoveObserver for NoRemoveSteps {
    fn reached(&self, _step: FileRemoveStep) {}
}

/// Removes the one job file `key` is tracked under, when there is one, reporting each step to `observer`.
///
/// A job file that is there under the job's own name and cannot be used (too large, not text, unreadable) is not
/// removed: it may be the job's, and nothing has said so, so the operator is told to deal with it by hand.
pub(crate) fn remove_job_file(
    jobs_root: &Path,
    key: &SharedJobKey,
    observer: &dyn FileRemoveObserver,
) -> Result<(), SharedLifecycleError> {
    remove_job_file_after_listing(jobs_root, key, observer, &|| {})
}

/// [`remove_job_file`], calling `after_listing` once the folder has been listed and before anything is removed, which
/// is the moment another process could swap a folder or the file for a link.
pub(crate) fn remove_job_file_after_listing(
    jobs_root: &Path,
    key: &SharedJobKey,
    observer: &dyn FileRemoveObserver,
    after_listing: &dyn Fn(),
) -> Result<(), SharedLifecycleError> {
    let entries = list_shared_job_entries(jobs_root)
        .map_err(|error| SharedLifecycleError::FileNotRemoved { kind: error.kind() })?;
    let found = find_job_files(entries, key);
    after_listing();
    match found.tracked.as_slice() {
        [file] if found.unusable.is_none() => {
            let package = jobs_root.join(&file.key.package);
            let workflow = package.join(&file.key.workflow);
            let jobs = crate::shared_jobs::job_folder(jobs_root, &file.key.package, &file.key.workflow);
            let path = crate::shared_jobs::job_file_path(jobs_root, &file.key, &file.key.job);
            // Revalidated at the point of removal: another process may have swapped any of the three for a link since
            // the listing, and removing through one would delete a file outside the jobs folder.
            require_plain(&package, true)?;
            require_plain(&workflow, true)?;
            require_plain(&jobs, true)?;
            require_plain(&path, false)?;
            // The folder is flushed with the removal, so the state written after it never outlives it.
            remove_file_observed(&path, observer)
                .map(|_| ())
                .map_err(|error| SharedLifecycleError::FileNotRemoved {
                    kind: match error {
                        FsError::DeletePath { source, .. } => source.kind(),
                        _ => std::io::ErrorKind::Other,
                    },
                })
        }
        // A usable file tracked under the key and an unusable file named by it both claim the key.
        [_, ..] if found.unusable.is_some() => Err(SharedLifecycleError::KeyTaken {
            files: found
                .tracked
                .iter()
                .map(|file| format!("{}.toml", file.key.job))
                .chain(std::iter::once(format!("{}.toml", key.job)))
                .collect::<Vec<_>>()
                .join(" and "),
        }),
        [] => found
            .unusable
            .map_or(Ok(()), |reason| Err(SharedLifecycleError::FileUnusable { reason })),
        several => Err(SharedLifecycleError::KeyTaken {
            files: several
                .iter()
                .map(|file| format!("{}.toml", file.key.job))
                .collect::<Vec<_>>()
                .join(" and "),
        }),
    }
}

/// Refuses a link or reparse point at `path`, and an entry that is not a folder (`folder`) or a file, as it is now.
fn require_plain(path: &Path, folder: bool) -> Result<(), SharedLifecycleError> {
    let found =
        std::fs::symlink_metadata(path).map_err(|error| SharedLifecycleError::FileNotRemoved { kind: error.kind() })?;
    let right_kind = if folder { found.is_dir() } else { found.is_file() };
    if right_kind && !is_link_or_reparse_point(&found) {
        Ok(())
    } else {
        Err(SharedLifecycleError::LinkInPlace)
    }
}

/// What a pass needs: where the job files are, where the state is, and what validation needs.
pub struct SharedPassHost<'a> {
    pub jobs_root: &'a Path,
    pub state: &'a SharedStateStore,
    pub validation: SharedValidationHost<'a>,
}

/// One change a pass decided on. The pass applies each to the state it read, so later decisions see earlier ones, and
/// then applies them again to the state as it is when the lock is taken, where each is checked afresh.
enum Step {
    /// A validated or registered job whose version changed goes back to draft.
    ReturnToDraft {
        key: SharedJobKey,
        current: Option<JobFingerprint>,
    },
    /// A draft's validation result is kept, and a pass moves it to `validated`.
    Validated {
        key: SharedJobKey,
        validation: Box<Checked>,
    },
    /// An enabled job's new version was validated, and the verdict is applied.
    Reviewed {
        key: SharedJobKey,
        validation: Box<Checked>,
        verdict: NewVersionVerdict,
        /// Whether the version failed because the package the job runs is no longer there.
        package_unavailable: bool,
    },
    /// A job whose result predates the validation rules was validated again: the result is kept, and a failure the job
    /// owns returns a validated or registered job to draft and suspends an enabled one.
    Revalidated {
        key: SharedJobKey,
        validation: Box<Checked>,
    },
}

/// A validation result with the time it was made, ready to be stored.
struct Checked {
    stored: StoredValidation,
    passed: bool,
}

impl Checked {
    fn new(validation: &JobValidation, at: OffsetDateTime) -> Self {
        Self {
            // The result carries what the version was made from, so a first passing validation on a workflow release
            // fixes its library lock in the same write that validates the job.
            stored: validation.stored(at),
            passed: validation.report.passed(),
        }
    }
}

impl Step {
    /// Applies the step to `jobs`, and counts what happened. Returns whether the result was made for other library
    /// releases than the lock another job's validation recorded first, so nothing was kept and it is to be repeated.
    fn apply(&self, jobs: &mut JobsState, now: OffsetDateTime, report: &mut PassReport) -> bool {
        match self {
            Self::ReturnToDraft { key, current } => {
                if jobs.version_drift(&key.package, &key.workflow, &key.job, current.as_ref())
                    && jobs
                        .transition(
                            &key.package,
                            &key.workflow,
                            &key.job,
                            Actor::System,
                            &Transition::VersionChanged,
                            now,
                        )
                        .is_ok()
                {
                    report.returned_to_draft += 1;
                }
                false
            }
            Self::Validated { key, validation } => {
                // Kept by the one rule every validation result is kept by, applied to the state as it is under the
                // lock, which can have moved on since the pass read it.
                let kept = jobs.keep_validation(&key.package, &key.workflow, &key.job, validation.stored.clone(), now);
                if kept.returned_to_draft {
                    report.returned_to_draft += 1;
                }
                if kept.validated {
                    report.validated += 1;
                } else if !validation.passed {
                    report.failed += 1;
                }
                kept.lock == LockOutcome::Conflict
            }
            Self::Revalidated { key, validation } => {
                let kept = jobs.keep_validation(&key.package, &key.workflow, &key.job, validation.stored.clone(), now);
                if kept.returned_to_draft {
                    report.returned_to_draft += 1;
                }
                if kept.suspended {
                    report.suspended += 1;
                }
                kept.lock == LockOutcome::Conflict
            }
            Self::Reviewed {
                key,
                validation,
                verdict,
                package_unavailable,
            } => {
                jobs.record_validation(&key.package, &key.workflow, &key.job, validation.stored.clone());
                let (actor, transition, on_probation) = match verdict {
                    NewVersionVerdict::OnProbation(version) => {
                        (Actor::Validator, Transition::OnProbation(version.clone()), true)
                    }
                    NewVersionVerdict::Suspend(reason) => (
                        Actor::System,
                        Transition::Suspend(Suspension {
                            cause: if *package_unavailable {
                                SuspensionCause::PackageUnavailable
                            } else {
                                SuspensionCause::ValidationFailed
                            },
                            reason: reason.clone(),
                        }),
                        false,
                    ),
                    NewVersionVerdict::Wait => return false,
                };
                if on_probation && let Some(basis) = &validation.stored.made_from {
                    // The first passing validation on a workflow release fixes its library lock, whichever job it is
                    // for. A lock fixed already is the one this was made against, so it agrees; one that differs, or a
                    // release that takes no new lock, puts nothing on trial.
                    if let Err(refusal) =
                        jobs.fix_release_lock(&key.package, &key.workflow, &basis.release, basis.lock.clone())
                    {
                        tracing::warn!(
                            package = %key.package,
                            workflow = %key.workflow,
                            job = %key.job,
                            %refusal,
                            "an enabled job's new version was not put on trial because its release's library lock could not be fixed"
                        );
                        return false;
                    }
                }
                if jobs
                    .transition(&key.package, &key.workflow, &key.job, actor, &transition, now)
                    .is_ok()
                {
                    if on_probation {
                        report.on_probation += 1;
                    } else {
                        report.suspended += 1;
                    }
                }
                false
            }
        }
    }

    /// The job the step is about.
    const fn key(&self) -> &SharedJobKey {
        match self {
            Self::ReturnToDraft { key, .. }
            | Self::Validated { key, .. }
            | Self::Reviewed { key, .. }
            | Self::Revalidated { key, .. } => key,
        }
    }
}

/// One pass over every shared job: validates the ready drafts, sends changed validated and registered jobs back to
/// draft, and validates an enabled job's new version at once, keeping it enabled on probation or suspending it. A
/// `suspended` job stays suspended whatever its files do, and a `retired` job does nothing. A state file that cannot be
/// read ends the pass, and the next one starts again.
pub async fn shared_validation_pass(host: &SharedPassHost<'_>, now: OffsetDateTime) -> PassReport {
    let mut total = PassReport::default();
    let jobs_root = host.jobs_root.to_path_buf();
    let mut pending = match tokio::task::spawn_blocking(move || list_shared_job_files(&jobs_root)).await {
        Ok(Ok(files)) => without_clashing_keys(files),
        Ok(Err(error)) => {
            tracing::warn!(%error, "the shared jobs could not be listed, so none was validated this pass");
            return total;
        }
        Err(error) => {
            tracing::warn!(%error, "the shared jobs could not be listed, so none was validated this pass");
            return total;
        }
    };
    // A validation that passed first on its workflow release while another job's validation recorded that release's
    // library lock kept nothing. It is repeated against the recorded lock, which it is then held to.
    let mut repeats = 0;
    while !pending.is_empty() {
        let Some(round) = pass_round(host, &pending, now).await else {
            return total;
        };
        total.validated += round.counted.validated;
        total.failed += round.counted.failed;
        total.returned_to_draft += round.counted.returned_to_draft;
        total.on_probation += round.counted.on_probation;
        total.suspended += round.counted.suspended;
        if round.conflicted.is_empty() || repeats >= MAX_LOCK_REPEATS {
            break;
        }
        repeats += 1;
        pending.retain(|file| file.tracked_key().is_some_and(|key| round.conflicted.contains(&key)));
    }
    // A job validated for the first time on its workflow release fixes that release's library lock, which can leave a
    // library release nothing keeps. A retention that cannot finish is a warning: what the pass did stands.
    if total.validated > 0 {
        let store_root = host.validation.store_root.to_path_buf();
        let saved = SavedJobs {
            jobs_root: host.jobs_root.to_path_buf(),
            state: host.state.clone(),
        };
        match tokio::task::spawn_blocking(move || retain_host(&store_root, &saved)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => total.warnings.push(RetentionWarning::raised(StoreKind::Host, &error)),
            Err(error) => total
                .warnings
                .push(RetentionWarning::raised(StoreKind::Host, &RetentionError::from(error))),
        }
    }
    total
}

/// How many times a validation is repeated against a library lock another job's validation recorded first.
const MAX_LOCK_REPEATS: u8 = 3;

/// What one look at a set of jobs did, and which of their results were made for the wrong libraries.
struct Round {
    counted: PassReport,
    conflicted: Vec<SharedJobKey>,
}

/// Looks at each of `files` with the state as it is now, validates outside the lock, and applies what it found to the
/// state as it is under the lock. `None` when the state could not be read or written, which ends the pass.
async fn pass_round(host: &SharedPassHost<'_>, files: &[SharedJobFile], now: OffsetDateTime) -> Option<Round> {
    let store = host.state.clone();
    let mut working = match tokio::task::spawn_blocking(move || store.read()).await {
        Ok(Ok(state)) => state,
        Ok(Err(error)) => {
            tracing::warn!(%error, "the shared job state could not be read, so jobs were not validated this pass");
            return None;
        }
        Err(error) => {
            tracing::warn!(%error, "the shared job state could not be read, so jobs were not validated this pass");
            return None;
        }
    };
    let mut steps = Vec::new();
    for file in files {
        visit(host, &mut working, file, now, &mut steps).await;
    }
    if steps.is_empty() {
        return Some(Round {
            counted: PassReport::default(),
            conflicted: Vec::new(),
        });
    }
    let store = host.state.clone();
    let applied = tokio::task::spawn_blocking(move || {
        store.update(|jobs| {
            let mut counted = PassReport::default();
            let mut conflicted = Vec::new();
            for step in &steps {
                if step.apply(jobs, now, &mut counted) {
                    conflicted.push(step.key().clone());
                }
            }
            Round { counted, conflicted }
        })
    })
    .await;
    match applied {
        Ok(Ok(round)) => Some(round),
        Ok(Err(error)) => {
            tracing::warn!(%error, "the shared job state could not be written, so this pass's results were dropped");
            None
        }
        Err(error) => {
            tracing::warn!(%error, "the shared job state could not be written, so this pass's results were dropped");
            None
        }
    }
}

/// Looks at one job and decides what the rules say, applying each decision to `working` so the next sees it.
async fn visit(
    host: &SharedPassHost<'_>,
    working: &mut JobsState,
    file: &SharedJobFile,
    now: OffsetDateTime,
    steps: &mut Vec<Step>,
) {
    let Some(key) = file.tracked_key() else {
        return;
    };
    let ready = names_ready(&file.text);
    let lifecycle = working.lifecycle_of(&key.package, &key.workflow, &key.job);
    let considered = match lifecycle {
        JobLifecycle::Draft => ready,
        JobLifecycle::Validated | JobLifecycle::Registered | JobLifecycle::Enabled => true,
        JobLifecycle::Suspended | JobLifecycle::Retired => false,
    };
    if !considered {
        return;
    }
    // The libraries are the recorded lock's, when a job on the workflow release has passed validation, and resolved only
    // for the first.
    let lock = recorded_lock(working, &key, &file.text);
    let current =
        match shared_job_state_blocking(host.validation.store_root, &key.package, &file.text, lock.clone()).await {
            SharedVersion::Known(version) => Some(version),
            SharedVersion::Unknown => None,
        };
    let mut scratch = PassReport::default();
    if working.new_version_under_enabled_job(&key.package, &key.workflow, &key.job, current.as_ref()) {
        let mut validation = validate_shared_job_against(&host.validation, file, lock.as_ref()).await;
        // Libraries that went away, or a release that changed, between the version being read and the check are the same
        // change on the host.
        if changed_on_host(&validation) {
            return;
        }
        // A file edited away from the release or predecessor the job was enabled with is another job, not a new version
        // of this one: it fails on that key and is suspended, and never put on trial.
        validation.apply_release_rules(working, &key.package, (&key.workflow, &key.job));
        let recorded = working
            .job(&key.package, &key.workflow, &key.job)
            .and_then(contextos_workflows::JobState::fingerprint)
            .cloned();
        // A version that turns out to be the one on record after all, because a read failed once or a file changed
        // back, changes nothing.
        if validation.fingerprint.is_some() && validation.fingerprint == recorded {
            return;
        }
        let verdict = NewVersionVerdict::from((&validation.report, validation.fingerprint.clone()));
        let package_unavailable = validation.package_unavailable;
        let step = Step::Reviewed {
            key,
            validation: Box::new(Checked::new(&validation, now)),
            verdict,
            package_unavailable,
        };
        step.apply(working, now, &mut scratch);
        steps.push(step);
        return;
    }
    if working.version_drift(&key.package, &key.workflow, &key.job, current.as_ref()) {
        let step = Step::ReturnToDraft {
            key: key.clone(),
            current: current.clone(),
        };
        step.apply(working, now, &mut scratch);
        steps.push(step);
    }
    if working.validation_due(&key.package, &key.workflow, &key.job, ready, current.as_ref()) {
        let validation = validate_shared_job_against(&host.validation, file, lock.as_ref()).await;
        // Libraries that went away, or a release that changed, between the version being read and the check are the same
        // change on the host.
        if changed_on_host(&validation) {
            return;
        }
        // The result belongs to the version that was read for the check, which can be newer than `current` when a file
        // changed in between, so it is recorded against that one.
        let step = Step::Validated {
            key,
            validation: Box::new(Checked::new(&validation, now)),
        };
        step.apply(working, now, &mut scratch);
        steps.push(step);
        return;
    }
    if working.rules_outdated(&key.package, &key.workflow, &key.job) {
        let validation = validate_shared_job_against(&host.validation, file, lock.as_ref()).await;
        // Libraries that went away, or a release that changed, between the version being read and the check are the same
        // change on the host.
        if changed_on_host(&validation) {
            return;
        }
        // A check that could not be made changes nothing, so the job is looked at again on a later pass.
        if validation.report.failures.is_empty() && !validation.report.unverifiable.is_empty() {
            return;
        }
        let step = Step::Revalidated {
            key,
            validation: Box::new(Checked::new(&validation, now)),
        };
        step.apply(working, now, &mut scratch);
        steps.push(step);
    }
}

/// Settles the trial of a shared job's first live run with how a live run of `version`, the version it ran, ended. See
/// [`crate::job_probation::settle_first_run`], whose rules these are.
pub async fn settle_shared_first_run(
    state: &SharedStateStore,
    key: &SharedJobKey,
    version: Option<JobFingerprint>,
    outcome: &RunOutcome,
    run: Option<u64>,
    now: OffsetDateTime,
) -> Settlement {
    let Ok(verdict) = FirstRunVerdict::try_from(outcome) else {
        return Settlement::NothingToSettle;
    };
    // A job that is not on trial has nothing to settle, whatever became of its version, so that is decided first.
    let store = state.clone();
    let tracked = key.clone();
    let on_trial = tokio::task::spawn_blocking(move || {
        store.read().map(|jobs| {
            jobs.job(&tracked.package, &tracked.workflow, &tracked.job)
                .is_some_and(JobState::first_run_pending)
        })
    })
    .await;
    if matches!(on_trial, Ok(Ok(false))) {
        return Settlement::NothingToSettle;
    }
    let Some(version) = version else {
        tracing::warn!(package = %key.package, workflow = %key.workflow, job = %key.job, "the version a live run ran could not be read, so its outcome was not applied to the job's trial");
        return Settlement::NotRecorded;
    };
    let suspends = matches!(verdict, FirstRunVerdict::Failed { .. });
    let change = change_shared_lifecycle(
        state,
        SharedJobChange {
            key,
            actor: Actor::System,
            transition: Transition::from((verdict, version, run)),
            now,
        },
    )
    .await;
    match change {
        Ok(_) if suspends => {
            tracing::warn!(package = %key.package, workflow = %key.workflow, job = %key.job, "the first live run of a new version failed, so the job was suspended");
            Settlement::Suspended
        }
        Ok(_) => {
            tracing::info!(package = %key.package, workflow = %key.workflow, job = %key.job, "the first live run of a new version passed");
            Settlement::Passed
        }
        Err(SharedLifecycleError::Refused {
            source:
                TransitionError::NotOnProbation { .. }
                | TransitionError::WrongVersion { .. }
                | TransitionError::WrongState { .. },
        }) => Settlement::NothingToSettle,
        Err(error) => {
            tracing::warn!(package = %key.package, workflow = %key.workflow, job = %key.job, %error, "the outcome of a live run could not be applied to the job's trial");
            Settlement::NotRecorded
        }
    }
}

/// Records that a dry run completed for `version`, the version the job had when the run started, which registers a job
/// that was validated for that version. Anything else is reported and recorded as nothing.
pub async fn record_shared_dry_run(
    state: &SharedStateStore,
    key: &SharedJobKey,
    version: JobFingerprint,
    now: OffsetDateTime,
) -> DryRunRecorded {
    match change_shared_lifecycle(
        state,
        SharedJobChange {
            key,
            actor: Actor::DryRun,
            transition: Transition::DryRunCompleted(version),
            now,
        },
    )
    .await
    {
        Ok(_) => DryRunRecorded::Registered,
        Err(SharedLifecycleError::Refused { source }) => DryRunRecorded::NotRecorded(NotRegistered::from(&source)),
        Err(_) => DryRunRecorded::NotRecorded(NotRegistered::StateUnavailable),
    }
}

#[cfg(test)]
#[path = "shared_lifecycle_test.rs"]
mod tests;
