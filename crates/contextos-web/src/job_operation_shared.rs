//! The shared adapter of the operator operation: a job whose state is kept in the host's state file and whose file is
//! under the host's jobs folder, with the release it names in the package store.
//!
//! It holds no rule. The operation of [`crate::job_operation`] decides what an operator's request means, and the
//! transition table checks it against the state as it is when it is written, inside the state file's own lock.

use std::path::Path;

use contextos_workflows::{
    Actor, Changed, JobFingerprint, JobLifecycle, JobState, LockOutcome, PackageName, ReleaseContext, StoreKind,
    StoredValidation, Transition,
};
use time::OffsetDateTime;

use crate::job_operation::{JobStore, OperationError, StoreError};
use crate::package_retention::RetentionError;
use crate::saved_jobs::SavedJobs;
use crate::shared_claim::SharedJobKey;
use crate::shared_jobs::{SharedJobFile, list_shared_job_files, without_clashing_keys};
use crate::shared_lifecycle::{
    SharedJobChange, SharedLifecycleError, change_shared_lifecycle_reporting, delete_shared_job,
};
use crate::shared_state::SharedStateStore;
use crate::shared_validation::{SharedValidationHost, changed_on_host, validate_shared_job_against};
use crate::shared_version::{recorded_lock_blocking, shared_job_version_under_blocking};
use crate::store_retention::{RetentionWarning, retain_host};
use crate::vault_job_release::NamedJobRelease;

/// One shared job, by the key its state is kept under.
pub struct SharedJobStore<'a> {
    pub state: &'a SharedStateStore,
    pub jobs_root: &'a Path,
    pub validation: &'a SharedValidationHost<'a>,
    pub key: &'a SharedJobKey,
    /// Where the host keeps run claims, which an enabling reads to see whether its predecessor is running.
    pub claims_root: &'a Path,
}

impl SharedJobStore<'_> {
    /// The job's file, found by the key it is tracked under, which a `slug` in the file can make differ from its name.
    async fn file(&self) -> Result<SharedJobFile, StoreError> {
        let jobs_root = self.jobs_root.to_path_buf();
        let files = tokio::task::spawn_blocking(move || list_shared_job_files(&jobs_root))
            .await
            .map_err(|error| StoreError::Unavailable(error.to_string()))?
            .map_err(|error| StoreError::Unavailable(format!("the shared jobs could not be listed: {error}")))?;
        without_clashing_keys(files)
            .into_iter()
            .find(|file| file.tracked_key().as_ref() == Some(self.key))
            .ok_or_else(|| {
                StoreError::NotFound(format!(
                    "no shared job {:?} for the workflow {:?} of package {:?} under {}",
                    self.key.job,
                    self.key.workflow,
                    self.key.package,
                    self.jobs_root.display()
                ))
            })
    }
}

/// How many times a validation is repeated against a library lock another job's validation recorded first.
const MAX_LOCK_REPEATS: u8 = 3;

impl SharedJobStore<'_> {
    /// What a change that depends on the job's release needs from outside the state file: the job's package, the
    /// `predecessor` its job file names, and the releases the host store holds of that package.
    async fn release_context(&self) -> Result<ReleaseContext, StoreError> {
        let file = self.file().await?;
        let named = NamedJobRelease::try_from(file.text.as_str()).ok().map(|named| named.0);
        let named_release = named.as_ref().map(|named| named.version.as_semver().clone());
        let predecessor = named.and_then(|named| named.predecessor);
        let package = PackageName::try_from(self.key.package.as_str())
            .map_err(|error| StoreError::Unavailable(error.to_string()))?;
        let store_root = self.validation.store_root.to_path_buf();
        let installed = tokio::task::spawn_blocking(move || {
            crate::package_store::installed_versions_of(&store_root, &package).map(|installed| installed.all())
        })
        .await
        .map_err(|error| StoreError::Unavailable(error.to_string()))?
        .map_err(|error| StoreError::Unavailable(error.to_string()))?;
        let predecessor_running = match &predecessor {
            Some(named) => SharedJobKey {
                job: named.as_str().to_owned(),
                ..self.key.clone()
            }
            .claim_slot(self.claims_root)
            .map_err(|error| StoreError::Unavailable(error.to_string()))?
            .is_held()
            .await
            .map_err(|error| {
                StoreError::Unavailable(format!("the predecessor's run claim could not be read: {error}"))
            })?,
            None => false,
        };
        Ok(ReleaseContext {
            package: self.key.package.clone(),
            predecessor,
            installed,
            predecessor_running,
            named_release,
        })
    }
}

