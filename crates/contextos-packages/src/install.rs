//! Installing and repairing a release.
//!
//! An install is staged, recorded, and made visible with one rename, in that order, so no incomplete release is ever
//! visible under a release's name and a crash at any step is cleared by the next operation. A repair of a recorded
//! release stages the replacement beside the old folder, renames the old one aside, and renames the replacement in.

use std::collections::{BTreeMap, BTreeSet};

use contextos_core::Clock;
use sha2::{Digest, Sha256};

use crate::check::{ReadCheck, UnverifiableReason};
use crate::engine::{PackageEngine, ReleaseRules, StoreRules};
use crate::error::PackagesError;
use crate::io::{IssuesOperationIds, StoreFailure, StoreFailureKind, StoreIo};
use crate::ledger::{EntryProvenance, LedgerEntry};
use crate::manifest::{ManifestRejection, ReleaseManifest};
use crate::name::ReleaseKey;
use crate::path::{ReleaseFilePath, StorePath};
use crate::store::{InstallOutcome, InstallRequest, Installed};

fn rejection(message: String) -> PackagesError {
    PackagesError::InvalidManifest(ManifestRejection {
        code: crate::codes::INVALID_MANIFEST,
        message,
    })
}

impl From<UnverifiableReason> for PackagesError {
    /// A release that could not be verified, as the error an operation that needed it verified reports.
    fn from(reason: UnverifiableReason) -> Self {
        match reason {
            UnverifiableReason::StoreUnavailable(failure) => Self::Store(failure),
            UnverifiableReason::LimitExceeded(limits) => Self::LimitExceeded { limits },
            UnverifiableReason::ChangedWhileRead => Self::Store(StoreFailure {
                kind: StoreFailureKind::Failed,
                detail: "the release changed while it was read".to_owned(),
            }),
        }
    }
}

impl<I: StoreIo, M: ReleaseManifest, K: Clock, N: IssuesOperationIds> PackageEngine<I, M, K, N> {
    /// Checks the request against its own manifest and the store's limits, before anything is read or written.
    fn validate_request(&self, request: &InstallRequest) -> Result<M, PackagesError> {
        let manifest = M::try_from(request.manifest_text.as_str()).map_err(PackagesError::InvalidManifest)?;
        let mut listed: BTreeMap<&ReleaseFilePath, &contextos_core::ContentHash> = BTreeMap::new();
        for file in manifest.listed_files() {
            if listed.insert(&file.path, &file.hash).is_some() {
                return Err(rejection(format!("the manifest lists {} more than once", file.path)));
            }
        }
        for path in request.files.0.keys() {
            if !listed.contains_key(path) {
                return Err(rejection(format!("{path} is given but the manifest does not list it")));
            }
        }
        for (path, hash) in &listed {
            let Some(bytes) = request.files.0.get(*path) else {
                return Err(rejection(format!("the manifest lists {path} but it is not given")));
            };
            if contextos_core::ContentHash::from(<[u8; 32]>::from(Sha256::digest(bytes))) != **hash {
                return Err(rejection(format!("{path} does not hash to what the manifest lists")));
            }
        }
        if StoreRules.digest(&request.files) != manifest.release().digest {
            return Err(rejection("the files do not hash to the manifest's digest".to_owned()));
        }

        manifest
            .check_files(&request.files, self.io.home())
            .map_err(PackagesError::InvalidManifest)?;

        let folders: BTreeSet<String> = request
            .files
            .0
            .keys()
            .flat_map(|path| (1..path.segments().len()).map(|depth| path.segments()[..depth].join("/")))
            .collect();
        let entries = 1 + request.files.0.len() + folders.len();
        let bytes = request
            .files
            .0
            .values()
            .map(Vec::len)
            .chain(std::iter::once(request.manifest_text.len()))
            .fold(0u64, |sum, len| {
                sum.saturating_add(u64::try_from(len).unwrap_or(u64::MAX))
            });
        if entries > self.limits.max_entries || bytes > self.limits.max_bytes {
            return Err(PackagesError::LimitExceeded { limits: self.limits });
        }
        Ok(manifest)
    }

    /// Writes the release into a new staging folder: its folders, its files, then its manifest.
    async fn stage(&self, staging: &StorePath, request: &InstallRequest) -> Result<(), PackagesError> {
        self.io.create_dir_all(staging).await?;
        let mut created: BTreeSet<StorePath> = BTreeSet::new();
        for (path, bytes) in &request.files.0 {
            let target = staging.within(path);
            if let Some(parent) = target.parent()
                && parent != *staging
                && created.insert(parent.clone())
            {
                self.io.create_dir_all(&parent).await?;
            }
            self.io.write_new(&target, bytes).await?;
        }
        self.io
            .write_new(&staging.manifest(), request.manifest_text.as_bytes())
            .await?;
        Ok(())
    }

