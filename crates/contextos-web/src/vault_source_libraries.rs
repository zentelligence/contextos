//! The libraries a one-off run of a workflow's source uses.
//!
//! A library the source names without a requirement is read from its own source in `registry/libraries/`, so the code
//! that is tested is the code that installing the workflow installs. A library the source names with a requirement comes
//! from the newest installed library release that satisfies it, and a run that finds none is refused before its script
//! starts. The same rule applies to the libraries a library read from source names in its own `library.toml`, while a
//! library taken from an installed release resolves its own dependencies over installed releases, as a release does.
//! Nothing is locked, recorded, or installed.

use std::collections::{BTreeMap, BTreeSet};

use contextos_workflows::{
    LibraryError, LibraryName, LibraryRequirements, PackageCode, PackageDependency, PackageName, SnapshotBudget,
    read_libraries,
};

use crate::mcp_client::McpClient;
use crate::run_records::RunLibrary;
use crate::vault_library_source::VaultLibrarySource;
use crate::vault_release_snapshot::{InstalledLibrariesRequest, read_installed_libraries};

/// What a run of source reads its libraries by.
pub(crate) struct SourceLibrariesRequest<'a> {
    pub client: &'a McpClient,
    pub vault: &'a str,
    /// The workflow's package: the source folder's name.
    pub package: &'a PackageName,
    /// What the source's `workflows.toml` states of its libraries.
    pub requirements: &'a LibraryRequirements,
}

/// The libraries a run of source uses.
pub(crate) struct SourceLibraries {
    /// Each library, in name order, as a release or as source.
    pub libraries: Vec<RunLibrary>,
    /// The code of each, keyed by name.
    pub code: BTreeMap<PackageName, PackageCode>,
}

/// Why the libraries of a run of source could not be had.
pub(crate) enum SourceLibrariesFailure {
    /// The server could not be asked, so nothing is known about the libraries: an outage, never a defect in the source.
    Unreachable,
    /// A library read from source is missing, unreadable, past a limit, or part of a loop.
    Unreadable(String),
    /// No installed library release serves what the source states, or the libraries cannot be told apart.
    Unavailable(String),
}

/// Reads the libraries `request.requirements` names: those without a requirement, and the libraries they name without
/// one, from `registry/libraries/`, and those with a requirement from the installed library releases.
///
/// # Errors
///
/// [`SourceLibrariesFailure::Unreachable`] when the server could not be asked, [`SourceLibrariesFailure::Unreadable`]
/// for a library read from source that cannot be used, and [`SourceLibrariesFailure::Unavailable`] when a stated
/// requirement is satisfied by no installed release or a library would be both read from source and taken from a
/// release.
pub(crate) async fn read_source_libraries(
    request: SourceLibrariesRequest<'_>,
) -> Result<SourceLibraries, SourceLibrariesFailure> {
    let SourceLibrariesRequest {
        client,
        vault,
        package,
        requirements,
    } = request;
    if requirements.names().is_empty() {
        return Ok(SourceLibraries {
            libraries: Vec::new(),
            code: BTreeMap::new(),
        });
    }
    let mut budget = SnapshotBudget::default();
    let named: Vec<LibraryName> = requirements.unstated().iter().map(LibraryName::from).collect();
    let source = VaultLibrarySource::new(client, vault);
    let read = read_libraries(&named, &source, &mut budget)
        .await
        .map_err(|error| match error {
            LibraryError::Unreachable(_) => SourceLibrariesFailure::Unreachable,
            other => SourceLibrariesFailure::Unreadable(other.to_string()),
        })?;
    let mut stated: Vec<PackageDependency> = requirements.entries().to_vec();
    for library in read.iter() {
        stated.extend(library.manifest().libraries().entries().iter().cloned());
    }
    let mut code = BTreeMap::new();
    let mut libraries = BTreeMap::new();
    for library in read.iter() {
        let converted = PackageCode::from(library);
        libraries.insert(
            converted.name().clone(),
            RunLibrary {
                name: library.name().to_string(),
                release: None,
            },
        );
        code.insert(converted.name().clone(), converted);
    }
    if !stated.is_empty() {
        let engine = client
            .vault_packages(vault)
            .await
            .map_err(|error| SourceLibrariesFailure::Unavailable(error.to_string()))?;
        let installed = read_installed_libraries(
            &engine,
            InstalledLibrariesRequest {
                package,
                requirements: stated,
                budget: &mut budget,
            },
        )
        .await
        .map_err(|failure| SourceLibrariesFailure::Unavailable(failure.to_string()))?;
        let both: BTreeSet<String> = installed
            .code
            .keys()
            .filter(|name| code.contains_key(*name))
            .map(ToString::to_string)
            .collect();
        if !both.is_empty() {
            return Err(SourceLibrariesFailure::Unavailable(format!(
                "{} would be read from source by one package and taken from an installed release by another, which a \
                 run cannot do; state a requirement for it in every package that names it, or in none",
                both.into_iter().collect::<Vec<_>>().join(", ")
            )));
        }
        for manifest in &installed.manifests {
            libraries.insert(manifest.identity.name.clone(), RunLibrary::from(manifest));
        }
        code.extend(installed.code);
    }
    Ok(SourceLibraries {
        libraries: libraries.into_values().collect(),
        code,
    })
}
