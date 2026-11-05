//! Everything a saved vault job's validation, version, dry run, and run read of its code: the release its job file names
//! in the vault's package store, `.contextos/packages/`, and that release's library lock.
//!
//! The release and every library release in its lock are read through the vault's store port and verified before use.
//! Libraries are resolved only at the first passing validation of any job on the workflow release, when there is no lock
//! yet; every later read takes the lock as recorded and resolves nothing.

use std::cell::RefCell;
use std::collections::BTreeMap;

use contextos_core::ContentHash;
use contextos_packages::{
    DamageFinding, IncompleteFinding, PackageStore, PackagesError, ReadCheck, ReleaseFilePath, ReleaseFiles,
    ReleaseKey, ReleaseName, ReleaseVersion, UnverifiableReason, VerifiedRelease,
};
use contextos_workflows::call_rules::ToolRef;
use contextos_workflows::workflow_definition::WorkflowText;
use contextos_workflows::{
    LibraryLock, LibraryResolution, LockedLibrary, ModuleOwner, ModuleUnreadable, PackageCode, PackageDependency,
    PackageError, PackageKind, PackageManifest, PackageName, PackageToRead, ReleaseCatalogue, ReleaseContents,
    ReleaseRequirements, ScriptParts, SnapshotBudget, WorkflowDefinition, WorkflowFailureKind, WorkflowFile,
    WorkflowFileFailure, WorkflowFilePath, WorkflowScript, resolve_libraries,
};
use semver::Version;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::package_release_manifest::{PackageFilesOf, PackageRelease};
use crate::release_basis::{BasisRelease, ReleaseBasis};
use crate::shared_run::MAX_SCRIPT_BYTES;

/// One release of a package, by name and version: a workflow release, or a library release in a lock.
pub use contextos_workflows::ReleaseRef;

/// Which release to read, and the library lock to read it with.
pub struct ReleaseRequest<'a> {
    /// The workflow's package in the vault's store: the workflow source folder's name.
    pub package: &'a PackageName,
    /// The exact release the job file names.
    pub release: &'a Version,
    /// The folder of the release that holds the workflow the job runs, which is the workflow's name.
    pub workflow: &'a str,
    /// The lock fixed for the workflow release, or `None` for a release no job has yet passed validation on, whose
    /// libraries are then chosen from the store's library releases.
    pub lock: Option<&'a LibraryLock>,
}

/// What one validation, version, dry run, or run of a saved vault job is made of, read once.
#[derive(Debug)]
pub struct ReleaseSnapshot {
    /// The release's `workflows.toml` or `library.toml`.
    pub kind: PackageKind,
    /// The job's release.
    pub manifest: PackageManifest,
    /// The workflow of that release the job runs.
    pub definition: WorkflowDefinition,
    /// The release's own listed `{workflow}/workflow.toml`, which declares what the workflow may call, its templates,
    /// inputs, outputs, and the rest. Validation and runs read their declarations here and never from source.
    pub workflow_toml: String,
    /// The library releases in the lock, in name order, with their manifests.
    pub libraries: Vec<PackageManifest>,
    /// The lock the snapshot was read with: the recorded one, or the one chosen for a release's first validation.
    pub lock: LibraryLock,
    /// The releases the job's version is made from.
    pub basis: ReleaseBasis,
    /// The entry script and every module of the release and of the library releases in the lock.
    pub script: WorkflowScript,
    /// The tools each locked library release declares in the `mcp_tools` of its `library.toml`, by library name.
    pub library_tools: BTreeMap<PackageName, Vec<ToolRef>>,
}

