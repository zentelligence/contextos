//! The `libraries` a package declares: one semver requirement for each library it uses, in `workflows.toml` or
//! `library.toml`.
//!
//! A bare version such as `1.4.2` means `^1.4.2`, as semver reads it. The same entry, `[[libraries]]` with a `name` and a
//! `requirement`, is written the same way in a vault's source and in an installed release. In a vault's source an entry
//! may leave its `requirement` out, and a one-off run of source then reads that library from its own source in
//! `registry/libraries/`, so what is tested is what is installed. An entry that states a requirement is taken from the
//! newest installed library release that satisfies it, and a run that finds none is refused before its script starts.
//! The same holds for the libraries a library read from source names in its own `library.toml`.

use std::collections::BTreeSet;

use crate::library_name::LibraryName;
use crate::package::{DependencyRaw, PackageDependency, PackageError};
use crate::package_name::PackageName;

/// The text of a package file kept in a vault's source, `workflows.toml` or `library.toml` under `registry/`. Source is for
/// development and one-off runs, so a library entry there may leave its `requirement` out, which a run reads as the
/// library's own source; an installed release may not.
#[derive(Clone, Copy, Debug)]
pub struct SourceText<'a>(pub &'a str);

/// Where a package file is read from, which decides whether a library entry must state its requirement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageFileOrigin {
    Release,
    Source,
}

/// The libraries a package requires, each named once, in the order declared. In a vault's source an entry may state no
/// requirement: its name is in [`LibraryRequirements::names`] and it has no entry in [`LibraryRequirements::entries`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LibraryRequirements {
    entries: Vec<PackageDependency>,
    names: Vec<LibraryName>,
    unstated: Vec<PackageName>,
}

impl TryFrom<(Vec<DependencyRaw>, PackageFileOrigin)> for LibraryRequirements {
    type Error = PackageError;

    fn try_from((raw, origin): (Vec<DependencyRaw>, PackageFileOrigin)) -> Result<Self, PackageError> {
        let mut seen = BTreeSet::new();
        let mut entries = Vec::with_capacity(raw.len());
        let mut library_names = Vec::with_capacity(raw.len());
        let mut unstated = Vec::new();
        for entry in raw {
            if origin == PackageFileOrigin::Source && entry.requirement.is_none() {
                let name = PackageName::try_from(entry.name)?;
                if !seen.insert(name.clone()) {
                    return Err(PackageError::DuplicateLibrary(name.to_string()));
                }
                library_names.push(LibraryName::from(&name));
                unstated.push(name);
                continue;
            }
            let library = PackageDependency::try_from(entry)?;
            if !seen.insert(library.name.clone()) {
                return Err(PackageError::DuplicateLibrary(library.name.to_string()));
            }
            library_names.push(LibraryName::from(&library.name));
            entries.push(library);
        }
        Ok(Self {
            entries,
            names: library_names,
            unstated,
        })
    }
}

impl LibraryRequirements {
    /// Each stated requirement, in the order declared. Every library of an installed release has one.
    #[must_use]
    pub fn entries(&self) -> &[PackageDependency] {
        &self.entries
    }

    /// The libraries whose requirement a vault's source leaves out, in the order declared. A run of source reads each from
    /// its own source, in `registry/libraries/`, rather than from an installed release.
    #[must_use]
    pub fn unstated(&self) -> &[PackageName] {
        &self.unstated
    }

    /// The names of the libraries, in the order declared, including any whose requirement a vault's source leaves out.
    #[must_use]
    pub fn names(&self) -> &[LibraryName] {
        &self.names
    }
}

#[cfg(test)]
#[path = "library_requirements_test.rs"]
mod tests;
