//! What a package store holds and why: each release with whether it can be read, where it came from, what it holds, what
//! keeps it, and which workflow releases hold a library release; the ledger; what was removed; and what unfinished
//! operations left behind. The same code reports the host's store and a vault's, from one read of the store.

use contextos_packages::{
    EntryProvenance, LedgerEntry, LedgerProblem, LedgerReading, LedgerState, PackageStore, REMOVING_PREFIX, ReadCheck,
    ReleaseKey, RemovalCause, STAGING_PREFIX,
};
use contextos_workflows::{
    JobsState, KeepReason, LockedLibrary, PackageKind, PackageName, ReleaseKind, ReleaseKinds as _, ReleaseRef,
    RetentionInput, decide_retention,
};
use std::collections::BTreeSet;

use time::OffsetDateTime;

use crate::package_op_error::PackageOpError;
use crate::package_release_manifest::PackageRelease;
use crate::package_retention::{Held, RetentionHome};
use crate::workflow_releases::ReleaseCondition;

/// The ledger as the store found it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LedgerStanding {
    /// There is no ledger and the store holds no release: the first install writes one.
    Absent,
    /// The ledger is read and valid.
    Present {
        /// How many releases it records, removed ones included.
        recorded: usize,
        /// When it was rebuilt from the releases present, if it was.
        rebuilt_at: Option<OffsetDateTime>,
    },
    /// The store holds releases and has no ledger, so installs and removals are refused.
    Missing,
    /// The ledger cannot be read, so installs and removals are refused.
    Unreadable(String),
}

/// One workflow of a release: whether the release is its current, previous, future, or retired release, and the library
/// releases locked for it.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkflowStanding {
    pub workflow: String,
    pub kind: ReleaseKind,
    /// The library releases the release is locked to, by library name, once a job on it has passed validation.
    pub lock: Option<Vec<(String, LockedLibrary)>>,
}

/// What a release that reads whole holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Contents {
    /// Its workflows; none for a library.
    pub workflows: Vec<WorkflowStanding>,
    /// The library requirements it states, as `(library, requirement)`.
    pub libraries: Vec<(String, String)>,
    /// The modules it exports, as its manifest writes them.
    pub exports: Vec<String>,
    /// The templates it declares, which a run reads from the package's source in a vault.
    pub templates: Vec<String>,
}

/// One release in the store.
#[derive(Clone, Debug, PartialEq)]
pub struct ReleaseStanding {
    pub release: ReleaseRef,
    pub condition: ReleaseCondition,
    /// The content digest its manifest states; only a release that reads whole is read this far.
    pub digest: Option<String>,
    /// How the ledger recorded it, when it has an entry.
    pub provenance: Option<EntryProvenance>,
    /// What it holds; only a release that reads whole is read this far.
    pub contents: Option<Contents>,
    /// Every reason a rule keeps it, or for a release that does not read whole, holds what its workflows are recorded as; empty
    /// for a release nothing keeps or holds.
    pub kept_by: Vec<KeepReason>,
    /// Whether the store records the release as a library although it does not read whole: its ledger entry lists no
    /// workflow, or a workflow release's lock names it. It is how a library release that is damaged or incomplete is
    /// still told from a workflow release; a release that reads whole is told by its contents.
    pub recorded_as_library: bool,
}

/// A release the ledger records as removed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemovedStanding {
    pub release: ReleaseRef,
    pub cause: RemovalCause,
    /// When the removal was recorded; none for a release read as removed with no record.
    pub removed_at: Option<OffsetDateTime>,
}

/// A package store: its ledger, its releases, what was removed, and what unfinished operations left.
#[derive(Clone, Debug, PartialEq)]
pub struct StoreStanding {
    /// The word the store is named by: `shared`, or the vault's name.
    pub store: String,
    pub ledger: LedgerStanding,
    /// Every release folder in the store, by package then version.
    pub releases: Vec<ReleaseStanding>,
    /// Every release the ledger records as removed.
    pub removed: Vec<RemovedStanding>,
    /// Staging folders left by an operation that did not finish.
    pub staging: Vec<String>,
    /// Removal folders: a release renamed aside.
    pub removing: Vec<String>,
    /// Entries that are none of these, as found.
    pub unrecognised: Vec<String>,
}