/// Why a snapshot could not be read. The release it concerns is the job's own or a library release in its lock, and a
/// failure is handled the same for both.
#[derive(Debug, Error)]
pub enum ReleaseFailure {
    /// The release has no folder in the store.
    #[error("{release} is not installed")]
    Removed { release: ReleaseRef },
    /// A listed file is absent, an entry is not listed, or there is no manifest.
    #[error("{release} is incomplete")]
    Incomplete {
        release: ReleaseRef,
        findings: Vec<IncompleteFinding>,
    },
    /// The release is whole but wrong: a hash or the digest differs, or its manifest breaks the schema.
    #[error("{release} is damaged")]
    Damaged {
        release: ReleaseRef,
        findings: Vec<DamageFinding>,
    },
    /// The release could not be verified, which says nothing about whether it is whole.
    #[error("{release} could not be verified")]
    Unverifiable {
        release: ReleaseRef,
        reason: UnverifiableReason,
    },
    /// A library release in the lock is installed with another digest than the lock records.
    #[error("{release} is installed with a digest other than the one it was locked with")]
    LockDigestChanged {
        release: ReleaseRef,
        locked: String,
        found: String,
    },
    /// The release lists no `{workflow}/workflow.toml` for the job's workflow.
    #[error("{release} holds no workflow {workflow}: it lists no {workflow}/workflow.toml")]
    NoSuchWorkflow { release: ReleaseRef, workflow: String },
    /// The workflow's `workflow.toml` in the release does not parse or breaks its schema.
    #[error("{release}: {workflow}/workflow.toml is not valid: {error}")]
    WorkflowInvalid {
        release: ReleaseRef,
        workflow: String,
        error: Box<PackageError>,
    },
    /// The release's `workflows.toml` or `library.toml` is absent, is not text, or breaks its schema.
    #[error("{release}: {error}")]
    PackageFileInvalid {
        release: ReleaseRef,
        file: &'static str,
        /// Whether the release is a library, which a failure names by its package.
        is_library: bool,
        error: Box<PackageError>,
    },
    /// The libraries could not be chosen.
    #[error("{0}")]
    Libraries(PackageError),
    /// A module of the release or of a library release could not be read, or the code is over a limit.
    #[error("{0}")]
    Module(ModuleUnreadable),
    /// The entry script could not be read.
    #[error("{failure}")]
    Entry {
        entry: String,
        failure: WorkflowFileFailure,
    },
    /// The release's own `{workflow}/workflow.toml` could not be read.
    #[error("{failure}")]
    Declarations { file: String, failure: WorkflowFileFailure },
}

/// What a read of one release found, as the verified release or the failure it is.
///
/// # Errors
///
/// The failure `check` is, when the release is not whole and verified.
pub(crate) fn require_verified(
    release: ReleaseRef,
    check: ReadCheck<PackageRelease>,
) -> Result<VerifiedRelease<PackageRelease>, ReleaseFailure> {
    match check {
        ReadCheck::Verified(verified) => Ok(verified),
        ReadCheck::Removed => Err(ReleaseFailure::Removed { release }),
        ReadCheck::Incomplete(findings) => Err(ReleaseFailure::Incomplete { release, findings }),
        ReadCheck::Damaged(findings) => Err(ReleaseFailure::Damaged { release, findings }),
        ReadCheck::Unverifiable(reason) => Err(ReleaseFailure::Unverifiable { release, reason }),
    }
}

/// Reads the release a saved vault job names, with its library lock, from the vault's store.
///
/// With a recorded lock, each library release in it is read and nothing is resolved. Without one, the release's
/// libraries are chosen from the store's library releases by the procedure every store shares, and the choice is
/// returned as the lock for the caller to record with the first passing validation.
///
/// # Errors
///
/// The failure to read the release or a library release, or to choose the libraries.
pub async fn read_release_snapshot<S>(
    store: &S,
    request: &ReleaseRequest<'_>,
) -> Result<ReleaseSnapshot, Box<ReleaseFailure>>
where
    S: PackageStore<Manifest = PackageRelease>,
{
    let opened = open_release(store, request.package, request.release).await?;
    read_opened_release(store, opened, request).await
}

/// A workflow release read and verified, before its library lock is known.
///
/// The workflow's slug, which keys the lock, is only known from the release itself, so a caller opens the release, asks
/// it which workflow it holds, looks the lock up, and then reads the rest.
pub struct OpenedRelease {
    root_ref: ReleaseRef,
    root: Loaded,
}

