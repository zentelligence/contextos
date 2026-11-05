//! Listing the job files and stored job state of the host: one row per file and one for each stored entry that has no
//! file, with the release each names, whether it is current, previous, or future, and the jobs it is linked to.

use contextos_core::ContentHash;
use contextos_workflows::{JobLifecycle, JobsState, ReleaseKind, ReleaseKinds, ValidationCode};
use semver::Version;

use crate::shared_claim::SharedJobKey;
use crate::shared_job_authoring::{AuthoringError, Digested, SharedJobAuthoring};
use crate::shared_jobs::{JobFileEntry, SharedJob, SharedJobFile, SharedJobSource, list_shared_job_entries};

/// Something wrong with one listed job.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListingProblem {
    /// The stable code: a `validation/` code when the file does not load, `workflow/job-key-taken` when another file
    /// has the same key, and `workflow/job-not-found` when stored state has no file.
    pub code: String,
    pub message: String,
}

/// One row of `list`: a job file, or stored state that has no file.
#[derive(Clone, Debug, PartialEq)]
pub struct JobListing {
    pub key: SharedJobKey,
    /// The file's stem, only when it differs from the job's key.
    pub file: Option<String>,
    pub name: Option<String>,
    pub lifecycle: JobLifecycle,
    pub schedule: Option<String>,
    pub ready: Option<bool>,
    pub version: Option<Version>,
    /// Whether the release the file names is current, previous, or future in its workflow.
    pub release_kind: Option<ReleaseKind>,
    /// The key of the job this one succeeds.
    pub predecessor: Option<String>,
    /// The key of the job that succeeds this one and has not been retired.
    pub successor: Option<String>,
    /// The key of the job the job's retirement record names as having taken its place, kept after that job is itself
    /// retired; `None` for a job nothing retired by succession.
    pub replaced_by: Option<String>,
    /// `None` when there is no file.
    pub hash: Option<ContentHash>,
    pub problems: Vec<ListingProblem>,
    /// Why the job is suspended, while it is: its code, who suspended it, and the run that failed.
    pub suspension: Option<contextos_workflows::SuspensionRecord>,
}

impl SharedJobAuthoring {
    /// Lists every job file and every stored state entry once, without taking the lock.
    ///
    /// Blocking: call it from a blocking thread.
    ///
    /// # Errors
    ///
    /// Returns the [`AuthoringError`] that says why the listing could not be made.
    pub fn list(&self) -> Result<Vec<JobListing>, AuthoringError> {
        let entries = list_shared_job_entries(&self.jobs_root)
            .map_err(|error| AuthoringError::unavailable("the jobs folder could not be listed", &error))?;
        let state = self.state.read()?;
        let mut rows: Vec<JobListing> = entries.iter().map(|entry| listing_of(entry, &state)).collect();
        for row_index in 0..rows.len() {
            let others: Vec<String> = rows
                .iter()
                .enumerate()
                .filter(|(index, other)| *index != row_index && other.key == rows[row_index].key)
                .map(|(_, other)| other.file.clone().unwrap_or_else(|| other.key.job.clone()))
                .collect();
            if let Some(row) = rows.get_mut(row_index)
                && !others.is_empty()
            {
                let message = format!("the job key is shared with {}", others.join(", "));
                row.problems.push(ListingProblem {
                    code: "workflow/job-key-taken".to_owned(),
                    message,
                });
            }
        }
        fill_successors(&mut rows);
        for (package, workflow, job) in state.recorded_jobs() {
            if rows
                .iter()
                .any(|row| row.key.package == package && row.key.workflow == workflow && row.key.job == job)
            {
                continue;
            }
            rows.push(JobListing {
                key: SharedJobKey {
                    package: package.to_owned(),
                    workflow: workflow.to_owned(),
                    job: job.to_owned(),
                },
                file: None,
                name: None,
                lifecycle: state.lifecycle_of(package, workflow, job),
                schedule: state
                    .job(package, workflow, job)
                    .and_then(|stored| stored.schedule())
                    .map(str::to_owned),
                ready: None,
                version: None,
                release_kind: None,
                predecessor: None,
                successor: None,
                replaced_by: None,
                hash: None,
                problems: vec![ListingProblem {
                    code: "workflow/job-not-found".to_owned(),
                    message: "stored state has no job file".to_owned(),
                }],
                suspension: state
                    .job(package, workflow, job)
                    .and_then(|stored| stored.suspension())
                    .cloned(),
            });
        }
        fill_replacements(&mut rows, &state);
        rows.sort_by(|a, b| {
            (&a.key.package, &a.key.workflow, &a.key.job, &a.file).cmp(&(
                &b.key.package,
                &b.key.workflow,
                &b.key.job,
                &b.file,
            ))
        });
        Ok(rows)
    }
}

