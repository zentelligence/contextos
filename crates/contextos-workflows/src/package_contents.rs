//! What a package release holds: its `manifest.toml` and the one package file that says what kind of package it is.

use crate::library_manifest::LibraryManifest;
use crate::library_requirements::LibraryRequirements;
use crate::package::{PackageError, PackageManifest};
use crate::script_modules::DeclaredModules;
use crate::workflow_files::DeclaredFiles;
use crate::workflows_file::WorkflowsFile;

/// The package file a release lists, parsed: `workflows.toml` for a workflow package and `library.toml` for a library.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackageKind {
    Workflows(WorkflowsFile),
    Library(LibraryManifest),
}

impl PackageKind {
    /// The modules the package ships, relative to its root.
    #[must_use]
    pub fn modules(&self) -> &DeclaredModules {
        match self {
            Self::Workflows(file) => file.modules(),
            Self::Library(file) => file.modules(),
        }
    }

    /// The modules other packages may import: none for a workflow package, since only a library exports.
    #[must_use]
    pub fn exports(&self) -> Option<&DeclaredModules> {
        match self {
            Self::Workflows(_) => None,
            Self::Library(file) => Some(file.exports()),
        }
    }

    /// The files and folders workflows may read: none for a library, which has no workflow.
    #[must_use]
    pub fn templates(&self) -> Option<&DeclaredFiles> {
        match self {
            Self::Workflows(file) => Some(file.templates()),
            Self::Library(_) => None,
        }
    }

    /// The libraries the package requires.
    #[must_use]
    pub fn libraries(&self) -> &LibraryRequirements {
        match self {
            Self::Workflows(file) => file.libraries(),
            Self::Library(file) => file.libraries(),
        }
    }

    /// The names of the package's workflows: none for a library.
    #[must_use]
    pub fn workflows(&self) -> &[String] {
        match self {
            Self::Workflows(file) => file.workflows(),
            Self::Library(_) => &[],
        }
    }
}

/// A manifest and the text of the package file it lists.
#[derive(Clone, Copy, Debug)]
pub struct PackageFileText<'a> {
    pub manifest: &'a PackageManifest,
    pub text: &'a str,
}

impl TryFrom<PackageFileText<'_>> for PackageKind {
    type Error = PackageError;

    fn try_from(source: PackageFileText<'_>) -> Result<Self, PackageError> {
        if source.manifest.is_library() {
            LibraryManifest::try_from(source.text).map(Self::Library)
        } else {
            WorkflowsFile::try_from(source.text).map(Self::Workflows)
        }
    }
}