impl OpenedRelease {
    /// The workflow of this release kept in `folder`, which is the workflow's slug: matched exactly, never by falling back
    /// to another workflow of the release.
    ///
    /// # Errors
    ///
    /// [`ReleaseFailure::NoSuchWorkflow`] when the release lists no `{folder}/workflow.toml`, and
    /// [`ReleaseFailure::Declarations`] or [`ReleaseFailure::WorkflowInvalid`] when it cannot be read or parsed.
    pub fn workflow_of(&self, folder: &str) -> Result<WorkflowDefinition, Box<ReleaseFailure>> {
        let (definition, _) = self.declarations_of(folder)?;
        Ok(definition)
    }

    /// The workflow in `folder` with the text of its `workflow.toml`.
    ///
    /// # Errors
    ///
    /// As [`OpenedRelease::workflow_of`].
    pub fn declarations_of(&self, folder: &str) -> Result<(WorkflowDefinition, String), Box<ReleaseFailure>> {
        if !self.root.kind.workflows().iter().any(|listed| listed == folder) {
            return Err(Box::new(ReleaseFailure::NoSuchWorkflow {
                release: self.root_ref.clone(),
                workflow: folder.to_owned(),
            }));
        }
        let file = format!("{folder}/workflow.toml");
        let text = text_of(&self.root.files, &file, MAX_SCRIPT_BYTES)
            .map_err(|failure| Box::new(ReleaseFailure::Declarations { file, failure }))?;
        let definition = WorkflowDefinition::try_from(WorkflowText { text: &text, folder }).map_err(|error| {
            Box::new(ReleaseFailure::WorkflowInvalid {
                release: self.root_ref.clone(),
                workflow: folder.to_owned(),
                error: Box::new(error),
            })
        })?;
        Ok((definition, text))
    }
}

/// Reads and verifies the release `release` of `package`.
///
/// # Errors
///
/// The failure to read the release whole.
pub async fn open_release<S>(
    store: &S,
    package: &PackageName,
    release: &Version,
) -> Result<OpenedRelease, Box<ReleaseFailure>>
where
    S: PackageStore<Manifest = PackageRelease>,
{
    let root_ref = ReleaseRef {
        name: package.clone(),
        version: release.clone(),
    };
    let root = load(store, &root_ref).await.map_err(Box::new)?;
    Ok(OpenedRelease { root_ref, root })
}

/// [`read_release_snapshot`] for a release already opened.
///
/// # Errors
///
/// As [`read_release_snapshot`].
pub async fn read_opened_release<S>(
    store: &S,
    opened: OpenedRelease,
    request: &ReleaseRequest<'_>,
) -> Result<ReleaseSnapshot, Box<ReleaseFailure>>
where
    S: PackageStore<Manifest = PackageRelease>,
{
    let (definition, workflow_toml) = opened.declarations_of(request.workflow)?;
    let OpenedRelease { root_ref: _, root } = opened;
    let entry_file = format!("{}/{}", definition.folder, definition.entry);
    let entry = text_of(&root.files, &entry_file, MAX_SCRIPT_BYTES).map_err(|failure| {
        Box::new(ReleaseFailure::Entry {
            entry: entry_file,
            failure,
        })
    })?;
    let code = release_code(
        store,
        root,
        &CodeRequest {
            package: request.package,
            lock: request.lock,
            entry,
        },
    )
    .await?;
    Ok(ReleaseSnapshot {
        kind: code.kind,
        manifest: code.manifest,
        definition,
        workflow_toml,
        libraries: code.libraries,
        lock: code.lock,
        basis: code.basis,
        script: code.script,
        library_tools: code.library_tools,
    })
}

