//! Retention: removing the installed releases nothing keeps, the same procedure for the host's store and a vault's.
//!
//! `contextos_workflows::decide_retention` says which releases are kept and why; this module carries the decision
//! out. The caller holds the locks that order a change to what is kept (the state lock then the host store lock, or the
//! vault store lock). The decision is made, each release it does not keep is renamed aside, what names releases is read
//! again because job files and a vault's state file are written without those locks, and a release now named, or of a
//! workflow whose job file became unreadable, is renamed back. What is left aside is recorded as removed by retention
//! and deleted. A release that is damaged, incomplete, or unverifiable is never touched: it is not known whole.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;

use contextos_packages::{PackageStore, PackagesError, ReadCheck, ReleaseKey, Settled};
use contextos_workflows::{
    JobFile, JobsState, PackageKind, PackageName, ReleaseContents, ReleaseRef, RetentionInput, StoredRelease,
    decide_retention,
};
use thiserror::Error;

use crate::mcp_client::McpCallError;
use crate::package_release_manifest::{PackageFilesOf, PackageRelease};
use crate::package_standing::KeyedRelease;
use crate::package_store::PackageStoreError;
use crate::scheduler::StateError;
use crate::shared_state::SharedStateError;
use crate::source_install_error::SourceInstallError;
use crate::vault_package_store::VaultStoreError;

/// What a store's home holds that names releases, read afresh each time it is asked.
pub(crate) trait RetentionHome: Sync {
    /// The state file as it is now.
    fn state(&self) -> impl Future<Output = Result<StateReading, RetentionError>> + Send;

    /// Every job file of every workflow of the home, as it is now.
    fn job_files(&self) -> impl Future<Output = Result<Vec<JobFile>, RetentionError>> + Send;
}

/// The state file of a store's home, read.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct StateReading {
    /// What the file records; empty when there is no file.
    pub(crate) state: JobsState,
    /// Whether the home has a state file at all. Without one nothing says which releases are in use, so a store that
    /// holds releases is left as it is rather than read as one that keeps nothing.
    pub(crate) recorded: bool,
}

