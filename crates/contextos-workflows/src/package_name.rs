//! A package's name: the one form of it that may become part of a path in the host package store.
//!
//! The store keeps a release at `packages/{name}/{version}/`, and a name arrives from an uploaded manifest, a manifest's
//! dependencies, a command line, a URL, and a shared job's folder, so every one of those passes through this type before
//! the store is touched.

use std::borrow::Borrow;
use std::fmt;

use crate::library_name::LibraryName;
use crate::package::PackageError;

/// The longest name a package may have, in characters.
pub const MAX_PACKAGE_NAME_LENGTH: usize = 64;

/// The name the built-in script library is imported under, which no installed package may take.
const RESERVED_NAME: &str = "contextos";

/// A package name: lower-case ASCII letters, digits, and hyphens, starting with a letter or digit, at most
/// [`MAX_PACKAGE_NAME_LENGTH`] characters, never a name Windows reserves for a device, and never `contextos`. So it is
/// one plain folder name on every platform, and never holds a separator, a colon, or a dot.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackageName(String);

impl TryFrom<&str> for PackageName {
    type Error = PackageError;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        check_folder_name(raw).map_err(|reason| PackageError::InvalidPackageName {
            name: raw.to_owned(),
            reason,
        })?;
        Ok(Self(raw.to_owned()))
    }
}

impl From<&LibraryName> for PackageName {
    /// A library and a package follow one naming rule, so a library's name is always a package's.
    fn from(name: &LibraryName) -> Self {
        Self(name.as_str().to_owned())
    }
}

/// Why `raw` is not a name that is one plain folder name on every platform and is not taken by the built-in library, or
/// nothing when it is. The rule a package's name and a vault library's name share.
pub(crate) fn check_folder_name(raw: &str) -> Result<(), &'static str> {
    if raw.is_empty() {
        return Err("it is empty");
    }
    if raw.len() > MAX_PACKAGE_NAME_LENGTH {
        return Err("it is longer than 64 characters");
    }
    if !raw
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err("it holds a character other than a lower-case letter, a digit, or a hyphen");
    }
    if raw.starts_with('-') {
        return Err("it starts with a hyphen");
    }
    if is_device_name(raw) {
        return Err("it is a name Windows reserves for a device");
    }
    if raw == RESERVED_NAME {
        return Err("it is the name of the built-in script library");
    }
    Ok(())
}

impl TryFrom<String> for PackageName {
    type Error = PackageError;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        Self::try_from(raw.as_str())
    }
}

/// Whether `name` is a device name Windows reserves in every folder. A package name holds no dot, so there is no
/// extension to strip first.
pub(crate) fn is_device_name(name: &str) -> bool {
    matches!(name, "con" | "prn" | "aux" | "nul")
        || (name.len() == 4
            && (name.starts_with("com") || name.starts_with("lpt"))
            && matches!(name.as_bytes().get(3), Some(b'1'..=b'9')))
}

impl PackageName {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for PackageName {
    /// A name compares, hashes, and orders as its text does, so a map keyed by names can be looked up by text.
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PackageName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl PartialEq<str> for PackageName {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for PackageName {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

#[cfg(test)]
#[path = "package_name_test.rs"]
mod tests;
