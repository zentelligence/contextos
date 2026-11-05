//! What installing a source package would make: each release, bottom-up, with the version it takes and whether an
//! installed release already holds the same content.
//!
//! A library the source names without a requirement is planned first, and the package that needs it requires `^` the
//! version the plan gives it. The plan reads the ledger and the job state it is given and writes nothing.

use std::collections::{BTreeMap, BTreeSet};

use contextos_packages::{
    LedgerEntry, ReleaseDigest, ReleaseFilePath, ReleaseFiles, ReleaseKey, ReleaseManifest, ReleaseName, ReleaseRules,
    ReleaseVersion, StoreRules,
};
use contextos_workflows::package::{LIBRARY_FILE_NAME, WORKFLOWS_FILE_NAME};
use contextos_workflows::{JobsState, LibraryManifest, PackageName, ReleaseKind, ValidationCode, WorkflowsFile};
use semver::{Version, VersionReq};

use crate::package_release_manifest::{PackageInterface, PackageRelease};
use crate::source_install_error::SourceInstallError;
use crate::source_release::{
    ManifestParts, ManifestText, PackageFileRewrite, RecordedHighest, ReleasePackageFile, VersionRequest,
};
use crate::vault_source_read::{SourceKind, SourcePackage, SourceSnapshot};

/// What the store and the job state hold, which a plan is made against.
pub(crate) struct Holdings<'a> {
    /// Every release the ledger records, removed or not.
    pub ledger: &'a [LedgerEntry<PackageInterface>],
    /// The releases whose folders stand in the store.
    pub present: &'a BTreeSet<ReleaseKey>,
    /// The job state, which says which releases are current, previous, or future.
    pub state: &'a JobsState,
}

/// What to plan.
pub(crate) struct PlanRequest<'a> {
    pub snapshot: &'a SourceSnapshot,
    pub holdings: Holdings<'a>,
    /// The exact version of the package to restore, when the install is a repair.
    pub at: Option<&'a Version>,
}

/// What the plan does for one release.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PlannedAction {
    /// The release is to be installed, or with a repair restored if the store finds it needs that.
    Install,
    /// An installed release already holds this content.
    Existing,
}

/// One release of the plan.
#[derive(Clone, Debug)]
pub(crate) struct PlannedRelease {
    pub name: PackageName,
    pub kind: SourceKind,
    pub key: ReleaseKey,
    pub digest: ReleaseDigest,
    pub manifest_text: String,
    pub files: ReleaseFiles,
    pub action: PlannedAction,
    /// The libraries the release requires and what it requires of each.
    pub requirements: Vec<(PackageName, VersionReq)>,
    /// The exact version of each source library the release was tested on, which the requirement it states is made from.
    pub tested: BTreeMap<PackageName, Version>,
}

impl PlannedRelease {
    /// The release's version.
    pub(crate) fn version(&self) -> &Version {
        self.key.version.as_semver()
    }
}

/// The releases to make, each library before the package that needs it and the package asked for last.
#[derive(Clone, Debug)]
pub(crate) struct Plan {
    pub releases: Vec<PlannedRelease>,
}

impl Plan {
    /// The exact releases a check of `release` reads libraries from: each library it was tested on, and those libraries'
    /// own tested libraries in turn.
    pub(crate) fn tested_releases(&self, release: &PlannedRelease) -> BTreeSet<ReleaseKey> {
        let mut found = BTreeSet::new();
        let mut pending = vec![release];
        while let Some(next) = pending.pop() {
            for (name, version) in &next.tested {
                if let Some(library) = self
                    .releases
                    .iter()
                    .find(|planned| planned.name == *name && planned.version() == version)
                    && found.insert(library.key.clone())
                {
                    pending.push(library);
                }
            }
        }
        found
    }

    /// The package that was asked for.
    pub(crate) fn root(&self) -> Option<&PlannedRelease> {
        self.releases.last()
    }
}

/// Plans the install of `request.snapshot`'s root.
///
/// # Errors
///
/// [`SourceInstallError::NameTaken`] when the other kind of package holds the name, [`SourceInstallError::NotInstalled`]
/// and [`SourceInstallError::SourceChanged`] for a repair the ledger does not allow, and [`SourceInstallError::Invalid`]
/// for a source that does not make a release.
pub(crate) fn plan(request: &PlanRequest<'_>) -> Result<Plan, SourceInstallError> {
    let mut planner = Planner {
        request,
        planned: BTreeMap::new(),
        releases: Vec::new(),
    };
    planner.package(&request.snapshot.root, true)?;
    Ok(Plan {
        releases: planner.releases,
    })
}

