//! Everything one run of a shared workflow is made of, read once.
//!
//! The snapshot is the job's package release and its manifest, the library releases it resolves and their manifests,
//! the entry script, and the text of every module of every one of those packages. It is read in a single blocking step.
//! No lock is held on the store while it is read, so the step ends by reading each resolved release's manifest again and
//! fails if any is gone or is not the one that was read: an install or removal in the middle of a read is never mixed
//! into one snapshot.
//!
//! Validation and a run are separate reads. What ties them together is the job's version: the run is held until the
//! version it resolves is one that was reviewed (`require_reviewed`), and the version is made from the same releases and
//! manifests a snapshot holds.
//!
//! Libraries are resolved by the pure procedure of [`contextos_workflows::library_resolution`]. The store is only asked
//! what is installed, one package name at a time, so a package nothing reaches is never read.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use contextos_workflows::call_rules::ToolRef;
use contextos_workflows::workflow_definition::WorkflowText;
use contextos_workflows::{
    DeclaredFiles, LibraryLock, LibraryResolution, LockedLibrary, PackageCode, PackageDependency, PackageError,
    PackageIdentity, PackageKind, PackageManifest, PackageName, PackageToRead, ReleaseCatalogue, ReleaseContents,
    ReleaseRef, ReleaseRequirements, ScriptParts, SnapshotBudget, ValidationCode, WorkflowDefinition,
    WorkflowFailureKind, WorkflowFileFailure, WorkflowFilePath, WorkflowScript, resolve_libraries,
};
use semver::Version;
use thiserror::Error;

use crate::bounded_read::read_text_within;
use crate::package_files::{file_size, read_file};
use crate::package_store::{self, InstalledRelease, PackageStoreError};
use crate::release_basis::{BasisRelease, ReleaseBasis};
use crate::shared_run::MAX_SCRIPT_BYTES;

/// Which snapshot to read.
pub struct SnapshotRequest<'a> {
    pub store_root: &'a Path,
    pub package: PackageName,
    /// The exact release of the package to run: the one the job's file names, never the newest.
    pub version: &'a Version,
    pub workflow: &'a str,
    /// The library lock recorded for the workflow release, or `None` for a release no job has yet passed validation on,
    /// whose libraries are resolved by this read and reported as the lock to record.
    pub lock: Option<&'a LibraryLock>,
}

/// What one run of a shared workflow is made of.
#[derive(Debug)]
pub struct SharedSnapshot {
    /// The job's package release.
    pub manifest: PackageManifest,
    /// The workflow of that release the job runs.
    pub definition: WorkflowDefinition,
    /// Where the release is installed, for the files its script reads with `workflow_file`.
    pub release_dir: PathBuf,
    /// The files and folders, relative to the release's root, that its scripts may read: those `workflows.toml` declares.
    pub templates: DeclaredFiles,
    /// The library releases the job resolved, in name order.
    pub libraries: Vec<PackageManifest>,
    /// The releases the job's version is made from.
    pub basis: ReleaseBasis,
    /// The lock the snapshot was read with: the recorded one, or the one chosen for a release's first validation.
    pub lock: LibraryLock,
    /// The entry script and every module of the package and its libraries, keyed by owner.
    pub script: WorkflowScript,
    /// The tools each library release declares in the `mcp_tools` of its `library.toml`, by library name, which
    /// validation checks against what the workflow permits and what the library's code calls.
    pub library_tools: BTreeMap<PackageName, Vec<ToolRef>>,
}

/// A snapshot, or why it could not be read. The failure is boxed because it carries a good deal of what was known.
pub type SnapshotResult<T> = Result<T, Box<SnapshotFailure>>;

/// Why a snapshot could not be read, and what was known by then.
#[derive(Debug, Error)]
#[error("{error}")]
pub struct SnapshotFailure {
    /// The release the job resolved to, when it got that far, to say which package asked.
    pub identity: Option<PackageIdentity>,
    /// The releases the job's version is made from, once its package and every library resolved. A job whose libraries
    /// do not resolve has none, and so has no version.
    pub basis: Option<ReleaseBasis>,
    /// The workflow, when the release has it, so the checks that need only its declarations can still be made.
    pub definition: Option<WorkflowDefinition>,
    #[source]
    pub error: PackageStoreError,
}

