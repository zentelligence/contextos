//! Who owns a script file, and what each owner's files may import.
//!
//! Every file a run loads belongs to one owner: the workflow itself, when its code is in a vault, or a package. A file
//! is known by its owner and its path, so two owners that each have a module of the same name never meet. An import
//! names a file in one of three ways:
//!
//! - `import "lib/dates"` is a module of the importing file's own owner, by its path without `.rhai`;
//! - `import "contextos:text"` is the built-in library, which the server provides; and
//! - `import "acme-text:strings"` is an exported module of a package the importing file's owner names directly in its
//!   `libraries`.
//!
//! Nothing else resolves: not a module of a package that is only a dependency of a dependency, not a module a package
//! keeps private, and not a package's own module spelled with its own name. So a library may change its own
//! dependencies without breaking the packages that use it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::package::PackageError;
use crate::package_contents::PackageKind;
use crate::package_name::PackageName;
use crate::script_library::BUILT_IN_MODULES;
use crate::workflow_files::WorkflowFilePath;

/// Who a script file belongs to.
#[derive(Clone, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ModuleOwner {
    /// A workflow whose code is kept in a vault, beside its manifest.
    #[default]
    Workflow,
    /// An installed package.
    Package(PackageName),
}

impl ModuleOwner {
    /// A file of this owner as a failure reports it: `package:path` for a package's file, and the path alone for a
    /// vault workflow's.
    #[must_use]
    pub fn artefact_for(&self, path: &WorkflowFilePath) -> String {
        match self {
            Self::Workflow => path.to_string(),
            Self::Package(package) => format!("{package}:{path}"),
        }
    }
}

/// A module of one owner, by its import name: its path relative to the owner's root, without `.rhai`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ModuleKey {
    pub owner: ModuleOwner,
    pub name: String,
}

impl fmt::Display for ModuleKey {
    /// `package:name` for a package's module, and the name alone for a vault workflow's, as it is imported.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.owner {
            ModuleOwner::Workflow => formatter.write_str(&self.name),
            ModuleOwner::Package(package) => write!(formatter, "{package}:{}", self.name),
        }
    }
}

/// What an import names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ImportTarget {
    /// A module of the built-in library.
    BuiltIn,
    /// A module an owner ships, which may or may not be among the files that were loaded.
    Module(ModuleKey),
    /// Nothing an import may name: not a built-in module, and not a form of package name and module.
    Nothing,
}

/// What each owner may import.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ImportRules {
    /// The packages each owner names directly in its `libraries`.
    dependencies: BTreeMap<ModuleOwner, BTreeSet<PackageName>>,
    /// The modules each package exports, by import name.
    exports: BTreeMap<PackageName, BTreeSet<String>>,
}

impl ImportRules {
    /// Records the package `name`: what it exports, which is nothing for a workflow package, and the libraries it names.
    pub(crate) fn add_package(&mut self, name: &PackageName, kind: &PackageKind) {
        self.dependencies.insert(
            ModuleOwner::Package(name.clone()),
            kind.libraries()
                .entries()
                .iter()
                .map(|library| library.name.clone())
                .chain(kind.libraries().unstated().iter().cloned())
                .collect(),
        );
        self.exports.insert(
            name.clone(),
            kind.exports()
                .map(|exports| {
                    exports
                        .entries()
                        .iter()
                        .map(|export| import_name_of(export.as_str()))
                        .collect()
                })
                .unwrap_or_default(),
        );
    }

    /// What `import "{name}"` in a file of `importer` names.
    ///
    /// # Errors
    ///
    /// Returns [`PackageError::ImportOfOwnPackage`] for a package's own module spelled with its own name,
    /// [`PackageError::ImportNotDependency`] for a package the importer's owner does not name directly, and
    /// [`PackageError::ImportNotExported`] for a module its package keeps private.
    pub(crate) fn target(&self, importer: &ModuleOwner, name: &str) -> Result<ImportTarget, PackageError> {
        let Some((package, module)) = name.split_once(':') else {
            return Ok(ImportTarget::Module(ModuleKey {
                owner: importer.clone(),
                name: name.to_owned(),
            }));
        };
        if package == "contextos" {
            return Ok(if BUILT_IN_MODULES.contains(&name) {
                ImportTarget::BuiltIn
            } else {
                ImportTarget::Nothing
            });
        }
        let Ok(package) = PackageName::try_from(package) else {
            return Ok(ImportTarget::Nothing);
        };
        if module.is_empty() || module.contains(':') {
            return Ok(ImportTarget::Nothing);
        }
        if *importer == ModuleOwner::Package(package.clone()) {
            return Err(PackageError::ImportOfOwnPackage { package });
        }
        let named_directly = self
            .dependencies
            .get(importer)
            .is_some_and(|dependencies| dependencies.contains(&package));
        if !named_directly {
            return Err(PackageError::ImportNotDependency { package });
        }
        let exported = self
            .exports
            .get(&package)
            .is_some_and(|exports| exports.contains(module));
        if !exported {
            return Err(PackageError::ImportNotExported {
                package,
                module: module.to_owned(),
            });
        }
        Ok(ImportTarget::Module(ModuleKey {
            owner: ModuleOwner::Package(package),
            name: module.to_owned(),
        }))
    }
}

/// A module file's import name: its path without `.rhai`.
fn import_name_of(path: &str) -> String {
    path.strip_suffix(".rhai").unwrap_or(path).to_owned()
}

#[cfg(test)]
#[path = "import_rules_test.rs"]
mod tests;
