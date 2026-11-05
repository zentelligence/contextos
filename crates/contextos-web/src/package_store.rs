//! Host-level, vault-independent store for installed workflow packages:
//! install/list/inspect/remove, reached by both the `contextos-web
//! packages` CLI subcommand and the local HTTP upload route. Every
//! operation here is direct host filesystem I/O against this store's own
//! root - never a vault, never an MCP session - matching
//! `contextos_workflows::package`'s own explicit scope boundary ("a
//! host-level package store is responsible for calling this against its
//! own real inventory"). `contextos_workflows` stays pure; this module
//! is where its parsing/validation/resolution meets real I/O, exactly as
//! `config.rs` and `service.rs` already do for this crate's other
//! host-local state.
//!
//! ## On-disk layout
//!
//! Rooted at [`default_store_root`] (or an injected root in tests),
//! mirroring `contextos-mcp::state_dir`'s own
//! `ProjectDirs::data_local_dir()` precedent. The layout and every rule
//! about it belong to the package engine (`contextos_packages`), which
//! both this store and a vault's store run:
//!
//! ```text
//! packages/
//! ├── .lock                  # the store's advisory lock, see below
//! ├── ledger.toml            # every release ever installed, and every removal
//! └── {name}/
//!     └── {version}/         # one immutable, fully verified package version
//!         ├── manifest.toml  # lists every other file with its hash
//!         └── ...            # entry script(s), declared templates, tests/
//! ```
//!
//! Every installed version is immutable, and installed versions are kept side
//! by side while something keeps them. "Upgrade" is not a distinct storage
//! operation from "install": both mean exactly "make this immutable
//! `(name, version)` pair available". Installing the same bundle again
//! changes nothing, and installing other content under a version already
//! installed is refused. After each install, retention
//! ([`crate::store_retention`]) removes every release nothing keeps, so the
//! release an install replaces as the newest is removed unless a workflow's
//! `current` or `previous`, a job, or a library lock still names it.
//!
//! ## Reading a release
//!
//! Every read of a release lists its folder against its manifest, hashes
//! every listed file against the manifest and the whole against its
//! declared digest, and only then hands the release over. A release is
//! then whole, removed, damaged (something present is wrong), or
//! incomplete or unverifiable (something is absent, or the store could
//! not be read). A release a caller names is reported as the state it is
//! in, and no read ever falls back to another release.
//!
//! ## An install must resolve
//!
//! A release's libraries are chosen by the procedure a run uses
//! ([`contextos_workflows::resolve_libraries`]). Install resolves the new
//! release and every installed release that would choose it, and refuses the
//! install when any would not resolve. Removal is the operator's, decided by
//! what keeps a release ([`crate::package_removal`]). The inventory is read
//! one release at a time, so a release that cannot be read is reported for
//! itself ([`Inventory::unreadable`]) and blocks nothing else.
//!
//! ## Concurrency
//!
//! The engine's lock (`packages/.lock`, an advisory lock the operating
//! system releases with the process) serialises mutating operations
//! (install, remove, retention) against each other, in this process and in every
//! other on the host. A caller that cannot take the lock fails fast with
//! [`PackageStoreError::StoreLocked`] rather than queuing or retrying,
//! the same fail-fast policy a job's busy claim uses.
//!

use std::io;
use std::path::{Path, PathBuf};

use contextos_host::HostFolders;
use contextos_packages::{
    DamageFinding, IncompleteFinding, InstallRequest, PackagesError, ReadCheck, ReleaseFilePath, ReleaseKey,
    ReleaseName, ReleaseOrigin, ReleaseVersion,
};
use contextos_workflows::package::{PackageError, PackageIdentity, PackageManifest};
use contextos_workflows::{JobsState, StoreKind};
use contextos_workflows::{PackageKind, PackageName, ReleaseContents};
use semver::Version;
use thiserror::Error;

