//! The rules for which release an authored job file may name, by the job's stage.

use semver::Version;
use thiserror::Error;

use crate::lifecycle::JobLifecycle;
use crate::release_state::{ReleaseKind, ReleaseKinds};

use super::JobFileRelease;

/// Whether a workflow release is in a store, as a store reports it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Presence {
    /// The release is installed.
    Installed,
    /// The store was read and holds no such release.
    Missing,
    /// The store could not be read, so whether it holds the release is not known.
    Unreadable,
}

/// A store of installed releases, for the one question authoring asks of it. The host package store and a vault's
/// package store both answer it.
pub trait InstalledReleases {
    /// Whether `release` of the workflow `workflow` of `package` is installed.
    fn presence(&self, package: &str, workflow: &str, release: &Version) -> Presence;
}

/// The workflow a job file belongs to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JobPlace<'a> {
    /// The package, or for a vault workflow the name it is installed under.
    pub package: &'a str,
    /// The workflow slug.
    pub workflow: &'a str,
}

/// Why an authored job file may not name the release it names. Every case is a refusal of the request, and nothing is
/// written.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ReleaseRefusal {
    /// The release is not installed in the store.
    #[error(
        "workflow/package-unavailable: release {version} of workflow {workflow:?} in package {package:?} is not installed"
    )]
    NotInstalled {
        package: String,
        workflow: String,
        version: Version,
    },
    /// The store could not be read, so it is not known whether the release is installed.
    #[error(
        "workflow/package-unavailable: whether release {version} of workflow {workflow:?} in package {package:?} is installed could not be checked, because the package store could not be read"
    )]
    NotChecked {
        package: String,
        workflow: String,
        version: Version,
    },
    /// The release is installed but is not one a job may name.
    #[error(
        "workflow/transition-refused: release {version} is a {kind}, and a job may name only the current release or a future one"
    )]
    KindRefused { version: Version, kind: &'static str },
    /// A job that is enabled, suspended, or retired changes its release only through a successor.
    #[error(
        "workflow/transition-refused: a {stage} job cannot change its version or predecessor; make a successor with `jobs successor`"
    )]
    NeedsSuccessor { stage: &'static str },
}

impl ReleaseRefusal {
    /// The stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NotInstalled { .. } | Self::NotChecked { .. } => "workflow/package-unavailable",
            Self::KindRefused { .. } | Self::NeedsSuccessor { .. } => "workflow/transition-refused",
        }
    }
}

/// Whether `release` may be named by a job that is new, or whose version is changed in place: installed, and current or future, which
/// takes a job.
fn check_nameable(
    catalogue: &dyn InstalledReleases,
    kinds: &dyn ReleaseKinds,
    place: JobPlace<'_>,
    release: &JobFileRelease,
) -> Result<(), ReleaseRefusal> {
    let version = release.version.as_semver();
    let located = || (place.package.to_owned(), place.workflow.to_owned(), version.clone());
    match catalogue.presence(place.package, place.workflow, version) {
        Presence::Installed => {}
        Presence::Missing => {
            let (package, workflow, version) = located();
            return Err(ReleaseRefusal::NotInstalled {
                package,
                workflow,
                version,
            });
        }
        Presence::Unreadable => {
            let (package, workflow, version) = located();
            return Err(ReleaseRefusal::NotChecked {
                package,
                workflow,
                version,
            });
        }
    }
    match kinds.release_kind(place.package, place.workflow, version) {
        ReleaseKind::Current | ReleaseKind::Future => Ok(()),
        kind @ (ReleaseKind::Previous | ReleaseKind::Retired) => Err(ReleaseRefusal::KindRefused {
            version: version.clone(),
            kind: kind.as_str(),
        }),
    }
}

/// Checks the release a new job file names.
///
/// # Errors
///
/// Returns the [`ReleaseRefusal`] for a release that is not installed, could not be looked for, or is a previous or retired release, which
/// takes no new job.
pub fn check_new_job(
    catalogue: &dyn InstalledReleases,
    kinds: &dyn ReleaseKinds,
    place: JobPlace<'_>,
    release: &JobFileRelease,
) -> Result<(), ReleaseRefusal> {
    check_nameable(catalogue, kinds, place, release)
}

/// Checks the release a replacement job file names against the release the file named before, for a job at `stage`.
/// `current` is `None` when the file being replaced does not load, so there is no release to compare.
///
/// # Errors
///
/// Returns the [`ReleaseRefusal`] for a standing job that changes its version or predecessor, and for a version change
/// to a release that is not installed or is a previous or retired release.
pub fn check_replacement(
    catalogue: &dyn InstalledReleases,
    kinds: &dyn ReleaseKinds,
    place: JobPlace<'_>,
    stage: JobLifecycle,
    current: Option<&JobFileRelease>,
    replacement: &JobFileRelease,
) -> Result<(), ReleaseRefusal> {
    let version_changed = current.is_none_or(|current| current.version != replacement.version);
    let predecessor_changed = current.is_some_and(|current| current.predecessor != replacement.predecessor);
    if current.is_some() && !version_changed && !predecessor_changed {
        return Ok(());
    }
    let standing = matches!(
        stage,
        JobLifecycle::Enabled | JobLifecycle::Suspended | JobLifecycle::Retired
    );
    if standing && current.is_some() {
        return Err(ReleaseRefusal::NeedsSuccessor { stage: stage.as_str() });
    }
    if version_changed {
        check_nameable(catalogue, kinds, place, replacement)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "job_release_authoring_test.rs"]
mod tests;
