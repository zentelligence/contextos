//! Which installed version of a package a run uses.

use semver::Version;
use thiserror::Error;

use crate::package::PackageError;
use crate::release_state::{PackageReleases, ReleaseKind};

/// Picks the version a run uses: exactly the release `wanted`, which a job names, and never another.
///
/// A job runs the release it names, so a newer release being installed changes nothing here, and a pre-release is
/// chosen only when it is the one named. The choice is made once, when a run starts, so an install during the run
/// cannot change it.
///
/// # Errors
///
/// Returns [`PackageError::ReleaseUnavailable`] when `wanted` is not installed.
pub fn select_release(installed: &[Version], wanted: &Version) -> Result<Version, PackageError> {
    installed
        .iter()
        .find(|version| *version == wanted)
        .cloned()
        .ok_or_else(|| PackageError::ReleaseUnavailable {
            requested: wanted.to_string(),
        })
}

/// Which release a one-off run is asked to take: a workflow's `current`, `future`, or `previous`, or an exact version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReleaseChoice {
    /// The workflow's recorded `current`.
    Current,
    /// The workflow's recorded `future`.
    Future,
    /// The workflow's recorded `previous`, so a return can be checked before it is made.
    Previous,
    /// An exact version, which must be the workflow's `current`, `previous`, or a `future` release. The selection
    /// judges only the workflow's standing for it; whether the package store has ever recorded the version, and holds
    /// it whole, is judged by the caller against the store's ledger.
    Version(Version),
}

/// A word that names no release choice, or a choice the workflow cannot run.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum ReleaseChoiceError {
    /// The word is not `current`, `future`, `previous`, or an exact version.
    #[error("{0:?} is not a release choice: use current, future, previous, or an exact version such as 1.2.0")]
    NotAChoice(String),
    /// The workflow has no release recorded under that name.
    #[error("the workflow has no {0} release, so there is nothing to run; name another release")]
    NoSuchStanding(&'static str),
    /// The release is retired, or is not one the workflow has as current, previous, or future.
    #[error(
        "release {0} is a retired release of the workflow, not its current, previous, or \
         future release, so it is not run; name one of those"
    )]
    Retired(Version),
}

impl TryFrom<&str> for ReleaseChoice {
    type Error = ReleaseChoiceError;

    fn try_from(text: &str) -> Result<Self, ReleaseChoiceError> {
        match text {
            "current" => Ok(Self::Current),
            "future" => Ok(Self::Future),
            "previous" => Ok(Self::Previous),
            other => Version::parse(other)
                .ok()
                .filter(|version| version.pre.is_empty() && version.build.is_empty())
                .map(Self::Version)
                .ok_or_else(|| ReleaseChoiceError::NotAChoice(other.to_owned())),
        }
    }
}

/// A choice and the workflow's recorded releases, from which the version a one-off run takes follows.
#[derive(Clone, Copy, Debug)]
pub struct OneOffRelease<'a> {
    pub choice: &'a ReleaseChoice,
    pub workflow: &'a str,
    /// What the state records for the workflow's package, when it records anything.
    pub releases: Option<&'a PackageReleases>,
}

impl TryFrom<OneOffRelease<'_>> for Version {
    type Error = ReleaseChoiceError;

    fn try_from(request: OneOffRelease<'_>) -> Result<Self, ReleaseChoiceError> {
        let recorded = request
            .releases
            .and_then(|releases| releases.workflows.get(request.workflow));
        let named = |standing: &'static str, version: Option<&Version>| {
            version.cloned().ok_or(ReleaseChoiceError::NoSuchStanding(standing))
        };
        match request.choice {
            ReleaseChoice::Current => named("current", recorded.and_then(|versions| versions.current.as_ref())),
            ReleaseChoice::Previous => named("previous", recorded.and_then(|versions| versions.previous.as_ref())),
            ReleaseChoice::Future => named("future", recorded.and_then(|versions| versions.future.as_ref())),
            ReleaseChoice::Version(version) => {
                let kind = match request.releases {
                    Some(releases) => releases.kind_of(request.workflow, version),
                    None => PackageReleases::default().kind_of(request.workflow, version),
                };
                if kind == ReleaseKind::Retired {
                    Err(ReleaseChoiceError::Retired(version.clone()))
                } else {
                    Ok(version.clone())
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "release_selection_test.rs"]
mod tests;