use crate::host_store::{Checked, HostStore};
use crate::package_bundle::{self, BundleError, ExtractedBundle};
use crate::package_futures::FutureRecord;
use crate::package_graph::{StoreGraph, UnresolvableRelease};
use crate::package_release_manifest::PackageFilesOf;
pub use crate::saved_jobs::SavedJobs;
use crate::store_retention::{RetentionWarning, retain_host_held};

const PACKAGES_DIR_NAME: &str = "packages";
const JOBS_DIR_NAME: &str = "workflows";
const CLAIMS_DIR_NAME: &str = "claims";
const RUNS_DIR_NAME: &str = "runs";
const STATE_DIR_NAME: &str = "state";

/// This boundary's own hard cap on an uploaded bundle's size, checked
/// before any extraction is attempted (`security.md`'s "enforce
/// documented limits before allocating or reading full content"),
/// independent of whatever request-body limit sits in front of the HTTP
/// route. A workflow package is source code plus small templates, not
/// media or a data export, so this is generously larger than the
/// existing 10&nbsp;MB Inbox upload ceiling while still being a real
/// bound.
pub const MAX_BUNDLE_BYTES: usize = 25 * 1024 * 1024;

/// One focused error enum for every package-store failure
/// (`rust-quality.md`). Dependency-resolution and duplicate-identity
/// failures are `contextos_workflows::package::PackageError` values
/// verbatim, carried through unchanged rather than re-shaped into a
/// second, parallel set of variants: this store calls those pure
/// functions directly and has nothing useful to add to their own
/// already-structured failure.
#[derive(Debug, Error)]
pub enum PackageStoreError {
    #[error("could not resolve a platform app-data directory for the package store; set an explicit store root")]
    AppDataUnavailable,
    #[error("the bundle is {size} bytes, larger than the {limit} byte limit")]
    BundleTooLarge { size: usize, limit: usize },
    #[error("the package folder holds {size} bytes, more than the {limit} byte limit")]
    FolderTooLarge { size: u64, limit: usize },
    #[error("another package store operation is already in progress")]
    StoreLocked,
    #[error(transparent)]
    Bundle(#[from] BundleError),
    #[error(transparent)]
    Package(#[from] PackageError),
    #[error("package {name} version {version} is not installed")]
    NotFound { name: String, version: Version },
    #[error("the install would leave releases unable to resolve their libraries: {}", describe(.releases))]
    Unresolvable { releases: Vec<UnresolvableRelease> },
    #[error("installed release {name} {version} is damaged: {findings}")]
    ReleaseDamaged {
        name: PackageName,
        version: Version,
        /// What is wrong with it, naming files relative to the release and no host path.
        findings: String,
    },
    #[error("installed release {name} {version} cannot be verified: {reason}")]
    ReleaseUnverifiable {
        name: PackageName,
        version: Version,
        /// What is absent or unexpected, or why the store could not be read.
        reason: String,
    },
    #[error(transparent)]
    Packages(PackagesError),
    #[error(
        "release {name} {version} changed while it was read (it was removed, replaced, or reinstalled), so what was read \
         may not be what was resolved; try again"
    )]
    ReleaseChangedWhileRead { name: PackageName, version: Version },
    #[error("package {package} version {version} has no workflow {workflow:?}: it lists no {workflow}/workflow.toml")]
    NoSuchWorkflow {
        package: String,
        version: Version,
        workflow: String,
    },
    #[error("package {package} version {version}: {workflow}/workflow.toml is not valid: {error}")]
    WorkflowInvalid {
        package: String,
        version: Version,
        workflow: String,
        error: Box<PackageError>,
    },
    #[error("entry script {entry:?} could not be read: {source}")]
    EntryUnreadable {
        entry: String,
        #[source]
        source: crate::bounded_read::BoundedReadError,
    },
    #[error(transparent)]
    ModuleUnreadable(#[from] contextos_workflows::ModuleUnreadable),
    #[error(transparent)]
    State(#[from] crate::shared_state::SharedStateError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

pub(crate) fn describe(releases: &[UnresolvableRelease]) -> String {
    let described: Vec<String> = releases
        .iter()
        .map(|unresolvable| format!("{} ({})", unresolvable.release, unresolvable.reason))
        .collect();
    described.join("; ")
}

/// The host's platform app-data directory: the same `HostFolders::application_data` root
/// `contextos-mcp::state_dir` uses for its own per-vault derived state, so every piece of this workspace's host-local
/// (non-vault) data lives under one platform-conventional tree.
fn app_data_root() -> Result<PathBuf, PackageStoreError> {
    HostFolders::application_data().ok_or(PackageStoreError::AppDataUnavailable)
}

/// Where the host keeps everything that is not in a vault, under one app-data directory.
#[derive(Clone, Debug)]
pub struct AppDataRoots {
    pub store: PathBuf,
    pub jobs: PathBuf,
    pub claims: PathBuf,
    pub runs: PathBuf,
    pub state: PathBuf,
}

impl From<&Path> for AppDataRoots {
    /// The roots under `app_data_root`, so a test can keep every one of them in a temporary directory.
    fn from(app_data_root: &Path) -> Self {
        Self {
            store: store_root(app_data_root),
            jobs: jobs_root(app_data_root),
            claims: claims_root(app_data_root),
            runs: runs_root(app_data_root),
            state: state_root(app_data_root),
        }
    }
}

impl AppDataRoots {
    /// The roots under this platform's app-data directory.
    ///
    /// # Errors
    ///
    /// Returns [`PackageStoreError::AppDataUnavailable`] when the platform cannot resolve a home directory.
    pub fn platform() -> Result<Self, PackageStoreError> {
        Ok(Self::from(app_data_root()?.as_path()))
    }
}

/// Resolves the package store's default root: `packages/` under this host's platform app-data directory.
///
/// # Errors
///
/// Returns [`PackageStoreError::AppDataUnavailable`] when the platform cannot resolve a home directory to root the
/// default under.
pub fn default_store_root() -> Result<PathBuf, PackageStoreError> {
    Ok(store_root(&app_data_root()?))
}

/// Resolves the shared jobs directory: `jobs/` beside the package store under the same platform app-data directory.
///
/// # Errors
///
/// Returns [`PackageStoreError::AppDataUnavailable`] when the platform cannot resolve a home directory.
pub fn default_jobs_root() -> Result<PathBuf, PackageStoreError> {
    Ok(jobs_root(&app_data_root()?))
}

/// Resolves the shared job claims directory: `claims/` beside the package store and the jobs under the same platform
/// app-data directory.
///
/// # Errors
///
/// Returns [`PackageStoreError::AppDataUnavailable`] when the platform cannot resolve a home directory.
pub fn default_claims_root() -> Result<PathBuf, PackageStoreError> {
    Ok(claims_root(&app_data_root()?))
}

/// Resolves the shared run records directory: `runs/` beside the claims under the same platform app-data directory.
///
/// # Errors
///
/// Returns [`PackageStoreError::AppDataUnavailable`] when the platform cannot resolve a home directory.
pub fn default_runs_root() -> Result<PathBuf, PackageStoreError> {
    Ok(runs_root(&app_data_root()?))
}

/// Resolves the shared job state directory: `state/` beside the claims and the run records under the same platform
/// app-data directory.
///
/// # Errors
///
/// Returns [`PackageStoreError::AppDataUnavailable`] when the platform cannot resolve a home directory.
pub fn default_state_root() -> Result<PathBuf, PackageStoreError> {
    Ok(state_root(&app_data_root()?))
}

// The compositions below are pure so they are testable without depending on the operator's actual home directory.

fn store_root(app_data_root: &Path) -> PathBuf {
    app_data_root.join(PACKAGES_DIR_NAME)
}

fn jobs_root(app_data_root: &Path) -> PathBuf {
    app_data_root.join(JOBS_DIR_NAME)
}

fn runs_root(app_data_root: &Path) -> PathBuf {
    app_data_root.join(RUNS_DIR_NAME)
}

fn state_root(app_data_root: &Path) -> PathBuf {
    app_data_root.join(STATE_DIR_NAME)
}

fn claims_root(app_data_root: &Path) -> PathBuf {
    app_data_root.join(CLAIMS_DIR_NAME)
}

/// The installed releases of one package, as a job choosing among them needs to know them.
#[derive(Debug, Default)]
pub struct InstalledVersions {
    /// Every installed version that reads whole, in no particular order.
    pub readable: Vec<Version>,
    /// Every installed version that is damaged: something in it is wrong.
    pub damaged: Vec<DamagedRelease>,
    /// Every installed version that cannot be verified: something is absent or unexpected, or the store could not be
    /// read.
    pub unverifiable: Vec<UnverifiableRelease>,
}

/// An installed release that is damaged.
#[derive(Debug)]
pub struct DamagedRelease {
    pub name: PackageName,
    pub version: Version,
    /// Always [`PackageStoreError::ReleaseDamaged`], which says what is wrong with it.
    pub error: PackageStoreError,
}

/// An installed release that cannot be verified.
#[derive(Debug)]
pub struct UnverifiableRelease {
    pub name: PackageName,
    pub version: Version,
    /// Always [`PackageStoreError::ReleaseUnverifiable`], which says why.
    pub error: PackageStoreError,
}

impl From<&DamagedRelease> for PackageError {
    fn from(damaged: &DamagedRelease) -> Self {
        Self::InstalledManifestInvalid {
            name: damaged.name.clone(),
            version: damaged.version.clone(),
            reason: damaged.error.to_string(),
        }
    }
}

impl InstalledVersions {
    /// Every installed version, whatever state it is in, so a choice among them can fall on one that is not whole.
    #[must_use]
    pub fn all(&self) -> Vec<Version> {
        let mut versions = self.readable.clone();
        versions.extend(self.damaged.iter().map(|damaged| damaged.version.clone()));
        versions.extend(self.unverifiable.iter().map(|release| release.version.clone()));
        versions
    }

    /// The release a run of `package` uses: exactly the `wanted` release a job names, never the newest. The release
    /// named is the release used: when it is damaged or cannot be verified that is what is reported, and no other
    /// release stands in for it.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::ReleaseUnavailable`] naming the package when `wanted` is not installed,
    /// [`PackageError::InstalledManifestInvalid`] when the release is damaged, and
    /// [`PackageStoreError::ReleaseUnverifiable`] when it cannot be verified.
    pub fn choose(&self, package: &PackageName, wanted: &Version) -> Result<Version, PackageStoreError> {
        let Some(chosen) = self.all().into_iter().find(|version| version == wanted) else {
            return Err(PackageStoreError::Package(PackageError::ReleaseUnavailable {
                requested: format!("{package} {wanted}"),
            }));
        };
        if let Some(damaged) = self.damaged.iter().find(|damaged| damaged.version == chosen) {
            return Err(PackageStoreError::Package(PackageError::from(damaged)));
        }
        if let Some(unverifiable) = self.unverifiable.iter().find(|release| release.version == chosen) {
            return Err(PackageStoreError::ReleaseUnverifiable {
                name: unverifiable.name.clone(),
                version: unverifiable.version.clone(),
                reason: match &unverifiable.error {
                    PackageStoreError::ReleaseUnverifiable { reason, .. } => reason.clone(),
                    other => other.to_string(),
                },
            });
        }
        Ok(chosen)
    }
}

/// Every installed version of one package, each in the state it reads in. A package that is not installed lists as
/// empty. A release is listed under the state it is in, and no state hides a release from the choice among them: a
/// release that is not whole is listed in [`InstalledVersions::damaged`] or [`InstalledVersions::unverifiable`] so a job
/// can say that this is why it cannot run, and is never passed over for an older one.
///
/// # Errors
///
/// Returns [`PackageStoreError::Packages`] for a store that cannot be read. A folder that is not a version is ignored.
pub fn installed_versions_of(store_root: &Path, name: &PackageName) -> Result<InstalledVersions, PackageStoreError> {
    let wanted = ReleaseName::try_from(name.as_str())?;
    let mut found = InstalledVersions::default();
    for checked in HostStore::new(store_root).read_package(&wanted)? {
        let version = checked.0.version.as_semver().clone();
        match InstalledRelease::try_from(checked) {
            Ok(_) => found.readable.push(version),
            Err(error @ PackageStoreError::ReleaseDamaged { .. }) => {
                tracing::warn!(package = %name, %version, "an installed release is damaged, so no job resolves to it");
                found.damaged.push(DamagedRelease {
                    name: name.clone(),
                    version,
                    error,
                });
            }
            Err(error @ PackageStoreError::ReleaseUnverifiable { .. }) => {
                tracing::warn!(package = %name, %version, "an installed release cannot be verified, so no job resolves to it");
                found.unverifiable.push(UnverifiableRelease {
                    name: name.clone(),
                    version,
                    error,
                });
            }
            // A release removed between listing it and reading it is not installed.
            Err(PackageStoreError::NotFound { .. }) => {}
            Err(other) => return Err(other),
        }
    }
    Ok(found)
}

/// The directory holding one installed version's files, where its entry scripts live. A [`PackageName`] is one plain
/// folder name, so the directory is always directly beneath the store root.
pub(crate) fn package_version_dir(store_root: &Path, name: &PackageName, version: &Version) -> PathBuf {
    store_root.join(name.as_str()).join(version.to_string())
}

/// A release that is installed and cannot be read, reported for itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnreadableRelease {
    /// The folder the release is kept under, which is the package's name.
    pub name: String,
    /// The folder the release's files are kept in, which is its version.
    pub version: String,
    /// Why it cannot be read, naming its manifest relative to the app-data folder.
    pub reason: String,
}

/// Every release in the store: the ones that can be read, with their manifests, and the ones that cannot.
#[derive(Clone, Debug, Default)]
pub struct Inventory {
    pub releases: Vec<InstalledRelease>,
    pub unreadable: Vec<UnreadableRelease>,
}

/// Reads the whole store one release at a time. An empty or not-yet-created store root is empty, not an error, matching
/// this workspace's own zero-configuration-is-valid convention. A folder that is not a package name is left alone, and
/// logged unless the store itself put it there. A release that is damaged, or cannot be verified, is reported in
/// [`Inventory::unreadable`] and does not stop the others being read, since a store's own installed content should
/// always be whole and one that was modified outside this module must not make every other package unusable.
///
/// # Errors
///
/// Returns [`PackageStoreError::Packages`] when the store cannot be listed.
pub fn inventory(store_root: &Path) -> Result<Inventory, PackageStoreError> {
    let mut found = Inventory::default();
    for checked in HostStore::new(store_root).read_all()? {
        let (name, version) = (checked.0.name.as_str().to_owned(), checked.0.version.to_string());
        match InstalledRelease::try_from(checked) {
            Ok(release) => found.releases.push(release),
            Err(PackageStoreError::NotFound { .. }) => {}
            Err(error) => found.unreadable.push(UnreadableRelease {
                name,
                version,
                reason: error.to_string(),
            }),
        }
    }
    // The order the folders are listed in is the file system's, so the report is put in one that does not change.
    found
        .unreadable
        .sort_by(|left, right| (&left.name, &left.version).cmp(&(&right.name, &right.version)));
    Ok(found)
}

/// Lists every installed package version's own manifest. A release that cannot be read is left out and logged, since
/// [`inventory`] is how one is reported.
///
/// # Errors
///
/// Returns [`PackageStoreError::Packages`] when the store cannot be listed.
pub fn list_installed(store_root: &Path) -> Result<Vec<PackageManifest>, PackageStoreError> {
    let found = inventory(store_root)?;
    for unreadable in &found.unreadable {
        tracing::warn!(package = %unreadable.name, version = %unreadable.version, "an installed release could not be read");
    }
    Ok(found.releases.into_iter().map(|release| release.manifest).collect())
}

/// One installed release's manifest and its text exactly as installed, which a job's version is made from.
#[derive(Clone, Debug)]
pub struct InstalledRelease {
    pub manifest: PackageManifest,
    /// The release's `workflows.toml` or `library.toml`, read.
    pub kind: PackageKind,
    pub manifest_text: String,
    /// The text of each `{workflow}/workflow.toml` the release holds, by the workflow's folder.
    pub workflow_files: std::collections::BTreeMap<String, String>,
}

impl InstalledRelease {
    /// The entry script the workflow in `folder` names, relative to its folder; none when the release holds no such
    /// workflow or its `workflow.toml` does not parse.
    #[must_use]
    pub fn entry_of(&self, folder: &str) -> Option<String> {
        let text = self.workflow_files.get(folder)?;
        contextos_workflows::workflow_definition::WorkflowDefinition::try_from(
            contextos_workflows::workflow_definition::WorkflowText { text, folder },
        )
        .ok()
        .map(|definition| definition.entry)
    }
}

impl TryFrom<Checked> for InstalledRelease {
    type Error = PackageStoreError;

    /// The release when it read whole, and otherwise the state it is in: not found when its folder is gone, damaged, or
    /// unable to be verified.
    fn try_from((key, check): Checked) -> Result<Self, PackageStoreError> {
        let name = PackageName::try_from(key.name.as_str()).map_err(PackageStoreError::Package)?;
        let version = key.version.as_semver().clone();
        match check {
            ReadCheck::Verified(verified) => {
                let manifest = verified.manifest.manifest().clone();
                let kind = PackageKind::try_from(PackageFilesOf {
                    manifest: &manifest,
                    files: &verified.files,
                })
                .map_err(|error| PackageStoreError::ReleaseDamaged {
                    name: name.clone(),
                    version: version.clone(),
                    findings: error.to_string(),
                })?;
                let workflow_files = kind
                    .workflows()
                    .iter()
                    .filter_map(|folder| {
                        let path = ReleaseFilePath::try_from(format!("{folder}/workflow.toml").as_str()).ok()?;
                        let text = String::from_utf8(verified.files.0.get(&path)?.clone()).ok()?;
                        Some((folder.to_owned(), text))
                    })
                    .collect();
                Ok(Self {
                    manifest,
                    kind,
                    manifest_text: verified.manifest_text,
                    workflow_files,
                })
            }
            ReadCheck::Removed => Err(PackageStoreError::NotFound {
                name: name.to_string(),
                version,
            }),
            ReadCheck::Damaged(findings) => Err(PackageStoreError::ReleaseDamaged {
                name,
                version,
                findings: joined(findings.iter().map(DamageFinding::to_string)),
            }),
            ReadCheck::Incomplete(findings) => Err(PackageStoreError::ReleaseUnverifiable {
                name,
                version,
                reason: joined(findings.iter().map(IncompleteFinding::to_string)),
            }),
            ReadCheck::Unverifiable(reason) => Err(PackageStoreError::ReleaseUnverifiable {
                name,
                version,
                reason: reason.to_string(),
            }),
        }
    }
}

fn joined(parts: impl Iterator<Item = String>) -> String {
    parts.collect::<Vec<_>>().join("; ")
}

impl From<PackagesError> for PackageStoreError {
    /// The engine's refusals that this store already had a variant for, as that variant, and the rest as they are.
    fn from(error: PackagesError) -> Self {
        match error {
            PackagesError::StoreLocked => Self::StoreLocked,
            PackagesError::DuplicateIdentity { release, .. } => Self::Package(PackageError::DuplicateIdentity {
                name: release.name.as_str().to_owned(),
                version: release.version.as_semver().clone(),
            }),
            PackagesError::NotFound { release } => Self::NotFound {
                name: release.name.as_str().to_owned(),
                version: release.version.as_semver().clone(),
            },
            other => Self::Packages(other),
        }
    }
}

/// Reads one specific installed version's manifest.
///
/// # Errors
///
/// As [`read_release`].
pub fn inspect(
    store_root: &Path,
    name: &PackageName,
    version: &Version,
) -> Result<InstalledRelease, PackageStoreError> {
    read_release(store_root, name, version)
}

/// Reads one specific installed version's manifest with its text, having checked the release in full.
///
/// # Errors
///
/// Returns [`PackageStoreError::NotFound`] when no such `(name, version)` is installed,
/// [`PackageStoreError::ReleaseDamaged`] when it is, and [`PackageStoreError::ReleaseUnverifiable`] when it cannot be
/// verified. The release asked for is the release reported on, and no other stands in for it.
pub fn read_release(
    store_root: &Path,
    name: &PackageName,
    version: &Version,
) -> Result<InstalledRelease, PackageStoreError> {
    let key = release_key(name, version)?;
    let check = HostStore::new(store_root).read_release(&key)?;
    InstalledRelease::try_from((key, check))
}

fn release_key(name: &PackageName, version: &Version) -> Result<ReleaseKey, PackageStoreError> {
    Ok(ReleaseKey {
        name: ReleaseName::try_from(name.as_str())?,
        version: ReleaseVersion::try_from(version.to_string().as_str())?,
    })
}

/// What an install did: the release now installed and the saved jobs whose version it changed.
#[derive(Clone, Debug)]
pub struct Installed {
    pub identity: PackageIdentity,
    /// Retention could not finish after the install. The release is installed all the same.
    pub warnings: Vec<RetentionWarning>,
    /// Whether retention removed the release again at once, because nothing keeps it: an older release than the one a
    /// workflow runs, with no job naming it.
    pub removed_by_retention: bool,
}

/// Verifies, validates, and installs a package bundle: size limit, then the bundle's files and digest (via
/// [`package_bundle::verify_and_extract`]), then the library-resolution check against this store's own real inventory,
/// then the engine's install, which stages the release, records it in the ledger, and makes it visible with one rename.
/// Any rejection at any step leaves the store unchanged: install is all-or-nothing.
///
/// # Errors
///
/// Returns [`PackageStoreError::BundleTooLarge`], [`PackageStoreError::StoreLocked`], [`PackageStoreError::Bundle`]
/// (malformed archive, unsafe path, files or digest that do not match the manifest), [`PackageStoreError::Package`]
/// (`DuplicateIdentity` for other content under a version already installed), [`PackageStoreError::Unresolvable`]
/// naming each release the install would leave unable to resolve its libraries, itself included, and
/// [`PackageStoreError::Packages`] for a ledger that cannot be used or a store that cannot be written.
pub fn install(store_root: &Path, jobs: &SavedJobs, bundle_bytes: &[u8]) -> Result<Installed, PackageStoreError> {
    if bundle_bytes.len() > MAX_BUNDLE_BYTES {
        return Err(PackageStoreError::BundleTooLarge {
            size: bundle_bytes.len(),
            limit: MAX_BUNDLE_BYTES,
        });
    }

    place(store_root, jobs, package_bundle::verify_and_extract(bundle_bytes)?)
}

/// Installs a package from a folder holding its files, laid out as a bundle would hold them: the folder's total size
/// is checked against the bundle limit and every entry must be a regular file or a folder, then the files are read and
/// verified exactly as a bundle's are, then installed as [`install`] installs one. The folder is only read.
///
/// # Errors
///
/// Returns [`PackageStoreError::FolderTooLarge`] before anything is read, and otherwise the errors [`install`] returns.
pub fn install_folder(store_root: &Path, jobs: &SavedJobs, folder: &Path) -> Result<Installed, PackageStoreError> {
    let size = package_bundle::folder_size(folder)?;
    if size > u64::try_from(MAX_BUNDLE_BYTES).unwrap_or(u64::MAX) {
        return Err(PackageStoreError::FolderTooLarge {
            size,
            limit: MAX_BUNDLE_BYTES,
        });
    }

    place(store_root, jobs, package_bundle::verify_and_copy_folder(folder)?)
}

/// Checks a verified package against the store and installs it, or refuses it. The shared job state is held for the whole
/// change, taken before the store's lock, so `future` is recorded in the same state write as the install.
fn place(store_root: &Path, jobs: &SavedJobs, extracted: ExtractedBundle) -> Result<Installed, PackageStoreError> {
    let store = HostStore::new(store_root);
    jobs.state.update(|state| {
        let lock = store.lock()?;
        validate_new_install(
            store_root,
            ReleaseContents {
                manifest: &extracted.manifest,
                kind: &extracted.kind,
            },
        )?;

        store.install(
            &lock,
            InstallRequest {
                manifest_text: extracted.manifest_text,
                files: extracted.files,
                origin: ReleaseOrigin::Bundle,
            },
        )?;
        record_futures(store_root, &extracted.manifest.identity.name, state)?;
        let retained = retain_host_held(&store, &lock, jobs, state);
        let identity = extracted.manifest.identity;
        Ok(Installed {
            removed_by_retention: retained
                .as_ref()
                .is_ok_and(|removed| removed.include(&identity.name, &identity.version)),
            warnings: retained
                .err()
                .map(|error| RetentionWarning::raised(StoreKind::Host, &error))
                .into_iter()
                .collect(),
            identity,
        })
    })?
}

/// Records `future` for every workflow of `package`, from the releases of it that read whole now, in the state the
/// caller holds. A workflow no installed release holds any longer has its `future` cleared.
///
/// # Errors
///
/// Returns [`PackageStoreError::Packages`] for a package name the store does not accept or a store that cannot be read.
pub fn record_futures(
    store_root: &Path,
    package: &PackageName,
    state: &mut JobsState,
) -> Result<(), PackageStoreError> {
    let mut installed = Vec::new();
    for checked in HostStore::new(store_root).read_package(&ReleaseName::try_from(package.as_str())?)? {
        let version = checked.0.version.as_semver().clone();
        // A release that does not read whole is not one a job can name, so it is not one that can be future.
        if let Ok(release) = InstalledRelease::try_from(checked) {
            installed.extend(
                release
                    .workflow_files
                    .keys()
                    .map(|workflow| (workflow.clone(), version.clone())),
            );
        }
    }
    FutureRecord { package, state }.record(installed);
    Ok(())
}

/// Refuses a release whose libraries do not resolve, and one that would leave an installed release unable to resolve its
/// own. A release that could not resolve before the install is not blamed on it. A version already installed with other
/// content is the engine's to refuse.
fn validate_new_install(store_root: &Path, contents: ReleaseContents<'_>) -> Result<(), PackageStoreError> {
    let installed = inventory(store_root)?;
    let graph: StoreGraph = installed
        .releases
        .iter()
        .map(|installed| ReleaseContents {
            manifest: &installed.manifest,
            kind: &installed.kind,
        })
        .collect();
    let broken = graph.broken_by(&graph.with(contents));
    if broken.is_empty() {
        Ok(())
    } else {
        Err(PackageStoreError::Unresolvable { releases: broken })
    }
}

/// Rebuilds a missing ledger from the releases that stand in the store, each of which must read whole, and returns how
/// many it recorded. A store that holds releases and has no ledger refuses every install and removal until this is done
/// or the ledger is restored from a copy.
///
/// # Errors
///
/// Returns [`PackageStoreError::Packages`] with `packages/ledger-present` when the store has a ledger, and with
/// `packages/release-incomplete` when a release is incomplete or cannot be verified.
pub fn rebuild_ledger(store_root: &Path) -> Result<usize, PackageStoreError> {
    let store = HostStore::new(store_root);
    let lock = store.lock()?;
    Ok(store.rebuild_ledger(&lock)?.recorded.len())
}

#[cfg(test)]
#[path = "package_store_test.rs"]
mod tests;

#[cfg(test)]
#[path = "package_store_libraries_test.rs"]
mod libraries_tests;

#[cfg(test)]
#[path = "package_store_retention_test.rs"]
mod retention_tests;
