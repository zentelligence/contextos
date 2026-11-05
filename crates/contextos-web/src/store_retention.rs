//! Retention for each store: the host's under its state lock and store lock, a vault's under its store lock.
//!
//! The run itself is [`crate::package_retention::retain`]. What belongs here is what surrounds it: taking the locks in
//! the one order, rewriting a workflow's recorded `future` when a removal changed it (by the one rule every writer of
//! `future` uses), and what a failed run is told as, which is a warning on the operation that triggered it and never a
//! failure of that operation.

use std::collections::BTreeSet;
use std::path::Path;

use contextos_packages::{PackageStore, ReleaseKey};
use contextos_workflows::{JobsState, PackageName, ReleaseRef, StoreKind};
use serde::Serialize;

use crate::host_store::{HostLock, HostStore};
use crate::mcp_client::McpClient;
use crate::package_futures::FutureRecord;
use crate::package_release_manifest::PackageRelease;
use crate::package_retention::{Retained, RetentionError, retain};
use crate::package_standing::KeyedRelease;
use crate::package_store::record_futures;
use crate::retention_job_files::{HostHome, VaultHome};
use crate::saved_jobs::SavedJobs;
use crate::scheduler::{read_state, write_state};
use crate::vault_source_install::installed_workflows;

/// The releases a retention run removed.
#[derive(Clone, Debug, Default)]
pub(crate) struct Removals(Vec<ReleaseKey>);

impl Removals {
    /// Whether the run removed `version` of `package`.
    pub(crate) fn include(&self, package: &PackageName, version: &semver::Version) -> bool {
        self.0
            .iter()
            .any(|key| key.name.as_str() == package.as_str() && key.version.as_semver() == version)
    }
}

impl From<&Removals> for Vec<ReleaseRef> {
    /// The releases removed, in the order the run removed them. A key the policy does not name is not one retention
    /// could have removed, so it is left out.
    fn from(removals: &Removals) -> Self {
        removals
            .0
            .iter()
            .filter_map(|key| ReleaseRef::try_from(KeyedRelease(key)).ok())
            .collect()
    }
}

/// What settling a store after an operator's removal did: what could not be finished, and the releases retention removed
/// because only the release removed kept them.
#[derive(Debug, Default)]
pub(crate) struct Settlement {
    pub(crate) warnings: Vec<RetentionWarning>,
    pub(crate) removed: Vec<ReleaseRef>,
}

impl From<&Retained> for Removals {
    fn from(retained: &Retained) -> Self {
        Self(
            retained
                .settled
                .removed
                .iter()
                .map(|release| release.key.clone())
                .collect(),
        )
    }
}

/// How many times recording `future` repeats when another writer changes the job state under it.
const STATE_ATTEMPTS: usize = 3;

/// A retention run that did not finish, as the operation that triggered it reports it. The operation itself succeeded;
/// the next change to what is kept runs retention again.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RetentionWarning {
    code: &'static str,
    message: String,
}

impl RetentionWarning {
    /// The stable machine-readable code.
    pub const CODE: &'static str = RetentionError::CODE;

