//! An index of the host's shared workflows and every job file for them, whether or not the job loads.
//!
//! A shared workflow is a workflow of an installed package, and a shared job is a job file under the host's jobs folder.
//! The index joins the two by package and workflow slug: a workflow with no jobs is listed, a job whose workflow is no longer
//! installed is listed, and a job file that does not load is listed unloaded, so an operator can see each and validate it. A
//! package's workflows come from its newest installed release.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use semver::Version;
use thiserror::Error;

use crate::package_store::{self, PackageStoreError};
use crate::shared_claim::SharedJobKey;
use crate::shared_jobs::{SharedJob, SharedJobSource, list_shared_job_files, without_clashing_keys};

/// One shared workflow, with every job file for it.
#[derive(Debug)]
pub struct SharedWorkflow {
    pub package: String,
    pub slug: String,
    /// The newest installed release of the package that defines the workflow, or `None` when no installed package does.
    pub release: Option<Version>,
    /// The workflow's entry script in that release.
    pub entry: Option<String>,
    /// Every job file, in slug order.
    pub jobs: Vec<SharedIndexedJob>,
}

/// One shared job file.
#[derive(Debug)]
pub struct SharedIndexedJob {
    /// The key the job is tracked under, which a `slug` in its file can make differ from its file's name.
    pub key: SharedJobKey,
    pub slug: String,
    /// The job, when its file loads.
    pub loaded: Option<SharedJob>,
}

/// The newest installed release of one package, and the workflows it defines with their entry scripts.
struct NewestRelease {
    version: Version,
    workflows: Vec<(String, String)>,
}

#[derive(Debug, Error)]
pub enum SharedIndexError {
    #[error("the package store could not be read: {0}")]
    Packages(#[from] PackageStoreError),
    #[error("the shared jobs could not be listed: {0}")]
    Jobs(#[from] io::Error),
}

/// Indexes every shared workflow and job, in package then workflow order.
///
/// Blocking: reads the package store and the jobs folder, so call it from a blocking thread.
///
/// # Errors
///
/// Returns [`SharedIndexError::Packages`] when the package store cannot be read, and [`SharedIndexError::Jobs`] when the
/// jobs folder cannot be listed.
pub fn index_shared(store_root: &Path, jobs_root: &Path) -> Result<Vec<SharedWorkflow>, SharedIndexError> {
    let mut workflows: BTreeMap<(String, String), SharedWorkflow> = BTreeMap::new();
    let mut newest: BTreeMap<String, NewestRelease> = BTreeMap::new();
    let inventory = package_store::inventory(store_root)?;
    for unreadable in &inventory.unreadable {
        tracing::warn!(package = %unreadable.name, version = %unreadable.version, "an installed release could not be read");
    }
    for release in &inventory.releases {
        let manifest = &release.manifest;
        let name = manifest.identity.name.to_string();
        let is_newer = newest
            .get(&name)
            .is_none_or(|kept| manifest.identity.version > kept.version);
        if is_newer {
            newest.insert(
                name,
                NewestRelease {
                    version: manifest.identity.version.clone(),
                    workflows: release
                        .kind
                        .workflows()
                        .iter()
                        .map(|folder| (folder.clone(), release.entry_of(folder).unwrap_or_default()))
                        .collect(),
                },
            );
        }
    }
    for (package, summary) in newest {
        for (slug, entry) in summary.workflows {
            workflows.insert(
                (package.clone(), slug.clone()),
                SharedWorkflow {
                    package: package.clone(),
                    slug,
                    release: Some(summary.version.clone()),
                    entry: Some(entry),
                    jobs: Vec::new(),
                },
            );
        }
    }
    for file in without_clashing_keys(list_shared_job_files(jobs_root)?) {
        let Some(key) = file.tracked_key() else {
            continue;
        };
        let loaded = SharedJob::try_from(SharedJobSource {
            key: file.key.clone(),
            text: &file.text,
        })
        .ok();
        workflows
            .entry((key.package.clone(), key.workflow.clone()))
            .or_insert_with(|| SharedWorkflow {
                package: key.package.clone(),
                slug: key.workflow.clone(),
                release: None,
                entry: None,
                jobs: Vec::new(),
            })
            .jobs
            .push(SharedIndexedJob {
                slug: key.job.clone(),
                key,
                loaded,
            });
    }
    let mut indexed: Vec<SharedWorkflow> = workflows.into_values().collect();
    for workflow in &mut indexed {
        workflow.jobs.sort_by(|a, b| a.slug.cmp(&b.slug));
    }
    Ok(indexed)
}

#[cfg(test)]
#[path = "shared_index_test.rs"]
mod tests;