struct Planner<'a> {
    request: &'a PlanRequest<'a>,
    planned: BTreeMap<PackageName, Version>,
    releases: Vec<PlannedRelease>,
}

impl Planner<'_> {
    /// Plans `source`, after the libraries it reads from source, and returns the version it takes.
    fn package(&mut self, source: &SourcePackage, is_root: bool) -> Result<Version, SourceInstallError> {
        if let Some(version) = self.planned.get(&source.name) {
            return Ok(version.clone());
        }
        self.check_kind_free(source)?;
        let mut tested = BTreeMap::new();
        for library in source.libraries.iter().filter(|library| library.requirement.is_none()) {
            let library_source = self.request.snapshot.libraries.get(&library.name).ok_or_else(|| {
                SourceInstallError::refusing(
                    ValidationCode::ArtefactMissing,
                    &format!("{}:{LIBRARY_FILE_NAME}", library.name),
                    "the library's source was not read",
                )
            })?;
            let version = self.package(library_source, false)?;
            tested.insert(library.name.clone(), version);
        }
        let release = self.release(source, &tested, is_root)?;
        let version = release.version().clone();
        self.planned.insert(source.name.clone(), version.clone());
        self.releases.push(release);
        Ok(version)
    }

    /// Refuses a name the other kind of package holds: a workflow package and a library share one name space.
    fn check_kind_free(&self, source: &SourcePackage) -> Result<(), SourceInstallError> {
        let held = self
            .request
            .holdings
            .ledger
            .iter()
            .find(|entry| entry.release.key.name.as_str() == source.name.as_str())
            .map(|entry| {
                if entry.interface.workflows.is_empty() {
                    SourceKind::Library
                } else {
                    SourceKind::Workflows
                }
            });
        match (held, source.kind) {
            (Some(SourceKind::Library), SourceKind::Workflows) => Err(SourceInstallError::NameTaken {
                name: source.name.to_string(),
                held: "library",
                wanted: "workflow package",
                wanted_folder: "workflows",
            }),
            (Some(SourceKind::Workflows), SourceKind::Library) => Err(SourceInstallError::NameTaken {
                name: source.name.to_string(),
                held: "workflow package",
                wanted: "library",
                wanted_folder: "libraries",
            }),
            _ => Ok(()),
        }
    }

    /// The release `source` makes, tested against `tested` versions of its source libraries.
    fn release(
        &self,
        source: &SourcePackage,
        tested: &BTreeMap<PackageName, Version>,
        is_root: bool,
    ) -> Result<PlannedRelease, SourceInstallError> {
        let (package_file, files) = release_files(source, tested)?;
        let digest = StoreRules.digest(&files);
        let interface = interface_of(&source.name, &files)?;
        let requirements = requirements_of(source.kind, &package_file)?;
        let name = ReleaseName::try_from(source.name.as_str())?;
        let entries: Vec<&LedgerEntry<PackageInterface>> = self
            .request
            .holdings
            .ledger
            .iter()
            .filter(|entry| entry.release.key.name == name)
            .collect();
        let (version, action) = match self.request.at.filter(|_| is_root) {
            Some(at) => (restore(&entries, source, at, &digest)?, PlannedAction::Install),
            None => self.next(&entries, source, &digest, &interface)?,
        };
        let key = ReleaseKey {
            name,
            version: ReleaseVersion::try_from(version.to_string().as_str())?,
        };
        let manifest_text = ManifestText::from(ManifestParts {
            name: &source.name,
            version: &version,
            digest: &digest,
            files: &files,
        })
        .0;
        Ok(PlannedRelease {
            name: source.name.clone(),
            kind: source.kind,
            key,
            digest,
            manifest_text,
            files,
            action,
            requirements,
            tested: tested.clone(),
        })
    }

    /// The release that holds the content, if an installed one does, and otherwise the next version.
    fn next(
        &self,
        entries: &[&LedgerEntry<PackageInterface>],
        source: &SourcePackage,
        digest: &ReleaseDigest,
        interface: &PackageInterface,
    ) -> Result<(Version, PlannedAction), SourceInstallError> {
        let holdings = &self.request.holdings;
        let held = entries
            .iter()
            .filter(|entry| {
                entry.removal.is_none()
                    && holdings.present.contains(&entry.release.key)
                    && entry.release.digest == *digest
                    && match source.kind {
                        SourceKind::Library => true,
                        SourceKind::Workflows => is_current_or_future(holdings.state, entry),
                    }
            })
            .map(|entry| entry.release.key.version.as_semver())
            .max();
        if let Some(version) = held {
            return Ok((version.clone(), PlannedAction::Existing));
        }
        if self.request.at.is_some() {
            return Err(SourceInstallError::SourceChanged {
                reason: format!(
                    "the source of the library {} no longer matches any release installed from it, so the release \
                     asked for cannot be restored from this source",
                    source.name
                ),
            });
        }
        let highest = entries
            .iter()
            .max_by_key(|entry| entry.release.key.version.as_semver())
            .map(|entry| RecordedHighest {
                version: entry.release.key.version.as_semver(),
                interface: &entry.interface,
            });
        Ok((
            Version::from(VersionRequest { highest, interface }),
            PlannedAction::Install,
        ))
    }
}

