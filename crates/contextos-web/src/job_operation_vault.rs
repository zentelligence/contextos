//! The vault adapter of the operator operation: a job whose state is kept in the vault's `.contextos/jobs.toml` and
//! whose files are under `registry/workflows/`, both reached through the primary MCP server.
//!
//! It holds no rule. The operation of [`crate::job_operation`] decides what an operator's request means, and the
//! transition table checks it against the state as it is when it is written, with the file's content hash standing in
//! for a lock.

use contextos_packages::{StoreEntryKind, StoreIo, StorePath};
use contextos_workflows::{
    Actor, Changed, JobFingerprint, JobLifecycle, JobState, ReleaseContext, StoreKind, StoredValidation, Transition,
};
use time::OffsetDateTime;

use crate::job_lifecycle::{LifecycleChange, LifecycleError, change_lifecycle_reporting};
use crate::job_operation::{JobStore, OperationError, StoreError};
use crate::job_validation::{JobLocation, Recording, locate_job, read_text, validate_and_record, version_at};
use crate::mcp_client::{McpCallError, McpClient};
use crate::scheduler::read_state;
use crate::store_retention::RetentionWarning;
use crate::store_retention::retain_vault;
use crate::vault_job_release::NamedJobRelease;
use crate::vault_runs::claim_slot_under;

/// One vault job, by the package, workflow, and job slugs its state is kept under.
pub struct VaultJobStore<'a> {
    pub client: &'a McpClient,
    pub vault_name: &'a str,
    /// Every server `web.toml` configures, which validation checks the workflow's allow-list against.
    pub known_mcp_servers: &'a [String],
    pub package: &'a str,
    pub workflow_slug: &'a str,
    pub job_slug: &'a str,
    /// Where the host keeps run claims, which an enabling reads to see whether its predecessor is running. `None` when
    /// the host has no claims directory, so no run can hold one.
    pub claims_root: Option<&'a std::path::Path>,
}

impl VaultJobStore<'_> {
    async fn location(&self) -> Result<JobLocation, StoreError> {
        locate_job(
            self.client,
            self.vault_name,
            self.package,
            self.workflow_slug,
            self.job_slug,
        )
        .await
        .map_err(|error| unavailable(&error))?
        .ok_or_else(|| {
            StoreError::NotFound(format!(
                "the vault {:?} has no job {:?} in the workflow {:?} of the package {:?} under registry/workflows/",
                self.vault_name, self.job_slug, self.workflow_slug, self.package
            ))
        })
    }
}

impl VaultJobStore<'_> {
    /// What a change that depends on the job's release needs from outside the state file: the package its workflow
    /// release belongs to (its source package's folder), the `predecessor` its job file names, and the releases the
    /// vault's store holds of that package.
    async fn release_context(&self) -> Result<ReleaseContext, StoreError> {
        let location = self.location().await?;
        let job_toml = read_text(
            self.client,
            &format!(
                "{}://registry/workflows/{}/jobs/{}",
                self.vault_name,
                location.folder(),
                location.file
            ),
        )
        .await
        .map_err(|error| unavailable(&error))?
        .ok_or_else(|| StoreError::Unavailable("the job file could not be read".to_owned()))?;
        let named = NamedJobRelease::try_from(job_toml.as_str()).ok().map(|named| named.0);
        let named_release = named.as_ref().map(|named| named.version.as_semver().clone());
        let predecessor = named.and_then(|named| named.predecessor);
        let store = self
            .client
            .vault_store(self.vault_name)
            .await
            .map_err(|error| StoreError::Unavailable(error.to_string()))?;
        let package = contextos_packages::ReleaseFilePath::try_from(location.package.as_str())
            .map(|folder| StorePath::root().within(&folder))
            .map_err(|error| StoreError::Unavailable(error.to_string()))?;
        let installed = match store.list(&package).await {
            Ok(entries) => entries
                .into_iter()
                .filter(|entry| matches!(entry.kind, StoreEntryKind::Directory))
                .filter_map(|entry| semver::Version::parse(&entry.name).ok())
                .collect(),
            Err(failure) if failure.kind == contextos_packages::StoreFailureKind::NotFound => Vec::new(),
            Err(failure) => return Err(StoreError::Unavailable(failure.detail)),
        };
        let predecessor_running = match (&predecessor, self.claims_root) {
            (Some(key), Some(claims_root)) => claim_slot_under(
                claims_root,
                self.vault_name,
                &location.package,
                self.workflow_slug,
                key.as_str(),
            )
            .map_err(|error| StoreError::Unavailable(error.to_string()))?
            .is_held()
            .await
            .map_err(|error| {
                StoreError::Unavailable(format!("the predecessor's run claim could not be read: {error}"))
            })?,
            _ => false,
        };
        Ok(ReleaseContext {
            package: location.package,
            predecessor,
            installed,
            predecessor_running,
            named_release,
        })
    }
}

