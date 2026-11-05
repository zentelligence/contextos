//! A vault's source package, read whole through the primary server in one snapshot.
//!
//! A workflow package is `registry/workflows/{package}/`: its `workflows.toml`, each listed workflow's `workflow.toml`
//! and entry script, and the modules and templates the package declares. A library is `registry/libraries/{library}/`:
//! its `library.toml` and modules. A library the source names without a requirement is read from its own source folder,
//! and so on for the libraries it names, so installing the package can install each as a release. Every file counts
//! against the limits a run's snapshot keeps.

use std::collections::BTreeMap;

use contextos_packages::{ReleaseFilePath, ReleaseFiles};
use contextos_workflows::package::{LIBRARY_FILE_NAME, WORKFLOWS_FILE_NAME};
use contextos_workflows::workflow_definition::WorkflowText;
use contextos_workflows::{
    LibraryManifest, LibraryRequirements, MAX_LIBRARY_PACKAGES, MAX_SNAPSHOT_FILES, PackageName, SnapshotBudget,
    SourceText, ValidationCode, WorkflowDefinition, WorkflowFailureKind, WorkflowFileFailure, WorkflowFilePath,
    WorkflowsFile,
};
use semver::VersionReq;

use crate::mcp_client::McpClient;
use crate::source_install_error::SourceInstallError;
use crate::vault_workflow_files::{list_files, read_file_or_outage};

/// Which kind of package a source folder is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceKind {
    /// `registry/workflows/{package}/`.
    Workflows,
    /// `registry/libraries/{library}/`.
    Library,
}

/// The package to read.
#[derive(Clone, Debug)]
pub struct SourceTarget {
    pub kind: SourceKind,
    pub name: PackageName,
}

/// A library a source package names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceLibrary {
    pub name: PackageName,
    /// The requirement the source states, or none when the library is read from source.
    pub requirement: Option<VersionReq>,
}

/// One source package: every file an installed release of it lists, the package file as it stands in source.
#[derive(Clone, Debug)]
pub struct SourcePackage {
    pub name: PackageName,
    pub kind: SourceKind,
    pub files: ReleaseFiles,
    pub libraries: Vec<SourceLibrary>,
}

/// A source package and each source library it reaches.
#[derive(Clone, Debug)]
pub struct SourceSnapshot {
    pub root: SourcePackage,
    pub libraries: BTreeMap<PackageName, SourcePackage>,
}

/// Reads the source `target` names, and each library it reaches that states no requirement.
///
/// # Errors
///
/// [`SourceInstallError::Invalid`] for a file that is missing, is not text, is over a limit, or breaks its schema, and
/// [`SourceInstallError::Unreachable`] when the server could not be asked.
pub async fn read_source(
    client: &McpClient,
    vault: &str,
    target: &SourceTarget,
) -> Result<SourceSnapshot, SourceInstallError> {
    let mut reader = Reader {
        client,
        vault,
        budget: SnapshotBudget::default(),
        libraries: BTreeMap::new(),
        reading: Vec::new(),
    };
    let root = Box::pin(reader.package(target)).await?;
    Ok(SourceSnapshot {
        root,
        libraries: reader.libraries,
    })
}

/// A snapshot being read: the limits spent so far, the libraries read, and the chain of libraries being read.
struct Reader<'a> {
    client: &'a McpClient,
    vault: &'a str,
    budget: SnapshotBudget,
    libraries: BTreeMap<PackageName, SourcePackage>,
    reading: Vec<PackageName>,
}

/// A package folder being read: where it is, and how a file of it is named in a refusal.
struct Folder<'a> {
    root: String,
    /// The library that a refusal names with its files, or none for the package that was asked for.
    library: Option<&'a PackageName>,
}

impl Folder<'_> {
    fn artefact(&self, path: &str) -> String {
        self.library
            .map_or_else(|| path.to_owned(), |library| format!("{library}:{path}"))
    }
}