impl SnapshotFailure {
    /// How the failure is reported when it is a failure to resolve the job's libraries: a missing artefact for a library
    /// or release that is not there, and an invalid one for a loop or a limit.
    #[must_use]
    pub fn dependency_code(&self) -> Option<ValidationCode> {
        match &self.error {
            PackageStoreError::Package(error) => ValidationCode::try_from(error).ok(),
            _ => None,
        }
    }

    /// Whether the job's own package, or a library it needs, is installed damaged: its manifest no longer parses or
    /// breaks its schema, a file no longer hashes to what the manifest lists, or it is not the release its folder
    /// names. That is an invalid artefact and not a missing one.
    #[must_use]
    pub fn release_damaged(&self) -> bool {
        matches!(
            &self.error,
            PackageStoreError::ReleaseDamaged { .. }
                | PackageStoreError::Package(PackageError::InstalledManifestInvalid { .. })
        )
    }

    /// Whether a release was removed, replaced, or changed while the snapshot was being read, or cannot be verified at
    /// all because a file is absent or unexpected or the store cannot be read. That is a fact about the host and not a
    /// defect in the job: nothing about the job changes, and the next read starts again.
    #[must_use]
    pub fn release_changed(&self) -> bool {
        matches!(
            &self.error,
            PackageStoreError::ReleaseChangedWhileRead { .. } | PackageStoreError::ReleaseUnverifiable { .. }
        )
    }
}

/// The workflow of `release` kept in `folder`, matched exactly: none when the release lists no `{folder}/workflow.toml`.
///
/// # Errors
///
/// [`PackageStoreError::WorkflowInvalid`] when the file does not parse or breaks the schema.
fn definition_of(
    release: &InstalledRelease,
    folder: &str,
) -> Result<Option<WorkflowDefinition>, Box<PackageStoreError>> {
    let Some(text) = release.workflow_files.get(folder) else {
        return Ok(None);
    };
    WorkflowDefinition::try_from(WorkflowText { text, folder })
        .map(Some)
        .map_err(|error| {
            Box::new(PackageStoreError::WorkflowInvalid {
                package: release.manifest.identity.name.to_string(),
                version: release.manifest.identity.version.clone(),
                workflow: folder.to_owned(),
                error: Box::new(error),
            })
        })
}

/// The entry script of `definition`, read from the release at `release_dir`, within the size limit.
fn read_entry(release_dir: &Path, definition: &WorkflowDefinition) -> Result<String, PackageStoreError> {
    let entry_file = format!("{}/{}", definition.folder, definition.entry);
    read_text_within(&release_dir.join(&entry_file), MAX_SCRIPT_BYTES).map_err(|source| {
        PackageStoreError::EntryUnreadable {
            entry: entry_file,
            source,
        }
    })
}

/// The package release a job runs and its libraries, resolved, before any module is read.
struct Resolved {
    release: InstalledRelease,
    /// The workflow the job runs, when the release has it.
    definition: Option<WorkflowDefinition>,
    libraries: Vec<InstalledRelease>,
    basis: ReleaseBasis,
    /// The lock the libraries were read with or chosen as.
    lock: LibraryLock,
}

impl SharedSnapshot {
    /// Resolves the package release and its libraries, which is all a job's version needs, without reading any
    /// module. A snapshot read afterwards sees the same releases only if the store is unchanged, so a caller that
    /// will run or validate reads the whole snapshot instead.
    ///
    /// # Errors
    ///
    /// Returns the failure when the package or release is not installed, or the libraries do not resolve.
    pub fn read_basis(request: &SnapshotRequest<'_>) -> SnapshotResult<ReleaseBasis> {
        Self::resolve(request).map(|resolved| resolved.basis)
    }