impl<F> From<&LedgerState<F>> for LedgerStanding {
    fn from(state: &LedgerState<F>) -> Self {
        match state {
            LedgerState::Absent => Self::Absent,
            LedgerState::Present(ledger) => Self::Present {
                recorded: ledger.entries.len(),
                rebuilt_at: ledger.rebuilt_at,
            },
            LedgerState::Unusable(LedgerProblem::Missing) => Self::Missing,
            LedgerState::Unusable(LedgerProblem::Unreadable(reason)) => Self::Unreadable(reason.clone()),
        }
    }
}

/// Every release the ledger records as removed, and those whose folder is gone with no removal recorded, by release.
fn removed_from<F>(reading: &LedgerReading<F>, entries: &[LedgerEntry<F>]) -> Vec<RemovedStanding> {
    let mut removed: Vec<RemovedStanding> = entries
        .iter()
        .filter_map(|entry| {
            let removal = entry.removal?;
            Some(RemovedStanding {
                release: ReleaseRef::try_from(KeyedRelease(&entry.release.key)).ok()?,
                cause: RemovalCause::from(removal.cause),
                removed_at: Some(removal.removed_at),
            })
        })
        .collect();
    removed.extend(reading.unrecorded_removals.iter().filter_map(|id| {
        Some(RemovedStanding {
            release: ReleaseRef::try_from(KeyedRelease(&id.key)).ok()?,
            cause: RemovalCause::Unrecorded,
            removed_at: None,
        })
    }));
    removed.sort_by(|left, right| left.release.cmp(&right.release));
    removed
}

/// A release as the store keys it, to be named as the policy names it.
pub(crate) struct KeyedRelease<'a>(pub(crate) &'a ReleaseKey);

impl TryFrom<KeyedRelease<'_>> for ReleaseRef {
    type Error = contextos_workflows::package::PackageError;

    fn try_from(keyed: KeyedRelease<'_>) -> Result<Self, Self::Error> {
        Ok(Self {
            name: PackageName::try_from(keyed.0.name.as_str())?,
            version: keyed.0.version.as_semver().clone(),
        })
    }
}

/// What a release holds, read from its package kind and the state file's record of its workflows.
struct ContentsOf<'a> {
    kind: &'a PackageKind,
    release: &'a ReleaseRef,
    state: &'a JobsState,
}

impl From<ContentsOf<'_>> for Contents {
    fn from(of: ContentsOf<'_>) -> Self {
        let package = of.release.name.as_str();
        Self {
            workflows: of
                .kind
                .workflows()
                .iter()
                .map(|workflow| WorkflowStanding {
                    workflow: workflow.clone(),
                    kind: of.state.release_kind(package, workflow, &of.release.version),
                    lock: of
                        .state
                        .library_lock(package, workflow, &of.release.version)
                        .map(|lock| {
                            lock.libraries
                                .iter()
                                .map(|(name, locked)| (name.clone(), locked.clone()))
                                .collect()
                        }),
                })
                .collect(),
            libraries: of
                .kind
                .libraries()
                .entries()
                .iter()
                .map(|library| (library.name.to_string(), library.requirement.to_string()))
                .collect(),
            exports: of
                .kind
                .exports()
                .map(|exports| exports.entries().iter().map(ToString::to_string).collect())
                .unwrap_or_default(),
            templates: of
                .kind
                .templates()
                .into_iter()
                .flat_map(contextos_workflows::DeclaredFiles::entries)
                .map(|template| template.as_str().to_owned())
                .collect(),
        }
    }
}

