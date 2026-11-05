//! What a `jobs successor` request is, what writing it found, and how that becomes the report: the parts a vault's store
//! and the host's share, above the two ways of reading and writing them.

use contextos_core::ContentHash;
use contextos_packages::{Ledger, LedgerState, PackageStore};
use contextos_workflows::{ReleaseKind, SuccessorTarget};
use semver::Version;

use crate::package_release_manifest::{PackageInterface, PackageRelease, WorkflowInterface};
use crate::successor_plan::{PlannedSuccessor, SuccessorRefused, SuccessorScope};
use crate::successor_report::{MadeSuccessor, SuccessorReport, Takeup};

#[cfg(test)]
#[path = "successor_service_test.rs"]
mod tests;

/// A request to make successors in one workflow.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SuccessorRequest {
    /// The package.
    pub package: String,
    /// The workflow.
    pub workflow: String,
    /// Which jobs to make successors for.
    pub scope: SuccessorScope,
    /// Which release they name.
    pub to: SuccessorTarget,
}

/// What carrying out a request found and wrote, before the interface changes of major steps are looked up.
#[derive(Debug)]
pub struct Attempt {
    /// The release the successors name, once it is known.
    pub to: Option<Version>,
    /// Whether that release is current, previous, or future.
    pub release: Option<ReleaseKind>,
    /// The successors written, each with the hash of its file.
    pub made: Vec<(PlannedSuccessor, ContentHash)>,
    /// Why the request, or some of its jobs, were refused.
    pub refused: Vec<SuccessorRefused>,
}

impl Attempt {
    /// An attempt that was refused before anything was written.
    #[must_use]
    pub fn refused(refused: Vec<SuccessorRefused>) -> Self {
        Self {
            to: None,
            release: None,
            made: Vec::new(),
            refused,
        }
    }
}

/// What a report is made from.
pub struct ReportInput<'a> {
    /// The store: `shared`, or the vault's name.
    pub store: &'a str,
    /// The request.
    pub request: &'a SuccessorRequest,
    /// What carrying it out found.
    pub attempt: Attempt,
    /// What each successor written must take up, in the order they were written.
    pub takeups: Vec<Takeup>,
}

impl From<ReportInput<'_>> for SuccessorReport {
    fn from(input: ReportInput<'_>) -> Self {
        let ReportInput {
            store,
            request,
            attempt,
            takeups,
        } = input;
        let made = attempt
            .made
            .into_iter()
            .zip(takeups)
            .map(|((planned, hash), takeup)| MadeSuccessor {
                predecessor: planned.predecessor,
                key: planned.key,
                from: planned.from,
                to: planned.to,
                step: planned.step,
                hash,
                takeup,
            })
            .collect();
        Self {
            store: store.to_owned(),
            package: request.package.clone(),
            workflow: request.workflow.clone(),
            to: attempt.to,
            release: attempt.release,
            made,
            refused: attempt.refused,
        }
    }
}

/// What each of `made` must take up, read from the interfaces `store`'s ledger keeps, for a step of any size and either
/// direction: nothing when the two releases declare the same inputs and outputs, otherwise the differences, and unknown
/// when the ledger does not hold the interfaces to say. A successor naming the release its predecessor names takes
/// nothing up.
pub async fn takeups<S>(
    store: &S,
    package: &str,
    workflow: &str,
    made: &[(PlannedSuccessor, ContentHash)],
) -> Vec<Takeup>
where
    S: PackageStore<Manifest = PackageRelease>,
{
    let any_moved = made.iter().any(|(planned, _)| planned.from != planned.to);
    let ledger = if any_moved {
        match store.ledger().await.map(|reading| reading.state) {
            Ok(LedgerState::Present(ledger)) => Some(ledger),
            Ok(LedgerState::Absent | LedgerState::Unusable(_)) | Err(_) => None,
        }
    } else {
        None
    };
    takeups_in(ledger.as_ref(), package, workflow, made)
}

/// As [`takeups`], from a ledger already read, or none when it could not be.
#[must_use]
pub fn takeups_in(
    ledger: Option<&Ledger<PackageInterface>>,
    package: &str,
    workflow: &str,
    made: &[(PlannedSuccessor, ContentHash)],
) -> Vec<Takeup> {
    let interface_of = |version: &Version| -> Option<WorkflowInterface> {
        let entry = ledger?.entries.iter().find(|entry| {
            entry.release.key.name.as_str() == package && entry.release.key.version.as_semver() == version
        })?;
        entry
            .interface
            .workflows
            .iter()
            .find(|held| held.slug == workflow)
            .cloned()
    };
    made.iter()
        .map(|(planned, _)| {
            if planned.from == planned.to {
                return Takeup::Nothing;
            }
            match (interface_of(&planned.from), interface_of(&planned.to)) {
                (Some(from), Some(to)) => match to.changes_since(&from) {
                    changes if changes.is_empty() => Takeup::Nothing,
                    changes => Takeup::Changes(changes),
                },
                _ => Takeup::Unknown,
            }
        })
        .collect()
}
