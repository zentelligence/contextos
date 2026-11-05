//! Installing a bundle, or a folder laid out as one, into a vault's package store: `packages install <vault> <bundle>`.
//!
//! The same procedure as the host's bundle install, over the vault's store: the bundle is verified before the store is
//! touched, the store's lock is taken and held, the libraries are checked against what the store holds, the release is
//! installed, and then the recorded `future` and retention are brought up to date, whose failures are warnings on an
//! install that was made.

use std::path::Path;

use contextos_packages::{InstallRequest, PackageStore, ReadCheck, ReleaseOrigin, VerifiedRelease};
use contextos_workflows::{PackageKind, ReleaseContents, ReleaseRef};

use crate::package_bundle::{self, ExtractedBundle};
use crate::package_graph::StoreGraph;
use crate::package_op_error::PackageOpError;
use crate::package_release_manifest::{PackageFilesOf, PackageRelease};
use crate::package_store::{Installed, MAX_BUNDLE_BYTES};
use crate::package_stores::VaultPackagesOf;
use crate::store_retention::settle_vault_after_change;

/// A bundle file or a package folder named on the command line.
pub struct BundlePath<'a>(pub &'a Path);

impl TryFrom<BundlePath<'_>> for ExtractedBundle {
    type Error = PackageOpError;

    /// Reads and verifies the bundle or folder, refusing one over the size limit before reading it. Blocking.
    fn try_from(path: BundlePath<'_>) -> Result<Self, Self::Error> {
        let path = path.0;
        let metadata = std::fs::metadata(path)?;
        let limit = u64::try_from(MAX_BUNDLE_BYTES).unwrap_or(u64::MAX);
        if metadata.is_dir() {
            let size = package_bundle::folder_size(path)?;
            if size > limit {
                return Err(PackageOpError::FolderTooLarge {
                    size,
                    limit: MAX_BUNDLE_BYTES,
                });
            }
            Ok(package_bundle::verify_and_copy_folder(path)?)
        } else if metadata.is_file() {
            if metadata.len() > limit {
                return Err(PackageOpError::BundleTooLarge {
                    size: usize::try_from(metadata.len()).unwrap_or(usize::MAX),
                    limit: MAX_BUNDLE_BYTES,
                });
            }
            Ok(package_bundle::verify_and_extract(&std::fs::read(path)?)?)
        } else {
            Err(PackageOpError::StoreIo(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the package path is neither a bundle file nor a folder",
            )))
        }
    }
}

impl VaultPackagesOf<'_> {
    /// Installs a verified bundle into the vault's store.
    ///
    /// The store's lock is taken, not waited for, and held until the release is installed and the recorded `future` and
    /// retention are brought up to date. A failure of either is a warning on the install, which was made.
    ///
    /// # Errors
    ///
    /// [`PackageOpError::Unresolvable`] when a library the bundle needs, or one an installed release needs, would not
    /// resolve; [`PackageOpError::Packages`] for a store that is locked, a ledger that cannot be used, or other content
    /// under a version already installed; nothing is installed unless the install is reported.
    pub async fn install(&self, extracted: ExtractedBundle) -> Result<Installed, PackageOpError> {
        let engine = self.client.vault_packages(self.vault).await?;
        let lock = engine.lock().await?;
        engine.recover(&lock).await?;
        check_resolves(
            &engine,
            ReleaseContents {
                manifest: &extracted.manifest,
                kind: &extracted.kind,
            },
        )
        .await?;
        engine
            .install(
                &lock,
                InstallRequest {
                    manifest_text: extracted.manifest_text,
                    files: extracted.files,
                    origin: ReleaseOrigin::Bundle,
                },
            )
            .await?;
        let identity = extracted.manifest.identity;
        let settled = settle_vault_after_change(&engine, &lock, self.client, self.vault, &identity.name).await;
        Ok(Installed {
            removed_by_retention: settled
                .removed
                .iter()
                .any(|gone: &ReleaseRef| gone.name == identity.name && gone.version == identity.version),
            warnings: settled.warnings,
            identity,
        })
    }
}

/// Refuses a release whose libraries do not resolve, and one that would leave an installed release unable to resolve its
/// own. A release that could not resolve before the install is not blamed on it.
async fn check_resolves<S>(engine: &S, contents: ReleaseContents<'_>) -> Result<(), PackageOpError>
where
    S: PackageStore<Manifest = PackageRelease>,
{
    let mut installed: Vec<(VerifiedRelease<PackageRelease>, PackageKind)> = Vec::new();
    for key in engine.list().await?.releases {
        if let ReadCheck::Verified(verified) = engine.read_release(&key).await
            && let Ok(kind) = PackageKind::try_from(PackageFilesOf {
                manifest: verified.manifest.manifest(),
                files: &verified.files,
            })
        {
            installed.push((verified, kind));
        }
    }
    let graph: StoreGraph = installed
        .iter()
        .map(|(verified, kind)| ReleaseContents {
            manifest: verified.manifest.manifest(),
            kind,
        })
        .collect();
    let broken = graph.broken_by(&graph.with(contents));
    if broken.is_empty() {
        Ok(())
    } else {
        Err(PackageOpError::Unresolvable { releases: broken })
    }
}

#[cfg(test)]
#[path = "vault_bundle_install_test.rs"]
mod tests;
