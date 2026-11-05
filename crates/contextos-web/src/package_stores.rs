//! The two homes `packages list`, `inspect`, and `remove` work on: the host's store, and a vault's, read and changed
//! through the session to its primary server. Each gathers its store, its state file, and its jobs, then hands them to the
//! one code that decides above the store port, so the two stores behave alike.

use std::path::{Path, PathBuf};

use contextos_packages::{PackageStore as _, PackagesError, Rebuilt, ReleaseKey, ReleaseName, ReleaseVersion};

use crate::host_store::HostStore;
use crate::mcp_client::McpClient;
use crate::package_op_error::PackageOpError;
use crate::package_removal::remove_guarded;
use crate::package_standing::{ReleaseStanding, StoreStanding};
use crate::package_store::SavedJobs;
use crate::package_store_report::{RebuiltReport, RemovalReport};
use crate::retention_job_files::{HostHome, VaultHome};
use crate::store_retention::{settle_host_after_change, settle_vault_after_change};

/// The word a report gives the host's package store.
pub const SHARED: &str = "shared";

/// A release as a command or request names it, before it is checked.
struct Asked<'a> {
    package: &'a str,
    version: &'a str,
}

impl TryFrom<Asked<'_>> for ReleaseKey {
    type Error = PackageOpError;

    fn try_from(asked: Asked<'_>) -> Result<Self, Self::Error> {
        Ok(Self {
            name: ReleaseName::try_from(asked.package)?,
            version: ReleaseVersion::try_from(asked.version)?,
        })
    }
}

/// The release `asked` names, as `standing` reports it.
fn release_of(standing: &StoreStanding, asked: Asked<'_>) -> Result<ReleaseStanding, PackageOpError> {
    let key = ReleaseKey::try_from(asked)?;
    standing
        .release(key.name.as_str(), key.version.as_semver())
        .cloned()
        .ok_or(PackageOpError::Packages(PackagesError::NotFound { release: key }))
}

impl From<&Rebuilt> for RebuiltReport {
    /// The releases a rebuilt ledger records, as `name version`, in a fixed order. The report is named for the store by
    /// the caller.
    fn from(rebuilt: &Rebuilt) -> Self {
        let mut recorded: Vec<String> = rebuilt
            .recorded
            .iter()
            .map(|id| format!("{} {}", id.key.name.as_str(), id.key.version.as_semver()))
            .collect();
        recorded.sort();
        Self {
            store: String::new(),
            recorded,
        }
    }
}

/// The host's package store, with the jobs folder and state file that say what keeps its releases.
#[derive(Clone, Debug)]
pub struct SharedPackages {
    /// The host package store's folder.
    pub store_root: PathBuf,
    /// The saved shared jobs and the host's state.
    pub jobs: SavedJobs,
}

impl SharedPackages {
    /// What the store holds and why.
    ///
    /// # Errors
    ///
    /// [`PackageOpError`] when the store, the state, or the jobs cannot be read.
    pub async fn standing(&self) -> Result<StoreStanding, PackageOpError> {
        let state_store = self.jobs.state.clone();
        // Reading the state file blocks, so it runs on a thread that may.
        let state = tokio::task::spawn_blocking(move || state_store.read())
            .await
            .map_err(|_| PackageOpError::Host {
                reason: "reading the host's state did not finish",
            })?
            .map_err(PackageOpError::from)?;
        let home = HostHome::of(state, &self.jobs);
        StoreStanding::read(HostStore::new(&self.store_root).engine(), &home, SHARED).await
    }

    /// One release of the store, as `standing` reports it.
    ///
    /// # Errors
    ///
    /// [`PackageOpError`] when the name or version is not usable, the release is not in the store, or `standing` fails.
    pub async fn release(&self, package: &str, version: &str) -> Result<ReleaseStanding, PackageOpError> {
        release_of(&self.standing().await?, Asked { package, version })
    }

    /// Removes one release. Refused with `packages/in-use` while a rule keeps it, unless `force` is set.
    ///
    /// # Errors
    ///
    /// [`PackageOpError::InUse`] for a release a rule keeps, and the other [`PackageOpError`]s for a name or version that
    /// is not usable, a release that is not there, a store that is locked, or a ledger that cannot be used.
    pub async fn remove(&self, package: &str, version: &str, force: bool) -> Result<RemovalReport, PackageOpError> {
        let key = ReleaseKey::try_from(Asked { package, version })?;
        let (store_root, jobs) = (self.store_root.clone(), self.jobs.clone());
        tokio::task::spawn_blocking(move || remove_on_host(&store_root, &jobs, &key, force))
            .await
            .map_err(|_| PackageOpError::Host {
                reason: "removing the release did not finish",
            })?
    }

