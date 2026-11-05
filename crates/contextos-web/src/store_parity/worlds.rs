//! The two layers every parity case is written against: the store's I/O (`IoWorld`) and the store as a whole
//! (`StoreWorld`), each implemented once for the host store and once for a vault's store.
//!
//! A world owns a real temporary store on disk. Both stores are plain folders, so a case that needs to lay out, tamper
//! with, or inspect the store does so with `std::fs` on [`IoWorld::store_dir`], and what it observes through the
//! adapter is compared with what is on disk.

use std::any::Any;
use std::path::{Path, PathBuf};
use std::time::Duration;

use contextos_core::SystemClock;
use contextos_packages::{
    InstallRequest, Installed, LedgerReading, PackageEngine, PackageStore, PackagesError, ReadCheck, ReadLimits,
    Recovered, ReleaseKey, ReleaseManifest, ReleaseName, ReleaseOrigin, ReleaseVersion, StoreIo, StoreListing,
};
use contextos_workflows::JobsState;

use crate::host_store::HostOperationIds;
use crate::package_op_error::PackageOpError;
use crate::package_release_manifest::PackageRelease;
use crate::package_retention::RetentionError;
use crate::package_standing::StoreStanding;
use crate::package_store_report::{RebuiltReport, RemovalReport};

pub(super) type BoxError = Box<dyn std::error::Error>;

/// The interface type a store's manifest model gives its ledger entries.
pub(super) type InterfaceOf = <PackageRelease as ReleaseManifest>::Interface;

/// The engine every case drives, over a world's I/O or a wrapper of it.
pub(super) type Engine<I> = PackageEngine<I, PackageRelease, SystemClock, HostOperationIds>;

const MAX_RELEASE_ENTRIES: usize = 10_000;
const MAX_RELEASE_BYTES: u64 = 256 * 1024 * 1024;

/// The store's I/O over a real temporary store.
pub(super) trait IoWorld: Sized {
    /// The adapter under test.
    type Io: StoreIo;

    /// A world whose store folder does not exist yet.
    async fn new() -> Result<Self, BoxError>;

    /// The store's folder on disk, which may not exist yet.
    fn store_dir(&self) -> PathBuf;

    /// A folder outside the store, which exists, for a link to point at.
    fn outside_dir(&self) -> PathBuf;

    /// A new I/O over the store, with a lock wait of zero.
    async fn io(&self) -> Result<Self::Io, BoxError>;

    /// An I/O over the same store whose store cannot be reached, for a store that can be unreachable; none for a
    /// store that cannot.
    async fn unreachable_io(&self) -> Result<Option<Self::Io>, BoxError>;
}

/// The store as the operator's commands and retention see it.
pub(super) trait StoreWorld: IoWorld {
    /// The word reports name the store by.
    const STORE: &'static str;

    /// Where the home's state file is on disk.
    fn state_path(&self) -> PathBuf;

    /// The folder holding the job files of the workflow `sync` of the package `crm`.
    fn jobs_dir(&self) -> PathBuf;

    /// Takes the store's lock as another operation would, from outside the adapter.
    fn hold_store_lock(&self) -> Result<Box<dyn Any>, BoxError> {
        std::fs::create_dir_all(self.store_dir())?;
        Ok(Box::new(contextos_fs::lock_host(
            &self.store_dir().join(".lock"),
            Duration::ZERO,
        )?))
    }

    /// What `packages list` reports.
    async fn standing(&self) -> Result<StoreStanding, PackageOpError>;

    /// `packages remove`.
    async fn remove(&self, package: &str, version: &str, force: bool) -> Result<RemovalReport, PackageOpError>;

    /// `packages rebuild-ledger`.
    async fn rebuild(&self) -> Result<RebuiltReport, PackageOpError>;

    /// A retention run.
    async fn retain(&self) -> Result<(), RetentionError>;

    /// Writes the home's state file as `text`.
    fn write_state(&self, text: &str) -> Result<(), BoxError> {
        let path = self.state_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, text)?;
        Ok(())
    }

    /// The home's state file, parsed.
    fn state(&self) -> Result<JobsState, BoxError> {
        Ok(toml::from_str(&std::fs::read_to_string(self.state_path())?)?)
    }

    /// Writes the job file `job` of the workflow `sync` of the package `crm`.
    fn write_job(&self, job: &str, text: &str) -> Result<(), BoxError> {
        std::fs::create_dir_all(self.jobs_dir())?;
        std::fs::write(self.jobs_dir().join(format!("{job}.toml")), text)?;
        Ok(())
    }
}

pub(super) fn engine_over<I: StoreIo>(io: I) -> Engine<I> {
    PackageEngine {
        io,
        clock: SystemClock,
        ids: HostOperationIds::default(),
        limits: ReadLimits {
            max_entries: MAX_RELEASE_ENTRIES,
            max_bytes: MAX_RELEASE_BYTES,
        },
        manifest: std::marker::PhantomData,
    }
}

pub(super) async fn engine<W: IoWorld>(world: &W) -> Result<Engine<W::Io>, BoxError> {
    Ok(engine_over(world.io().await?))
}

/// The key of `crm` at `version`.
pub(super) fn key(version: &str) -> Result<ReleaseKey, BoxError> {
    package_key("crm", version)
}

pub(super) fn package_key(package: &str, version: &str) -> Result<ReleaseKey, BoxError> {
    Ok(ReleaseKey {
        name: ReleaseName::try_from(package)?,
        version: ReleaseVersion::try_from(version)?,
    })
}

/// The install request an archive extracts to.
pub(super) fn request_of(archive: &[u8]) -> Result<InstallRequest, BoxError> {
    let extracted = crate::package_bundle::verify_and_extract(archive)?;
    Ok(InstallRequest {
        manifest_text: extracted.manifest_text,
        files: extracted.files,
        origin: ReleaseOrigin::Bundle,
    })
}

/// Installs the release in `archive` under the store's lock, without running retention.
pub(super) async fn install<W: IoWorld>(world: &W, archive: &[u8]) -> Result<Installed, BoxError> {
    let engine = engine(world).await?;
    let lock = engine.lock().await?;
    Ok(engine.install(&lock, request_of(archive)?).await?)
}

/// As [`install`], reporting the engine's refusal as it stands.
pub(super) async fn try_install<W: IoWorld>(
    world: &W,
    archive: &[u8],
) -> Result<Result<Installed, PackagesError>, BoxError> {
    let engine = engine(world).await?;
    let lock = engine.lock().await?;
    Ok(engine.install(&lock, request_of(archive)?).await)
}

pub(super) async fn read<W: IoWorld>(world: &W, version: &str) -> Result<ReadCheck<PackageRelease>, BoxError> {
    Ok(engine(world).await?.read_release(&key(version)?).await)
}

pub(super) async fn listing<W: IoWorld>(world: &W) -> Result<StoreListing, BoxError> {
    Ok(engine(world).await?.list().await?)
}

pub(super) async fn ledger<W: IoWorld>(world: &W) -> Result<LedgerReading<InterfaceOf>, BoxError> {
    Ok(engine(world).await?.ledger().await?)
}

pub(super) async fn recover<W: IoWorld>(world: &W) -> Result<Recovered, BoxError> {
    let engine = engine(world).await?;
    let lock = engine.lock().await?;
    Ok(engine.recover(&lock).await?)
}

/// The version folders of the package `package` on disk, sorted.
pub(super) fn versions(store_dir: &Path, package: &str) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(store_dir.join(package))
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

/// The top-level entries of the store on disk other than the lock files, sorted.
pub(super) fn top_level(store_dir: &Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(store_dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| !name.starts_with(".lock"))
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}
