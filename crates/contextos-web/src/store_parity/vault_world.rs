//! A vault's store as a parity world: a temporary vault served by a real in-process server, read through that server
//! and written through the vault's write path, as the host does.

// The trait the world implements is `async` so the host and vault worlds share it; building the vault's I/O waits for nothing.
#![allow(clippy::unused_async, clippy::unused_async_trait_impl)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;

use super::worlds::{BoxError, IoWorld, StoreWorld};
use crate::fake_vault::FakeVault;
use crate::mcp_client::McpClient;
use crate::package_op_error::PackageOpError;
use crate::package_retention::RetentionError;
use crate::package_standing::StoreStanding;
use crate::package_store_report::{RebuiltReport, RemovalReport};
use crate::package_stores::VaultPackagesOf;
use crate::source_install_world::World;
use crate::store_retention::retain_vault;
use crate::vault_package_store::{ReportedLocation, VaultLocation, VaultStoreIo, VaultStoreSettings};

const VAULT: &str = "vault";

pub(super) struct VaultWorld {
    world: World,
    client: Arc<McpClient>,
    outside: TempDir,
}

impl VaultWorld {
    fn location(&self) -> Result<VaultLocation, BoxError> {
        let report = serde_json::from_value(serde_json::json!({
            "vaults": [{"name": VAULT, "path": self.world.vault()}]
        }))?;
        Ok(VaultLocation::try_from(ReportedLocation { report, vault: VAULT })?)
    }

    fn io_over(&self, client: McpClient) -> Result<VaultStoreIo, BoxError> {
        Ok(VaultStoreIo::try_from(VaultStoreSettings {
            client: Arc::new(client),
            location: self.location()?,
            lock_wait: Duration::ZERO,
        })?)
    }

    fn packages(&self) -> VaultPackagesOf<'_> {
        VaultPackagesOf {
            client: &self.client,
            vault: VAULT,
        }
    }
}

impl IoWorld for VaultWorld {
    type Io = VaultStoreIo;

    async fn new() -> Result<Self, BoxError> {
        let world = World::new()?;
        let client = Arc::new(world.session().await?);
        Ok(Self {
            world,
            client,
            outside: tempfile::tempdir()?,
        })
    }

    fn store_dir(&self) -> PathBuf {
        self.world.vault().join(".contextos/packages")
    }

    fn outside_dir(&self) -> PathBuf {
        self.outside.path().to_path_buf()
    }

    async fn io(&self) -> Result<VaultStoreIo, BoxError> {
        Ok(VaultStoreIo::try_from(VaultStoreSettings {
            client: Arc::clone(&self.client),
            location: self.location()?,
            lock_wait: Duration::ZERO,
        })?)
    }

    async fn unreachable_io(&self) -> Result<Option<VaultStoreIo>, BoxError> {
        let dropped = FakeVault::new().dropping_after(0).session().await?;
        Ok(Some(self.io_over(dropped)?))
    }
}

impl StoreWorld for VaultWorld {
    const STORE: &'static str = VAULT;

    fn state_path(&self) -> PathBuf {
        self.world.vault().join(".contextos/jobs.toml")
    }

    fn jobs_dir(&self) -> PathBuf {
        self.world.vault().join("registry/workflows/crm/sync/jobs")
    }

    async fn standing(&self) -> Result<StoreStanding, PackageOpError> {
        self.packages().standing().await
    }

    async fn remove(&self, package: &str, version: &str, force: bool) -> Result<RemovalReport, PackageOpError> {
        self.packages().remove(package, version, force).await
    }

    async fn rebuild(&self) -> Result<RebuiltReport, PackageOpError> {
        self.packages().rebuild_ledger().await
    }

    async fn retain(&self) -> Result<(), RetentionError> {
        retain_vault(&self.client, VAULT).await
    }
}

impl VaultWorld {
    /// The folder the in-process server keeps its derived state in, where it takes its own write lock.
    pub(super) fn server_state_dir(&self) -> PathBuf {
        self.world.vault().join("state")
    }
}