impl Reader<'_> {
    async fn package(&mut self, target: &SourceTarget) -> Result<SourcePackage, SourceInstallError> {
        let folder_name = match target.kind {
            SourceKind::Workflows => "workflows",
            SourceKind::Library => "libraries",
        };
        let nested = !self.reading.is_empty();
        let folder = Folder {
            root: format!("{}://registry/{folder_name}/{}", self.vault, target.name),
            library: if nested { Some(&target.name) } else { None },
        };
        let mut files = BTreeMap::new();
        let libraries = match target.kind {
            SourceKind::Workflows => self.workflows(&folder, &mut files).await?,
            SourceKind::Library => self.library(&folder, &mut files).await?,
        };
        let package = SourcePackage {
            name: target.name.clone(),
            kind: target.kind,
            files: ReleaseFiles(files),
            libraries,
        };
        self.reading.push(target.name.clone());
        let sources = self.source_libraries(&package).await;
        self.reading.pop();
        sources?;
        Ok(package)
    }

    /// Reads each library `package` names without a requirement from its own source folder.
    async fn source_libraries(&mut self, package: &SourcePackage) -> Result<(), SourceInstallError> {
        for library in package.libraries.iter().filter(|library| library.requirement.is_none()) {
            if self.reading.contains(&library.name) {
                let chain: Vec<String> = self
                    .reading
                    .iter()
                    .skip_while(|name| **name != library.name)
                    .chain(std::iter::once(&library.name))
                    .map(ToString::to_string)
                    .collect();
                return Err(SourceInstallError::refusing(
                    ValidationCode::ArtefactInvalid,
                    &format!("{}:library.toml", library.name),
                    format!(
                        "libraries that name each other cannot be installed: {}",
                        chain.join(" -> ")
                    ),
                ));
            }
            if self.libraries.contains_key(&library.name) {
                continue;
            }
            if self.libraries.len() >= MAX_LIBRARY_PACKAGES {
                return Err(SourceInstallError::refusing(
                    ValidationCode::ArtefactInvalid,
                    &format!("{}:library.toml", library.name),
                    format!("the source reaches more than {MAX_LIBRARY_PACKAGES} libraries"),
                ));
            }
            let read = Box::pin(self.package(&SourceTarget {
                kind: SourceKind::Library,
                name: library.name.clone(),
            }))
            .await?;
            self.libraries.insert(library.name.clone(), read);
        }
        Ok(())
    }

    async fn workflows(
        &mut self,
        folder: &Folder<'_>,
        files: &mut BTreeMap<ReleaseFilePath, Vec<u8>>,
    ) -> Result<Vec<SourceLibrary>, SourceInstallError> {
        let text = self.text(folder, WORKFLOWS_FILE_NAME).await?;
        let declared =
            WorkflowsFile::try_from(SourceText(&text)).map_err(|error| invalid(folder, WORKFLOWS_FILE_NAME, &error))?;
        Self::keep(folder, files, WORKFLOWS_FILE_NAME, text)?;
        for workflow in declared.workflows() {
            let file = format!("{workflow}/workflow.toml");
            let text = self.text(folder, &file).await?;
            let definition = WorkflowDefinition::try_from(WorkflowText {
                text: &text,
                folder: workflow,
            })
            .map_err(|error| invalid(folder, &file, &error))?;
            Self::keep(folder, files, &file, text)?;
            let entry = format!("{workflow}/{}", definition.entry);
            let script = self.text(folder, &entry).await?;
            Self::keep(folder, files, &entry, script)?;
        }
        for module in declared.modules().entries() {
            let text = self.text(folder, module.as_str()).await?;
            Self::keep(folder, files, module.as_str(), text)?;
        }
        for template in declared.templates().entries() {
            self.tree(folder, files, template).await?;
        }
        Ok(libraries_of(declared.libraries()))
    }

    async fn library(
        &mut self,
        folder: &Folder<'_>,
        files: &mut BTreeMap<ReleaseFilePath, Vec<u8>>,
    ) -> Result<Vec<SourceLibrary>, SourceInstallError> {
        let text = self.text(folder, LIBRARY_FILE_NAME).await?;
        let declared =
            LibraryManifest::try_from(SourceText(&text)).map_err(|error| invalid(folder, LIBRARY_FILE_NAME, &error))?;
        Self::keep(folder, files, LIBRARY_FILE_NAME, text)?;
        for module in declared.modules().entries() {
            let text = self.text(folder, module.as_str()).await?;
            Self::keep(folder, files, module.as_str(), text)?;
        }
        Ok(libraries_of(declared.libraries()))
    }

    /// The text of `relative` beneath `folder`, charged against the snapshot's limits.
    async fn text(&mut self, folder: &Folder<'_>, relative: &str) -> Result<String, SourceInstallError> {
        let path = WorkflowFilePath::try_from(relative).map_err(|error| {
            SourceInstallError::refusing(
                ValidationCode::ArtefactInvalid,
                &folder.artefact(relative),
                error.to_string(),
            )
        })?;
        match read_file_or_outage(self.client, &folder.root, &path).await? {
            Ok(file) => {
                let size = u64::try_from(file.content.len()).unwrap_or(u64::MAX);
                self.budget
                    .charge(relative, size)
                    .map_err(|error| invalid(folder, relative, &error))?;
                Ok(file.content)
            }
            Err(failure) => Err(unreadable(folder, relative, &failure)),
        }
    }

    /// Every file at or beneath `entry`, which is a file or a folder.
    async fn tree(
        &mut self,
        folder: &Folder<'_>,
        files: &mut BTreeMap<ReleaseFilePath, Vec<u8>>,
        entry: &WorkflowFilePath,
    ) -> Result<(), SourceInstallError> {
        let listed = list_files(self.client, &folder.root, entry, MAX_SNAPSHOT_FILES)
            .await
            .map_err(|failure| unreadable(folder, entry.as_str(), &failure))?;
        for path in listed {
            let text = self.text(folder, path.as_str()).await?;
            Self::keep(folder, files, path.as_str(), text)?;
        }
        Ok(())
    }

    fn keep(
        folder: &Folder<'_>,
        files: &mut BTreeMap<ReleaseFilePath, Vec<u8>>,
        relative: &str,
        text: String,
    ) -> Result<(), SourceInstallError> {
        let path = ReleaseFilePath::try_from(relative).map_err(|error| {
            SourceInstallError::refusing(
                ValidationCode::ArtefactInvalid,
                &folder.artefact(relative),
                error.to_string(),
            )
        })?;
        files.insert(path, text.into_bytes());
        Ok(())
    }
}

fn libraries_of(requirements: &LibraryRequirements) -> Vec<SourceLibrary> {
    requirements
        .names()
        .iter()
        .filter_map(|name| {
            let package = PackageName::try_from(name.as_str()).ok()?;
            let requirement = requirements
                .entries()
                .iter()
                .find(|entry| entry.name == package)
                .map(|entry| entry.requirement.clone());
            Some(SourceLibrary {
                name: package,
                requirement,
            })
        })
        .collect()
}

fn invalid(folder: &Folder<'_>, relative: &str, error: &dyn std::fmt::Display) -> SourceInstallError {
    SourceInstallError::refusing(
        ValidationCode::ArtefactInvalid,
        &folder.artefact(relative),
        error.to_string(),
    )
}

fn unreadable(folder: &Folder<'_>, relative: &str, failure: &WorkflowFileFailure) -> SourceInstallError {
    let code = match failure.kind {
        WorkflowFailureKind::FileNotFound => ValidationCode::ArtefactMissing,
        _ => ValidationCode::ArtefactInvalid,
    };
    SourceInstallError::refusing(code, &folder.artefact(relative), failure.message.clone())
}

#[cfg(test)]
#[path = "vault_source_read_test.rs"]
mod tests;
