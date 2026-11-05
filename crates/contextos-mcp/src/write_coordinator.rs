//! Serialises mutations of a vault, within this process and across every `contextos` process on the host.
//!
//! One operator commonly runs several `contextos` instances against the same vault at once: one per interactive
//! agent session, one per scheduled session, and the one `contextos-web` holds. A conflict-checked write compares the
//! file's hash and then renames a temporary file over it, and two processes that both pass the comparison would both
//! rename, the second silently replacing the first. So a mutating tool call holds two locks per vault it touches for
//! its whole duration: an in-process lock, so this process's own calls queue rather than contend for the file, and
//! an operating-system advisory lock on a file in the vault's state directory, which every process on the host
//! resolves to the same place. The advisory lock is released by the operating system when its holder exits, so a
//! crashed process never leaves a vault locked.
//!
//! The lock lives in the state directory rather than the vault because advisory locks are only dependable on a local
//! disk, and a vault may sit in a synchronised or network folder. It is shared by processes that resolve the same
//! state directory, which is every process run by the same user on the same host with default configuration.
//!
//! The host lock, with its first-come queue and bounded wait, is `contextos_fs::lock_host`; `contextos-web` takes the
//! same lock through `contextos_fs::lock_vault_writes`. This module adds the in-process lock in front of it.
//!
//! Lock order: a vault's locks here come first, then the host-level projection lock (`projection_lock.rs`), never
//! the reverse. A tool write holds its vault locks while the router calls agent host projection, which takes the
//! projection lock; projection never takes a vault lock.
//!

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use contextos_core::VaultRootId;
use contextos_fs::{DEFAULT_HOST_WAIT, HostLockGuard, lock_host};
use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::tool_error::ToolError;

#[derive(Debug)]
struct RootLock {
    process: Arc<Mutex<()>>,
    host: PathBuf,
}

/// Holds a vault's write locks for as long as it lives: the in-process lock and the host lock file, whose advisory
/// lock is released when the file is closed on drop.
#[derive(Debug)]
pub(crate) struct RootGuard {
    _process: OwnedMutexGuard<()>,
    _host: HostLockGuard,
}

/// Hands out the write locks of every configured vault, in the order the vaults are configured.
#[derive(Clone, Debug)]
pub(crate) struct WriteCoordinator {
    roots: Arc<Vec<RootLock>>,
    host_wait: Duration,
}

/// The host lock file of each configured vault, in configuration order, and how long to wait for one held by another
/// process.
pub(crate) struct WriteCoordinatorConfig {
    pub(crate) host_locks: Vec<PathBuf>,
    pub(crate) host_wait: Duration,
}

impl From<Vec<PathBuf>> for WriteCoordinatorConfig {
    fn from(host_locks: Vec<PathBuf>) -> Self {
        Self {
            host_locks,
            host_wait: DEFAULT_HOST_WAIT,
        }
    }
}

impl From<WriteCoordinatorConfig> for WriteCoordinator {
    fn from(value: WriteCoordinatorConfig) -> Self {
        let roots = value
            .host_locks
            .into_iter()
            .map(|host| RootLock {
                process: Arc::new(Mutex::new(())),
                host,
            })
            .collect();
        Self {
            roots: Arc::new(roots),
            host_wait: value.host_wait,
        }
    }
}

impl WriteCoordinator {
    /// Takes one vault's host lock file for a caller that is not on an async task and shares the vault with nothing
    /// in this process yet, as the server's own start-up does. The lock is released when the returned file is
    /// dropped.
    ///
    /// # Errors
    ///
    /// As [`Self::lock_roots`], for the host lock alone.
    pub(crate) fn hold_host_lock(&self, root_index: usize) -> Result<HostLockGuard, ToolError> {
        let root = self
            .roots
            .get(root_index)
            .ok_or(ToolError::RootLockMissing { root_index })?;
        Ok(lock_host(&root.host, self.host_wait)?)
    }

    /// Takes the write locks of every vault in `roots`, always in configuration order so two calls touching the same
    /// vaults cannot deadlock, waiting first for this process's other calls and then for other processes.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::RootLockMissing`] for a root this coordinator was not configured with,
    /// [`ToolError::VaultBusy`] when another process holds a vault's host lock beyond the configured wait, and
    /// [`ToolError::HostWriteLock`] when the host lock file cannot be created, opened, or locked.
    pub(crate) async fn lock_roots(&self, roots: &[VaultRootId]) -> Result<Vec<RootGuard>, ToolError> {
        let mut indices = roots
            .iter()
            .copied()
            .map(usize::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        indices.sort_unstable();
        indices.dedup();

        let mut guards = Vec::with_capacity(indices.len());
        for root_index in indices {
            let root = self
                .roots
                .get(root_index)
                .ok_or(ToolError::RootLockMissing { root_index })?;
            let process = Arc::clone(&root.process).lock_owned().await;
            let host_path = root.host.clone();
            let wait = self.host_wait;
            let host = tokio::task::spawn_blocking(move || lock_host(&host_path, wait)).await??;
            guards.push(RootGuard {
                _process: process,
                _host: host,
            });
        }
        Ok(guards)
    }
}

#[cfg(test)]
#[path = "write_coordinator_test.rs"]
mod tests;