/// The code of a release that is not tied to one of its workflows: what a library release is checked by, and the part of
/// a workflow's snapshot that follows from its release and libraries.
#[derive(Debug)]
pub struct ReleaseCode {
    /// The release's `workflows.toml` or `library.toml`.
    pub kind: PackageKind,
    /// The release.
    pub manifest: PackageManifest,
    /// The library releases in the lock, in name order, with their manifests.
    pub libraries: Vec<PackageManifest>,
    /// The lock the code was read with: the recorded one, or the one chosen from the store's library releases.
    pub lock: LibraryLock,
    /// The releases the code is made from.
    pub basis: ReleaseBasis,
    /// The entry script, the modules of the release, and those of the library releases in the lock.
    pub script: WorkflowScript,
    /// The tools each locked library release declares, by library name.
    pub library_tools: BTreeMap<PackageName, Vec<ToolRef>>,
}

/// Which code to read from an opened release.
struct CodeRequest<'a> {
    package: &'a PackageName,
    lock: Option<&'a LibraryLock>,
    entry: String,
}

/// Reads the libraries of an opened release by `request.lock`, or chooses them, and reads every module.
async fn release_code<S>(store: &S, root: Loaded, request: &CodeRequest<'_>) -> Result<ReleaseCode, Box<ReleaseFailure>>
where
    S: PackageStore<Manifest = PackageRelease>,
{
    let libraries = if let Some(lock) = request.lock {
        locked_libraries(store, lock).await?
    } else {
        let requirements = ReleaseRequirements::from(ReleaseContents {
            manifest: root.manifest.manifest(),
            kind: &root.kind,
        });
        chosen_libraries(store, &requirements).await?
    };
    let lock = LibraryLock {
        libraries: libraries
            .iter()
            .map(|library| {
                let identity = &library.manifest.manifest().identity;
                (
                    identity.name.to_string(),
                    LockedLibrary {
                        version: identity.version.clone(),
                        digest: identity.digest.as_str().to_owned(),
                    },
                )
            })
            .collect(),
    };
    let mut budget = SnapshotBudget::default();
    let packages = package_codes(
        libraries.iter().chain(std::iter::once(&root)),
        &mut budget,
        request.package,
    )?;
    let basis = ReleaseBasis {
        package: BasisRelease::from((root.manifest.manifest().identity.clone(), root.manifest_text.clone())),
        libraries: libraries
            .iter()
            .map(|library| {
                BasisRelease::from((
                    library.manifest.manifest().identity.clone(),
                    library.manifest_text.clone(),
                ))
            })
            .collect(),
    };
    // A library checked on its own is one of the libraries its code is checked against: what it calls, it declares.
    let library_tools = tools_of(libraries.iter().chain(std::iter::once(&root)));
    let script = WorkflowScript::from(ScriptParts {
        entry: request.entry.clone(),
        package: Some(request.package.clone()),
        packages,
    });
    Ok(ReleaseCode {
        kind: root.kind.clone(),
        manifest: root.manifest.manifest().clone(),
        libraries: libraries
            .iter()
            .map(|library| library.manifest.manifest().clone())
            .collect(),
        lock,
        basis,
        script,
        library_tools,
    })
}

/// The code of each release in `releases`, every module it lists read from its files and charged to `budget`. A module
/// of the package `own` is reported by its path, as a vault workflow's always was; a library's by its package.
fn package_codes<'a>(
    releases: impl IntoIterator<Item = &'a Loaded>,
    budget: &mut SnapshotBudget,
    own: &PackageName,
) -> Result<BTreeMap<PackageName, PackageCode>, Box<ReleaseFailure>> {
    let mut packages = BTreeMap::new();
    for loaded in releases {
        let read = |path: &WorkflowFilePath| {
            let over_limit =
                |error: PackageError| WorkflowFileFailure::new(WorkflowFailureKind::FileTooLarge, error.to_string());
            let file = file_of(&loaded.files, path.as_str())?;
            let size = u64::try_from(file.content.len()).unwrap_or(u64::MAX);
            budget.charge(path.as_str(), size).map_err(over_limit)?;
            Ok(file)
        };
        let code = PackageCode::read(
            PackageToRead {
                name: loaded.manifest.manifest().identity.name.clone(),
                kind: loaded.kind.clone(),
            },
            read,
        )
        .map_err(|mut error| {
            if error.owner == ModuleOwner::Package(own.clone()) {
                error.owner = ModuleOwner::Workflow;
            }
            Box::new(ReleaseFailure::Module(error))
        })?;
        packages.insert(code.name().clone(), code);
    }
    Ok(packages)
}

