//! The vault libraries a workflow reaches, read whole into one snapshot.
//!
//! A vault workflow names libraries in `libraries`, and each library names others in its own; those named without a
//! requirement are read from source, and those named with one come from an installed release, so are not read here. A library is a folder
//! under `registry/libraries/` with a `library.toml` and the modules it declares. [`read_libraries`] finds the libraries
//! by listing that folder and matching names exactly, never by building a path from a name, reads each reached library
//! once, and enforces the limits before it reads a file: at most [`MAX_LIBRARY_PACKAGES`] libraries, [`MAX_LIBRARY_DEPTH`]
//! levels deep, and the file and text limits every snapshot shares. Where the files are kept is a [`LibrarySource`], so
//! the same rules apply to any host.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;

use thiserror::Error;

use crate::import_rules::ModuleOwner;
use crate::library_manifest::LibraryManifest;
use crate::library_name::LibraryName;
use crate::library_resolution::{MAX_LIBRARY_DEPTH, MAX_LIBRARY_PACKAGES, SnapshotBudget};
use crate::package::PackageError;
use crate::package_name::PackageName;
use crate::script_modules::ModuleSource;
use crate::workflow_files::{MAX_WORKFLOW_FILE_BYTES, WorkflowFile, WorkflowFileFailure, WorkflowFilePath};

/// Why a library file could not be read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LibraryReadFailure {
    /// The source answered and the file cannot be read: it is missing, a folder, not text, or too large.
    File(WorkflowFileFailure),
    /// The source could not be asked, so nothing is known about the file. An outage, never a change to the library.
    Unreachable(String),
}

/// A boxed future, so a source can be held as `dyn LibrarySource`.
pub type SourceFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, LibraryReadFailure>> + Send + 'a>>;

/// Where a vault keeps its libraries.
pub trait LibrarySource: Send + Sync {
    /// The names of the folders directly under `registry/libraries/`, as they are on disk.
    fn folders(&self) -> SourceFuture<'_, Vec<String>>;

    /// The size in bytes of the regular file at `path` in the library's folder, from its details and without reading
    /// it.
    fn size<'a>(&'a self, library: &'a LibraryName, path: &'a WorkflowFilePath) -> SourceFuture<'a, u64>;

    /// The whole of the regular file at `path` in the library's folder.
    fn read<'a>(&'a self, library: &'a LibraryName, path: &'a WorkflowFilePath) -> SourceFuture<'a, WorkflowFile>;
}

/// One library as it was read: its manifest and its modules.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LibraryCode {
    name: LibraryName,
    manifest: LibraryManifest,
    modules: Vec<ModuleSource>,
}

impl LibraryCode {
    #[must_use]
    pub fn name(&self) -> &LibraryName {
        &self.name
    }

    #[must_use]
    pub fn manifest(&self) -> &LibraryManifest {
        &self.manifest
    }

    /// Every module, in the order the manifest declares them.
    #[must_use]
    pub fn modules(&self) -> &[ModuleSource] {
        &self.modules
    }
}

/// Every library a workflow reaches, by name.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VaultLibraries(pub(crate) BTreeMap<LibraryName, LibraryCode>);

impl VaultLibraries {
    /// Each library, in name order.
    pub fn iter(&self) -> impl Iterator<Item = &LibraryCode> {
        self.0.values()
    }
}

/// Why the libraries a workflow names could not be read.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum LibraryError {
    /// No folder under `registry/libraries/` has the library's name.
    #[error("library \"{library}\" is not in registry/libraries/{hint}")]
    NotFound { library: LibraryName, hint: String },
    /// Two or more folders differ from the name, or from each other, only in case.
    #[error("library \"{library}\" is ambiguous: registry/libraries/ holds folders that differ only in case ({})", .folders.join(", "))]
    Ambiguous { library: LibraryName, folders: Vec<String> },
    /// `library.toml` does not match its schema.
    #[error("library \"{library}\": {reason}")]
    Manifest { library: LibraryName, reason: String },
    /// A file of the library cannot be read.
    #[error("library \"{library}\" file {path:?} could not be read: {message}")]
    Unreadable {
        library: LibraryName,
        path: String,
        message: String,
    },
    /// Libraries reach each other in a loop.
    #[error("libraries name each other in a loop: {}", .path.iter().map(LibraryName::as_str).collect::<Vec<_>>().join(" -> "))]
    Cycle { path: Vec<LibraryName> },
    /// More libraries are reached than the limit.
    #[error("the workflow reaches more than {limit} libraries; library \"{library}\" would be one more")]
    TooMany { library: LibraryName, limit: usize },
    /// A chain of libraries is deeper than the limit.
    #[error("library \"{library}\" is more than {limit} levels of libraries deep")]
    TooDeep { library: LibraryName, limit: usize },
    /// A file would take the snapshot past a limit.
    #[error("library \"{library}\" file {path:?} would take the workflow's code past its limit of {limit}")]
    OverLimit {
        library: LibraryName,
        path: String,
        limit: &'static str,
    },
    /// The source could not be asked, so nothing is known about the libraries.
    #[error("the libraries could not be read: {0}")]
    Unreachable(String),
}

