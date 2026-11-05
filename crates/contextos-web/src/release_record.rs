//! Whether a package store's ledger has ever recorded a release, which separates a version the store never had from
//! one it had and no longer holds whole.

use contextos_packages::{LedgerState, PackageStore, PackagesError, ReleaseKey, ReleaseName, ReleaseVersion};
use contextos_workflows::PackageName;
use semver::Version;

/// Whether `store`'s ledger records `version` of `package`, removed or not.
///
/// A store with no ledger and no releases has recorded nothing. A ledger that cannot be used says nothing either way,
/// so the release is taken as recorded and whatever is wrong with the store is reported when the release is read.
///
/// # Errors
///
/// Returns the error that reading the store's ledger gives.
pub(crate) async fn ever_recorded<S: PackageStore>(
    store: &S,
    package: &PackageName,
    version: &Version,
) -> Result<bool, PackagesError> {
    let release = ReleaseKey {
        name: ReleaseName::try_from(package.as_str())?,
        version: ReleaseVersion::try_from(version.to_string().as_str())?,
    };
    Ok(match store.ledger().await?.state {
        LedgerState::Present(ledger) => ledger.entries.iter().any(|entry| entry.release.key == release),
        LedgerState::Absent => false,
        LedgerState::Unusable(_) => true,
    })
}

/// Whether the host store at `store_root` has never recorded `version` of `package`, read on a blocking thread.
///
/// # Errors
///
/// Returns the failure to read the store, or to ask about a package or version the store cannot name, as text.
pub(crate) async fn host_never_recorded(
    store_root: &std::path::Path,
    package: &str,
    version: &Version,
) -> Result<bool, String> {
    let root = store_root.to_path_buf();
    let package = PackageName::try_from(package).map_err(|error| error.to_string())?;
    let version = version.clone();
    tokio::task::spawn_blocking(move || crate::host_store::HostStore::new(&root).ever_recorded(&package, &version))
        .await
        .map_err(|error| error.to_string())?
        .map(|recorded| !recorded)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
#[path = "release_record_test.rs"]
mod tests;
