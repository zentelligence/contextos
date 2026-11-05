//! What the job files of a store's home say, for retention: which release each names, or that it cannot be read.
//!
//! Both homes read a job file by the one rule: a file that is not TOML cannot be understood, so retention keeps every
//! release of its workflow; a file that parses and names an exact release names it; one that parses and names none
//! (no `version`, a range, build metadata) names nothing, which keeps nothing. The `version` is read on its own: it names
//! the release whether or not the file's `predecessor` is usable.

use std::future::Future;
use std::path::PathBuf;

use contextos_workflows::{JobFile, JobFileReading, JobFileRelease, JobFileText, JobsState, PackageName, WorkflowName};

use crate::job_discovery::{WorkflowFolder, list_job_files, list_registry_directories};
use crate::mcp_client::McpClient;
use crate::package_retention::{RetentionError, RetentionHome, StateReading};
use crate::saved_jobs::SavedJobs;
use crate::scheduler::read_state;
use crate::shared_claim::SharedJobKey;
use crate::shared_jobs::{JobFileEntry, list_shared_job_entries};
use crate::source_read::{SourceRead, read_source};

/// The text of a job file, as read.
pub(crate) struct JobFileSource<'a>(pub(crate) &'a str);

impl From<JobFileSource<'_>> for JobFileReading {
    fn from(source: JobFileSource<'_>) -> Self {
        let Ok(table) = toml::from_str::<toml::Table>(source.0) else {
            return Self::Unreadable;
        };
        // The version names the release whatever else the file says, so a predecessor that is malformed does not hide it.
        table
            .get("version")
            .and_then(toml::Value::as_str)
            .and_then(|version| {
                JobFileRelease::try_from(JobFileText {
                    version,
                    predecessor: None,
                })
                .ok()
            })
            .map_or(Self::NamesNoRelease, |named| {
                Self::Names(named.version.as_semver().clone())
            })
    }
}

/// The host's home: its jobs folder, and the state file as the caller holds it under the state lock.
pub(crate) struct HostHome {
    pub state: JobsState,
    pub jobs_root: PathBuf,
    recorded: bool,
}

impl HostHome {
    /// The host's home with `state`, which the caller holds under the state lock. The host has a state file when one is
    /// on disk: the caller's own change is written after it, so it does not make a missing file one.
    pub(crate) fn of(state: JobsState, saved: &SavedJobs) -> Self {
        Self {
            recorded: saved.state.state_path().exists(),
            state,
            jobs_root: saved.jobs_root.clone(),
        }
    }
}

impl RetentionHome for HostHome {
    fn state(&self) -> impl Future<Output = Result<StateReading, RetentionError>> + Send {
        std::future::ready(Ok(StateReading {
            state: self.state.clone(),
            recorded: self.recorded,
        }))
    }

    async fn job_files(&self) -> Result<Vec<JobFile>, RetentionError> {
        let jobs_root = self.jobs_root.clone();
        let entries = tokio::task::spawn_blocking(move || list_shared_job_entries(&jobs_root)).await??;
        Ok(entries.iter().filter_map(host_job_file).collect())
    }
}

/// The job file an entry of the jobs folder is. A file whose name is not a job's cannot be one and is left out; one
/// that cannot be used, or whose own `slug` is not usable, cannot be understood.
fn host_job_file(entry: &JobFileEntry) -> Option<JobFile> {
    let (key, reading) = match entry {
        JobFileEntry::Usable(file) => match file.tracked_key() {
            Some(key) => (key, JobFileReading::from(JobFileSource(&file.text))),
            None => (file.key.clone(), JobFileReading::Unreadable),
        },
        JobFileEntry::Unusable { key, .. } => (key.clone(), JobFileReading::Unreadable),
        JobFileEntry::Misnamed { .. } => return None,
    };
    let SharedJobKey { package, workflow, job } = key;
    Some(JobFile {
        package,
        workflow,
        job,
        reading,
    })
}

/// A vault's home: its `registry/workflows/` and `.contextos/jobs.toml`, both read through the primary server.
pub(crate) struct VaultHome<'a> {
    pub client: &'a McpClient,
    pub vault: &'a str,
}

impl RetentionHome for VaultHome<'_> {
    async fn state(&self) -> Result<StateReading, RetentionError> {
        read_state(self.client, self.vault)
            .await
            .map(|snapshot| StateReading {
                recorded: snapshot.hash.is_some(),
                state: snapshot.value,
            })
            .map_err(RetentionError::from)
    }

    async fn job_files(&self) -> Result<Vec<JobFile>, RetentionError> {
        let mut files = Vec::new();
        for package in list_registry_directories(self.client, self.vault, "registry/workflows").await? {
            if PackageName::try_from(package.as_str()).is_err() {
                continue;
            }
            for workflow in
                list_registry_directories(self.client, self.vault, &format!("registry/workflows/{package}")).await?
            {
                if WorkflowName::try_from(workflow.as_str()).is_err() {
                    continue;
                }
                let folder = WorkflowFolder {
                    package: &package,
                    directory: &workflow,
                };
                for file_name in list_job_files(self.client, self.vault, folder).await? {
                    let Some(job) = file_name.strip_suffix(".toml") else {
                        continue;
                    };
                    let path = format!("{}://registry/workflows/{folder}/jobs/{file_name}", self.vault);
                    let reading = match read_source(self.client, &path).await? {
                        SourceRead::Text(text) => JobFileReading::from(JobFileSource(&text)),
                        SourceRead::Unusable(_) => JobFileReading::Unreadable,
                        SourceRead::Missing => continue,
                    };
                    files.push(JobFile {
                        package: package.clone(),
                        workflow: workflow.clone(),
                        job: job.to_owned(),
                        reading,
                    });
                }
            }
        }
        Ok(files)
    }
}

#[cfg(test)]
#[path = "retention_job_files_test.rs"]
mod tests;
