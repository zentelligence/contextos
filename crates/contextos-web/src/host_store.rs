//! The host package store as the package engine runs it: the engine over the store's folder on this host, driven from
//! the synchronous callers this crate's package store has always had.
//!
//! The engine is asynchronous because a vault's store is read through a session. The host store's own callers are
//! synchronous (the command line, and handlers that already move their work to a blocking thread), so each call here
//! runs the engine to completion on a thread of its own with a small runtime of its own. That keeps the caller's thread
//! free of any runtime it may or may not be inside, and the engine's blocking filesystem work still runs on that
//! runtime's blocking threads.

use std::future::Future;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use contextos_core::SystemClock;
use contextos_packages::host_io::HostStoreIo;
use contextos_packages::{
    InstallRequest, IssuesOperationIds, OperationId, PackageEngine, PackageStore, ReadCheck, ReadLimits, Rebuilt,
    ReleaseKey, ReleaseName,
};

use crate::package_op_error::PackageOpError;
use crate::package_release_manifest::PackageRelease;
use crate::package_removal::{Removal, remove_guarded};
use crate::package_retention::{Retained, RetentionError, RetentionHome, retain};
use crate::package_store::{MAX_BUNDLE_BYTES, PackageStoreError};

/// The most entries a release folder may hold, files and folders together.
const MAX_RELEASE_ENTRIES: usize = 10_000;

/// A caller that finds the store's lock held is turned away at once, not queued behind the holder.
const LOCK_WAIT: Duration = Duration::ZERO;

/// Issues the identifiers of staging and removal folders: the time in microseconds, and a sequence number for the
/// operations of one process that fall in the same microsecond.
#[derive(Debug, Default)]
pub(crate) struct HostOperationIds {
    sequence: AtomicU64,
}

impl IssuesOperationIds for HostOperationIds {
    fn issue(&self) -> OperationId {
        let micros = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| u64::try_from(elapsed.as_micros()).ok())
            .unwrap_or_default();
        // Microseconds since the epoch are below 2^51 for the next few thousand years, so 12 bits of sequence fit.
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) % 4096;
        OperationId::from(micros.saturating_mul(4096).saturating_add(sequence))
    }
}

pub(crate) type HostEngine = PackageEngine<HostStoreIo, PackageRelease, SystemClock, HostOperationIds>;

/// The guard of the host store's lock, released when dropped.
pub(crate) type HostLock = <HostEngine as PackageStore>::Lock;

/// One release as the engine read it.
pub(crate) type Checked = (ReleaseKey, ReadCheck<PackageRelease>);

/// The host package store at one root.
pub(crate) struct HostStore {
    root: std::path::PathBuf,
    engine: HostEngine,
}