impl JobStore for SharedJobStore<'_> {
    async fn stored(&self) -> Result<Option<JobState>, StoreError> {
        let store = self.state.clone();
        let key = self.key.clone();
        tokio::task::spawn_blocking(move || {
            store
                .read()
                .map(|jobs| jobs.job(&key.package, &key.workflow, &key.job).cloned())
        })
        .await
        .map_err(|error| StoreError::Unavailable(error.to_string()))?
        .map_err(|error| StoreError::Unavailable(error.to_string()))
    }

    async fn exists(&self) -> Result<bool, StoreError> {
        if self.stored().await?.is_some() {
            return Ok(true);
        }
        match self.file().await {
            Ok(_) => Ok(true),
            Err(StoreError::NotFound(_)) => Ok(false),
            Err(other) => Err(other),
        }
    }

    async fn lifecycle(&self) -> Result<JobLifecycle, StoreError> {
        let store = self.state.clone();
        let key = self.key.clone();
        tokio::task::spawn_blocking(move || {
            store
                .read()
                .map(|jobs| jobs.lifecycle_of(&key.package, &key.workflow, &key.job))
        })
        .await
        .map_err(|error| StoreError::Unavailable(error.to_string()))?
        .map_err(|error| StoreError::Unavailable(error.to_string()))
    }

    async fn predecessor_schedule(&self) -> Result<Option<String>, StoreError> {
        let store = self.state.clone();
        let key = self.key.clone();
        tokio::task::spawn_blocking(move || {
            store.read().map(|jobs| {
                jobs.predecessor_schedule(&key.package, &key.workflow, &key.job)
                    .map(str::to_owned)
            })
        })
        .await
        .map_err(|error| StoreError::Unavailable(error.to_string()))?
        .map_err(|error| StoreError::Unavailable(error.to_string()))
    }

    async fn version(&self) -> Result<Option<JobFingerprint>, StoreError> {
        let file = self.file().await?;
        let lock = recorded_lock_blocking(self.state, self.key, &file.text)
            .await
            .map_err(|error| StoreError::Unavailable(error.to_string()))?;
        Ok(shared_job_version_under_blocking(self.validation.store_root, &self.key.package, &file.text, lock).await)
    }

    async fn validate(&self, now: OffsetDateTime) -> Result<StoredValidation, StoreError> {
        // A validation that passed first on its workflow release while another job's validation recorded that release's
        // library lock is repeated against the recorded lock, which this one is then held to.
        let mut repeats = 0;
        loop {
            let file = self.file().await?;
            let lock = recorded_lock_blocking(self.state, self.key, &file.text)
                .await
                .map_err(|error| StoreError::Unavailable(error.to_string()))?;
            let validation = validate_shared_job_against(self.validation, &file, lock.as_ref()).await;
            let stored = validation.stored(now);
            if changed_on_host(&validation) {
                // The result is for the person who asked. Keeping it would replace what the job earned for the version it
                // has again when the host is as it was.
                return Ok(stored);
            }
            let store = self.state.clone();
            let key = self.key.clone();
            let kept = stored.clone();
            let outcome = tokio::task::spawn_blocking(move || {
                store.update(|jobs| {
                    // A validated or registered job whose release changed since goes back to draft first, so a result
                    // that passes for the release installed now validates it for that release.
                    let lock = jobs
                        .keep_validation(&key.package, &key.workflow, &key.job, kept, now)
                        .lock;
                    // What was kept is the result with the rules about releases and the link applied, which is what
                    // the person who asked is told.
                    let recorded = jobs
                        .job(&key.package, &key.workflow, &key.job)
                        .and_then(JobState::validation)
                        .cloned();
                    (lock, recorded)
                })
            })
            .await
            .map_err(|error| StoreError::Unavailable(error.to_string()))?
            .map_err(|error| {
                StoreError::Unavailable(format!("the validation result could not be recorded: {error}"))
            })?;
            let (lock, recorded) = outcome;
            if lock == LockOutcome::Conflict && repeats < MAX_LOCK_REPEATS {
                repeats += 1;
                continue;
            }
            return Ok(recorded.filter(|_| lock != LockOutcome::Conflict).unwrap_or(stored));
        }
    }

    fn removes_files_on_delete(&self) -> bool {
        true
    }

    async fn retain(&self) -> Result<(), RetentionWarning> {
        let store_root = self.validation.store_root.to_path_buf();
        let saved = SavedJobs {
            jobs_root: self.jobs_root.to_path_buf(),
            state: self.state.clone(),
        };
        tokio::task::spawn_blocking(move || retain_host(&store_root, &saved))
            .await
            .map_err(RetentionError::from)
            .and_then(|outcome| outcome)
            .map_err(|error| RetentionWarning::raised(StoreKind::Host, &error))
    }

    async fn apply(&self, transition: Transition, now: OffsetDateTime) -> Result<Changed, OperationError> {
        // A delete removes the job's file with its state, inside the state lock, so the page's delete and the command's
        // are one operation and a deleted job cannot return as a draft.
        let applied = if matches!(transition, Transition::Delete) {
            delete_shared_job(self.state, self.jobs_root, self.key, now)
                .await
                .map(Changed::from)
        } else {
            // An enabling, a resume, and a re-activation depend on whether the job's release is current, previous, or future, and an enabling
            // retires the predecessor and moves the package's `current` and `previous`, so they are made with the release context.
            let context = if matches!(
                transition,
                Transition::Enable(..) | Transition::Resume(_) | Transition::Reactivate
            ) {
                Some(self.release_context().await.map_err(OperationError::from)?)
            } else {
                None
            };
            change_shared_lifecycle_reporting(
                self.state,
                SharedJobChange {
                    key: self.key,
                    actor: Actor::Operator,
                    transition,
                    now,
                },
                context.as_ref(),
            )
            .await
        };
        applied.map_err(|error| match error {
            SharedLifecycleError::Refused { source } => OperationError::Refused(source),
            SharedLifecycleError::KeyTaken { files } => {
                OperationError::KeyTaken(format!("the job is the key of {files}, so neither was removed"))
            }
            SharedLifecycleError::FileNotRemoved { .. }
            | SharedLifecycleError::FileUnusable { .. }
            | SharedLifecycleError::LinkInPlace => OperationError::JobStoreUnavailable(error.to_string()),
            SharedLifecycleError::Unavailable(_) | SharedLifecycleError::Interrupted(_) => {
                OperationError::Unavailable(error.to_string())
            }
        })
    }
}
