//! A workflow's releases as its page shows them: each release as current, previous, future, or retired, with the jobs on
//! it by lifecycle stage, the jobs that need a successor, and anything that stops the release being read whole.
//!
//! The facts come from the `workflows releases` report, which already knows each release's kind, install date, lock,
//! condition, and missing templates. This module adds the jobs themselves, by name and address, and the flags a page
//! raises from them. People are told a release's kind and when it was installed; its version appears only in its details.

use askama::Template;
use contextos_workflows::{JobLifecycle, ReleaseKind};

use crate::shared_job_authoring::JobListing;
use crate::workflow_releases::{ReleaseLine, STAGES, WorkflowReleases};
use crate::workflow_releases_report::{Day, cause_word, kind_word, origin_word, summary};

/// What the section is built from.
pub(crate) struct ReleasesInput<'a> {
    /// The workflow's releases.
    pub(crate) report: &'a WorkflowReleases,
    /// Every job in the store the report was read from.
    pub(crate) jobs: &'a [JobListing],
    /// The address of the job with the given key.
    pub(crate) job_href: &'a dyn Fn(&str) -> String,
}

/// A job as the section names it.
pub(crate) struct JobLink {
    pub(crate) name: String,
    pub(crate) href: String,
}

/// The jobs of one release in one lifecycle stage.
pub(crate) struct StageJobs {
    pub(crate) stage: &'static str,
    pub(crate) jobs: Vec<JobLink>,
}

/// One release.
pub(crate) struct ReleaseView {
    /// `current`, `previous`, `future`, or `retired`, for styling.
    pub(crate) kind: &'static str,
    /// The kind and the install date, as a sentence head: `Current, installed 3 November 2026`.
    pub(crate) summary: String,
    /// Set for a current release with no job `enabled` or `suspended`.
    pub(crate) nothing_running: bool,
    pub(crate) stages: Vec<StageJobs>,
    /// The `enabled` or `suspended` jobs on a previous or retired release that have no successor.
    pub(crate) needs_successor: Vec<JobLink>,
    /// What stops the release being read whole, with the word for its condition.
    pub(crate) problem_kind: &'static str,
    pub(crate) problems: Vec<String>,
    pub(crate) missing_templates: Vec<String>,
    /// The release's details, shown on request: its version first.
    pub(crate) version: String,
    pub(crate) origin: Option<&'static str>,
    pub(crate) retired: Option<String>,
    /// The library releases it is locked to, as `name version`, or why it has no lock.
    pub(crate) lock: Vec<String>,
    pub(crate) no_lock_yet: bool,
}

/// A workflow's releases, or why they could not be read.
#[derive(Template)]
#[template(path = "workflow_releases.html")]
pub(crate) struct ReleasesSection {
    pub(crate) releases: Vec<ReleaseView>,
    /// Set when the releases could not be read, in place of the list.
    pub(crate) unavailable: Option<String>,
}

/// A job as its release names it: by its name when its file loads, else by its key.
fn link_to(job: &JobListing, href: &dyn Fn(&str) -> String) -> JobLink {
    JobLink {
        name: job.name.clone().unwrap_or_else(|| job.key.job.clone()),
        href: href(&job.key.job),
    }
}

/// Whether a job is in use: the scheduler may start it, or it is paused but may be resumed.
pub(crate) const fn in_use(lifecycle: JobLifecycle) -> bool {
    matches!(lifecycle, JobLifecycle::Enabled | JobLifecycle::Suspended)
}

impl From<(&ReleaseLine, &ReleasesInput<'_>)> for ReleaseView {
    fn from((line, input): (&ReleaseLine, &ReleasesInput<'_>)) -> Self {
        let on_release = |job: &&JobListing| {
            job.key.package == input.report.package
                && job.key.workflow == input.report.workflow
                && job.version.as_ref() == Some(&line.version)
        };
        let jobs: Vec<&JobListing> = input.jobs.iter().filter(on_release).collect();
        let stages = STAGES
            .into_iter()
            .map(|stage| StageJobs {
                stage: stage.as_str(),
                jobs: jobs
                    .iter()
                    .filter(|job| job.lifecycle == stage)
                    .map(|job| link_to(job, input.job_href))
                    .collect(),
            })
            .filter(|stage| !stage.jobs.is_empty())
            .collect();
        let past = matches!(line.kind, ReleaseKind::Previous | ReleaseKind::Retired);
        Self {
            kind: kind_word(line.kind),
            summary: summary(line),
            nothing_running: line.kind == ReleaseKind::Current && !jobs.iter().any(|job| in_use(job.lifecycle)),
            stages,
            needs_successor: jobs
                .iter()
                .filter(|job| past && in_use(job.lifecycle) && job.successor.is_none())
                .map(|job| link_to(job, input.job_href))
                .collect(),
            problem_kind: line.condition.word(),
            problems: line.condition.problems(),
            missing_templates: line.missing_templates.clone(),
            version: line.version.to_string(),
            origin: line.origin.map(origin_word),
            retired: line
                .retired
                .as_ref()
                .map(|marker| format!("{}, {}", Day(marker.at), cause_word(marker.cause))),
            lock: line
                .lock
                .iter()
                .flatten()
                .map(|(name, locked)| format!("{name} {}", locked.version))
                .collect(),
            no_lock_yet: line.lock.is_none(),
        }
    }
}

impl From<ReleasesInput<'_>> for ReleasesSection {
    fn from(input: ReleasesInput<'_>) -> Self {
        Self {
            releases: input
                .report
                .releases
                .iter()
                .map(|line| ReleaseView::from((line, &input)))
                .collect(),
            unavailable: None,
        }
    }
}

impl ReleasesSection {
    /// The section for releases that could not be read.
    pub(crate) fn unreadable(reason: &str) -> Self {
        Self {
            releases: Vec::new(),
            unavailable: Some(reason.to_owned()),
        }
    }
}

#[cfg(test)]
#[path = "release_views_test.rs"]
mod tests;