/// What a run of a workflow kept in source takes from installed library releases.
pub struct InstalledLibrariesRequest<'a> {
    /// The workflow's package: the source folder's name.
    pub package: &'a PackageName,
    /// Every requirement the source states, its own and those of the libraries it reads from source.
    pub requirements: Vec<PackageDependency>,
    /// What the source's own files and the libraries read from source have already used of the snapshot's limits.
    pub budget: &'a mut SnapshotBudget,
}

/// The installed library releases a run of source uses, chosen for that run only.
#[derive(Debug)]
pub struct InstalledLibraries {
    /// The chosen library releases, in name order, with their manifests.
    pub manifests: Vec<PackageManifest>,
    /// The code of each, keyed by name.
    pub code: BTreeMap<PackageName, PackageCode>,
}

/// Chooses, from the store's installed library releases, the newest that satisfies every requirement `request` states
/// of a library, with the libraries those releases require chosen the same way, and reads every module of them. Nothing
/// is locked or recorded, and no library source is read.
///
/// # Errors
///
/// The failure to choose the libraries, or to read or verify a library release.
pub async fn read_installed_libraries<S>(
    store: &S,
    request: InstalledLibrariesRequest<'_>,
) -> Result<InstalledLibraries, Box<ReleaseFailure>>
where
    S: PackageStore<Manifest = PackageRelease>,
{
    let requirements = ReleaseRequirements {
        release: ReleaseRef {
            name: request.package.clone(),
            version: Version::new(0, 0, 0),
        },
        dependencies: request.requirements,
    };
    let libraries = chosen_libraries(store, &requirements).await?;
    let code = package_codes(&libraries, request.budget, request.package)?;
    Ok(InstalledLibraries {
        manifests: libraries
            .iter()
            .map(|library| library.manifest.manifest().clone())
            .collect(),
        code,
    })
}

/// Reads and verifies the library release `release` of `package`, with the libraries it requires chosen from the store,
/// and reads every module of them, as a check of the library's own code needs.
///
/// # Errors
///
/// The failure to read the release or a library release, or to choose the libraries.
pub async fn read_library_code<S>(
    store: &S,
    package: &PackageName,
    release: &Version,
) -> Result<ReleaseCode, Box<ReleaseFailure>>
where
    S: PackageStore<Manifest = PackageRelease>,
{
    let OpenedRelease { root_ref: _, root } = open_release(store, package, release).await?;
    release_code(
        store,
        root,
        &CodeRequest {
            package,
            lock: None,
            entry: String::new(),
        },
    )
    .await
}

/// The tools each library release declares in the `mcp_tools` of its `library.toml`, by library name. A release that is
/// not a library declares none.
fn tools_of<'a>(libraries: impl IntoIterator<Item = &'a Loaded>) -> BTreeMap<PackageName, Vec<ToolRef>> {
    libraries
        .into_iter()
        .filter_map(|library| match &library.kind {
            PackageKind::Library(manifest) => Some((
                library.manifest.manifest().identity.name.clone(),
                manifest.mcp_tools().to_vec(),
            )),
            PackageKind::Workflows(_) => None,
        })
        .collect()
}

/// A release read and verified.
#[derive(Clone)]
struct Loaded {
    manifest: PackageRelease,
    /// The release's `workflows.toml` or `library.toml`.
    kind: PackageKind,
    manifest_text: String,
    files: ReleaseFiles,
}