impl HostStore {
    /// The store whose folder is `root`, which need not exist yet.
    pub(crate) fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            engine: PackageEngine {
                io: HostStoreIo::new(root.to_path_buf(), LOCK_WAIT),
                clock: SystemClock,
                ids: HostOperationIds::default(),
                limits: ReadLimits {
                    max_entries: MAX_RELEASE_ENTRIES,
                    max_bytes: u64::try_from(MAX_BUNDLE_BYTES).unwrap_or(u64::MAX),
                },
                manifest: std::marker::PhantomData,
            },
        }
    }

    /// The folder the store is rooted at.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// The engine over the store's folder, for an asynchronous caller that is already on a runtime.
    pub(crate) const fn engine(&self) -> &HostEngine {
        &self.engine
    }

    /// Runs `work` against the engine to completion on a thread of its own.
    fn run<'a, F, T>(&'a self, work: impl FnOnce(&'a HostEngine) -> F + Send) -> Result<T, PackageStoreError>
    where
        F: Future<Output = Result<T, contextos_packages::PackagesError>>,
        T: Send,
    {
        self.run_to_end(work)?.map_err(PackageStoreError::from)
    }

    /// Runs `work`, whatever it answers, to completion on a thread of its own.
    fn run_to_end<'a, F, T>(&'a self, work: impl FnOnce(&'a HostEngine) -> F + Send) -> Result<T, PackageStoreError>
    where
        F: Future<Output = T>,
        T: Send,
    {
        std::thread::scope(|scope| {
            std::thread::Builder::new()
                .name("package-store".to_owned())
                .spawn_scoped(scope, || {
                    let runtime = tokio::runtime::Builder::new_current_thread().build()?;
                    Ok::<_, std::io::Error>(runtime.block_on(work(&self.engine)))
                })
                .map_err(PackageStoreError::Io)
                .map(|handle| {
                    handle
                        .join()
                        .unwrap_or_else(|_| Err(std::io::Error::other("the package store operation did not finish")))
                })
        })?
        .map_err(PackageStoreError::Io)
    }

    /// Runs retention under the lock `lock` holds. The outer error is the thread or runtime failing to run it; the inner is the
    /// run's own.
    pub(crate) fn retain(
        &self,
        lock: &HostLock,
        home: &impl RetentionHome,
    ) -> Result<Result<Retained, RetentionError>, PackageStoreError> {
        self.run_to_end(|engine| retain(engine, lock, home))
    }

    /// Removes a release at an operator's request, under the lock `lock` holds, refusing one a rule keeps unless `force`. The
    /// outer error is the thread or runtime failing to run it; the inner is the removal's own.
    pub(crate) fn remove_guarded(
        &self,
        lock: &HostLock,
        home: &impl RetentionHome,
        (key, force): (&ReleaseKey, bool),
    ) -> Result<Result<Removal, PackageOpError>, PackageStoreError> {
        self.run_to_end(|engine| remove_guarded(engine, lock, home, key, force))
    }

    /// Takes the store's lock.
    ///
    /// # Errors
    ///
    /// [`PackageStoreError::StoreLocked`] when another operation holds it.
    pub(crate) fn lock(&self) -> Result<HostLock, PackageStoreError> {
        self.run(PackageStore::lock)
    }

    /// Whether the store's ledger has ever recorded `version` of `package`, removed or not.
    pub(crate) fn ever_recorded(
        &self,
        package: &contextos_workflows::PackageName,
        version: &semver::Version,
    ) -> Result<bool, PackageStoreError> {
        self.run(|engine| crate::release_record::ever_recorded(engine, package, version))
    }

    /// Every release in the store, read and checked, in release order. An empty or absent store has none.
    pub(crate) fn read_all(&self) -> Result<Vec<Checked>, PackageStoreError> {
        self.read_where(|_| true)
    }

    /// Every release of one package, read and checked, in version order.
    pub(crate) fn read_package(&self, name: &ReleaseName) -> Result<Vec<Checked>, PackageStoreError> {
        self.read_where(|key| key.name == *name)
    }

    fn read_where(&self, wanted: impl Fn(&ReleaseKey) -> bool + Send) -> Result<Vec<Checked>, PackageStoreError> {
        self.run(|engine| async move {
            let listing = engine.list().await?;
            let mut checked = Vec::new();
            for key in listing.releases.into_iter().filter(|key| wanted(key)) {
                let check = engine.read_release(&key).await;
                checked.push((key, check));
            }
            Ok(checked)
        })
    }

    /// One release, read and checked.
    pub(crate) fn read_release(&self, key: &ReleaseKey) -> Result<ReadCheck<PackageRelease>, PackageStoreError> {
        self.run(|engine| async move { Ok(engine.read_release(key).await) })
    }

    /// Installs a release, under the lock `lock` holds.
    pub(crate) fn install(
        &self,
        lock: &HostLock,
        request: InstallRequest,
    ) -> Result<contextos_packages::Installed, PackageStoreError> {
        self.run(|engine| engine.install(lock, request))
    }

    /// Rebuilds a missing ledger from the releases present, under the lock `lock` holds.
    pub(crate) fn rebuild_ledger(&self, lock: &HostLock) -> Result<Rebuilt, PackageStoreError> {
        self.run(|engine| engine.rebuild_ledger(lock))
    }
}
