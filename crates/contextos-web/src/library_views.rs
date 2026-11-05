//! A store's library releases and the workflow releases locked to each, as a page shows them.
//!
//! A library fix reaches a workflow only through a new workflow release, so an older library release can stay in use for
//! as long as a workflow release is locked to it. This lists each library release with the workflow releases whose lock
//! names it, and marks an older one that is still locked.

use std::collections::BTreeMap;

use askama::Template;
use contextos_workflows::ReleaseRef;
use semver::Version;

use crate::library_dependents::{Carried, DependentsBlock, DependentsInput, dependents_of, element_id};
use crate::library_status::{Check, LibraryStatus};
use crate::package_standing::StoreStanding;
use crate::shared_claim::SharedJobKey;
use crate::shared_job_authoring::JobListing;
use crate::workflow_releases_report::kind_word;

/// A workflow release locked to a library release.
pub(crate) struct LockHolder {
    /// `ops sync, current`: the package and workflow, and whether the release is current, previous, future, or retired.
    pub(crate) label: String,
    /// The workflow's page.
    pub(crate) href: String,
}

/// One check of a library release as the page shows it.
pub(crate) struct CheckRow {
    pub(crate) label: &'static str,
    /// `check-passed`, `check-failed`, or `check-skipped`.
    pub(crate) class: &'static str,
    pub(crate) word: &'static str,
    /// What was found, or why the check was not made.
    pub(crate) lines: Vec<String>,
}

impl CheckRow {
    fn new(label: &'static str, check: &Check) -> Self {
        let (class, word, lines) = match check {
            Check::Passed => ("check-passed", "Passed", Vec::new()),
            Check::Failed(found) => ("check-failed", "Failed", found.clone()),
            Check::NotChecked(reason) => ("check-skipped", "Not checked", vec![reason.clone()]),
        };
        Self {
            label,
            class,
            word,
            lines,
        }
    }
}

impl From<&LibraryStatus> for Vec<CheckRow> {
    fn from(status: &LibraryStatus) -> Self {
        vec![
            CheckRow::new("Manifest", &status.manifest),
            CheckRow::new("Syntax", &status.syntax),
            CheckRow::new("Imports", &status.imports),
            CheckRow::new("Declared tools", &status.declared_tools),
        ]
    }
}

/// One library release.
pub(crate) struct LibraryRelease {
    /// The element's id.
    pub(crate) id: String,
    pub(crate) library: String,
    pub(crate) version: Version,
    /// Whether the release is older than the newest release of its library and a workflow release is still locked to it.
    pub(crate) older_but_locked: bool,
    pub(crate) holders: Vec<LockHolder>,
    /// What the release checks as; none when the page is built without making the checks.
    pub(crate) checks: Vec<CheckRow>,
    /// The jobs on the workflow releases locking it, and the actions on them.
    pub(crate) dependents: DependentsBlock,
}

/// A store's library releases, or why they could not be read.
#[derive(Template)]
#[template(path = "library_releases.html")]
pub(crate) struct LibrariesSection {
    pub(crate) libraries: Vec<LibraryRelease>,
    /// Set when the store could not be read, in place of the list.
    pub(crate) unavailable: Option<String>,
}

/// What the section is made from.
pub(crate) struct LibrariesInput<'a> {
    pub(crate) standing: &'a StoreStanding,
    /// The address of the page of a workflow, from its package and its name.
    pub(crate) workflow_href: &'a dyn Fn(&str, &str) -> String,
    /// What each library release checks as; a release with none is listed without its checks.
    pub(crate) statuses: &'a BTreeMap<ReleaseRef, LibraryStatus>,
    /// Every job of the store.
    pub(crate) jobs: &'a [JobListing],
    /// The address of a library release, from its name and version, which its actions are beneath.
    pub(crate) release_href: &'a dyn Fn(&str, &str) -> String,
    /// The address of a job's page.
    pub(crate) job_href: &'a dyn Fn(&SharedJobKey) -> String,
}

impl From<LibrariesInput<'_>> for LibrariesSection {
    fn from(input: LibrariesInput<'_>) -> Self {
        let standing = input.standing;
        let mut libraries: Vec<LibraryRelease> = standing
            .releases
            .iter()
            .filter(|held| held.is_library())
            .map(|library| {
                let name = library.release.name.as_str();
                let newest = standing
                    .releases
                    .iter()
                    .filter(|other| other.is_library() && other.release.name == library.release.name)
                    .map(|other| &other.release.version)
                    .max();
                let holders: Vec<LockHolder> = standing
                    .releases
                    .iter()
                    .flat_map(|held| {
                        held.contents
                            .iter()
                            .flat_map(|contents| &contents.workflows)
                            .filter(|workflow| {
                                workflow.lock.iter().flatten().any(|(locked, release)| {
                                    locked == name && release.version == library.release.version
                                })
                            })
                            .map(move |workflow| (held, workflow))
                    })
                    .map(|(held, workflow)| LockHolder {
                        label: format!(
                            "{} {}, {}",
                            held.release.name,
                            workflow.workflow,
                            kind_word(workflow.kind)
                        ),
                        href: (input.workflow_href)(held.release.name.as_str(), &workflow.workflow),
                    })
                    .collect();
                let version = library.release.version.clone();
                let id = element_id(name, &version.to_string());
                LibraryRelease {
                    dependents: DependentsBlock::from(DependentsInput {
                        id: format!("{id}-jobs"),
                        base: (input.release_href)(name, &version.to_string()),
                        dependents: &dependents_of(standing, input.jobs, name, &library.release.version),
                        href: input.job_href,
                        carried: Carried::default(),
                    }),
                    checks: input
                        .statuses
                        .get(&library.release)
                        .map(Vec::<CheckRow>::from)
                        .unwrap_or_default(),
                    id,
                    library: name.to_owned(),
                    version,
                    older_but_locked: newest.is_some_and(|newest| *newest > library.release.version)
                        && !holders.is_empty(),
                    holders,
                }
            })
            .collect();
        libraries.sort_by(|left, right| (&left.library, &left.version).cmp(&(&right.library, &right.version)));
        Self {
            libraries,
            unavailable: None,
        }
    }
}

impl LibrariesSection {
    /// The section for a store that could not be read.
    pub(crate) fn unreadable(reason: &str) -> Self {
        Self {
            libraries: Vec::new(),
            unavailable: Some(reason.to_owned()),
        }
    }
}

#[cfg(test)]
#[path = "library_views_test.rs"]
mod tests;