async fn load<S>(store: &S, release: &ReleaseRef) -> Result<Loaded, ReleaseFailure>
where
    S: PackageStore<Manifest = PackageRelease>,
{
    let key = ReleaseKey::try_from(Asked(release)).map_err(|error| {
        ReleaseFailure::Libraries(PackageError::CatalogueUnreadable {
            name: release.name.to_string(),
            reason: error.to_string(),
        })
    })?;
    let verified = require_verified(release.clone(), store.read_release(&key).await)?;
    let kind = PackageKind::try_from(PackageFilesOf {
        manifest: verified.manifest.manifest(),
        files: &verified.files,
    })
    .map_err(|error| ReleaseFailure::PackageFileInvalid {
        release: release.clone(),
        file: verified.manifest.manifest().package_file_name(),
        is_library: verified.manifest.manifest().is_library(),
        error: Box::new(error),
    })?;
    Ok(Loaded {
        manifest: verified.manifest,
        kind,
        manifest_text: verified.manifest_text,
        files: verified.files,
    })
}

/// The library releases a recorded lock names, each read and held to the digest the lock records.
async fn locked_libraries<S>(store: &S, lock: &LibraryLock) -> Result<Vec<Loaded>, Box<ReleaseFailure>>
where
    S: PackageStore<Manifest = PackageRelease>,
{
    let mut libraries = Vec::new();
    for (name, locked) in &lock.libraries {
        let release = ReleaseRef {
            name: PackageName::try_from(name.as_str()).map_err(|error| Box::new(ReleaseFailure::Libraries(error)))?,
            version: locked.version.clone(),
        };
        let loaded = load(store, &release).await.map_err(Box::new)?;
        let found = loaded.manifest.manifest().identity.digest.as_str();
        if found != locked.digest {
            return Err(Box::new(ReleaseFailure::LockDigestChanged {
                release,
                locked: locked.digest.clone(),
                found: found.to_owned(),
            }));
        }
        libraries.push(loaded);
    }
    Ok(libraries)
}

/// The library releases a workflow release runs with when no lock is fixed yet: chosen from the store's releases, each
/// read the first time the procedure asks for what it requires.
async fn chosen_libraries<S>(store: &S, requirements: &ReleaseRequirements) -> Result<Vec<Loaded>, Box<ReleaseFailure>>
where
    S: PackageStore<Manifest = PackageRelease>,
{
    let listing = store.list().await.map_err(|error| {
        Box::new(ReleaseFailure::Libraries(PackageError::CatalogueUnreadable {
            name: requirements.release.name.to_string(),
            reason: error.to_string(),
        }))
    })?;
    let catalogue = LoadingCatalogue::new(listing.releases);
    let resolved = loop {
        let outcome = resolve_libraries(&LibraryResolution {
            root: requirements,
            catalogue: &catalogue,
        });
        match outcome {
            Ok(resolved) => break resolved,
            Err(error) => {
                if let Some(wanted) = catalogue.take_wanted() {
                    let loaded = load(store, &wanted).await;
                    catalogue.keep(wanted, loaded);
                } else if let Some(failure) = catalogue.take_failure() {
                    return Err(Box::new(failure));
                } else {
                    return Err(Box::new(ReleaseFailure::Libraries(error)));
                }
            }
        }
    };
    let mut libraries = Vec::new();
    for release in resolved.releases() {
        match catalogue.loaded(&release) {
            Some(loaded) => libraries.push(loaded),
            None => libraries.push(load(store, &release).await.map_err(Box::new)?),
        }
    }
    Ok(libraries)
}

/// What the resolution procedure asks of the store: the versions of a package and the dependencies of a release. A
/// release it has not been given is noted as wanted and refused, and the caller reads it and asks again.
struct LoadingCatalogue {
    versions: BTreeMap<PackageName, Vec<Version>>,
    kept: RefCell<BTreeMap<ReleaseRef, Result<Loaded, ()>>>,
    failure: RefCell<Option<ReleaseFailure>>,
    wanted: RefCell<Option<ReleaseRef>>,
}