    fn resolve(request: &SnapshotRequest<'_>) -> SnapshotResult<Resolved> {
        let store_root = request.store_root;
        let package = &request.package;
        let before_release = |error: PackageStoreError| {
            Box::new(SnapshotFailure {
                identity: None,
                basis: None,
                definition: None,
                error,
            })
        };
        let installed = package_store::installed_versions_of(store_root, package).map_err(before_release)?;
        let release = installed.choose(package, request.version).map_err(before_release)?;
        let release = package_store::read_release(store_root, package, &release).map_err(before_release)?;
        let manifest = &release.manifest;
        let definition = match definition_of(&release, request.workflow) {
            Ok(definition) => definition,
            Err(error) => {
                return Err(Box::new(SnapshotFailure {
                    identity: Some(manifest.identity.clone()),
                    basis: None,
                    definition: None,
                    error: *error,
                }));
            }
        };
        let catalogue = StoreCatalogue::new(store_root);
        let root = ReleaseRequirements::from(ReleaseContents {
            manifest,
            kind: &release.kind,
        });
        let unresolved = |error: PackageError| {
            Box::new(SnapshotFailure {
                identity: Some(manifest.identity.clone()),
                basis: None,
                definition: definition.clone(),
                error: catalogue.take_unverifiable().unwrap_or_else(|| error.into()),
            })
        };
        // With a recorded lock the libraries are the locked releases and nothing is resolved. Without one this is the
        // workflow release's first validation, and the procedure chooses them.
        let wanted: Vec<ReleaseRef> = match request.lock {
            Some(lock) => locked_releases(lock).map_err(unresolved)?,
            None => resolve_libraries(&LibraryResolution {
                root: &root,
                catalogue: &catalogue,
            })
            .map_err(unresolved)?
            .releases()
            .collect(),
        };
        let libraries = wanted
            .iter()
            .map(|library| catalogue.release(library))
            .collect::<Result<Vec<_>, _>>()
            .map_err(unresolved)?;
        if let Some(lock) = request.lock {
            held_to_lock(lock, &libraries).map_err(unresolved)?;
        }
        let lock = LibraryLock::from(ChosenLibraries(&libraries));
        let basis = ReleaseBasis {
            package: BasisRelease::from(&release),
            libraries: libraries.iter().map(BasisRelease::from).collect(),
        };
        Ok(Resolved {
            release,
            definition,
            libraries,
            basis,
            lock,
        })
    }

    /// Reads the snapshot on the calling thread, which must be one that may block.
    ///
    /// # Errors
    ///
    /// Returns the failure and the release resolved so far: the package or release is not installed, the workflow is
    /// not in it, the libraries do not resolve, or the entry script or a module cannot be read or is over a limit.
    pub fn read(request: &SnapshotRequest<'_>) -> SnapshotResult<Self> {
        let store_root = request.store_root;
        #[cfg(test)]
        crate::snapshot_hooks::note_reading_thread(store_root);
        let package = &request.package;
        let Resolved {
            release,
            definition,
            libraries,
            basis,
            lock,
        } = Self::resolve(request)?;
        let manifest = release.manifest.clone();
        let identity = manifest.identity.clone();
        let release_version = identity.version.clone();
        let Some(definition) = definition else {
            return Err(Box::new(SnapshotFailure {
                identity: Some(identity),
                basis: Some(basis),
                definition: None,
                error: PackageStoreError::NoSuchWorkflow {
                    package: package.to_string(),
                    version: release_version,
                    workflow: request.workflow.to_owned(),
                },
            }));
        };
        let failed = |error: PackageStoreError| {
            Box::new(SnapshotFailure {
                identity: Some(identity.clone()),
                basis: Some(basis.clone()),
                definition: Some(definition.clone()),
                error,
            })
        };
        // A file that cannot be read because its release was removed or replaced under the read is that change, caught
        // mid-read, and not a fault of the release: so a read that fails first looks at whether the release is still
        // the one that was resolved.
        let read_failed = |error: PackageStoreError| {
            failed(changed_release(store_root, std::iter::once(&release).chain(&libraries)).unwrap_or(error))
        };
        let release_dir = package_store::package_version_dir(store_root, package, &release_version);
        let entry = read_entry(&release_dir, &definition).map_err(read_failed)?;
        let library_manifests: Vec<PackageManifest> =
            libraries.iter().map(|library| library.manifest.clone()).collect();

        // Every module of every package, in name order, charged against the limits from its size before it is read and
        // for any bytes the read then finds beyond that size.
        let mut budget = SnapshotBudget::default();
        let mut packages = BTreeMap::new();
        let mut every_package: Vec<&InstalledRelease> = libraries.iter().chain(std::iter::once(&release)).collect();
        every_package.sort_by(|left, right| left.manifest.identity.name.cmp(&right.manifest.identity.name));
        for installed in every_package {
            let identity = &installed.manifest.identity;
            let dir = package_store::package_version_dir(store_root, &identity.name, &identity.version);
            let read = |path: &WorkflowFilePath| {
                let over_limit = |error: PackageError| {
                    WorkflowFileFailure::new(WorkflowFailureKind::FileTooLarge, error.to_string())
                };
                let size = file_size(&dir, path)?;
                budget.charge(path.as_str(), size).map_err(over_limit)?;
                #[cfg(test)]
                crate::snapshot_hooks::after_size_checked(store_root, path.as_str());
                let file = read_file(&dir, path)?;
                // The file can have grown since its size was read, and what was read is what the snapshot holds.
                let read = u64::try_from(file.content.len()).unwrap_or(u64::MAX);
                budget
                    .charge_growth(path.as_str(), read.saturating_sub(size))
                    .map_err(over_limit)?;
                Ok(file)
            };
            let code = PackageCode::read(
                PackageToRead {
                    name: identity.name.clone(),
                    kind: installed.kind.clone(),
                },
                read,
            )
            .map_err(|error| read_failed(error.into()))?;
            packages.insert(code.name().clone(), code);
        }
        let library_tools = declared_tools(&libraries);
        #[cfg(test)]
        crate::snapshot_hooks::after_modules_read(store_root);
        // Nothing held the store while the modules were read, so the releases they were read from must still be the
        // ones that were resolved.
        if let Some(changed) = changed_release(store_root, std::iter::once(&release).chain(&libraries)) {
            return Err(failed(changed));
        }
        let script = WorkflowScript::from(ScriptParts {
            entry,
            package: Some(package.clone()),
            packages,
        });
        #[cfg(test)]
        crate::snapshot_hooks::after_snapshot_read(store_root);
        Ok(Self {
            manifest,
            definition,
            release_dir,
            templates: release.kind.templates().cloned().unwrap_or_default(),
            libraries: library_manifests,
            basis,
            lock,
            script,
            library_tools,
        })
    }
}