/// The version to restore: the one the ledger records with this content.
fn restore(
    entries: &[&LedgerEntry<PackageInterface>],
    source: &SourcePackage,
    at: &Version,
    digest: &ReleaseDigest,
) -> Result<Version, SourceInstallError> {
    let recorded = entries
        .iter()
        .find(|entry| entry.release.key.version.as_semver() == at)
        .ok_or_else(|| SourceInstallError::NotInstalled {
            package: source.name.to_string(),
            version: at.to_string(),
        })?;
    if recorded.release.digest != *digest {
        return Err(SourceInstallError::SourceChanged {
            reason: format!(
                "the source of {} no longer matches {at}, which is recorded with another digest; a version is never \
                 reused with other content, so install the source as a new release",
                source.name
            ),
        });
    }
    Ok(at.clone())
}

/// Whether the release is current or future for any workflow it holds.
fn is_current_or_future(state: &JobsState, entry: &LedgerEntry<PackageInterface>) -> bool {
    let releases = state
        .package_releases(entry.release.key.name.as_str())
        .cloned()
        .unwrap_or_default();
    entry.interface.workflows.iter().any(|workflow| {
        matches!(
            releases.kind_of(&workflow.slug, entry.release.key.version.as_semver()),
            ReleaseKind::Current | ReleaseKind::Future
        )
    })
}

/// The files an installed release lists, with the package file made to state a requirement for every library, and the
/// package file's text.
fn release_files(
    source: &SourcePackage,
    tested: &BTreeMap<PackageName, Version>,
) -> Result<(String, ReleaseFiles), SourceInstallError> {
    let file_name = match source.kind {
        SourceKind::Workflows => WORKFLOWS_FILE_NAME,
        SourceKind::Library => LIBRARY_FILE_NAME,
    };
    let path = ReleaseFilePath::try_from(file_name)?;
    let text = source
        .files
        .0
        .get(&path)
        .and_then(|bytes| String::from_utf8(bytes.clone()).ok())
        .ok_or_else(|| {
            SourceInstallError::refusing(
                ValidationCode::ArtefactMissing,
                file_name,
                "the package file was not read",
            )
        })?;
    let ReleasePackageFile(rewritten) = ReleasePackageFile::try_from(PackageFileRewrite { text: &text, tested })
        .map_err(|error| SourceInstallError::refusing(ValidationCode::ArtefactInvalid, file_name, error.to_string()))?;
    let mut files = source.files.clone();
    files.0.insert(path, rewritten.clone().into_bytes());
    Ok((rewritten, files))
}

/// The interface a release made of `files` has, read through a manifest of the same files.
fn interface_of(name: &PackageName, files: &ReleaseFiles) -> Result<PackageInterface, SourceInstallError> {
    let digest = StoreRules.digest(files);
    let manifest = ManifestText::from(ManifestParts {
        name,
        version: &Version::new(0, 0, 0),
        digest: &digest,
        files,
    });
    let release =
        PackageRelease::try_from(manifest.0.as_str()).map_err(contextos_packages::PackagesError::InvalidManifest)?;
    Ok(release.interface(files))
}

/// What a release requires of each library, from its package file.
fn requirements_of(kind: SourceKind, package_file: &str) -> Result<Vec<(PackageName, VersionReq)>, SourceInstallError> {
    let invalid = |error: &dyn std::fmt::Display| {
        SourceInstallError::refusing(ValidationCode::ArtefactInvalid, "libraries", error.to_string())
    };
    let requirements = match kind {
        SourceKind::Workflows => WorkflowsFile::try_from(package_file)
            .map_err(|error| invalid(&error))?
            .libraries()
            .clone(),
        SourceKind::Library => LibraryManifest::try_from(package_file)
            .map_err(|error| invalid(&error))?
            .libraries()
            .clone(),
    };
    Ok(requirements
        .entries()
        .iter()
        .map(|entry| (entry.name.clone(), entry.requirement.clone()))
        .collect())
}