/// The most libraries one workflow may reach.
const MAX_VAULT_LIBRARIES: usize = MAX_LIBRARY_PACKAGES;

/// Reads the libraries `named` and every library they name without a requirement in turn, in one pass. A library named
/// with a requirement is not read: it comes from an installed release. `budget` holds what the workflow's own
/// files have already used.
///
/// Libraries are taken in name order, so two hosts reading the same vault read the same libraries in the same order.
///
/// # Errors
///
/// Returns the first [`LibraryError`] found: a library that cannot be found or read, then a loop, then a chain that is
/// too deep.
pub async fn read_libraries(
    named: &[LibraryName],
    source: &dyn LibrarySource,
    budget: &mut SnapshotBudget,
) -> Result<VaultLibraries, LibraryError> {
    if named.is_empty() {
        return Ok(VaultLibraries::default());
    }
    let folders = source.folders().await.map_err(|failure| match failure {
        LibraryReadFailure::Unreachable(reason) => LibraryError::Unreachable(reason),
        LibraryReadFailure::File(failure) => LibraryError::Unreachable(failure.to_string()),
    })?;
    let mut loaded = BTreeMap::new();
    let mut pending: BTreeSet<LibraryName> = named.iter().cloned().collect();
    while let Some(name) = pending.pop_first() {
        if loaded.contains_key(&name) {
            continue;
        }
        if loaded.len() >= MAX_VAULT_LIBRARIES {
            return Err(LibraryError::TooMany {
                library: name,
                limit: MAX_VAULT_LIBRARIES,
            });
        }
        let code = load_library(&name, &folders, source, budget).await?;
        // Only a library the manifest names without a requirement is read from source; one with a requirement comes from
        // an installed release, which the caller resolves.
        pending.extend(
            code.manifest()
                .libraries()
                .unstated()
                .iter()
                .map(LibraryName::from)
                .filter(|library| !loaded.contains_key(library)),
        );
        loaded.insert(name, code);
    }
    let roots: BTreeSet<&LibraryName> = named.iter().collect();
    check_no_loop(&roots, &loaded)?;
    check_not_too_deep(&roots, &loaded)?;
    Ok(VaultLibraries(loaded))
}

/// Finds the folder of `name` among `folders`, by exact match, and reads the library in it.
async fn load_library(
    name: &LibraryName,
    folders: &[String],
    source: &dyn LibrarySource,
    budget: &mut SnapshotBudget,
) -> Result<LibraryCode, LibraryError> {
    let mut alike: Vec<&String> = folders
        .iter()
        .filter(|folder| folder.eq_ignore_ascii_case(name.as_str()))
        .collect();
    alike.sort();
    match alike.as_slice() {
        [] => {
            return Err(LibraryError::NotFound {
                library: name.clone(),
                hint: String::new(),
            });
        }
        [only] if only.as_str() == name.as_str() => {}
        [other] => {
            return Err(LibraryError::NotFound {
                library: name.clone(),
                hint: format!("; the folder {other:?} differs only in case and is not taken for it"),
            });
        }
        several => {
            return Err(LibraryError::Ambiguous {
                library: name.clone(),
                folders: several.iter().map(|folder| (*folder).clone()).collect(),
            });
        }
    }
    let manifest_path = WorkflowFilePath::try_from(MANIFEST_FILE).map_err(|error| LibraryError::Manifest {
        library: name.clone(),
        reason: error.to_string(),
    })?;
    let manifest_file = read_file(name, &manifest_path, source, budget).await?;
    let manifest = LibraryManifest::try_from(crate::library_requirements::SourceText(manifest_file.content.as_str()))
        .map_err(|error| LibraryError::Manifest {
        library: name.clone(),
        reason: error.to_string(),
    })?;
    let owner = ModuleOwner::Package(PackageName::from(name));
    let mut modules = Vec::with_capacity(manifest.modules().entries().len());
    for path in manifest.modules().entries() {
        let file = read_file(name, path, source, budget).await?;
        let module = ModuleSource::try_from((&owner, path, Ok(file))).map_err(|error| LibraryError::Unreadable {
            library: name.clone(),
            path: path.to_string(),
            message: error.failure.to_string(),
        })?;
        modules.push(module);
    }
    Ok(LibraryCode {
        name: name.clone(),
        manifest,
        modules,
    })
}

/// The file every library has, which names the rest.
const MANIFEST_FILE: &str = crate::package::LIBRARY_FILE_NAME;