/// Names, on each row, the job of the same workflow that names it as its predecessor and has not been retired.
fn fill_successors(rows: &mut [JobListing]) {
    let successors: Vec<(SharedJobKey, String)> = rows
        .iter()
        .filter(|row| row.lifecycle != JobLifecycle::Retired)
        .filter_map(|row| {
            let predecessor = row.predecessor.as_ref()?;
            Some((
                SharedJobKey {
                    package: row.key.package.clone(),
                    workflow: row.key.workflow.clone(),
                    job: predecessor.clone(),
                },
                row.key.job.clone(),
            ))
        })
        .collect();
    for row in rows.iter_mut() {
        row.successor = successors
            .iter()
            .find(|(predecessor, _)| *predecessor == row.key)
            .map(|(_, successor)| successor.clone());
    }
}

/// Names, on each row of a job that a successor's enabling retired, the job its retirement record says took its place.
/// The record outlives the job's file and its place in the chain, so it finds the successor of a job that is itself retired.
pub(crate) fn fill_replacements(rows: &mut [JobListing], state: &JobsState) {
    for row in rows.iter_mut() {
        row.replaced_by = state
            .job(&row.key.package, &row.key.workflow, &row.key.job)
            .and_then(|stored| stored.retirement())
            .map(|retirement| retirement.replaced_by.job.clone());
    }
}

/// One file's row: what it loads as, or why it does not, and the state stored under its tracked key.
fn listing_of(entry: &JobFileEntry, state: &JobsState) -> JobListing {
    match entry {
        JobFileEntry::Usable(file) => usable_listing(file, state),
        JobFileEntry::Unusable { key, reason } => problem_listing(
            key,
            state,
            ListingProblem {
                code: ValidationCode::ArtefactInvalid.as_str().to_owned(),
                message: format!("the file is {reason}"),
            },
        ),
        JobFileEntry::Misnamed { key } => problem_listing(
            key,
            state,
            ListingProblem {
                code: "workflow/job-key-invalid".to_owned(),
                message: "the file name is not a usable job name: use lowercase letters, digits, and hyphens"
                    .to_owned(),
            },
        ),
    }
}

/// A row for a file that cannot be loaded and has no manifest to read anything else from.
fn problem_listing(key: &SharedJobKey, state: &JobsState, problem: ListingProblem) -> JobListing {
    JobListing {
        key: key.clone(),
        file: None,
        name: None,
        lifecycle: state.lifecycle_of(&key.package, &key.workflow, &key.job),
        schedule: None,
        ready: None,
        version: None,
        release_kind: None,
        predecessor: None,
        successor: None,
        replaced_by: None,
        hash: None,
        problems: vec![problem],
        suspension: state
            .job(&key.package, &key.workflow, &key.job)
            .and_then(|stored| stored.suspension())
            .cloned(),
    }
}

fn usable_listing(file: &SharedJobFile, state: &JobsState) -> JobListing {
    let key = file.tracked_key().unwrap_or_else(|| file.key.clone());
    let loaded = SharedJob::try_from(SharedJobSource {
        key: file.key.clone(),
        text: &file.text,
    });
    let (name, ready, release, problems) = match loaded {
        Ok(job) => (Some(job.name), Some(job.ready), Some(job.release), Vec::new()),
        Err(error) => {
            let cause = AuthoringError::from(&error);
            let code = cause.cause().unwrap_or(ValidationCode::ArtefactInvalid.as_str());
            (
                None,
                None,
                None,
                vec![ListingProblem {
                    code: code.to_owned(),
                    message: error.to_string(),
                }],
            )
        }
    };
    JobListing {
        file: (file.key.job != key.job).then(|| file.key.job.clone()),
        suspension: state
            .job(&key.package, &key.workflow, &key.job)
            .and_then(|stored| stored.suspension())
            .cloned(),
        lifecycle: state.lifecycle_of(&key.package, &key.workflow, &key.job),
        schedule: state
            .job(&key.package, &key.workflow, &key.job)
            .and_then(|stored| stored.schedule())
            .map(str::to_owned),
        hash: Some(ContentHash::from(Digested(file.text.as_bytes()))),
        name,
        ready,
        release_kind: release
            .as_ref()
            .map(|release| state.release_kind(&key.package, &key.workflow, release.version.as_semver())),
        version: release.as_ref().map(|release| release.version.as_semver().clone()),
        predecessor: release
            .and_then(|release| release.predecessor)
            .map(|predecessor| predecessor.as_str().to_owned()),
        successor: None,
        replaced_by: None,
        problems,
        key,
    }
}
