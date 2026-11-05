//! What `workflows releases` reports about one workflow: each release of its package that the store holds or the state
//! records, whether it is current, previous, future, or retired, when it was installed, whether it can be read, the
//! library releases it is locked to, and the jobs on it by stage.
//!
//! Which kind a release is comes from the state file's recorded values through [`ReleaseKinds`]; nothing here works it
//! out again. The facts about each release are gathered above the package store port, so a vault's store and the host's
//! are reported by this one code.

use contextos_packages::ReleaseOrigin;
use contextos_workflows::{JobLifecycle, JobsState, LockedLibrary, ReleaseKind, ReleaseKinds as _, RetiredRelease};
use semver::Version;
use time::OffsetDateTime;

use crate::shared_job_authoring::JobListing;

/// Whether a release can be read from the store, and if not, why. Told apart by what is present, never by cause.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReleaseCondition {
    /// The release is whole and verified.
    Whole,
    /// There is no folder for the release.
    Removed,
    /// Something listed is absent or something unlisted is present, or there is no manifest. Each finding is one line.
    Incomplete(Vec<String>),
    /// The release is complete but wrong. Each finding is one line.
    Damaged(Vec<String>),
    /// The release could not be verified, for the reason given.
    Unverifiable(String),
}

impl From<&contextos_packages::ReadCheck<crate::package_release_manifest::PackageRelease>> for ReleaseCondition {
    fn from(check: &contextos_packages::ReadCheck<crate::package_release_manifest::PackageRelease>) -> Self {
        use contextos_packages::ReadCheck;
        match check {
            ReadCheck::Verified(_) => Self::Whole,
            ReadCheck::Removed => Self::Removed,
            ReadCheck::Incomplete(findings) => Self::Incomplete(findings.iter().map(ToString::to_string).collect()),
            ReadCheck::Damaged(findings) => Self::Damaged(findings.iter().map(ToString::to_string).collect()),
            ReadCheck::Unverifiable(reason) => Self::Unverifiable(reason.to_string()),
        }
    }
}

/// What the store and its ledger say about one release, before the state file is consulted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseFacts {
    /// The release's version.
    pub version: Version,
    /// Whether it can be read.
    pub condition: ReleaseCondition,
    /// Whether the release lists the workflow asked about. Only a whole release can say; any other holds `true`.
    pub holds_workflow: bool,
    /// When the ledger says it was installed, when it records that.
    pub installed_at: Option<OffsetDateTime>,
    /// Where its content came from, when the ledger records that.
    pub origin: Option<ReleaseOrigin>,
    /// The templates the release declares that are no longer where a run reads them. Always empty for a release whose
    /// templates are held in the release itself.
    pub missing_templates: Vec<String>,
}

/// What is asked of the report builder: the workflow, the store's facts, the state, and the store's jobs.
pub struct ReleasesInput<'a> {
    /// The store: `shared`, or the vault's name.
    pub store: &'a str,
    /// The package.
    pub package: &'a str,
    /// The workflow.
    pub workflow: &'a str,
    /// The facts about every release the store holds or the state records.
    pub facts: Vec<ReleaseFacts>,
    /// The state file, which records which release is current, previous, or future.
    pub state: &'a JobsState,
    /// Every job in the store.
    pub jobs: &'a [JobListing],
}

/// One release in the report.
#[derive(Clone, Debug, PartialEq)]
pub struct ReleaseLine {
    /// The release's version.
    pub version: Version,
    /// Whether it is current, previous, future, or retired.
    pub kind: ReleaseKind,
    /// When it was installed.
    pub installed_at: Option<OffsetDateTime>,
    /// Where its content came from.
    pub origin: Option<ReleaseOrigin>,
    /// Whether it can be read.
    pub condition: ReleaseCondition,
    /// The templates the release declares that a run would not find, in the order declared.
    pub missing_templates: Vec<String>,
    /// The marker recorded when it was retired.
    pub retired: Option<RetiredRelease>,
    /// The library releases it is locked to, by library name, once a job on it has passed validation.
    pub lock: Option<Vec<(String, LockedLibrary)>>,
    /// How many jobs are on it in each lifecycle stage, in stage order, leaving out stages with none.
    pub jobs: Vec<(JobLifecycle, usize)>,
}

/// The report: the releases of one workflow, newest first.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkflowReleases {
    /// The store: `shared`, or the vault's name.
    pub store: String,
    /// The package.
    pub package: String,
    /// The workflow.
    pub workflow: String,
    /// The releases, newest first.
    pub releases: Vec<ReleaseLine>,
}

/// The lifecycle stages in the order a release's job counts are reported.
pub(crate) const STAGES: [JobLifecycle; 6] = [
    JobLifecycle::Draft,
    JobLifecycle::Validated,
    JobLifecycle::Registered,
    JobLifecycle::Enabled,
    JobLifecycle::Suspended,
    JobLifecycle::Retired,
];

/// Every version of `workflow` the state file mentions: its current, previous, or future release, and any release with a
/// lock or marker of its own.
pub(crate) fn recorded_versions(state: &JobsState, package: &str, workflow: &str) -> Vec<Version> {
    let Some(releases) = state.package_releases(package) else {
        return Vec::new();
    };
    let mut versions: Vec<Version> = releases
        .workflows
        .get(workflow)
        .into_iter()
        .flat_map(|held| [&held.current, &held.previous, &held.future])
        .flatten()
        .cloned()
        .collect();
    versions.extend(
        releases
            .releases
            .iter()
            .filter(|(_, release)| release.workflows.contains_key(workflow))
            .map(|(version, _)| version.clone()),
    );
    versions.sort();
    versions.dedup();
    versions
}

impl From<ReleasesInput<'_>> for WorkflowReleases {
    fn from(input: ReleasesInput<'_>) -> Self {
        let ReleasesInput {
            store,
            package,
            workflow,
            facts,
            state,
            jobs,
        } = input;
        let recorded = recorded_versions(state, package, workflow);
        let mut releases: Vec<ReleaseLine> = facts
            .into_iter()
            .filter(|facts| facts.holds_workflow || recorded.contains(&facts.version))
            .map(|facts| {
                let held = state
                    .package_releases(package)
                    .and_then(|releases| releases.workflow_release(&facts.version, workflow));
                let on_release = |job: &&JobListing| {
                    job.key.package == package
                        && job.key.workflow == workflow
                        && job.version.as_ref() == Some(&facts.version)
                };
                ReleaseLine {
                    kind: state.release_kind(package, workflow, &facts.version),
                    retired: held.and_then(|held| held.retired.clone()),
                    lock: state.library_lock(package, workflow, &facts.version).map(|lock| {
                        lock.libraries
                            .iter()
                            .map(|(name, locked)| (name.clone(), locked.clone()))
                            .collect()
                    }),
                    jobs: STAGES
                        .into_iter()
                        .map(|stage| {
                            let count = jobs
                                .iter()
                                .filter(on_release)
                                .filter(|job| job.lifecycle == stage)
                                .count();
                            (stage, count)
                        })
                        .filter(|(_, count)| *count > 0)
                        .collect(),
                    version: facts.version,
                    installed_at: facts.installed_at,
                    origin: facts.origin,
                    condition: facts.condition,
                    missing_templates: facts.missing_templates,
                }
            })
            .collect();
        releases.sort_by(|a, b| b.version.cmp(&a.version));
        Self {
            store: store.to_owned(),
            package: package.to_owned(),
            workflow: workflow.to_owned(),
            releases,
        }
    }
}

#[cfg(test)]
#[path = "workflow_releases_test.rs"]
mod tests;
