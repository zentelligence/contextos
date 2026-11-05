//! Clearing what operations that did not finish left, and rebuilding a missing ledger.
//!
//! A staging folder holds a release that was never made visible: it is deleted. A removal folder holds a release
//! renamed aside: if the ledger records its removal, or the release's own folder has been refilled since (a repair),
//! it is deleted, and otherwise the removal never became final and the release is renamed back. A removal folder
//! names no release, so which release it holds is read from the `manifest.toml` inside it; one whose manifest cannot
//! be read is left untouched, since deleting it could lose a release that was never recorded as removed.

use contextos_core::Clock;

use crate::check::ReadCheck;
use crate::engine::PackageEngine;
use crate::error::PackagesError;
use crate::io::{IssuesOperationIds, StoreFailureKind, StoreIo};
use crate::ledger::{EntryProvenance, Ledger, LedgerEntry};
use crate::manifest::{ReleaseFiles, ReleaseManifest};
use crate::name::ReleaseKey;
use crate::path::StorePath;
use crate::store::{Rebuilt, Recovered};

type InterfaceOf<M> = <M as ReleaseManifest>::Interface;

impl<I: StoreIo, M: ReleaseManifest, K: Clock, N: IssuesOperationIds> PackageEngine<I, M, K, N> {
    /// Clears what operations that did not finish left, as the ledger directs.
    pub(crate) async fn recover_unfinished(&self) -> Result<Recovered, PackagesError> {
        let ledger = self.usable_ledger().await?;
        self.recover_with(&ledger).await
    }

    /// The release a removal folder holds, read from its manifest; none when the manifest cannot be read.
    async fn release_aside(&self, folder: &StorePath) -> Option<ReleaseKey> {
        let bytes = self.io.read(&folder.manifest(), self.limits.max_bytes).await.ok()?;
        let text = String::from_utf8(bytes).ok()?;
        M::try_from(text.as_str())
            .ok()
            .map(|manifest| manifest.release().key.clone())
    }

    async fn recover_with(&self, ledger: &Ledger<InterfaceOf<M>>) -> Result<Recovered, PackagesError> {
        let listing = self.list_store().await?;
        let mut recovered = Recovered::default();
        for id in listing.staging {
            self.io.remove_tree(&StorePath::staging_folder(&id)).await?;
            recovered.staging_deleted.push(id);
        }
        for id in listing.removing {
            let folder = StorePath::removing_folder(&id);
            let Some(key) = self.release_aside(&folder).await else {
                tracing::warn!(folder = %folder, "a removal folder holds no readable manifest; it is left untouched");
                continue;
            };
            let removal_recorded = ledger
                .entries
                .iter()
                .any(|entry| entry.release.key == key && entry.removal.is_some());
            if removal_recorded || self.release_folder_present(&key).await? {
                self.io.remove_tree(&folder).await?;
                recovered.removing_deleted.push(id);
            } else {
                self.io.create_dir_all(&StorePath::from(&key.name)).await?;
                self.io.rename(&folder, &StorePath::from(&key)).await?;
                recovered.restored.push(key);
            }
        }
        Ok(recovered)
    }

    /// Rebuilds a missing ledger from the releases present.
    pub(crate) async fn rebuild_missing_ledger(&self) -> Result<Rebuilt, PackagesError> {
        match self.io.read(&StorePath::ledger_file(), 1).await {
            Ok(_) => return Err(PackagesError::LedgerPresent),
            Err(failure) => match failure.kind {
                StoreFailureKind::NotFound => {}
                StoreFailureKind::TooLarge | StoreFailureKind::Refused => return Err(PackagesError::LedgerPresent),
                _ => return Err(failure.into()),
            },
        }
        self.recover_with(&Ledger {
            entries: Vec::new(),
            rebuilt_at: None,
        })
        .await?;

        let listing = self.list_store().await?;
        let mut entries: Vec<LedgerEntry<InterfaceOf<M>>> = Vec::new();
        for key in listing.releases {
            let (manifest, files) = match self.check_release(&key).await {
                ReadCheck::Verified(verified) => (verified.manifest, verified.files),
                ReadCheck::Removed => continue,
                ReadCheck::Damaged(findings) if findings.iter().all(is_recordable) => {
                    match self.release_manifest(&key).await {
                        Some(manifest) => {
                            let files = self.listed_files_as_found(&key, &manifest).await;
                            (manifest, files)
                        }
                        None => return Err(PackagesError::ReleaseIncomplete { release: key }),
                    }
                }
                ReadCheck::Damaged(_) | ReadCheck::Incomplete(_) | ReadCheck::Unverifiable(_) => {
                    return Err(PackagesError::ReleaseIncomplete { release: key });
                }
            };
            entries.push(LedgerEntry {
                release: manifest.release().clone(),
                interface: manifest.interface(&files),
                provenance: EntryProvenance::Rebuilt,
                removal: None,
            });
        }
        let recorded = entries.iter().map(|entry| entry.release.clone()).collect();
        self.write_ledger(&Ledger {
            entries,
            rebuilt_at: Some(self.clock.now()),
        })
        .await?;
        Ok(Rebuilt { recorded })
    }

    /// The files a damaged release's manifest lists, as the folder holds them; one that cannot be read is left out.
    async fn listed_files_as_found(&self, key: &ReleaseKey, manifest: &M) -> ReleaseFiles {
        let folder = StorePath::from(key);
        let mut found = std::collections::BTreeMap::new();
        for listed in manifest.listed_files() {
            if let Ok(bytes) = self.io.read(&folder.within(&listed.path), self.limits.max_bytes).await {
                found.insert(listed.path.clone(), bytes);
            }
        }
        ReleaseFiles(found)
    }

    /// The manifest of a release folder, parsed; none when it cannot be read.
    async fn release_manifest(&self, key: &ReleaseKey) -> Option<M> {
        let bytes = self
            .io
            .read(&StorePath::from(key).manifest(), self.limits.max_bytes)
            .await
            .ok()?;
        let text = String::from_utf8(bytes).ok()?;
        M::try_from(text.as_str()).ok()
    }
}

/// Damage a rebuild can record the release despite: its manifest is sound, its content or a link in it is not.
fn is_recordable(finding: &crate::check::DamageFinding) -> bool {
    use crate::check::DamageFinding as Damage;
    matches!(
        finding,
        Damage::FileHashMismatch(_)
            | Damage::DigestMismatch { .. }
            | Damage::LinkInRelease(_)
            | Damage::NotARegularFile(_)
    )
}

#[cfg(test)]
#[path = "recovery_test.rs"]
mod tests;