/// Reads one file of a library: its size first, charged against the snapshot's limits before it is read, then its
/// content, with any growth since charged too.
async fn read_file(
    library: &LibraryName,
    path: &WorkflowFilePath,
    source: &dyn LibrarySource,
    budget: &mut SnapshotBudget,
) -> Result<WorkflowFile, LibraryError> {
    let unreadable = |failure: LibraryReadFailure| match failure {
        LibraryReadFailure::Unreachable(reason) => LibraryError::Unreachable(reason),
        LibraryReadFailure::File(failure) => LibraryError::Unreadable {
            library: library.clone(),
            path: path.to_string(),
            message: failure.to_string(),
        },
    };
    let over = |error: PackageError| match error {
        PackageError::SnapshotOverLimit { path, limit } => LibraryError::OverLimit {
            library: library.clone(),
            path,
            limit,
        },
        other => LibraryError::Unreadable {
            library: library.clone(),
            path: path.to_string(),
            message: other.to_string(),
        },
    };
    let size = source.size(library, path).await.map_err(unreadable)?;
    if size > MAX_WORKFLOW_FILE_BYTES {
        return Err(LibraryError::Unreadable {
            library: library.clone(),
            path: path.to_string(),
            message: format!("{path} is larger than the {MAX_WORKFLOW_FILE_BYTES} byte limit"),
        });
    }
    budget.charge(path.as_str(), size).map_err(over)?;
    let file = source.read(library, path).await.map_err(unreadable)?;
    let read = u64::try_from(file.content.len()).unwrap_or(u64::MAX);
    // The limit was checked from the details, and the file can have grown since: the bytes that came back are held to it
    // too.
    if read > MAX_WORKFLOW_FILE_BYTES {
        return Err(LibraryError::Unreadable {
            library: library.clone(),
            path: path.to_string(),
            message: format!("{path} is larger than the {MAX_WORKFLOW_FILE_BYTES} byte limit"),
        });
    }
    if read > size {
        budget.charge_growth(path.as_str(), read - size).map_err(over)?;
    }
    Ok(file)
}

/// Fails with the first loop of libraries naming each other, found depth first from `roots` in name order.
fn check_no_loop(
    roots: &BTreeSet<&LibraryName>,
    loaded: &BTreeMap<LibraryName, LibraryCode>,
) -> Result<(), LibraryError> {
    fn visit<'a>(
        name: &'a LibraryName,
        loaded: &'a BTreeMap<LibraryName, LibraryCode>,
        path: &mut Vec<&'a LibraryName>,
        finished: &mut BTreeSet<&'a LibraryName>,
    ) -> Result<(), LibraryError> {
        if path.contains(&name) {
            let mut looped: Vec<LibraryName> = path.iter().map(|library| (*library).clone()).collect();
            looped.push(name.clone());
            let start = looped.iter().position(|library| library == name).unwrap_or(0);
            return Err(LibraryError::Cycle {
                path: looped.split_off(start),
            });
        }
        if finished.contains(name) {
            return Ok(());
        }
        let Some(code) = loaded.get(name) else {
            return Ok(());
        };
        path.push(name);
        for next in code.manifest().libraries().names() {
            visit(next, loaded, path, finished)?;
        }
        path.pop();
        finished.insert(name);
        Ok(())
    }
    let mut finished = BTreeSet::new();
    for root in roots {
        visit(root, loaded, &mut Vec::new(), &mut finished)?;
    }
    Ok(())
}

/// Fails with the first library past [`MAX_LIBRARY_DEPTH`] levels, a library the workflow names being the first level.
fn check_not_too_deep(
    roots: &BTreeSet<&LibraryName>,
    loaded: &BTreeMap<LibraryName, LibraryCode>,
) -> Result<(), LibraryError> {
    fn visit<'a>(
        name: &'a LibraryName,
        level: usize,
        loaded: &'a BTreeMap<LibraryName, LibraryCode>,
        deepest: &mut BTreeMap<&'a LibraryName, usize>,
    ) -> Result<(), LibraryError> {
        if level > MAX_LIBRARY_DEPTH {
            return Err(LibraryError::TooDeep {
                library: name.clone(),
                limit: MAX_LIBRARY_DEPTH,
            });
        }
        if deepest.get(name).is_some_and(|seen| *seen >= level) {
            return Ok(());
        }
        deepest.insert(name, level);
        if let Some(code) = loaded.get(name) {
            for next in code.manifest().libraries().names() {
                visit(next, level + 1, loaded, deepest)?;
            }
        }
        Ok(())
    }
    let mut deepest = BTreeMap::new();
    for root in roots {
        visit(root, 1, loaded, &mut deepest)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "vault_libraries_test.rs"]
mod tests;