/// Why a retention run did not finish. Nothing is removed that was not recorded as removed.
#[derive(Debug, Error)]
pub enum RetentionError {
    /// The store could not be used.
    #[error(transparent)]
    Store(#[from] PackagesError),
    /// The host's package store could not be used.
    #[error("the host's package store could not be used: {0}")]
    HostStore(#[from] PackageStoreError),
    /// The host's job state could not be read, locked, or written.
    #[error("the host's job state could not be used: {0}")]
    HostState(#[from] SharedStateError),
    /// The shared jobs folder could not be listed.
    #[error("the shared jobs could not be listed: {0}")]
    JobsFolder(#[from] std::io::Error),
    /// A vault's job state could not be read or written.
    #[error("the vault's job state could not be used: {0}")]
    VaultState(#[from] StateError),
    /// A vault's server could not be asked.
    #[error("the vault's server could not be asked: {0}")]
    Server(#[from] McpCallError),
    /// A vault's package store could not be located.
    #[error("the vault's package store could not be located: {0}")]
    Located(#[from] VaultStoreError),
    /// The workflows of the installed releases could not be read.
    #[error("the installed workflows could not be read: {0}")]
    Installed(#[from] SourceInstallError),
    /// The work did not run to its end.
    #[error("retention did not run to its end: {0}")]
    Interrupted(#[from] tokio::task::JoinError),
    /// A vault's job state kept changing under the write that records a workflow's `future`.
    #[error("the job state kept changing while the future release was being recorded")]
    StateKeptChanging,
}

impl RetentionError {
    /// The stable machine-readable code every failure of a run carries.
    pub const CODE: &'static str = "packages/retention-failed";

    /// The stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        Self::CODE
    }
}

/// What a retention run did, and every package whose recorded `future` it leaves to be checked against the store.
#[derive(Debug, Default)]
pub(crate) struct Retained {
    pub(crate) settled: Settled,
    /// Every package that holds a workflow release that reads whole or has something recorded in the state, and every
    /// package a release was removed from: a recorded `future` that names no release that reads whole is repaired
    /// whether or not this run removed anything.
    pub(crate) packages: BTreeSet<PackageName>,
}

/// Runs retention over `store`, which `lock` holds the lock of, reading what names releases from `home`.
///
/// # Errors
///
/// [`RetentionError::Store`] when the store cannot be read or changed, and the other variants when what names
/// releases cannot be read; in either case every release set aside is renamed back or left for the store's recovery.
pub(crate) async fn retain<S, H>(store: &S, lock: &S::Lock, home: &H) -> Result<Retained, RetentionError>
where
    S: PackageStore<Manifest = PackageRelease>,
    H: RetentionHome,
{
    let reading = home.state().await?;
    if !reading.recorded {
        tracing::info!("the home has no job state, so retention was not run");
        return Ok(Retained::default());
    }
    store.recover(lock).await?;
    let held = Held::read(store).await?;
    let (state, files) = (reading.state, home.job_files().await?);
    let packages: BTreeSet<PackageName> = held
        .kinds
        .iter()
        .filter(|(_, kind)| !kind.workflows().is_empty())
        .map(|(release, _)| release.name.clone())
        .chain(
            state
                .recorded_packages()
                .into_iter()
                .filter_map(|name| PackageName::try_from(name).ok()),
        )
        .collect();
    let decided = decide_retention(&RetentionInput {
        releases: &held.releases,
        unverified: &held.unverified,
        state: &state,
        job_files: &files,
    });
    let to_remove: Vec<ReleaseKey> = decided
        .removable()
        .filter_map(|release| held.keys.get(release))
        .cloned()
        .collect();
    if to_remove.is_empty() {
        return Ok(Retained {
            settled: Settled::default(),
            packages,
        });
    }
    let batch = store.set_aside(lock, &to_remove).await?;
    // Job files, and a vault's state file, are written without the locks held here, so what names releases is read again
    // now that the removals are aside. If it cannot be, nothing is removed.
    let (state, files) = match (home.state().await, home.job_files().await) {
        (Ok(reading), Ok(files)) => (reading.state, files),
        (Err(error), _) | (_, Err(error)) => {
            store.settle(lock, batch, &to_remove).await?;
            return Err(error);
        }
    };
    let again = decide_retention(&RetentionInput {
        releases: &held.releases,
        unverified: &held.unverified,
        state: &state,
        job_files: &files,
    });
    let keep: Vec<ReleaseKey> = held
        .keys
        .iter()
        .filter(|(release, key)| to_remove.contains(key) && again.is_kept(release))
        .map(|(_, key)| key.clone())
        .collect();
    let settled = store.settle(lock, batch, &keep).await?;
    Ok(Retained { settled, packages })
}

/// The releases of a store that read whole, and the key each is kept under.
#[derive(Default)]
pub(crate) struct Held {
    /// Each release that read whole, as retention's policy reads it.
    pub(crate) releases: Vec<StoredRelease>,
    /// Each release that does not read whole, which retention neither keeps nor removes.
    pub(crate) unverified: Vec<ReleaseRef>,
    /// The key each release is kept under.
    pub(crate) keys: BTreeMap<ReleaseRef, ReleaseKey>,
    /// What each release holds.
    pub(crate) kinds: BTreeMap<ReleaseRef, PackageKind>,
}

impl Held {
    /// Reads every release in the store. A release that is damaged, incomplete, or unverifiable is left out, so
    /// retention neither keeps nor removes it.
    pub(crate) async fn read<S: PackageStore<Manifest = PackageRelease>>(store: &S) -> Result<Self, RetentionError> {
        let mut held = Self::default();
        for key in store.list().await?.releases {
            held.admit(&key, &store.read_release(&key).await);
        }
        Ok(held)
    }

    /// Takes in the release `key` as `check` found it, when it is whole.
    pub(crate) fn admit(&mut self, key: &ReleaseKey, check: &ReadCheck<PackageRelease>) {
        let whole = match check {
            ReadCheck::Verified(verified) => PackageKind::try_from(PackageFilesOf {
                manifest: verified.manifest.manifest(),
                files: &verified.files,
            })
            .ok()
            .map(|kind| (verified, kind)),
            _ => None,
        };
        let Some((verified, kind)) = whole else {
            self.unverified.extend(ReleaseRef::try_from(KeyedRelease(key)).ok());
            return;
        };
        let stored = StoredRelease::from(ReleaseContents {
            manifest: verified.manifest.manifest(),
            kind: &kind,
        });
        let release = stored.requirements.release.clone();
        self.keys.insert(release.clone(), key.clone());
        self.kinds.insert(release, kind);
        self.releases.push(stored);
    }
}

#[cfg(test)]
#[path = "package_retention_test.rs"]
mod tests;
