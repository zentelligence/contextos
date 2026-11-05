//! Removing releases: aside first, then recorded, then deleted.
//!
//! A release is renamed into a `_removing-{id}` folder before anything else changes, so no half-deleted release is
//! ever visible under its name. The ledger records the removal next, and only then is the folder deleted. A crash
//! before the record leaves a removal folder the next operation renames back; after it, one the next operation
//! deletes.

use contextos_core::Clock;

use crate::engine::PackageEngine;
use crate::error::PackagesError;
use crate::install::not_recorded;
use crate::io::{IssuesOperationIds, StoreFailureKind, StoreIo};
use crate::ledger::{Ledger, LedgerRemoval, RecordedCause, RequestedRemoval};
use crate::manifest::ReleaseManifest;
use crate::name::{ReleaseId, ReleaseKey, ReleaseName};
use crate::path::StorePath;
use crate::store::{AsideBatch, AsideRelease, Removed, Settled};

type InterfaceOf<M> = <M as ReleaseManifest>::Interface;

impl<I: StoreIo, M: ReleaseManifest, K: Clock, N: IssuesOperationIds> PackageEngine<I, M, K, N> {
    /// The ledger's record of a release, or the reason it has none: not found when no folder stands there either.
    async fn recorded_release(
        &self,
        ledger: &Ledger<InterfaceOf<M>>,
        key: &ReleaseKey,
    ) -> Result<ReleaseId, PackagesError> {
        if let Some(entry) = ledger.entries.iter().find(|entry| entry.release.key == *key) {
            return Ok(entry.release.clone());
        }
        if self.release_folder_present(key).await? {
            Err(not_recorded(key))
        } else {
            Err(PackagesError::NotFound { release: key.clone() })
        }
    }

    /// Renames a release into a new removal folder; not found when it is not there.
    async fn rename_aside(&self, key: &ReleaseKey) -> Result<crate::name::OperationId, PackagesError> {
        let id = self.ids.issue();
        match self
            .io
            .rename(&StorePath::from(key), &StorePath::removing_folder(&id))
            .await
        {
            Ok(()) => Ok(id),
            Err(failure) if failure.kind == StoreFailureKind::NotFound => {
                Err(PackagesError::NotFound { release: key.clone() })
            }
            Err(failure) => Err(failure.into()),
        }
    }

    /// Renames a removal folder back to its release's name, logging when it cannot: the next operation retries.
    async fn rename_back(&self, key: &ReleaseKey, folder: &crate::name::OperationId) -> Result<(), PackagesError> {
        self.io.create_dir_all(&StorePath::from(&key.name)).await?;
        self.io
            .rename(&StorePath::removing_folder(folder), &StorePath::from(key))
            .await?;
        Ok(())
    }

    /// Deletes a package's folder once no release stands in it. A folder that holds anything, a release or an entry
    /// the engine does not know, is left as it is. Logs rather than failing: an empty folder harms nothing and the next
    /// removal of that package tries again.
    async fn prune_empty_package(&self, name: &ReleaseName) {
        let folder = StorePath::from(name);
        match self.io.list(&folder).await {
            Ok(entries) if entries.is_empty() => {
                if let Err(failure) = self.io.remove_tree(&folder).await {
                    tracing::warn!(detail = %failure.detail, "an empty package folder could not be deleted");
                }
            }
            Ok(_) => {}
            Err(failure) if failure.kind == StoreFailureKind::NotFound => {}
            Err(failure) => {
                tracing::warn!(detail = %failure.detail, "a package folder could not be checked for removal");
            }
        }
    }

    pub(crate) async fn remove_release(
        &self,
        release: &ReleaseKey,
        reason: RequestedRemoval,
    ) -> Result<Removed, PackagesError> {
        self.recover_unfinished().await?;
        let mut ledger = self.usable_ledger().await?;
        let id = self.recorded_release(&ledger, release).await?;
        let folder = self.rename_aside(release).await?;

        let cause = RecordedCause::from(reason);
        if let Some(entry) = ledger.entries.iter_mut().find(|entry| entry.release.key == *release) {
            entry.removal = Some(LedgerRemoval {
                removed_at: self.clock.now(),
                cause,
            });
        }
        if let Err(error) = self.write_ledger(&ledger).await {
            if let Err(restore) = self.rename_back(release, &folder).await {
                tracing::warn!(error = %restore, "a release could not be renamed back; the next operation restores it");
            }
            return Err(error);
        }
        self.discard(&StorePath::removing_folder(&folder)).await;
        self.prune_empty_package(&release.name).await;
        tracing::debug!(package = release.name.as_str(), version = %release.version, "release removed");
        Ok(Removed { release: id, cause })
    }

    pub(crate) async fn set_releases_aside(&self, releases: &[ReleaseKey]) -> Result<AsideBatch, PackagesError> {
        self.recover_unfinished().await?;
        let ledger = self.usable_ledger().await?;
        let mut unique: Vec<&ReleaseKey> = Vec::new();
        for release in releases {
            if !unique.contains(&release) {
                unique.push(release);
            }
        }
        let mut recorded = Vec::with_capacity(unique.len());
        for release in &unique {
            recorded.push(self.recorded_release(&ledger, release).await?);
        }

        let mut batch = AsideBatch::default();
        for (release, id) in unique.into_iter().zip(recorded) {
            match self.rename_aside(release).await {
                Ok(folder) => batch.releases.push(AsideRelease { release: id, folder }),
                Err(error) => {
                    for done in batch.releases.iter().rev() {
                        if let Err(restore) = self.rename_back(&done.release.key, &done.folder).await {
                            tracing::warn!(error = %restore, "a release could not be renamed back; the next operation restores it");
                        }
                    }
                    return Err(error);
                }
            }
        }
        Ok(batch)
    }

    pub(crate) async fn settle_releases(
        &self,
        batch: AsideBatch,
        keep: &[ReleaseKey],
        cause: RecordedCause,
    ) -> Result<Settled, PackagesError> {
        let mut ledger = self.usable_ledger().await?;
        let (kept, removed): (Vec<AsideRelease>, Vec<AsideRelease>) = batch
            .releases
            .into_iter()
            .partition(|aside| keep.contains(&aside.release.key));
        let mut settled = Settled::default();

        for aside in &kept {
            self.rename_back(&aside.release.key, &aside.folder).await?;
            settled.restored.push(aside.release.clone());
        }

        let removed_at = self.clock.now();
        for aside in &removed {
            let Some(entry) = ledger
                .entries
                .iter_mut()
                .find(|entry| entry.release.key == aside.release.key)
            else {
                return Err(not_recorded(&aside.release.key));
            };
            entry.removal = Some(LedgerRemoval { removed_at, cause });
        }
        if !removed.is_empty() {
            self.write_ledger(&ledger).await?;
        }
        for aside in removed {
            self.discard(&StorePath::removing_folder(&aside.folder)).await;
            self.prune_empty_package(&aside.release.key.name).await;
            settled.removed.push(aside.release);
        }
        Ok(settled)
    }
}

#[cfg(test)]
#[path = "removal_test.rs"]
mod tests;
