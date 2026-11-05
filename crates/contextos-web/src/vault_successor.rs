//! Making successors of a vault's jobs: written through the vault's primary server, as `jobs create` writes a job file,
//! and under the vault's package store lock, so no release is installed, removed, or kept differently between the
//! checks and the writes.

use contextos_core::ContentHash;
use contextos_packages::{PackageStore as _, PackagesError};
use contextos_workflows::{ReleaseKinds as _, ReleaseRefusal, resolve_target};

use super::{AuthoringError, Digested, Located, VaultJobAuthoring, check_without_workflow, unix_text};
use crate::successor_plan::{PlanInput, PlannedSuccessor, SuccessorRefused, plan_successors};
use crate::successor_service::{Attempt, SuccessorRequest};

fn refused(error: impl Into<AuthoringError>) -> Attempt {
    Attempt::refused(vec![SuccessorRefused {
        job: None,
        error: error.into(),
    }])
}

impl VaultJobAuthoring<'_> {
    /// Makes the successors `request` asks for, writing nothing when the request or any job in it is refused.
    pub async fn make_successors(&self, request: &SuccessorRequest) -> Attempt {
        let Ok(engine) = self.client.vault_packages(self.vault).await else {
            return refused(AuthoringError::blocked(
                "the vault's package store could not be located",
            ));
        };
        // Held until the successors are written; a caller that finds it held is turned away at once.
        let _store_lock = match engine.lock().await {
            Ok(lock) => lock,
            Err(PackagesError::StoreLocked) => return refused(AuthoringError::StoreBusy),
            Err(_) => {
                return refused(AuthoringError::blocked(
                    "the vault's package store lock could not be taken",
                ));
            }
        };
        self.make_locked(request).await
    }

    async fn make_locked(&self, request: &SuccessorRequest) -> Attempt {
        let (package, workflow) = (request.package.as_str(), request.workflow.as_str());
        let state = match self.state().await {
            Ok(state) => state,
            Err(error) => return refused(error),
        };
        let versions = state
            .package_releases(package)
            .and_then(|releases| releases.workflows.get(workflow));
        let to = match resolve_target(&request.to, versions) {
            Ok(to) => to,
            Err(refusal) => return refused(refusal),
        };
        let release = state.release_kind(package, workflow, &to);
        let located = match self.find(package, workflow).await {
            Ok(Some(located)) => located,
            Ok(None) => {
                return refused(ReleaseRefusal::NotInstalled {
                    package: package.to_owned(),
                    workflow: workflow.to_owned(),
                    version: to,
                });
            }
            Err(error) => return refused(error),
        };
        let entries = match self.entries(&located).await {
            Ok(entries) => entries,
            Err(error) => return refused(error),
        };
        let presence = self.presence(&located, &to).await;
        let check_file = |job: &str, text: &str| -> Result<(), AuthoringError> {
            check_without_workflow(package, workflow, job, &unix_text(text)).map(|_| ())
        };
        let planned = plan_successors(&PlanInput {
            package,
            workflow,
            scope: &request.scope,
            to: &to,
            presence,
            state: &state,
            entries: &entries,
            check_file: &check_file,
        });
        let mut attempt = Attempt {
            to: Some(to),
            release: Some(release),
            made: Vec::new(),
            refused: Vec::new(),
        };
        match planned {
            Err(refusals) => attempt.refused = refusals,
            Ok(items) => {
                for item in items {
                    match self.write_successor(&located, &item).await {
                        Ok(hash) => attempt.made.push((item, hash)),
                        Err(error) => {
                            attempt.refused.push(SuccessorRefused {
                                job: Some(item.predecessor),
                                error,
                            });
                            break;
                        }
                    }
                }
            }
        }
        attempt
    }

    /// Writes one successor's file, never replacing a file.
    async fn write_successor(&self, located: &Located, item: &PlannedSuccessor) -> Result<ContentHash, AuthoringError> {
        let text = unix_text(&item.text);
        self.write(located, &item.key, &text, None).await?;
        tracing::info!(vault = %self.vault, package = %located.workflow.package, workflow = %located.workflow.slug, job = %item.key, "vault successor created");
        Ok(ContentHash::from(Digested(text.as_bytes())))
    }
}