/// The tools each library release declares in the `mcp_tools` of its `library.toml`, by library name. A release that is
/// not a library declares none.
fn declared_tools(libraries: &[InstalledRelease]) -> BTreeMap<PackageName, Vec<ToolRef>> {
    libraries
        .iter()
        .filter_map(|library| match &library.kind {
            PackageKind::Library(manifest) => {
                Some((library.manifest.identity.name.clone(), manifest.mcp_tools().to_vec()))
            }
            PackageKind::Workflows(_) => None,
        })
        .collect()
}

/// As [`SharedSnapshot::read`], on a blocking thread, so a run, a validation, or a version never reads the store on an
/// async executor thread.
///
/// # Errors
///
/// As [`SharedSnapshot::read`]. A read that did not complete is an I/O failure with no release.
pub async fn read_snapshot(request: SnapshotRequest<'_>) -> SnapshotResult<SharedSnapshot> {
    let store_root = request.store_root.to_path_buf();
    let package = request.package;
    let version = request.version.clone();
    let workflow = request.workflow.to_owned();
    let lock = request.lock.cloned();
    tokio::task::spawn_blocking(move || {
        SharedSnapshot::read(&SnapshotRequest {
            store_root: &store_root,
            package,
            version: &version,
            workflow: &workflow,
            lock: lock.as_ref(),
        })
    })
    .await
    .unwrap_or_else(|join_error| {
        Err(Box::new(SnapshotFailure {
            identity: None,
            basis: None,
            definition: None,
            error: PackageStoreError::Io(std::io::Error::other(format!(
                "the read did not complete: {join_error}"
            ))),
        }))
    })
}

/// The library releases a recorded lock names, which are the ones to read.
fn locked_releases(lock: &LibraryLock) -> Result<Vec<ReleaseRef>, PackageError> {
    lock.libraries
        .iter()
        .map(|(name, locked)| {
            Ok(ReleaseRef {
                name: PackageName::try_from(name.as_str())?,
                version: locked.version.clone(),
            })
        })
        .collect()
}

/// Holds each library release read to the digest the lock recorded for it. A release installed with another digest is
/// not the one that was locked, whatever its files hold, and is as unusable as one that is damaged.
fn held_to_lock(lock: &LibraryLock, libraries: &[InstalledRelease]) -> Result<(), PackageError> {
    for library in libraries {
        let identity = &library.manifest.identity;
        let locked = lock.libraries.get(identity.name.as_str());
        if locked.is_none_or(|locked| locked.digest != identity.digest.as_str()) {
            return Err(PackageError::InstalledManifestInvalid {
                name: identity.name.clone(),
                version: identity.version.clone(),
                reason: format!(
                    "it is installed with digest {}, which is not the digest it was locked with",
                    identity.digest.as_str()
                ),
            });
        }
    }
    Ok(())
}

