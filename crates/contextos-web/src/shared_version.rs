//! The version of a shared job: which package release and library releases it runs, as far as that decides whether the
//! facts recorded about the job still apply.
//!
//! A shared job's version is a digest of its package's recorded content digest, the release its job file names, the job
//! file, the package manifest it was installed with, and, when it resolves libraries, each library's name, release,
//! content digest, and manifest. Installing a newer release of the job's package changes nothing about the job, which
//! names the release it runs.

use std::path::Path;

use contextos_workflows::{JobFingerprint, JobVersion, JobsState, LibraryLock, PackageName};
use semver::Version;
use serde::Deserialize;

use crate::shared_claim::SharedJobKey;
use crate::shared_snapshot::{SharedSnapshot, SnapshotRequest};
use crate::shared_state::{SharedStateError, SharedStateStore};

/// What the store says a job's version is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SharedVersion {
    /// The job resolves, and this is its version.
    Known(JobFingerprint),
    /// The job's package or the release it names is not installed or cannot be read, its libraries do not resolve, or its
    /// file names no exact release, so it has no version. A library that is gone is handled as a package that is gone.
    Unknown,
}

impl SharedVersion {
    /// The version, when the job has one.
    #[must_use]
    pub fn known(self) -> Option<JobFingerprint> {
        match self {
            Self::Known(version) => Some(version),
            Self::Unknown => None,
        }
    }
}

/// The version of the shared job in `package` whose job file is `job_toml`, resolved against the installed releases the
/// way a run resolves them: exactly the `release` the job names, with the libraries of the workflow release's recorded
/// `lock`. A release with no lock yet, because no job on it has passed validation, takes the newest libraries that
/// satisfy.
#[must_use]
pub fn shared_version(
    store_root: &Path,
    package: &str,
    release: &Version,
    job_toml: &str,
    lock: Option<&LibraryLock>,
) -> SharedVersion {
    let Ok(package) = PackageName::try_from(package) else {
        return SharedVersion::Unknown;
    };
    // Only the releases are needed, so no module is read.
    let basis = SharedSnapshot::read_basis(&SnapshotRequest {
        store_root,
        package,
        version: release,
        // The workflow decides nothing the version holds.
        workflow: "",
        lock,
    });
    match basis {
        Ok(basis) => SharedVersion::Known(basis.version(job_toml)),
        Err(_) => SharedVersion::Unknown,
    }
}

/// The release a job file's `version` names, read without checking the rest of the file.
pub(crate) struct NamedRelease(pub(crate) Version);

/// A job file that does not parse or names no exact release.
pub(crate) struct NoNamedRelease;

#[derive(Deserialize)]
struct VersionOnly {
    version: String,
}

impl TryFrom<&str> for NamedRelease {
    type Error = NoNamedRelease;

    fn try_from(job_toml: &str) -> Result<Self, NoNamedRelease> {
        let named: VersionOnly = toml::from_str(job_toml).map_err(|_| NoNamedRelease)?;
        let release = JobVersion::try_from(named.version.as_str()).map_err(|_| NoNamedRelease)?;
        Ok(Self(release.as_semver().clone()))
    }
}

/// The library lock the host state records for the workflow release the job file `job_toml` of `key` names, when a job
/// on that release has passed validation. A file that names no exact release has none.
#[must_use]
pub fn recorded_lock(state: &JobsState, key: &SharedJobKey, job_toml: &str) -> Option<LibraryLock> {
    let NamedRelease(release) = NamedRelease::try_from(job_toml).ok()?;
    state.library_lock(&key.package, &key.workflow, &release).cloned()
}

/// As [`recorded_lock`], read from the host state file on a blocking thread.
///
/// # Errors
///
/// Returns the failure when the state cannot be read, since a lock that cannot be read cannot be said to be absent.
pub async fn recorded_lock_blocking(
    state: &SharedStateStore,
    key: &SharedJobKey,
    job_toml: &str,
) -> Result<Option<LibraryLock>, SharedStateError> {
    let store = state.clone();
    let key = key.clone();
    let job_toml = job_toml.to_owned();
    tokio::task::spawn_blocking(move || store.read().map(|state| recorded_lock(&state, &key, &job_toml)))
        .await
        .unwrap_or_else(|error| Err(SharedStateError::Io(std::io::Error::other(error.to_string()))))
}

/// As [`shared_version`] for the job file `job_toml`, whose release it reads. A file that names no exact release has no
/// version.
#[must_use]
pub fn shared_job_state(store_root: &Path, package: &str, job_toml: &str, lock: Option<&LibraryLock>) -> SharedVersion {
    match NamedRelease::try_from(job_toml) {
        Ok(NamedRelease(release)) => shared_version(store_root, package, &release, job_toml, lock),
        Err(NoNamedRelease) => SharedVersion::Unknown,
    }
}

/// As [`shared_job_state`], off the async executor: it lists the installed releases and reads their manifests.
pub async fn shared_job_state_blocking(
    store_root: &Path,
    package: &str,
    job_toml: &str,
    lock: Option<LibraryLock>,
) -> SharedVersion {
    let store_root = store_root.to_path_buf();
    let package = package.to_owned();
    let job_toml = job_toml.to_owned();
    tokio::task::spawn_blocking(move || shared_job_state(&store_root, &package, &job_toml, lock.as_ref()))
        .await
        .unwrap_or(SharedVersion::Unknown)
}

/// As [`shared_job_version`], off the async executor. For a release no job has passed validation on: it resolves the
/// libraries. A caller that holds the host state uses [`shared_job_version_under_blocking`].
pub async fn shared_job_version_blocking(store_root: &Path, package: &str, job_toml: &str) -> Option<JobFingerprint> {
    shared_job_version_under_blocking(store_root, package, job_toml, None).await
}

/// As [`shared_job_version`], off the async executor, with the workflow release's recorded `lock`.
pub async fn shared_job_version_under_blocking(
    store_root: &Path,
    package: &str,
    job_toml: &str,
    lock: Option<LibraryLock>,
) -> Option<JobFingerprint> {
    shared_job_state_blocking(store_root, package, job_toml, lock)
        .await
        .known()
}

/// The version of the shared job in `package` whose job file is `job_toml`, resolving the release the file names.
/// `None` when the file names no exact release, or the package, that release, or its libraries do not resolve.
#[must_use]
pub fn shared_job_version(store_root: &Path, package: &str, job_toml: &str) -> Option<JobFingerprint> {
    shared_job_state(store_root, package, job_toml, None).known()
}

#[cfg(test)]
#[path = "shared_version_test.rs"]
mod tests;
