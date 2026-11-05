//! A package store as it would be once some releases are installed: the real store with extra, already verified releases
//! laid over it.
//!
//! An install checks the code it is about to make in the way validation checks a job's, which reads releases and the
//! libraries they require through a store. Reading through this overlay checks the new releases and the libraries
//! they were tested on before any is written.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;

use contextos_packages::{
    AsideBatch, InstallRequest, Installed, LedgerReading, PackageStore, PackagesError, ReadCheck, Rebuilt, Recovered,
    ReleaseKey, Removed, RequestedRemoval, Settled, StoreListing, VerifiedRelease,
};

use crate::package_release_manifest::{PackageInterface, PackageRelease};

/// `inner` with `releases` laid over it. A release in `releases` reads as verified whatever `inner` holds under its key.
///
/// A package named by a key in `pinned` lists only that release, so a library is resolved to exactly the release a
/// package was tested on and never to a newer one that merely satisfies the requirement written for it.
pub(crate) struct OverlayStore<'a, S> {
    pub inner: &'a S,
    pub releases: &'a BTreeMap<ReleaseKey, VerifiedRelease<PackageRelease>>,
    pub pinned: &'a BTreeSet<ReleaseKey>,
}

impl<S: PackageStore<Manifest = PackageRelease>> PackageStore for OverlayStore<'_, S> {
    type Manifest = PackageRelease;
    type Lock = S::Lock;

    fn lock(&self) -> impl Future<Output = Result<Self::Lock, PackagesError>> + Send {
        self.inner.lock()
    }

    async fn list(&self) -> Result<StoreListing, PackagesError> {
        let mut listing = self.inner.list().await?;
        listing
            .releases
            .retain(|key| self.pinned.contains(key) || !self.pinned.iter().any(|pin| pin.name == key.name));
        for key in self.releases.keys() {
            if !listing.releases.contains(key)
                && (self.pinned.contains(key) || !self.pinned.iter().any(|pin| pin.name == key.name))
            {
                listing.releases.push(key.clone());
            }
        }
        Ok(listing)
    }

    async fn read_release(&self, release: &ReleaseKey) -> ReadCheck<PackageRelease> {
        match self.releases.get(release) {
            Some(verified) => ReadCheck::Verified(verified.clone()),
            None => self.inner.read_release(release).await,
        }
    }

    fn ledger(&self) -> impl Future<Output = Result<LedgerReading<PackageInterface>, PackagesError>> + Send {
        self.inner.ledger()
    }

    fn install(
        &self,
        lock: &Self::Lock,
        request: InstallRequest,
    ) -> impl Future<Output = Result<Installed, PackagesError>> + Send {
        self.inner.install(lock, request)
    }

    fn remove(
        &self,
        lock: &Self::Lock,
        release: &ReleaseKey,
        reason: RequestedRemoval,
    ) -> impl Future<Output = Result<Removed, PackagesError>> + Send {
        self.inner.remove(lock, release, reason)
    }

    fn set_aside(
        &self,
        lock: &Self::Lock,
        releases: &[ReleaseKey],
    ) -> impl Future<Output = Result<AsideBatch, PackagesError>> + Send {
        self.inner.set_aside(lock, releases)
    }

    fn settle(
        &self,
        lock: &Self::Lock,
        batch: AsideBatch,
        keep: &[ReleaseKey],
    ) -> impl Future<Output = Result<Settled, PackagesError>> + Send {
        self.inner.settle(lock, batch, keep)
    }

    fn settle_requested(
        &self,
        lock: &Self::Lock,
        batch: AsideBatch,
        keep: &[ReleaseKey],
        reason: RequestedRemoval,
    ) -> impl Future<Output = Result<Settled, PackagesError>> + Send {
        self.inner.settle_requested(lock, batch, keep, reason)
    }

    fn recover(&self, lock: &Self::Lock) -> impl Future<Output = Result<Recovered, PackagesError>> + Send {
        self.inner.recover(lock)
    }

    fn rebuild_ledger(&self, lock: &Self::Lock) -> impl Future<Output = Result<Rebuilt, PackagesError>> + Send {
        self.inner.rebuild_ledger(lock)
    }
}
