//! A workflow package's `workflows.toml`: which workflows the package holds, and the modules, templates, and libraries
//! every one of them shares.
//!
//! It names its workflows and says nothing else about them, since each workflow declares itself in
//! `{workflow}/workflow.toml`. Modules, templates, and libraries are declared here once for the package, with paths
//! relative to the package root, so an import name or a template path means exactly one file whichever workflow uses it.

use std::collections::BTreeSet;

use serde::Deserialize;

use crate::library_requirements::{LibraryRequirements, PackageFileOrigin, SourceText};
use crate::package::{DependencyRaw, PackageError};
use crate::script_modules::DeclaredModules;
use crate::workflow_files::DeclaredFiles;
use crate::workflow_name::WorkflowName;

/// What `workflows.toml` declares, checked.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkflowsFile {
    workflows: Vec<String>,
    modules: DeclaredModules,
    templates: DeclaredFiles,
    libraries: LibraryRequirements,
}

/// `workflows.toml` as written. Unknown keys are refused.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkflowsFileRaw {
    workflows: Vec<String>,
    #[serde(default)]
    modules: Vec<String>,
    #[serde(default)]
    templates: Vec<String>,
    #[serde(default)]
    libraries: Vec<DependencyRaw>,
}

impl TryFrom<&str> for WorkflowsFile {
    type Error = PackageError;

    /// The `workflows.toml` of an installed release, in which every library states its requirement.
    fn try_from(text: &str) -> Result<Self, PackageError> {
        Self::try_from((text, PackageFileOrigin::Release))
    }
}

impl TryFrom<SourceText<'_>> for WorkflowsFile {
    type Error = PackageError;

    /// The `workflows.toml` of a vault's source, in which a library may state no requirement.
    fn try_from(source: SourceText<'_>) -> Result<Self, PackageError> {
        Self::try_from((source.0, PackageFileOrigin::Source))
    }
}

impl TryFrom<(&str, PackageFileOrigin)> for WorkflowsFile {
    type Error = PackageError;

    fn try_from((text, origin): (&str, PackageFileOrigin)) -> Result<Self, PackageError> {
        let invalid = |reason: String| PackageError::InvalidWorkflowsFile { reason };
        let raw: WorkflowsFileRaw = toml::from_str(text).map_err(|error| invalid(error.to_string()))?;
        if raw.workflows.is_empty() {
            return Err(invalid(
                "`workflows` is empty: a workflow package holds at least one workflow".to_owned(),
            ));
        }
        let mut named = BTreeSet::new();
        for name in &raw.workflows {
            WorkflowName::try_from(name.as_str()).map_err(|error| invalid(format!("`workflows` {error}")))?;
            if !named.insert(name.as_str()) {
                return Err(invalid(format!("`workflows` names {name:?} twice")));
            }
        }
        let modules = DeclaredModules::try_from(raw.modules).map_err(|error| invalid(error.to_string()))?;
        let templates =
            DeclaredFiles::try_from(raw.templates).map_err(|error| invalid(format!("templates entry {error}")))?;
        let libraries =
            LibraryRequirements::try_from((raw.libraries, origin)).map_err(|error| invalid(error.to_string()))?;
        Ok(Self {
            workflows: raw.workflows,
            modules,
            templates,
            libraries,
        })
    }
}

impl WorkflowsFile {
    /// The names of the package's workflows, in the order listed. Each is the name of the folder it is kept in.
    #[must_use]
    pub fn workflows(&self) -> &[String] {
        &self.workflows
    }

    /// The `.rhai` files, relative to the package root, that any workflow of the package may import.
    #[must_use]
    pub fn modules(&self) -> &DeclaredModules {
        &self.modules
    }

    /// The files and folders, relative to the package root, that any workflow of the package may read.
    #[must_use]
    pub fn templates(&self) -> &DeclaredFiles {
        &self.templates
    }

    /// The libraries the package's files may import from.
    #[must_use]
    pub fn libraries(&self) -> &LibraryRequirements {
        &self.libraries
    }
}

#[cfg(test)]
#[path = "workflows_file_test.rs"]
mod tests;
