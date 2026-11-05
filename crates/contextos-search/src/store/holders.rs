//! Which `contextos` instances on the host have a derived store open, so a damaged store is only ever moved aside
//! when no other instance is using it.
//!
//! Every instance that opens a store registers as one of its holders for as long as it has it open: a file of its own
//! in the store's holder directory (`holders/<store>/` in the vault's state directory), named for its process and
//! held under an exclusive operating-system advisory lock. The lock is the proof the holder is alive: the operating
//! system releases it when the process exits, so a file whose lock can be taken belonged to a process that has gone,
//! and is removed. Registering, and anything that must happen while no other instance holds the store, run under the
//! registry's own lock (`holders/<store>.lock`), so a holder can never register in the middle of a store being moved
//! aside.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::{DerivedStore, StoreFailure, StoreWriteLock};

/// The extension every holder file carries.
const HOLDER_EXTENSION: &str = "holder";

/// Distinguishes the holder files one process registers, one per open store handle.
static NEXT_HOLDER: AtomicU64 = AtomicU64::new(0);

/// The holders of one derived store of one vault.
#[derive(Clone, Debug)]
pub struct HolderRegistry {
    store: DerivedStore,
    directory: PathBuf,
    lock: PathBuf,
}

/// This process's registration as a holder of a store, kept for as long as it has the store open and removed when it
/// is dropped.
#[derive(Debug)]
pub struct StoreHolder {
    path: PathBuf,
    file: Option<File>,
}

/// How many live holders a store has, by whether they are this process or another.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HolderCensus {
    /// Holders registered by this process, other than any the caller excluded.
    pub this_process: usize,
    /// Holders registered by other processes: other `contextos` instances.
    pub other_processes: usize,
}

impl HolderCensus {
    /// The number of holders counted.
    #[must_use]
    pub const fn total(self) -> usize {
        self.this_process.saturating_add(self.other_processes)
    }
}

impl HolderRegistry {
    /// The holders of `store` in the vault whose derived state lives in `state_directory`.
    #[must_use]
    pub fn new(state_directory: &Path, store: DerivedStore) -> Self {
        let holders = state_directory.join("holders");
        Self {
            store,
            directory: holders.join(store.name()),
            lock: holders.join(format!("{}.lock", store.name())),
        }
    }

    /// Registers this process as a holder of the store, waiting up to `wait` for the registry's lock.
    ///
    /// # Errors
    ///
    /// Returns a busy failure when the registry stays locked past `wait`, and an I/O failure when the holder file
    /// cannot be created or locked.
    pub fn register(&self, wait: Duration) -> Result<StoreHolder, StoreFailure> {
        let _registry = StoreWriteLock::acquire(self.store, &self.lock, wait)?;
        self.register_locked()
    }

