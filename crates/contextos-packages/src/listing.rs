//! Reading a store's folder by name, without reading any release.

use contextos_core::Clock;

use crate::engine::PackageEngine;
use crate::error::PackagesError;
use crate::io::{IssuesOperationIds, StoreEntryKind, StoreFailureKind, StoreIo};
use crate::manifest::ReleaseManifest;
use crate::name::{ReleaseKey, ReleaseVersion};
use crate::path::{LOCK_QUEUE_NAME, StoreEntryName, StorePath};
use crate::store::StoreListing;

impl<I: StoreIo, M: ReleaseManifest, K: Clock, N: IssuesOperationIds> PackageEngine<I, M, K, N> {
    /// What the store's folder holds, by name. An empty listing for a store whose root does not exist yet.
    pub(crate) async fn list_store(&self) -> Result<StoreListing, PackagesError> {
        let mut listing = StoreListing::default();
        let top = match self.io.list(&StorePath::root()).await {
            Ok(entries) => entries,
            Err(failure) if failure.kind == StoreFailureKind::NotFound => return Ok(listing),
            Err(failure) => return Err(failure.into()),
        };
        for entry in top {
            let is_folder = entry.kind == StoreEntryKind::Directory;
            match StoreEntryName::from(entry.name.as_str()) {
                StoreEntryName::Ledger if matches!(entry.kind, StoreEntryKind::File { .. }) => {}
                // The queue is a folder; the lock and the queue's guard are files.
                StoreEntryName::Lock if is_folder == (entry.name == LOCK_QUEUE_NAME) => {}
                StoreEntryName::Package(name) if is_folder => {
                    let children = match self.io.list(&StorePath::from(&name)).await {
                        Ok(children) => children,
                        Err(failure) if failure.kind == StoreFailureKind::NotFound => continue,
                        Err(failure) => return Err(failure.into()),
                    };
                    for child in children {
                        let version = ReleaseVersion::try_from(child.name.as_str())
                            .ok()
                            .filter(|version| version.to_string() == child.name);
                        match version {
                            Some(version) if child.kind == StoreEntryKind::Directory => {
                                listing.releases.push(ReleaseKey {
                                    name: name.clone(),
                                    version,
                                });
                            }
                            _ => listing.unrecognised.push(format!("{}/{}", name.as_str(), child.name)),
                        }
                    }
                }
                StoreEntryName::Staging(id) if is_folder => listing.staging.push(id),
                StoreEntryName::Removing(id) if is_folder => listing.removing.push(id),
                _ => listing.unrecognised.push(entry.name),
            }
        }
        listing.releases.sort();
        listing.staging.sort();
        listing.removing.sort();
        listing.unrecognised.sort();
        Ok(listing)
    }

    /// Whether anything stands where the release's folder belongs, of any kind.
    pub(crate) async fn release_folder_present(&self, key: &ReleaseKey) -> Result<bool, PackagesError> {
        match self.io.list(&StorePath::from(&key.name)).await {
            Ok(children) => {
                let folder = key.version.to_string();
                Ok(children.iter().any(|child| child.name == folder))
            }
            Err(failure) if failure.kind == StoreFailureKind::NotFound => Ok(false),
            Err(failure) => Err(failure.into()),
        }
    }
}

#[cfg(test)]
#[path = "listing_test.rs"]
mod tests;
