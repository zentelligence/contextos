//! A job's place among releases as its page shows it: the release it names and whether that is current, previous, future,
//! or retired, the library lock the release runs with, the job it succeeded and the job that succeeded it, a successor whose
//! link to its predecessor no longer holds, and why a job file that cannot be used cannot.
//!
//! The facts come from the same listing and report the workflow page is made from, so the two cannot disagree.

use askama::Template;
use contextos_workflows::{JobsState, LinkRefusal, PredecessorKey, ReleaseKind};

use crate::release_views::{JobLink, in_use};
use crate::shared_job_authoring::JobListing;
use crate::workflow_releases::WorkflowReleases;
use crate::workflow_releases_report::summary;

/// What the view is made from.
pub(crate) struct JobReleaseInput<'a> {
    pub(crate) package: &'a str,
    pub(crate) workflow: &'a str,
    pub(crate) job: &'a str,
    /// Every job of the store the job is in.
    pub(crate) listings: &'a [JobListing],
    /// The workflow's releases.
    pub(crate) report: &'a WorkflowReleases,
    /// The store's state file.
    pub(crate) state: &'a JobsState,
    /// The address of the job with the given key.
    pub(crate) job_href: &'a dyn Fn(&str) -> String,
}

/// The release a job names.
pub(crate) struct NamedRelease {
    /// The kind and install date: `Previous, installed 3 November 2026`.
    pub(crate) summary: String,
    pub(crate) version: String,
    /// The library releases the release is locked to, as `name version`.
    pub(crate) lock: Vec<String>,
    pub(crate) no_lock_yet: bool,
    /// What stops the release being read whole, with the word for its condition.
    pub(crate) problem_kind: &'static str,
    pub(crate) problems: Vec<String>,
}

/// A job's release section.
#[derive(Template)]
#[template(path = "job_release.html")]
pub(crate) struct JobReleaseView {
    /// The release the job names and the store holds.
    pub(crate) release: Option<NamedRelease>,
    /// What to say when the job names a release the store does not hold, or none.
    pub(crate) no_release: Option<String>,
    pub(crate) predecessor: Option<JobLink>,
    pub(crate) successor: Option<JobLink>,
    /// Why the predecessor the job's file names is not the job its link recorded.
    pub(crate) link_mismatch: Option<String>,
    /// Why the job's file cannot be used, one reason each.
    pub(crate) file_problems: Vec<String>,
    /// Whether the job is in use on a release that is no longer current and nothing has taken its place.
    pub(crate) needs_successor: bool,
}

impl From<JobReleaseInput<'_>> for JobReleaseView {
    fn from(input: JobReleaseInput<'_>) -> Self {
        let row = input.listings.iter().find(|row| {
            row.key.package == input.package && row.key.workflow == input.workflow && row.key.job == input.job
        });
        let named = row.and_then(|row| row.version.as_ref());
        let line = named.and_then(|version| input.report.releases.iter().find(|line| line.version == *version));
        let release = line.map(|line| NamedRelease {
            summary: summary(line),
            version: line.version.to_string(),
            lock: line
                .lock
                .iter()
                .flatten()
                .map(|(name, locked)| format!("{name} {}", locked.version))
                .collect(),
            no_lock_yet: line.lock.is_none(),
            problem_kind: line.condition.word(),
            problems: line.condition.problems(),
        });
        let no_release = match (&release, named, row) {
            (Some(_), _, _) | (None, None, None) => None,
            (None, Some(version), _) => Some(format!("This job names release {version}, which is not installed.")),
            (None, None, Some(_)) => Some("This job's file names no release.".to_owned()),
        };
        let job_link = |key: &str| JobLink {
            name: key.to_owned(),
            href: (input.job_href)(key),
        };
        let link_mismatch = row
            .and_then(|row| row.predecessor.as_deref())
            .and_then(|predecessor| PredecessorKey::try_from(predecessor).ok())
            .and_then(|predecessor| {
                match input
                    .state
                    .check_link(input.package, input.workflow, input.job, &predecessor)
                {
                    Err(refusal @ LinkRefusal::IdentityChanged { .. }) => Some(refusal.to_string()),
                    Err(_) | Ok(_) => None,
                }
            });
        Self {
            needs_successor: line.is_some_and(|line| matches!(line.kind, ReleaseKind::Previous | ReleaseKind::Retired))
                && row.is_some_and(|row| in_use(row.lifecycle) && row.successor.is_none()),
            release,
            no_release,
            predecessor: row.and_then(|row| row.predecessor.as_deref()).map(job_link),
            // A retired job's page names the job that took its place, though no job stands as its successor.
            successor: row
                .and_then(|row| row.successor.as_deref().or(row.replaced_by.as_deref()))
                .map(job_link),
            link_mismatch,
            file_problems: row
                .into_iter()
                .flat_map(|row| &row.problems)
                .map(|problem| format!("{} ({})", problem.message, problem.code))
                .collect(),
        }
    }
}

#[cfg(test)]
#[path = "job_release_view_test.rs"]
mod tests;