impl ReleaseStanding {
    /// Whether the release is a library: it reads whole and lists no workflow, or it does not read whole and the store
    /// records it as one.
    #[must_use]
    pub fn is_library(&self) -> bool {
        match &self.contents {
            Some(contents) => contents.workflows.is_empty(),
            None => self.recorded_as_library,
        }
    }
}

impl StoreStanding {
    /// Reads `store`, deciding what keeps each release from `home`. Nothing is changed.
    ///
    /// # Errors
    ///
    /// [`PackageOpError::Packages`] when the store cannot be read, and [`PackageOpError::KeepUnreadable`] when what keeps releases
    /// cannot be read.
    pub(crate) async fn read<S, H>(store: &S, home: &H, name: &str) -> Result<Self, PackageOpError>
    where
        S: PackageStore<Manifest = PackageRelease>,
        H: RetentionHome,
    {
        let listing = store.list().await?;
        let mut held = Held::default();
        let mut checks = Vec::with_capacity(listing.releases.len());
        for key in &listing.releases {
            let check = store.read_release(key).await;
            held.admit(key, &check);
            checks.push((key, check));
        }
        let (state, files) = (home.state().await?.state, home.job_files().await?);
        let decided = decide_retention(&RetentionInput {
            releases: &held.releases,
            unverified: &held.unverified,
            state: &state,
            job_files: &files,
        });
        let reading = store.ledger().await?;
        let entries = match &reading.state {
            LedgerState::Present(ledger) => ledger.entries.as_slice(),
            LedgerState::Absent | LedgerState::Unusable(_) => &[],
        };
        let releases = checks
            .iter()
            .filter_map(|(key, check)| {
                let release = ReleaseRef::try_from(KeyedRelease(key)).ok()?;
                let digest = match check {
                    ReadCheck::Verified(verified) => {
                        Some(verified.manifest.manifest().identity.digest.as_str().to_owned())
                    }
                    _ => None,
                };
                Some(ReleaseStanding {
                    digest,
                    condition: ReleaseCondition::from(check),
                    provenance: entries
                        .iter()
                        .find(|entry| entry.release.key == **key)
                        .map(|entry| entry.provenance),
                    contents: held.kinds.get(&release).map(|kind| {
                        Contents::from(ContentsOf {
                            kind,
                            release: &release,
                            state: &state,
                        })
                    }),
                    kept_by: decided.holding(&release).to_vec(),
                    recorded_as_library: entries
                        .iter()
                        .find(|entry| entry.release.key == **key)
                        .is_some_and(|entry| entry.interface.workflows.is_empty()),
                    release,
                })
            })
            .collect();
        let mut releases: Vec<ReleaseStanding> = releases;
        let locked: BTreeSet<(String, semver::Version)> = releases
            .iter()
            .flat_map(|held| held.contents.iter().flat_map(|contents| &contents.workflows))
            .flat_map(|workflow| workflow.lock.iter().flatten())
            .map(|(name, locked)| (name.clone(), locked.version.clone()))
            .collect();
        for held in &mut releases {
            if locked.contains(&(held.release.name.to_string(), held.release.version.clone())) {
                held.recorded_as_library = true;
            }
        }
        Ok(Self {
            store: name.to_owned(),
            ledger: LedgerStanding::from(&reading.state),
            releases,
            removed: removed_from(&reading, entries),
            staging: listing
                .staging
                .iter()
                .map(|id| format!("{STAGING_PREFIX}{}", id.as_str()))
                .collect(),
            removing: listing
                .removing
                .iter()
                .map(|id| format!("{REMOVING_PREFIX}{}", id.as_str()))
                .collect(),
            unrecognised: listing.unrecognised,
        })
    }

    /// The release `name` `version`, when the store holds a folder for it.
    #[must_use]
    pub fn release(&self, name: &str, version: &semver::Version) -> Option<&ReleaseStanding> {
        self.releases
            .iter()
            .find(|held| held.release.name.as_str() == name && held.release.version == *version)
    }
}

#[cfg(test)]
#[path = "package_standing_test.rs"]
mod tests;