    /// Rebuilds a missing ledger from the releases present.
    ///
    /// # Errors
    ///
    /// [`PackageOpError::Packages`] with `packages/ledger-present` when the store has a ledger, and with
    /// `packages/release-incomplete` while a release is incomplete or cannot be verified.
    pub async fn rebuild_ledger(&self) -> Result<RebuiltReport, PackageOpError> {
        let store_root = self.store_root.clone();
        tokio::task::spawn_blocking(move || {
            let store = HostStore::new(&store_root);
            let lock = store.lock()?;
            let rebuilt = store.rebuild_ledger(&lock)?;
            Ok(RebuiltReport {
                store: SHARED.to_owned(),
                ..RebuiltReport::from(&rebuilt)
            })
        })
        .await
        .map_err(|_| PackageOpError::Host {
            reason: "rebuilding the ledger did not finish",
        })?
    }
}

/// Removes a release from the host's store under the state lock and then the store lock, as every change to what is kept
/// does, so the recorded `future` the removal changes is written in the same state write.
fn remove_on_host(
    store_root: &Path,
    jobs: &SavedJobs,
    key: &ReleaseKey,
    force: bool,
) -> Result<RemovalReport, PackageOpError> {
    let store = HostStore::new(store_root);
    jobs.state
        .update(|state| {
            let lock = store.lock()?;
            let home = HostHome::of(state.clone(), jobs);
            let removal = store.remove_guarded(&lock, &home, (key, force))??;
            let settled = settle_host_after_change(&store, &lock, jobs, state, &removal.release.name);
            Ok(RemovalReport {
                store: SHARED.to_owned(),
                removal,
                warnings: settled.warnings,
                retention_removed: settled.removed,
            })
        })
        .map_err(PackageOpError::from)?
}

/// A vault's package store, reached through the session to the vault's primary server.
pub struct VaultPackagesOf<'a> {
    /// The session to the vault's primary server.
    pub client: &'a McpClient,
    /// The vault.
    pub vault: &'a str,
}

impl VaultPackagesOf<'_> {
    /// What the store holds and why.
    ///
    /// # Errors
    ///
    /// [`PackageOpError`] when the store, the state, or the jobs cannot be read.
    pub async fn standing(&self) -> Result<StoreStanding, PackageOpError> {
        let engine = self.client.vault_packages(self.vault).await?;
        let home = VaultHome {
            client: self.client,
            vault: self.vault,
        };
        StoreStanding::read(&engine, &home, self.vault).await
    }

    /// One release of the store, as `standing` reports it.
    ///
    /// # Errors
    ///
    /// As [`SharedPackages::release`].
    pub async fn release(&self, package: &str, version: &str) -> Result<ReleaseStanding, PackageOpError> {
        release_of(&self.standing().await?, Asked { package, version })
    }

    /// Removes one release. Refused with `packages/in-use` while a rule keeps it, unless `force` is set.
    ///
    /// # Errors
    ///
    /// As [`SharedPackages::remove`].
    pub async fn remove(&self, package: &str, version: &str, force: bool) -> Result<RemovalReport, PackageOpError> {
        let key = ReleaseKey::try_from(Asked { package, version })?;
        let engine = self.client.vault_packages(self.vault).await?;
        let lock = engine.lock().await?;
        let home = VaultHome {
            client: self.client,
            vault: self.vault,
        };
        let removal = remove_guarded(&engine, &lock, &home, &key, force).await?;
        let settled = settle_vault_after_change(&engine, &lock, self.client, self.vault, &removal.release.name).await;
        Ok(RemovalReport {
            store: self.vault.to_owned(),
            removal,
            warnings: settled.warnings,
            retention_removed: settled.removed,
        })
    }

    /// Rebuilds a missing ledger from the releases present.
    ///
    /// # Errors
    ///
    /// As [`SharedPackages::rebuild_ledger`].
    pub async fn rebuild_ledger(&self) -> Result<RebuiltReport, PackageOpError> {
        let engine = self.client.vault_packages(self.vault).await?;
        let lock = engine.lock().await?;
        let rebuilt = engine.rebuild_ledger(&lock).await?;
        Ok(RebuiltReport {
            store: self.vault.to_owned(),
            ..RebuiltReport::from(&rebuilt)
        })
    }
}

#[cfg(test)]
#[path = "package_stores_test.rs"]
mod tests;