    /// Deletes a folder left by a step that failed, logging rather than failing: the next operation clears it.
    pub(crate) async fn discard(&self, folder: &StorePath) {
        if let Err(failure) = self.io.remove_tree(folder).await {
            tracing::warn!(folder = %folder, detail = %failure.detail, "a folder could not be deleted; the next operation retries");
        }
    }

    pub(crate) async fn install_release(&self, request: InstallRequest) -> Result<Installed, PackagesError> {
        let manifest = self.validate_request(&request)?;
        self.recover_unfinished().await?;
        let mut ledger = self.usable_ledger().await?;
        let release = manifest.release().clone();
        let interface = manifest.interface(&request.files);
        let folder = StorePath::from(&release.key);

        let position = ledger.entries.iter().position(|entry| entry.release.key == release.key);
        let mut folder_present = false;
        if let Some(index) = position {
            let recorded = &ledger.entries[index].release.digest;
            if *recorded != release.digest {
                return Err(PackagesError::DuplicateIdentity {
                    release: release.key,
                    recorded: recorded.clone(),
                    offered: release.digest,
                });
            }
            match self.check_release(&release.key).await {
                ReadCheck::Verified(_) => {
                    return Ok(Installed {
                        release,
                        outcome: InstallOutcome::Unchanged,
                    });
                }
                ReadCheck::Removed => {}
                ReadCheck::Incomplete(_) | ReadCheck::Damaged(_) => folder_present = true,
                ReadCheck::Unverifiable(reason) => return Err(reason.into()),
            }
        } else {
            match self.check_release(&release.key).await {
                ReadCheck::Removed => {}
                ReadCheck::Unverifiable(reason) => return Err(reason.into()),
                _ => return Err(not_recorded(&release.key)),
            }
            Self::check_version_step(&ledger, &release, &interface)?;
        }

        let staging = StorePath::staging_folder(&self.ids.issue());
        if let Err(error) = self.stage(&staging, &request).await {
            self.discard(&staging).await;
            return Err(error);
        }

        let now = self.clock.now();
        match position {
            Some(index) => {
                let entry = &mut ledger.entries[index];
                entry.removal = None;
                if entry.provenance == EntryProvenance::Rebuilt {
                    entry.provenance = EntryProvenance::Installed {
                        origin: request.origin,
                        installed_at: now,
                    };
                }
            }
            None => ledger.entries.push(LedgerEntry {
                release: release.clone(),
                interface,
                provenance: EntryProvenance::Installed {
                    origin: request.origin,
                    installed_at: now,
                },
                removal: None,
            }),
        }
        if let Err(error) = self.write_ledger(&ledger).await {
            self.discard(&staging).await;
            return Err(error);
        }

        match self.make_visible(&release.key, &staging, &folder, folder_present).await {
            Ok(()) => {
                tracing::debug!(package = release.key.name.as_str(), version = %release.key.version, "release installed");
                let outcome = if position.is_some() {
                    InstallOutcome::Repaired
                } else {
                    InstallOutcome::Installed
                };
                Ok(Installed { release, outcome })
            }
            Err(error) => {
                self.discard(&staging).await;
                Err(error)
            }
        }
    }

    /// Renames the staged release into place, moving an old folder aside first, and deletes the old folder.
    async fn make_visible(
        &self,
        key: &ReleaseKey,
        staging: &StorePath,
        folder: &StorePath,
        replace_old: bool,
    ) -> Result<(), PackagesError> {
        self.io.create_dir_all(&StorePath::from(&key.name)).await?;
        let aside = if replace_old {
            let aside = StorePath::removing_folder(&self.ids.issue());
            self.io.rename(folder, &aside).await?;
            Some(aside)
        } else {
            None
        };
        if let Err(failure) = self.io.rename(staging, folder).await {
            if let Some(aside) = &aside
                && let Err(restore) = self.io.rename(aside, folder).await
            {
                tracing::warn!(detail = %restore.detail, "the old release could not be renamed back; the next operation restores it");
            }
            return Err(failure.into());
        }
        if let Some(aside) = aside {
            self.discard(&aside).await;
        }
        Ok(())
    }
}

/// A release folder stands where the ledger records nothing: the ledger cannot be trusted to version the package.
pub(crate) fn not_recorded(key: &ReleaseKey) -> PackagesError {
    PackagesError::LedgerUnreadable {
        reason: format!(
            "{} {} is present but the ledger does not record it",
            key.name.as_str(),
            key.version
        ),
    }
}

#[cfg(test)]
#[path = "install_test.rs"]
mod tests;
