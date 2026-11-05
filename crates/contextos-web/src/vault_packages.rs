//! How a session to the vault's primary server reaches that vault's package store.
//!
//! The session learns where a vault is by asking its server, with `vault_info`, which every server answers whether it
//! was reached over stdio or HTTP. The server runs on this host, so the path it reports is a path here. Everything
//! that validates, versions, or runs a saved vault job already holds the session, so the store comes from it.

use std::collections::HashMap;
use std::time::Duration;

use contextos_core::SystemClock;
use contextos_packages::{PackageEngine, ReadLimits};

use crate::host_store::HostOperationIds;
use crate::mcp_client::McpClient;
use crate::package_release_manifest::PackageRelease;
use crate::vault_package_store::{ReportedLocation, VaultLocation, VaultStoreError, VaultStoreIo, VaultStoreSettings};

/// How long taking the vault store lock waits: a caller that finds it held is turned away at once.
const LOCK_WAIT: Duration = Duration::ZERO;

/// The most entries a release folder may hold, and the most bytes one read of a release may hold.
const MAX_RELEASE_ENTRIES: usize = 10_000;
const MAX_RELEASE_BYTES: u64 = 256 * 1024 * 1024;

/// Where each vault's store is, asked of the primary server on first use and kept for the session.
#[derive(Debug, Default)]
pub(crate) struct VaultPackages {
    locations: tokio::sync::Mutex<HashMap<String, VaultLocation>>,
}

impl VaultPackages {
    /// `vault`'s location from `vault_info` on `client`'s server, which runs on this host, so the path it reports is
    /// usable here. Kept once found; a failure is not kept, so a later call asks again.
    async fn location(&self, client: &McpClient, vault: &str) -> Result<VaultLocation, VaultStoreError> {
        let mut known = self.locations.lock().await;
        if let Some(found) = known.get(vault) {
            return Ok(found.clone());
        }
        let report = client.vault_info().await?;
        let found = VaultLocation::try_from(ReportedLocation { report, vault })?;
        known.insert(vault.to_owned(), found.clone());
        Ok(found)
    }
}

/// The package engine over a vault's store, read through the session that handed it out.
pub(crate) type VaultEngine<'a> =
    PackageEngine<VaultStoreIo<&'a McpClient>, PackageRelease, SystemClock, HostOperationIds>;

impl McpClient {
    /// The I/O of `vault`'s package store, `.contextos/packages/`, reading through this session.
    ///
    /// # Errors
    ///
    /// Returns the error that locating the vault through `vault_info` gives, which names the cause.
    pub async fn vault_store(&self, vault: &str) -> Result<VaultStoreIo<&Self>, VaultStoreError> {
        let location = self.packages.location(self, vault).await?;
        VaultStoreIo::try_from(VaultStoreSettings {
            client: self,
            location,
            lock_wait: LOCK_WAIT,
        })
    }

    /// The package engine over `vault`'s store, which reads, verifies, and classifies releases.
    ///
    /// # Errors
    ///
    /// As [`Self::vault_store`].
    pub(crate) async fn vault_packages(&self, vault: &str) -> Result<VaultEngine<'_>, VaultStoreError> {
        Ok(PackageEngine {
            io: self.vault_store(vault).await?,
            clock: SystemClock,
            ids: HostOperationIds::default(),
            limits: ReadLimits {
                max_entries: MAX_RELEASE_ENTRIES,
                max_bytes: MAX_RELEASE_BYTES,
            },
            manifest: std::marker::PhantomData,
        })
    }
}

#[cfg(test)]
#[path = "vault_packages_test.rs"]
mod tests;