impl LoadingCatalogue {
    fn new(releases: Vec<ReleaseKey>) -> Self {
        let mut versions: BTreeMap<PackageName, Vec<Version>> = BTreeMap::new();
        for key in releases {
            if let Ok(name) = PackageName::try_from(key.name.as_str()) {
                versions.entry(name).or_default().push(key.version.as_semver().clone());
            }
        }
        Self {
            versions,
            kept: RefCell::default(),
            failure: RefCell::default(),
            wanted: RefCell::default(),
        }
    }

    fn take_wanted(&self) -> Option<ReleaseRef> {
        self.wanted.borrow_mut().take()
    }

    fn take_failure(&self) -> Option<ReleaseFailure> {
        self.failure.borrow_mut().take()
    }

    fn keep(&self, release: ReleaseRef, loaded: Result<Loaded, ReleaseFailure>) {
        let entry = match loaded {
            Ok(loaded) => Ok(loaded),
            Err(failure) => {
                self.failure.borrow_mut().get_or_insert(failure);
                Err(())
            }
        };
        self.kept.borrow_mut().insert(release, entry);
    }

    fn loaded(&self, release: &ReleaseRef) -> Option<Loaded> {
        self.kept.borrow().get(release)?.clone().ok()
    }
}

impl ReleaseCatalogue for LoadingCatalogue {
    fn versions(&self, name: &PackageName) -> Result<Vec<Version>, PackageError> {
        Ok(self.versions.get(name).cloned().unwrap_or_default())
    }

    fn dependencies(&self, release: &ReleaseRef) -> Result<Vec<PackageDependency>, PackageError> {
        match self.kept.borrow().get(release) {
            Some(Ok(loaded)) => Ok(loaded.kind.libraries().entries().to_vec()),
            Some(Err(())) => Err(PackageError::CatalogueUnreadable {
                name: release.name.to_string(),
                reason: format!("{release} could not be read"),
            }),
            None => {
                *self.wanted.borrow_mut() = Some(release.clone());
                Err(PackageError::CatalogueUnreadable {
                    name: release.name.to_string(),
                    reason: format!("{release} has not been read yet"),
                })
            }
        }
    }
}

/// A release as the job or a lock names it, on its way to the key the store finds it by.
struct Asked<'a>(&'a ReleaseRef);

impl TryFrom<Asked<'_>> for ReleaseKey {
    type Error = PackagesError;

    fn try_from(Asked(release): Asked<'_>) -> Result<Self, PackagesError> {
        Ok(Self {
            name: ReleaseName::try_from(release.name.as_str())?,
            version: ReleaseVersion::try_from(release.version.to_string().as_str())?,
        })
    }
}

/// The text of the file at `path`, within `limit` bytes.
fn text_of(files: &ReleaseFiles, path: &str, limit: u64) -> Result<String, WorkflowFileFailure> {
    let file = file_of(files, path)?;
    if u64::try_from(file.content.len()).map_or(true, |length| length > limit) {
        return Err(WorkflowFileFailure::new(
            WorkflowFailureKind::FileTooLarge,
            format!("{path} is larger than the {limit} byte limit"),
        ));
    }
    Ok(file.content)
}

/// The file at `path` as a script reads it.
fn file_of(files: &ReleaseFiles, path: &str) -> Result<WorkflowFile, WorkflowFileFailure> {
    let missing = || {
        WorkflowFileFailure::new(
            WorkflowFailureKind::FileNotFound,
            format!("{path} is not in the release"),
        )
    };
    let key = ReleaseFilePath::try_from(path).map_err(|_| missing())?;
    let bytes = files.0.get(&key).ok_or_else(missing)?;
    let content = String::from_utf8(bytes.clone())
        .map_err(|_| WorkflowFileFailure::new(WorkflowFailureKind::FileNotText, format!("{path} is not UTF-8 text")))?;
    Ok(WorkflowFile {
        content,
        content_hash: ContentHash::from(<[u8; 32]>::from(Sha256::digest(bytes))),
    })
}

#[cfg(test)]
#[path = "vault_release_snapshot_test.rs"]
mod tests;
