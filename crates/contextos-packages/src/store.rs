//! The package store port: what every caller above the stores uses, the same for the host store and a vault's store.
//!
//! The port holds the store's mechanics only. Which release is current, previous, or future, which a job names, which a
//! library lock keeps, and whether a removal is guarded are decided above it and passed in; a caller that must decide
//! under the store's lock takes [`PackageStore::lock`] first and passes the guard to every changing call.

use std::future::Future;

use crate::check::ReadCheck;
use crate::error::PackagesError;
use crate::ledger::{LedgerReading, RecordedCause, ReleaseOrigin, RequestedRemoval};
use crate::manifest::{ReleaseFiles, ReleaseManifest};
use crate::name::{OperationId, ReleaseId, ReleaseKey};

/// The interface type a store's manifest model gives its ledger entries.
type InterfaceOf<S> = <<S as PackageStore>::Manifest as ReleaseManifest>::Interface;

/// What a store's folder holds, by name alone, without reading any release.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StoreListing {
    /// Every release folder, `{package}/{version}/`, whose names read as a package and a version.
    pub releases: Vec<ReleaseKey>,
    /// Staging folders, each left by an operation that did not finish.
    pub staging: Vec<OperationId>,
    /// Removal folders, each a release renamed aside.
    pub removing: Vec<OperationId>,
    /// Entries at the top of the store or in a package folder that are none of these, as found. Never read as a
    /// release and never changed.
    pub unrecognised: Vec<String>,
}

/// A release to install: the text of its manifest and the files it lists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallRequest {
    /// The text of `manifest.toml`, written as given.
    pub manifest_text: String,
    /// Every file the manifest lists, and nothing else.
    pub files: ReleaseFiles,
    /// Where the content came from, for the ledger.
    pub origin: ReleaseOrigin,
}

/// What an install did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstallOutcome {
    /// The release was new to the ledger and is now installed and recorded.
    Installed,
    /// The ledger records the release with the same digest, and it is present and verified: nothing changed.
    Unchanged,
    /// The ledger records the release with the same digest, and it was removed, incomplete, or damaged: it is
    /// restored, staged beside the old folder, which is renamed aside before the replacement is renamed in.
    Repaired,
}

/// A finished install.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Installed {
    /// The release.
    pub release: ReleaseId,
    /// What the install did.
    pub outcome: InstallOutcome,
}

/// A finished removal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Removed {
    /// The release removed.
    pub release: ReleaseId,
    /// The cause recorded.
    pub cause: RecordedCause,
}

/// One release renamed aside by [`PackageStore::set_aside`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AsideRelease {
    /// The release.
    pub release: ReleaseId,
    /// The removal folder it now stands in.
    pub folder: OperationId,
}

/// Releases renamed aside for retention, not yet recorded as removed. Settled with [`PackageStore::settle`] under the
/// same lock; left unsettled by a crash, each folder is renamed back by the next [`PackageStore::recover`], since the
/// ledger records no removal for it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AsideBatch {
    /// The releases set aside.
    pub releases: Vec<AsideRelease>,
}

/// What settling a retention batch did.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Settled {
    /// Releases recorded as removed by retention and deleted.
    pub removed: Vec<ReleaseId>,
    /// Releases renamed back, because the caller found them named again.
    pub restored: Vec<ReleaseId>,
}

/// What recovering from operations that did not finish did.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Recovered {
    /// Staging folders deleted.
    pub staging_deleted: Vec<OperationId>,
    /// Removal folders the ledger records no removal for, renamed back to their release's folder.
    pub restored: Vec<ReleaseKey>,
    /// Removal folders the ledger records a removal for, deleted.
    pub removing_deleted: Vec<OperationId>,
}

/// What rebuilding a missing ledger recorded.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Rebuilt {
    /// Every release present and verified, now recorded as [`crate::EntryProvenance::Rebuilt`].
    pub recorded: Vec<ReleaseId>,
}

/// One package store: the host store or a vault's store. The same rules hold for both.
pub trait PackageStore: Send + Sync {
    /// The manifest model the store's releases are read with.
    type Manifest: ReleaseManifest;

    /// The guard of the store's lock. Every changing call takes it, so a change is made only under the lock.
    type Lock: Send + Sync;

    /// Takes the store's lock. A caller holding the state lock takes it after, never before.
    ///
    /// # Errors
    ///
    /// [`PackagesError::StoreLocked`] when another holder keeps it past the store's wait; [`PackagesError::Store`]
    /// when it cannot be opened.
    fn lock(&self) -> impl Future<Output = Result<Self::Lock, PackagesError>> + Send;

    /// What the store's folder holds, by name. An empty listing for a store whose root does not exist yet.
    ///
    /// # Errors
    ///
    /// [`PackagesError::Store`] when the store cannot be read.
    fn list(&self) -> impl Future<Output = Result<StoreListing, PackagesError>> + Send;

