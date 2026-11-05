//! The engine: the one implementation of [`crate::PackageStore`], for any [`crate::StoreIo`], and the pure rules it
//! applies that callers outside a store also need.

use std::marker::PhantomData;

use sha2::{Digest, Sha256};

use contextos_core::Clock;

use crate::check::{ReadCheck, ReadLimits};
use crate::error::PackagesError;
use crate::io::{IssuesOperationIds, StoreIo};
use crate::ledger::{LedgerReading, RecordedCause, RequestedRemoval};
use crate::manifest::{ReleaseFiles, ReleaseManifest};
use crate::name::{ReleaseDigest, ReleaseKey};
use crate::store::{
    AsideBatch, InstallRequest, Installed, PackageStore, Rebuilt, Recovered, Removed, Settled, StoreListing,
};

/// One package store: a store's I/O with the engine's rules over it. Implements [`crate::PackageStore`] for every
/// `I: StoreIo`, `M: ReleaseManifest`, `K: contextos_core::Clock`, and `N: IssuesOperationIds`, so the host store and
/// every vault's store share one code path and differ only in `io`.
#[derive(Clone, Debug)]
pub struct PackageEngine<I, M, K, N> {
    /// The store's I/O.
    pub io: I,
    /// The clock install and removal times are taken from.
    pub clock: K,
    /// The source of staging and removal folder identifiers.
    pub ids: N,
    /// The most one read of a release may hold.
    pub limits: ReadLimits,
    /// The manifest model releases are read with. Holds no value.
    pub manifest: PhantomData<fn() -> M>,
}

/// The pure rules of a release, for callers that build or check one outside a store: a bundle's extraction, and an
/// install from source writing a manifest.
pub trait ReleaseRules {
    /// The digest of a release's files: for each file in path order, its `/`-separated path, a zero byte, its bytes,
    /// and a zero byte, all hashed as one SHA-256 and written as `sha256:` and 64 lower-case hexadecimal characters.
    /// The same digest every release installed before this crate existed carries.
    fn digest(&self, files: &ReleaseFiles) -> ReleaseDigest;
}

/// The engine's rules. Implements [`ReleaseRules`].
#[derive(Clone, Copy, Debug, Default)]
pub struct StoreRules;

impl ReleaseRules for StoreRules {
    fn digest(&self, files: &ReleaseFiles) -> ReleaseDigest {
        // A `BTreeMap` over `ReleaseFilePath` already iterates in the segment-by-segment order the digest takes files
        // in, so a release hashes the same on every platform.
        let mut hasher = Sha256::new();
        for (path, bytes) in &files.0 {
            hasher.update(path.to_string().as_bytes());
            hasher.update([0u8]);
            hasher.update(bytes);
            hasher.update([0u8]);
        }
        ReleaseDigest::from(<[u8; 32]>::from(hasher.finalize()))
    }
}

impl<I: StoreIo, M: ReleaseManifest, K: Clock, N: IssuesOperationIds> PackageStore for PackageEngine<I, M, K, N> {
    type Manifest = M;
    type Lock = I::Lock;

    async fn lock(&self) -> Result<Self::Lock, PackagesError> {
        Ok(self.io.lock().await?)
    }

    async fn list(&self) -> Result<StoreListing, PackagesError> {
        self.list_store().await
    }

    async fn read_release(&self, release: &ReleaseKey) -> ReadCheck<M> {
        self.check_release(release).await
    }

    async fn ledger(&self) -> Result<LedgerReading<M::Interface>, PackagesError> {
        self.read_ledger().await
    }

    async fn install(&self, _lock: &Self::Lock, request: InstallRequest) -> Result<Installed, PackagesError> {
        self.install_release(request).await
    }

    async fn remove(
        &self,
        _lock: &Self::Lock,
        release: &ReleaseKey,
        reason: RequestedRemoval,
    ) -> Result<Removed, PackagesError> {
        self.remove_release(release, reason).await
    }

    async fn set_aside(&self, _lock: &Self::Lock, releases: &[ReleaseKey]) -> Result<AsideBatch, PackagesError> {
        self.set_releases_aside(releases).await
    }

    async fn settle(
        &self,
        _lock: &Self::Lock,
        batch: AsideBatch,
        keep: &[ReleaseKey],
    ) -> Result<Settled, PackagesError> {
        self.settle_releases(batch, keep, RecordedCause::Retention).await
    }

    async fn settle_requested(
        &self,
        _lock: &Self::Lock,
        batch: AsideBatch,
        keep: &[ReleaseKey],
        reason: RequestedRemoval,
    ) -> Result<Settled, PackagesError> {
        self.settle_releases(batch, keep, RecordedCause::from(reason)).await
    }

    async fn recover(&self, _lock: &Self::Lock) -> Result<Recovered, PackagesError> {
        self.recover_unfinished().await
    }

    async fn rebuild_ledger(&self, _lock: &Self::Lock) -> Result<Rebuilt, PackagesError> {
        self.rebuild_missing_ledger().await
    }
}

#[cfg(test)]
#[path = "engine_test.rs"]
mod tests;
