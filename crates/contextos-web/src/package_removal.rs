//! Removing one release at an operator's request, the same procedure for the host's store and a vault's.
//!
//! The caller holds the locks that order a change to what is kept. This decides, with the policy retention uses, whether a
//! rule keeps the release: a reason that guards a removal refuses it, naming the jobs and releases, unless the removal is
//! forced. A forced removal is recorded as forced and reports the jobs the next validation pass will move, since their
//! release is gone. What the removal changes about what is kept (a recorded `future`, and releases only it held) is the
//! caller's to bring up to date afterwards.

use std::collections::BTreeSet;

use contextos_packages::{PackageStore, RecordedCause, ReleaseKey, RequestedRemoval};
use contextos_workflows::{
    JobLifecycle, JobsState, KeepReason, LibraryKeep, PackageName, ReleaseRef, Retention, RetentionInput, WorkflowKeep,
    decide_retention,
};

use crate::package_op_error::PackageOpError;
use crate::package_release_manifest::PackageRelease;
use crate::package_retention::{Held, RetentionHome};

/// A job whose release the removal takes away, and the lifecycle stage it is in until the next validation pass moves it:
/// an `enabled` job to `suspended`, a `validated` or `registered` one to `draft`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AffectedJob {
    pub package: String,
    pub workflow: String,
    pub job: String,
    pub lifecycle: JobLifecycle,
}

/// A removal that was made.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Removal {
    /// The release removed.
    pub release: ReleaseRef,
    /// The cause the ledger records.
    pub cause: RecordedCause,
    /// The reasons that refused the removal and were overridden; empty unless the removal was forced and a rule kept the
    /// release.
    pub overridden: Vec<KeepReason>,
    /// The jobs the next validation pass moves, because their release is gone.
    pub affected_jobs: Vec<AffectedJob>,
}

/// Removes `key` from `store`, which `lock` holds the lock of, deciding what keeps it from `home`.
///
/// # Errors
///
/// [`PackageOpError::InUse`] when a rule keeps the release and `force` is not set, [`PackageOpError::Packages`] when the
/// store refuses or fails (a release that is not there, a ledger that cannot be used), and [`PackageOpError::KeepUnreadable`]
/// when what keeps releases cannot be read.
pub(crate) async fn remove_guarded<S, H>(
    store: &S,
    lock: &S::Lock,
    home: &H,
    key: &ReleaseKey,
    force: bool,
) -> Result<Removal, PackageOpError>
where
    S: PackageStore<Manifest = PackageRelease>,
    H: RetentionHome,
{
    store.recover(lock).await?;
    let release = ReleaseRef {
        name: PackageName::try_from(key.name.as_str()).map_err(|error| PackageOpError::Name {
            value: key.name.as_str().to_owned(),
            reason: error.to_string(),
        })?,
        version: key.version.as_semver().clone(),
    };
    let held = Held::read(store).await?;
    let (first, _) = keeping(&held, home, &release).await?;
    if !force && !first.is_empty() {
        return Err(PackageOpError::InUse {
            release,
            reasons: first,
        });
    }
    // Job files, and a vault's state file, are written without the locks held here, so the release is set aside and what
    // names releases is read again. A release named in the meantime is renamed back and the removal refused, unless it is
    // forced; the cause recorded is still the operator's, never retention's.
    let batch = store.set_aside(lock, std::slice::from_ref(key)).await?;
    let (guards, decided_state) = match keeping(&held, home, &release).await {
        Ok(again) => again,
        Err(error) => {
            store
                .settle_requested(lock, batch, std::slice::from_ref(key), RequestedRemoval::ByRequest)
                .await?;
            return Err(error);
        }
    };
    if !force && !guards.is_empty() {
        store
            .settle_requested(lock, batch, std::slice::from_ref(key), RequestedRemoval::ByRequest)
            .await?;
        return Err(PackageOpError::InUse {
            release,
            reasons: guards,
        });
    }
    let requested = if guards.is_empty() {
        RequestedRemoval::ByRequest
    } else {
        RequestedRemoval::Forced
    };
    store.settle_requested(lock, batch, &[], requested).await?;
    let affected_jobs = affected_by(&release, &guards, &decided_state.0, &decided_state.1);
    Ok(Removal {
        release,
        cause: RecordedCause::from(requested),
        overridden: guards,
        affected_jobs,
    })
}

/// What keeps `release` as the state and job files say now, and the retention decision and state that says so. What the
/// state and job files say about a release counts whether or not it reads whole: a damaged current release is still a
/// job's release.
async fn keeping<H: RetentionHome>(
    held: &Held,
    home: &H,
    release: &ReleaseRef,
) -> Result<(Vec<KeepReason>, (Retention, JobsState)), PackageOpError> {
    let (state, files) = (home.state().await?.state, home.job_files().await?);
    let decided = decide_retention(&RetentionInput {
        releases: &held.releases,
        unverified: &held.unverified,
        state: &state,
        job_files: &files,
    });
    let guards = decided
        .holding(release)
        .iter()
        .filter(|reason| reason.guards_removal())
        .cloned()
        .collect();
    Ok((guards, (decided, state)))
}

/// The jobs a removal takes the release from, that the next validation pass changes: those that name the release removed
/// or, for a library release, name a workflow release whose lock or first validation holds it, and are `validated`,
/// `registered`, or `enabled`.
fn affected_by(
    release: &ReleaseRef,
    guards: &[KeepReason],
    decided: &Retention,
    state: &JobsState,
) -> Vec<AffectedJob> {
    let mut named = BTreeSet::new();
    for guard in guards {
        match guard {
            KeepReason::Workflow { .. } => named.extend(jobs_naming(release, std::slice::from_ref(guard), None)),
            KeepReason::Library(
                LibraryKeep::LockedBy {
                    release: holder,
                    workflow,
                }
                | LibraryKeep::ChosenFor {
                    release: holder,
                    workflow,
                },
            ) => {
                named.extend(jobs_naming(holder, decided.holding(holder), Some(workflow)));
            }
            KeepReason::Library(LibraryKeep::NewestRelease | LibraryKeep::NeededByNewest { .. }) => {}
        }
    }
    named
        .into_iter()
        .map(|(package, workflow, job)| AffectedJob {
            lifecycle: state.lifecycle_of(&package, &workflow, &job),
            package,
            workflow,
            job,
        })
        .filter(|job| {
            matches!(
                job.lifecycle,
                JobLifecycle::Validated | JobLifecycle::Registered | JobLifecycle::Enabled
            )
        })
        .collect()
}

/// The `(package, workflow, job)` of each job `reasons` say names `release`, of `workflow` only when that is given.
fn jobs_naming<'a>(
    release: &'a ReleaseRef,
    reasons: &'a [KeepReason],
    workflow: Option<&'a str>,
) -> impl Iterator<Item = (String, String, String)> + 'a {
    reasons.iter().filter_map(move |reason| match reason {
        KeepReason::Workflow {
            workflow: named,
            reason: WorkflowKeep::NamedByJob { job, .. },
        } if workflow.is_none_or(|only| only == named) => {
            Some((release.name.as_str().to_owned(), named.clone(), job.clone()))
        }
        _ => None,
    })
}

#[cfg(test)]
#[path = "package_removal_test.rs"]
mod tests;
