//! Making successors of shared jobs: every check and every write happens under the host state's lock, the lock every
//! lifecycle change takes, so no predecessor changes stage between being judged and having its successor written.

use std::io;

use contextos_core::ContentHash;
use contextos_fs::FsError;
use contextos_fs::host_file::{NoFileFaults, create_file_observed};
use contextos_workflows::{InstalledReleases as _, JobsState, ReleaseKinds as _, resolve_target};

use super::{AuthoringError, Digested, HostCatalogue, SharedJobAuthoring, check_job_file, log_written};
use crate::shared_claim::SharedJobKey;
use crate::shared_jobs::{JobFileEntry, list_shared_job_entries};
use crate::successor_plan::{PlanInput, PlannedSuccessor, SuccessorRefused, plan_successors};
use crate::successor_service::{Attempt, SuccessorRequest};

#[cfg(test)]
#[path = "shared_successor_test.rs"]
mod tests;

impl SharedJobAuthoring {
    /// Makes the successors `request` asks for, writing nothing when the request or any job in it is refused.
    ///
    /// Blocking: waits for the state lock, so call it from a blocking thread.
    #[must_use]
    pub fn make_successors(&self, request: &SuccessorRequest) -> Attempt {
        match self.state.update(|jobs| self.make_locked(jobs, request)) {
            Ok(attempt) => attempt,
            Err(error) => Attempt::refused(vec![SuccessorRefused {
                job: None,
                error: error.into(),
            }]),
        }
    }

    fn make_locked(&self, jobs: &JobsState, request: &SuccessorRequest) -> Attempt {
        let (package, workflow) = (request.package.as_str(), request.workflow.as_str());
        let versions = jobs
            .package_releases(package)
            .and_then(|releases| releases.workflows.get(workflow));
        let to = match resolve_target(&request.to, versions) {
            Ok(to) => to,
            Err(refusal) => {
                return Attempt::refused(vec![SuccessorRefused {
                    job: None,
                    error: refusal.into(),
                }]);
            }
        };
        let release = jobs.release_kind(package, workflow, &to);
        let entries = match list_shared_job_entries(&self.jobs_root) {
            Ok(entries) => entries,
            Err(error) => {
                return Attempt::refused(vec![SuccessorRefused {
                    job: None,
                    error: AuthoringError::unavailable("the jobs folder could not be listed", &error),
                }]);
            }
        };
        let entries: Vec<JobFileEntry> = entries
            .into_iter()
            .filter(|entry| {
                let key = match entry {
                    JobFileEntry::Usable(file) => &file.key,
                    JobFileEntry::Unusable { key, .. } | JobFileEntry::Misnamed { key } => key,
                };
                key.package == package && key.workflow == workflow
            })
            .collect();
        let check_file = |job: &str, text: &str| -> Result<(), AuthoringError> {
            let key = SharedJobKey {
                package: package.to_owned(),
                workflow: workflow.to_owned(),
                job: job.to_owned(),
            };
            check_job_file(&key, &key, text.as_bytes()).map(|_| ())
        };
        let planned = plan_successors(&PlanInput {
            package,
            workflow,
            scope: &request.scope,
            to: &to,
            presence: HostCatalogue(&self.store_root).presence(package, workflow, &to),
            state: jobs,
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
            Err(refused) => attempt.refused = refused,
            Ok(items) => {
                for item in items {
                    match self.write_successor(request, &item) {
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
    fn write_successor(
        &self,
        request: &SuccessorRequest,
        item: &PlannedSuccessor,
    ) -> Result<ContentHash, AuthoringError> {
        let key = SharedJobKey {
            package: request.package.clone(),
            workflow: request.workflow.clone(),
            job: item.key.clone(),
        };
        self.make_folders(&key)?;
        let bytes = item.text.as_bytes();
        match create_file_observed(&self.file_path(&key, &key.job), bytes, &NoFileFaults) {
            Ok(()) => {
                log_written(&key, super::AuthoredOutcome::Created, "shared successor created");
                Ok(ContentHash::from(Digested(bytes)))
            }
            // Another writer's file arrived after the checks, which the filesystem refused to replace.
            Err(FsError::PersistTemporary { source, .. }) if source.kind() == io::ErrorKind::AlreadyExists => {
                Err(AuthoringError::JobExists { key: key.to_string() })
            }
            Err(error) => Err(error.into()),
        }
    }
}