fn unavailable(error: &McpCallError) -> StoreError {
    StoreError::Unavailable(error.to_string())
}

/// Whether a validation result was kept. One that was not cannot allow anything, so it is an error: a conflict when
/// another writer changed the state in between, which repeating the request resolves, and unavailable otherwise.
fn kept(recording: Recording) -> Result<(), StoreError> {
    match recording {
        Recording::Recorded | Recording::Withheld => Ok(()),
        Recording::NotRecorded { conflict: true, .. } => Err(StoreError::Conflict),
        Recording::NotRecorded { reason, .. } => Err(StoreError::Unavailable(format!(
            "the validation result could not be recorded: {reason}"
        ))),
    }
}

impl JobStore for VaultJobStore<'_> {
    async fn stored(&self) -> Result<Option<JobState>, StoreError> {
        let snapshot = read_state(self.client, self.vault_name)
            .await
            .map_err(|error| StoreError::Unavailable(error.to_string()))?;
        Ok(snapshot
            .value
            .job(self.package, self.workflow_slug, self.job_slug)
            .cloned())
    }

    async fn exists(&self) -> Result<bool, StoreError> {
        if self.stored().await?.is_some() {
            return Ok(true);
        }
        Ok(locate_job(
            self.client,
            self.vault_name,
            self.package,
            self.workflow_slug,
            self.job_slug,
        )
        .await
        .map_err(|error| unavailable(&error))?
        .is_some())
    }

    async fn lifecycle(&self) -> Result<JobLifecycle, StoreError> {
        let snapshot = read_state(self.client, self.vault_name)
            .await
            .map_err(|error| StoreError::Unavailable(error.to_string()))?;
        Ok(snapshot
            .value
            .lifecycle_of(self.package, self.workflow_slug, self.job_slug))
    }

    async fn predecessor_schedule(&self) -> Result<Option<String>, StoreError> {
        let snapshot = read_state(self.client, self.vault_name)
            .await
            .map_err(|error| StoreError::Unavailable(error.to_string()))?;
        Ok(snapshot
            .value
            .predecessor_schedule(self.package, self.workflow_slug, self.job_slug)
            .map(str::to_owned))
    }

    async fn version(&self) -> Result<Option<JobFingerprint>, StoreError> {
        let location = self.location().await?;
        version_at(self.client, self.vault_name, &location)
            .await
            .map_err(|error| unavailable(&error))
    }

    async fn validate(&self, now: OffsetDateTime) -> Result<StoredValidation, StoreError> {
        let location = self.location().await?;
        let (validation, recording) =
            validate_and_record(self.client, self.vault_name, self.known_mcp_servers, &location, now)
                .await
                .map_err(|error| unavailable(&error))?;
        kept(recording)?;
        Ok(validation.stored(now))
    }

    async fn retain(&self) -> Result<(), RetentionWarning> {
        retain_vault(self.client, self.vault_name)
            .await
            .map_err(|error| RetentionWarning::raised(StoreKind::Vault, &error))
    }

    async fn apply(&self, transition: Transition, now: OffsetDateTime) -> Result<Changed, OperationError> {
        // An enabling, a resume, and a re-activation depend on whether the job's release is current, previous, or future, and
        // an enabling retires the predecessor and moves the workflow's `current` and `previous`, so they are made with the release context.
        let context = if matches!(
            transition,
            Transition::Enable(..) | Transition::Resume(_) | Transition::Reactivate
        ) {
            Some(self.release_context().await.map_err(OperationError::from)?)
        } else {
            None
        };
        change_lifecycle_reporting(
            self.client,
            self.vault_name,
            LifecycleChange {
                package: self.package,
                workflow_slug: self.workflow_slug,
                job_slug: self.job_slug,
                actor: Actor::Operator,
                transition,
                now,
            },
            context.as_ref(),
        )
        .await
        .map_err(|error| match error {
            LifecycleError::Refused { source } => OperationError::Refused(source),
            LifecycleError::Conflict => OperationError::Conflict,
            LifecycleError::Unavailable(reason) => OperationError::Unavailable(reason),
        })
    }
}

#[cfg(test)]
#[path = "job_operation_vault_test.rs"]
mod tests;