    /// Reads one release and checks it fully. Every state of the release, an unreachable store included
    /// ([`crate::UnverifiableReason::StoreUnavailable`]), is an outcome, never an error.
    fn read_release(&self, release: &ReleaseKey) -> impl Future<Output = ReadCheck<Self::Manifest>> + Send;

    /// The store's ledger, read against its folders.
    ///
    /// # Errors
    ///
    /// [`PackagesError::Store`] when the store cannot be read. A ledger that cannot be used is
    /// [`crate::LedgerState::Unusable`], not an error.
    fn ledger(&self) -> impl Future<Output = Result<LedgerReading<InterfaceOf<Self>>, PackagesError>> + Send;

    /// Installs a release: checks the files against the manifest, stages them in a `_staging-{id}` folder with the
    /// manifest, records the ledger entry, and makes the release visible with one rename. Repairs a recorded release
    /// that is removed, incomplete, or damaged; changes nothing for one present and verified. Which version a release
    /// takes, and whether its step is allowed, is decided before this call.
    ///
    /// # Errors
    ///
    /// [`PackagesError::LedgerMissing`] or [`PackagesError::LedgerUnreadable`]; [`PackagesError::InvalidManifest`] when
    /// the manifest is refused or does not describe the files given; [`PackagesError::DuplicateIdentity`] when the
    /// ledger records the version with another digest; [`PackagesError::LimitExceeded`]; [`PackagesError::Store`] when
    /// a write fails, leaving no partial release visible.
    fn install(
        &self,
        lock: &Self::Lock,
        request: InstallRequest,
    ) -> impl Future<Output = Result<Installed, PackagesError>> + Send;

    /// Removes one release at an operator's request: renames it aside, records the removal, and deletes it. Whether a
    /// rule keeps it is decided before this call; `reason` records whether that guard was overridden.
    ///
    /// # Errors
    ///
    /// [`PackagesError::LedgerMissing`] or [`PackagesError::LedgerUnreadable`]; [`PackagesError::NotFound`] when the
    /// release is not present; [`PackagesError::Store`] when a step fails, leaving the release whole under its name or
    /// aside for [`PackageStore::recover`].
    fn remove(
        &self,
        lock: &Self::Lock,
        release: &ReleaseKey,
        reason: RequestedRemoval,
    ) -> impl Future<Output = Result<Removed, PackagesError>> + Send;

    /// Renames each release aside for retention, recording nothing yet, so the caller can re-read what names
    /// releases before any removal is final.
    ///
    /// # Errors
    ///
    /// [`PackagesError::LedgerMissing`] or [`PackagesError::LedgerUnreadable`]; [`PackagesError::NotFound`];
    /// [`PackagesError::Store`], after renaming back every release this call set aside.
    fn set_aside(
        &self,
        lock: &Self::Lock,
        releases: &[ReleaseKey],
    ) -> impl Future<Output = Result<AsideBatch, PackagesError>> + Send;

    /// Settles a retention batch: renames back each release in `keep`, records every other as removed by
    /// retention, and deletes it.
    ///
    /// # Errors
    ///
    /// [`PackagesError::LedgerMissing`] or [`PackagesError::LedgerUnreadable`]; [`PackagesError::Store`] when a step
    /// fails, leaving each release either renamed back or aside for [`PackageStore::recover`].
    fn settle(
        &self,
        lock: &Self::Lock,
        batch: AsideBatch,
        keep: &[ReleaseKey],
    ) -> impl Future<Output = Result<Settled, PackagesError>> + Send;

    /// Settles a batch set aside for one release an operator asked to remove, as [`PackageStore::settle`] settles one
    /// for retention, but recording the removal with the cause the operator's request has (`reason`) instead of
    /// retention. The caller sets the release aside, re-reads what keeps it, and either keeps it (renamed back, an
    /// operator's removal refused) or settles it removed.
    ///
    /// # Errors
    ///
    /// As [`PackageStore::settle`].
    fn settle_requested(
        &self,
        lock: &Self::Lock,
        batch: AsideBatch,
        keep: &[ReleaseKey],
        reason: RequestedRemoval,
    ) -> impl Future<Output = Result<Settled, PackagesError>> + Send;

    /// Clears what operations that did not finish left: deletes every staging folder, renames back every removal
    /// folder the ledger records no removal for, and deletes every one it does. Every changing operation calls it
    /// first.
    ///
    /// # Errors
    ///
    /// [`PackagesError::LedgerMissing`] or [`PackagesError::LedgerUnreadable`]; [`PackagesError::Store`].
    fn recover(&self, lock: &Self::Lock) -> impl Future<Output = Result<Recovered, PackagesError>> + Send;

    /// Rebuilds a missing ledger from the releases present, marking the rebuild.
    ///
    /// # Errors
    ///
    /// [`PackagesError::LedgerPresent`] when a ledger exists, readable or not; [`PackagesError::ReleaseIncomplete`]
    /// while any release is incomplete or unverifiable; [`PackagesError::Store`].
    fn rebuild_ledger(&self, lock: &Self::Lock) -> impl Future<Output = Result<Rebuilt, PackagesError>> + Send;
}
