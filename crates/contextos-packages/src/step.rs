//! How far a release's version must step from the highest the ledger records for its package.
//!
//! The rules for what each step means belong to the manifest model, which knows what a job sees of a release, so the
//! engine reads them through [`InterfaceChanges`]. The engine applies them in one place, under the store's lock, to every
//! release installed from a bundle, and refuses one whose version steps less than its interface changes need.

use std::fmt;

use semver::Version;

use crate::engine::PackageEngine;
use crate::error::{PackagesError, VersionStepRefusal};
use crate::io::{IssuesOperationIds, StoreIo};
use crate::ledger::Ledger;
use crate::manifest::ReleaseManifest;
use crate::name::ReleaseId;
use contextos_core::Clock;

/// How far a release's version must step from the release it follows, judged by what a job sees.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum VersionStep {
    /// The interface is unchanged: only code, modules, or other files differ.
    Patch,
    /// Compatible changes only.
    Minor,
    /// A job may have to change to use the release.
    Major,
}

impl fmt::Display for VersionStep {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Patch => "patch",
            Self::Minor => "minor",
            Self::Major => "major",
        })
    }
}

/// One difference between two interfaces and the step it requires.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterfaceChange {
    /// The smallest step that may carry the change.
    pub step: VersionStep,
    /// What changed, for an operator.
    pub what: String,
}

/// The rules for comparing a release's interface with the one it follows.
pub trait InterfaceChanges {
    /// Every difference between `self` and `previous`. Empty when the interface is unchanged.
    fn changes_from(&self, previous: &Self) -> Vec<InterfaceChange>;
}

impl VersionStep {
    /// The step `version` takes from `followed`, which it is above.
    fn taken(version: &Version, followed: &Version) -> Self {
        if version.major != followed.major {
            Self::Major
        } else if version.minor != followed.minor {
            Self::Minor
        } else {
            Self::Patch
        }
    }
}

impl<I: StoreIo, M: ReleaseManifest, K: Clock, N: IssuesOperationIds> PackageEngine<I, M, K, N> {
    /// Refuses a release whose version steps less than its interface changes need, from the release it follows and to
    /// the release that follows it. The release it follows is the highest version the ledger records for its package
    /// below its own, and the release that follows it is the lowest recorded above, removed releases included in both.
    /// A release below every recorded version has nothing to step from, and one above every recorded version has
    /// nothing to step to; the first of a package has neither.
    pub(crate) fn check_version_step(
        ledger: &Ledger<M::Interface>,
        release: &ReleaseId,
        interface: &M::Interface,
    ) -> Result<(), PackagesError> {
        let offered = release.key.version.as_semver();
        let same_package =
            |entry: &&crate::ledger::LedgerEntry<M::Interface>| entry.release.key.name == release.key.name;
        let below = ledger
            .entries
            .iter()
            .filter(same_package)
            .filter(|entry| entry.release.key.version.as_semver() < offered)
            .max_by(|a, b| a.release.key.version.as_semver().cmp(b.release.key.version.as_semver()));
        if let Some(followed) = below {
            let changes = interface.changes_from(&followed.interface);
            Self::judge(release, &followed.release.key.version, &release.key.version, &changes)?;
        }
        let above = ledger
            .entries
            .iter()
            .filter(same_package)
            .filter(|entry| entry.release.key.version.as_semver() > offered)
            .min_by(|a, b| a.release.key.version.as_semver().cmp(b.release.key.version.as_semver()));
        if let Some(following) = above {
            let changes = following.interface.changes_from(interface);
            Self::judge(release, &release.key.version, &following.release.key.version, &changes)?;
        }
        Ok(())
    }

    /// Refuses when the step from `from` up to `to` is smaller than `changes` need.
    fn judge(
        release: &ReleaseId,
        from: &crate::name::ReleaseVersion,
        to: &crate::name::ReleaseVersion,
        changes: &[InterfaceChange],
    ) -> Result<(), PackagesError> {
        let required = changes
            .iter()
            .map(|change| change.step)
            .max()
            .unwrap_or(VersionStep::Patch);
        let taken = VersionStep::taken(to.as_semver(), from.as_semver());
        if taken >= required {
            return Ok(());
        }
        Err(PackagesError::VersionStepInvalid(Box::new(VersionStepRefusal {
            release: release.key.clone(),
            from: from.clone(),
            to: to.clone(),
            taken,
            required,
            changes: changes.iter().map(|change| change.what.clone()).collect(),
        })))
    }
}

#[cfg(test)]
#[path = "step_test.rs"]
mod tests;
