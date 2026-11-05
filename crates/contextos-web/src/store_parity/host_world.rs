//! The host store as a parity world: a temporary folder holding the store, a jobs folder, and a state file.

// The host store's I/O is a blocking adapter wrapped in futures, and so is this world: its methods are `async` to meet
// the trait the vault's world needs to wait in, without waiting themselves.
#![allow(clippy::unused_async, clippy::unused_async_trait_impl)]

use std::path::PathBuf;
use std::time::Duration;

use contextos_packages::host_io::HostStoreIo;
use tempfile::TempDir;

use super::worlds::{BoxError, IoWorld, StoreWorld};
use crate::package_op_error::PackageOpError;
use crate::package_retention::RetentionError;
use crate::package_standing::StoreStanding;
use crate::package_store::SavedJobs;
use crate::package_store_report::{RebuiltReport, RemovalReport};
use crate::package_stores::SharedPackages;
use crate::shared_state::SharedStateStore;
use crate::store_retention::retain_host;

pub(super) struct HostWorld {
    dir: TempDir,
}

impl HostWorld {
    fn saved(&self) -> SavedJobs {
        SavedJobs {
            jobs_root: self.dir.path().join("workflows"),
            state: SharedStateStore::new(self.dir.path().join("state")),
        }
    }

    fn packages(&self) -> SharedPackages {
        SharedPackages {
            store_root: self.store_dir(),
            jobs: self.saved(),
        }
    }
}

impl IoWorld for HostWorld {
    type Io = HostStoreIo;

    async fn new() -> Result<Self, BoxError> {
        let world = Self {
            dir: tempfile::tempdir()?,
        };
        std::fs::create_dir_all(world.outside_dir())?;
        Ok(world)
    }

    fn store_dir(&self) -> PathBuf {
        self.dir.path().join("packages")
    }

    fn outside_dir(&self) -> PathBuf {
        self.dir.path().join("outside")
    }

    async fn io(&self) -> Result<HostStoreIo, BoxError> {
        Ok(HostStoreIo::new(self.store_dir(), Duration::ZERO))
    }

    async fn unreachable_io(&self) -> Result<Option<HostStoreIo>, BoxError> {
        Ok(None)
    }
}

impl StoreWorld for HostWorld {
    const STORE: &'static str = "shared";

    fn state_path(&self) -> PathBuf {
        self.saved().state.state_path()
    }

    fn jobs_dir(&self) -> PathBuf {
        self.dir.path().join("workflows/crm/sync/jobs")
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
        let (root, saved) = (self.store_dir(), self.saved());
        tokio::task::spawn_blocking(move || retain_host(&root, &saved)).await?
    }
}