    /// What went wrong, in words, with no vault content.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for RetentionWarning {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

#[cfg(test)]
impl RetentionWarning {
    /// A warning with the message given, for tests of what reports one.
    pub(crate) fn raised_for_report(message: &str) -> Self {
        Self {
            code: Self::CODE,
            message: message.to_owned(),
        }
    }
}

impl RetentionWarning {
    /// The warning for a run that did not finish on `store`, which is logged once with the code and no content.
    pub(crate) fn raised(store: StoreKind, error: &RetentionError) -> Self {
        tracing::warn!(
            store = store.as_str(),
            code = error.code(),
            "retention did not finish; the next change to what is kept runs it again"
        );
        Self {
            code: Self::CODE,
            message: error.to_string(),
        }
    }
}

/// Retention on the host's store, under the state lock and then the store lock.
///
/// Blocking: waits for the state lock, so call it from a blocking thread.
///
/// # Errors
///
/// [`RetentionError`] when a lock cannot be taken, or the run does not finish.
pub fn retain_host(store_root: &Path, saved: &SavedJobs) -> Result<(), RetentionError> {
    saved.state.update(|state| {
        let store = HostStore::new(store_root);
        let lock = store.lock()?;
        retain_host_held(&store, &lock, saved, state).map(|_| ())
    })?
}

/// Retention on the host's store, by a caller that holds the state lock and the store lock, so a change of its own and
/// the removals it makes are written in the one state write. `state` is the state the caller holds. Returns what it
/// removed.
///
/// Blocking.
///
/// # Errors
///
/// [`RetentionError`] when the run does not finish.
pub(crate) fn retain_host_held(
    store: &HostStore,
    lock: &HostLock,
    saved: &SavedJobs,
    state: &mut JobsState,
) -> Result<Removals, RetentionError> {
    let home = HostHome::of(state.clone(), saved);
    let retained = store.retain(lock, &home)??;
    for package in checked_packages(&retained) {
        record_futures(store.root(), &package, state)?;
    }
    Ok(Removals::from(&retained))
}

/// What a change to a store's releases at an operator's request (a removal) leaves to bring up to date on the host, by a caller that holds the state lock and
/// the store lock: the `future` the removal changed, then retention, since releases only the removed one held are no
/// longer kept. The removal itself is done, so a failure is a warning on it.
///
/// Blocking.
pub(crate) fn settle_host_after_change(
    store: &HostStore,
    lock: &HostLock,
    saved: &SavedJobs,
    state: &mut JobsState,
    package: &PackageName,
) -> Settlement {
    let outcome = record_futures(store.root(), package, state)
        .map_err(RetentionError::from)
        .and_then(|()| retain_host_held(store, lock, saved, state));
    Settlement::from((StoreKind::Host, outcome))
}

impl From<(StoreKind, Result<Removals, RetentionError>)> for Settlement {
    fn from((store, outcome): (StoreKind, Result<Removals, RetentionError>)) -> Self {
        match outcome {
            Ok(removals) => Self {
                warnings: Vec::new(),
                removed: Vec::from(&removals),
            },
            Err(error) => Self {
                warnings: vec![RetentionWarning::raised(store, &error)],
                removed: Vec::new(),
            },
        }
    }
}

/// What a change to a store's releases (a removal, or an install) leaves to bring up to date in a vault, by a caller that
/// holds the store lock, as
/// [`settle_host_after_change`] does for the host.
pub(crate) async fn settle_vault_after_change<S>(
    engine: &S,
    lock: &S::Lock,
    client: &McpClient,
    vault: &str,
    package: &PackageName,
) -> Settlement
where
    S: PackageStore<Manifest = PackageRelease>,
{
    let outcome = async {
        let installed = installed_workflows(engine, package).await?;
        rewrite_future(client, vault, package, &installed).await?;
        retain_vault_held(engine, lock, client, vault).await
    }
    .await;
    Settlement::from((StoreKind::Vault, outcome))
}

/// Retention on `vault`'s store, under the vault store lock.
///
/// # Errors
///
/// [`RetentionError`] when the lock cannot be taken, or the run does not finish.
pub async fn retain_vault(client: &McpClient, vault: &str) -> Result<(), RetentionError> {
    let engine = client.vault_packages(vault).await?;
    let lock = engine.lock().await?;
    retain_vault_held(&engine, &lock, client, vault).await.map(|_| ())
}

/// Retention on a vault's store, by a caller that holds the store lock.
///
/// The job state is optimistic, so a recorded `future` the removals changed is rewritten from the store as it then
/// stands, and repeated if another writer changed the state in the meantime.
///
/// # Errors
///
/// [`RetentionError`] when the run does not finish.
pub(crate) async fn retain_vault_held<S>(
    engine: &S,
    lock: &S::Lock,
    client: &McpClient,
    vault: &str,
) -> Result<Removals, RetentionError>
where
    S: PackageStore<Manifest = PackageRelease>,
{
    let retained = retain(engine, lock, &VaultHome { client, vault }).await?;
    for package in checked_packages(&retained) {
        let installed = installed_workflows(engine, &package).await?;
        rewrite_future(client, vault, &package, &installed).await?;
    }
    Ok(Removals::from(&retained))
}

/// Records `package`'s `future` for each workflow from `installed`, repeating when another writer changes the state.
async fn rewrite_future(
    client: &McpClient,
    vault: &str,
    package: &PackageName,
    installed: &[(String, semver::Version)],
) -> Result<(), RetentionError> {
    for _ in 0..STATE_ATTEMPTS {
        let mut snapshot = read_state(client, vault).await?;
        let changed = FutureRecord {
            package,
            state: &mut snapshot.value,
        }
        .record(installed.iter().cloned());
        if !changed {
            return Ok(());
        }
        match write_state(client, vault, &snapshot).await {
            Ok(()) => return Ok(()),
            Err(error) if error.is_conflict() => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(RetentionError::StateKeptChanging)
}

/// The packages whose recorded `future` a run checks against the store: every package it read, and every one it removed
/// a release of.
fn checked_packages(retained: &Retained) -> BTreeSet<PackageName> {
    retained
        .settled
        .removed
        .iter()
        .filter_map(|release| PackageName::try_from(release.key.name.as_str()).ok())
        .chain(retained.packages.iter().cloned())
        .collect()
}

#[cfg(test)]
#[path = "store_retention_test.rs"]
mod tests;
