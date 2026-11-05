//! A vault library's name: the folder it is kept in under `registry/libraries/`.

use std::borrow::Borrow;
use std::fmt;

use crate::package::PackageError;
use crate::package_name::PackageName;

/// A vault library's name, which is its folder's name. It follows the rules of a package name (lower-case ASCII
/// letters, digits, and hyphens, starting with a letter or digit, at most 64 characters, never a name Windows reserves
/// for a device, and never `contextos`), and is a type of its own so a library's errors and imports are never a
/// package's.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LibraryName(String);

impl TryFrom<&str> for LibraryName {
    type Error = PackageError;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        crate::package_name::check_folder_name(raw).map_err(|reason| PackageError::InvalidLibraryName {
            name: raw.to_owned(),
            reason,
        })?;
        Ok(Self(raw.to_owned()))
    }
}

impl TryFrom<String> for LibraryName {
    type Error = PackageError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::try_from(raw.as_str())
    }
}

impl From<&PackageName> for LibraryName {
    /// A library and a package follow one naming rule, so a package's name is always a library's.
    fn from(name: &PackageName) -> Self {
        Self(name.as_str().to_owned())
    }
}

impl LibraryName {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for LibraryName {
    /// A name compares, hashes, and orders as its text does, so a map keyed by names can be looked up by text.
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LibraryName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(test)]
#[path = "library_name_test.rs"]
mod tests;
