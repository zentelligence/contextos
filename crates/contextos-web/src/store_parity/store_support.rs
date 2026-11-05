//! Helpers the store cases share.

use contextos_packages::{LedgerEntry, LedgerState, PackagesError, ReadCheck};

use super::worlds::{BoxError, InterfaceOf, IoWorld, StoreWorld, install, ledger};
use crate::package_release_manifest::PackageRelease;
use crate::retention_fixture::archive;

/// Installs `crm` at each of `versions`, in order, without running retention or recording any state.
pub(super) async fn install_versions<W: IoWorld>(world: &W, versions: &[&str]) -> Result<(), BoxError> {
    for version in versions {
        install(world, &archive(version)?).await?;
    }
    Ok(())
}

/// A world holding `crm` at 1.0.0, 1.1.0, and 1.2.0 and the state `state`.
pub(super) async fn three_releases<W: StoreWorld>(state: &str) -> Result<W, BoxError> {
    let world = W::new().await?;
    install_versions(&world, &["1.0.0", "1.1.0", "1.2.0"]).await?;
    world.write_state(state)?;
    Ok(world)
}

/// The ledger entries, failing the test when the ledger is not usable.
pub(super) async fn entries<W: IoWorld>(world: &W) -> Result<Vec<LedgerEntry<InterfaceOf>>, BoxError> {
    match ledger(world).await?.state {
        LedgerState::Present(ledger) => Ok(ledger.entries),
        other => Err(format!("expected a usable ledger, found {other:?}").into()),
    }
}

/// The ledger entry of `crm` at `version`.
pub(super) async fn entry_of<W: IoWorld>(world: &W, version: &str) -> Result<LedgerEntry<InterfaceOf>, BoxError> {
    entries(world)
        .await?
        .into_iter()
        .find(|entry| entry.release.key.version.to_string() == version)
        .ok_or_else(|| format!("no ledger entry for crm {version}").into())
}

/// What a refusal's stable code is.
pub(super) fn code_of<T>(result: Result<T, PackagesError>) -> Result<&'static str, BoxError> {
    match result {
        Ok(_) => Err("expected a refusal, but the call succeeded".into()),
        Err(error) => Ok(error.code()),
    }
}

/// A short name for how a release read, for assertion messages and comparisons.
pub(super) fn condition_of(check: &ReadCheck<PackageRelease>) -> &'static str {
    match check {
        ReadCheck::Verified(_) => "verified",
        ReadCheck::Removed => "removed",
        ReadCheck::Incomplete(_) => "incomplete",
        ReadCheck::Damaged(_) => "damaged",
        ReadCheck::Unverifiable(_) => "unverifiable",
    }
}

/// Overwrites the workflow script of `crm` at `version` on disk, so the release no longer matches its manifest.
pub(super) fn tamper<W: IoWorld>(world: &W, version: &str) -> Result<(), BoxError> {
    std::fs::write(
        world.store_dir().join(format!("crm/{version}/sync/sync.rhai")),
        b"tampered",
    )?;
    Ok(())
}

/// Copies the folder `from` to `to`, files and folders only.
pub(super) fn copy_tree(from: &std::path::Path, to: &std::path::Path) -> Result<(), BoxError> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// The versions the recorded `future` of the workflow `sync` of `crm` names, if any.
pub(super) fn future_of<W: super::worlds::StoreWorld>(world: &W) -> Result<Option<String>, BoxError> {
    Ok(world
        .state()?
        .package_releases("crm")
        .and_then(|releases| releases.workflows.get("sync"))
        .and_then(|versions| versions.future.as_ref())
        .map(ToString::to_string))
}