    /// Registers this process as a holder; called with the registry's lock held.
    fn register_locked(&self) -> Result<StoreHolder, StoreFailure> {
        std::fs::create_dir_all(&self.directory).map_err(|error| self.io("creating the holder directory", &error))?;
        let name = format!(
            "{}-{}.{HOLDER_EXTENSION}",
            std::process::id(),
            NEXT_HOLDER.fetch_add(1, Ordering::Relaxed)
        );
        let path = self.directory.join(name);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| self.io("creating a holder file", &error))?;
        if !contextos_fs::try_lock_exclusive(&file).map_err(|error| self.io("locking a holder file", &error))? {
            return Err(self.io(
                "locking a holder file",
                &std::io::Error::other("a new holder file was already locked"),
            ));
        }
        Ok(StoreHolder { path, file: Some(file) })
    }

    /// Counts the store's live holders, other than `own`, removing the files of holders that have gone.
    ///
    /// # Errors
    ///
    /// Returns a busy failure when the registry stays locked past `wait`, and an I/O failure when the holder directory
    /// cannot be read.
    pub fn census(&self, own: Option<&StoreHolder>, wait: Duration) -> Result<HolderCensus, StoreFailure> {
        let _registry = StoreWriteLock::acquire(self.store, &self.lock, wait)?;
        self.live_holders(own)
    }

    /// Counts the store's live holders without writing anything: no lock is taken and no gone holder's file removed,
    /// for a diagnostic that must leave derived state as it found it. A holder registering or leaving meanwhile may be
    /// missed or counted, which a diagnostic tolerates and a decision to move a store aside never relies on.
    #[must_use]
    pub fn observe(&self) -> HolderCensus {
        let Ok(entries) = std::fs::read_dir(&self.directory) else {
            return HolderCensus::default();
        };
        let this_process = std::process::id();
        let mut census = HolderCensus::default();
        for path in entries.flatten().map(|entry| entry.path()) {
            let Some(pid) = holder_pid(&path) else {
                continue;
            };
            let Ok(file) = OpenOptions::new().read(true).write(true).open(&path) else {
                continue;
            };
            let live = match contextos_fs::try_lock_exclusive(&file) {
                Ok(taken) => !taken,
                Err(_) => true,
            };
            if !live {
                continue;
            }
            if pid == this_process {
                census.this_process = census.this_process.saturating_add(1);
            } else {
                census.other_processes = census.other_processes.saturating_add(1);
            }
        }
        census
    }

    /// Runs `body` only if the store has no live holder other than `own`, holding the registry's lock throughout so
    /// none can register meanwhile; otherwise returns the holders found, without running it.
    ///
    /// # Errors
    ///
    /// Returns a busy failure when the registry stays locked past `wait`, and an I/O failure when the holder directory
    /// cannot be read.
    pub fn while_sole_holder<T>(
        &self,
        own: Option<&StoreHolder>,
        wait: Duration,
        body: impl FnOnce() -> T,
    ) -> Result<Result<T, HolderCensus>, StoreFailure> {
        let _registry = StoreWriteLock::acquire(self.store, &self.lock, wait)?;
        let census = self.live_holders(own)?;
        if census.total() > 0 {
            return Ok(Err(census));
        }
        Ok(Ok(body()))
    }

    /// Runs `body` only if the store has no live holder at all, then registers this process as its holder, all with
    /// the registry's lock held throughout: no holder can register in between, and two instances doing this at once
    /// are ordered, the second finding the first registered. Otherwise returns the holders found, without running it.
    ///
    /// # Errors
    ///
    /// Returns a busy failure when the registry stays locked past `wait`, and an I/O failure when the holder directory
    /// cannot be read or the registration cannot be made.
    pub fn while_sole_holder_then_register<T>(
        &self,
        wait: Duration,
        body: impl FnOnce() -> T,
    ) -> Result<Result<(StoreHolder, T), HolderCensus>, StoreFailure> {
        let _registry = StoreWriteLock::acquire(self.store, &self.lock, wait)?;
        let census = self.live_holders(None)?;
        if census.total() > 0 {
            return Ok(Err(census));
        }
        let done = body();
        Ok(Ok((self.register_locked()?, done)))
    }

    /// Counts live holders other than `own`; called with the registry's lock held, so removing a dead holder's file
    /// can never race a new holder creating and locking one.
    fn live_holders(&self, own: Option<&StoreHolder>) -> Result<HolderCensus, StoreFailure> {
        let entries = match std::fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HolderCensus::default()),
            Err(error) => return Err(self.io("listing holders", &error)),
        };
        let this_process = std::process::id();
        let mut census = HolderCensus::default();
        for entry in entries {
            let entry = entry.map_err(|error| self.io("listing holders", &error))?;
            let path = entry.path();
            if own.is_some_and(|own| own.path == path) {
                continue;
            }
            let Some(pid) = holder_pid(&path) else {
                continue;
            };
            if !holder_is_live(&path) {
                continue;
            }
            if pid == this_process {
                census.this_process = census.this_process.saturating_add(1);
            } else {
                census.other_processes = census.other_processes.saturating_add(1);
            }
        }
        Ok(census)
    }

    fn io(&self, what: &str, error: &std::io::Error) -> StoreFailure {
        StoreFailure::io(
            self.store,
            self.directory.display().to_string(),
            format!("{what}: {error}"),
        )
    }
}

/// The process id a holder file is named for, or `None` for a file that is not a holder file.
fn holder_pid(path: &Path) -> Option<u32> {
    if path.extension()? != HOLDER_EXTENSION {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    let (pid, sequence) = stem.split_once('-')?;
    sequence.parse::<u64>().ok()?;
    pid.parse().ok()
}

/// Whether the holder file at `path` is still held by a live process: its lock cannot be taken. A file whose lock can
/// be taken belonged to a process that has gone, and is removed. A file that cannot be opened at all, for a reason
/// other than having gone, counts as live, so an unexpected error can only ever keep a store from being moved aside,
/// never allow it.
fn holder_is_live(path: &Path) -> bool {
    let file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return false,
        Err(error) => {
            tracing::debug!(%error, "a store holder file could not be opened; counting it as live");
            return true;
        }
    };
    match contextos_fs::try_lock_exclusive(&file) {
        Ok(true) => {
            if let Err(error) = std::fs::remove_file(path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::debug!(%error, "a gone store holder's file could not be removed; a later census retries");
            }
            false
        }
        Ok(false) => true,
        Err(error) => {
            tracing::debug!(%error, "a store holder file could not be checked; counting it as live");
            true
        }
    }
}

impl Drop for StoreHolder {
    /// Removes the holder file before closing it, so its name never outlives the lock that proves it live.
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::debug!(%error, "a store holder file could not be removed; a later census removes it");
        }
        drop(self.file.take());
    }
}

#[cfg(test)]
#[path = "holders_test.rs"]
mod tests;