/// The library releases a snapshot was read with, which are what its lock records.
struct ChosenLibraries<'a>(&'a [InstalledRelease]);

impl From<ChosenLibraries<'_>> for LibraryLock {
    fn from(chosen: ChosenLibraries<'_>) -> Self {
        Self {
            libraries: chosen
                .0
                .iter()
                .map(|library| {
                    let identity = &library.manifest.identity;
                    (
                        identity.name.to_string(),
                        LockedLibrary {
                            version: identity.version.clone(),
                            digest: identity.digest.as_str().to_owned(),
                        },
                    )
                })
                .collect(),
        }
    }
}

/// The installed releases of the store, read one package name at a time, with each manifest read once.
struct StoreCatalogue<'a> {
    store_root: &'a Path,
    releases: RefCell<BTreeMap<ReleaseRef, InstalledRelease>>,
    /// The first release read that could not be verified. Resolution knows only that a release could not be read, and a
    /// release that cannot be verified is not one the job is wrong about, so the snapshot reports this instead.
    unverifiable: RefCell<Option<PackageStoreError>>,
}

impl<'a> StoreCatalogue<'a> {
    fn new(store_root: &'a Path) -> Self {
        Self {
            store_root,
            releases: RefCell::new(BTreeMap::new()),
            unverifiable: RefCell::new(None),
        }
    }

    /// The installed `release`, read the first time and kept.
    fn release(&self, release: &ReleaseRef) -> Result<InstalledRelease, PackageError> {
        if let Some(kept) = self.releases.borrow().get(release) {
            return Ok(kept.clone());
        }
        let read = package_store::read_release(self.store_root, &release.name, &release.version).map_err(|error| {
            match error {
                // A damaged release is the invalid artefact it is, not a release that is missing.
                PackageStoreError::ReleaseDamaged {
                    name,
                    version,
                    findings,
                } => PackageError::InstalledManifestInvalid {
                    name,
                    version,
                    reason: findings,
                },
                error => {
                    let described = PackageError::CatalogueUnreadable {
                        name: release.name.to_string(),
                        reason: error.to_string(),
                    };
                    if matches!(error, PackageStoreError::ReleaseUnverifiable { .. }) {
                        self.unverifiable.borrow_mut().get_or_insert(error);
                    }
                    described
                }
            }
        })?;
        self.releases.borrow_mut().insert(release.clone(), read.clone());
        Ok(read)
    }

    /// The release read that could not be verified, if one was, which is what a failure to resolve is then about.
    fn take_unverifiable(&self) -> Option<PackageStoreError> {
        self.unverifiable.borrow_mut().take()
    }
}

impl ReleaseCatalogue for StoreCatalogue<'_> {
    fn versions(&self, name: &PackageName) -> Result<Vec<Version>, PackageError> {
        let installed = package_store::installed_versions_of(self.store_root, name).map_err(|error| {
            PackageError::CatalogueUnreadable {
                name: name.to_string(),
                reason: error.to_string(),
            }
        })?;
        // Every installed version is a candidate, whatever state it is in: a release that is not whole is found out
        // when it is read, and is never passed over for an older one.
        Ok(installed.all())
    }

    fn dependencies(&self, release: &ReleaseRef) -> Result<Vec<PackageDependency>, PackageError> {
        Ok(self.release(release)?.kind.libraries().entries().to_vec())
    }
}

/// The first of `releases` that is no longer the one that was resolved, as the error that says so: gone, or with another
/// identity or manifest.
fn changed_release<'a>(
    store_root: &Path,
    releases: impl Iterator<Item = &'a InstalledRelease>,
) -> Option<PackageStoreError> {
    releases.into_iter().find_map(|resolved| {
        let identity = &resolved.manifest.identity;
        let unchanged = package_store::read_release(store_root, &identity.name, &identity.version)
            .is_ok_and(|now| now.manifest.identity == *identity && now.manifest_text == resolved.manifest_text);
        (!unchanged).then(|| PackageStoreError::ReleaseChangedWhileRead {
            name: identity.name.clone(),
            version: identity.version.clone(),
        })
    })
}

#[cfg(test)]
#[path = "shared_snapshot_test.rs"]
mod tests;
