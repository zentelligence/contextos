//! The parts of a release made from a vault's source that follow from its content alone: the package file an installed
//! release lists, the `manifest.toml` that describes it, and the version it takes.
//!
//! Source is what is edited and tested, so an installed release is made to say what it was tested on. The installed
//! package file names a requirement for every library, where source may leave one out.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use contextos_packages::{ReleaseDigest, ReleaseFiles};
use contextos_workflows::{PackageError, PackageName};
use semver::Version;
use sha2::{Digest, Sha256};

use contextos_packages::VersionStep;

use crate::package_release_manifest::PackageInterface;

/// A source package file and the versions of the libraries it was tested on.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PackageFileRewrite<'a> {
    /// The text of `workflows.toml` or `library.toml` as it stands in source.
    pub text: &'a str,
    /// The version of each library the package was tested on, for every library whose entry states no requirement.
    pub tested: &'a BTreeMap<PackageName, Version>,
}

/// The text of the package file an installed release lists: every library entry states a requirement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReleasePackageFile(pub String);

impl TryFrom<PackageFileRewrite<'_>> for ReleasePackageFile {
    type Error = PackageError;

    fn try_from(source: PackageFileRewrite<'_>) -> Result<Self, PackageError> {
        let mut table: toml::Table =
            toml::from_str(source.text).map_err(|error| PackageError::Malformed(error.to_string()))?;
        if let Some(libraries) = table.get_mut("libraries").and_then(toml::Value::as_array_mut) {
            for entry in libraries.iter_mut().filter_map(toml::Value::as_table_mut) {
                if entry.contains_key("requirement") {
                    continue;
                }
                let name = entry
                    .get("name")
                    .and_then(toml::Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_default();
                let version = PackageName::try_from(name.as_str())
                    .ok()
                    .and_then(|package| source.tested.get(&package))
                    .ok_or_else(|| {
                        PackageError::Malformed(format!(
                            "library {name:?} states no requirement and no version it was tested on is known"
                        ))
                    })?;
                entry.insert("requirement".to_owned(), toml::Value::String(format!("^{version}")));
            }
        }
        toml::to_string(&table)
            .map(Self)
            .map_err(|error| PackageError::Malformed(error.to_string()))
    }
}

/// What a release's `manifest.toml` says: the release and the files it lists.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ManifestParts<'a> {
    pub name: &'a PackageName,
    pub version: &'a Version,
    pub digest: &'a ReleaseDigest,
    pub files: &'a ReleaseFiles,
}

/// The text of a release's `manifest.toml`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ManifestText(pub String);

impl From<ManifestParts<'_>> for ManifestText {
    fn from(parts: ManifestParts<'_>) -> Self {
        let mut text = format!(
            "name = {:?}\nversion = {:?}\ndigest = {:?}\n",
            parts.name.as_str(),
            parts.version.to_string(),
            parts.digest.as_str()
        );
        for (path, bytes) in &parts.files.0 {
            let hash = Sha256::digest(bytes).iter().fold(String::new(), |mut hex, byte| {
                let _ = write!(hex, "{byte:02x}");
                hex
            });
            let _ = write!(text, "\n[[files]]\npath = {:?}\nhash = {hash:?}\n", path.to_string());
        }
        Self(text)
    }
}

/// The highest version the ledger records for a package and the interface that release had.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RecordedHighest<'a> {
    pub version: &'a Version,
    pub interface: &'a PackageInterface,
}

/// The version a release takes: the first release is 1.0.0, and a later one steps from the highest recorded by what its
/// interface changed.
#[derive(Clone, Copy, Debug)]
pub(crate) struct VersionRequest<'a> {
    pub highest: Option<RecordedHighest<'a>>,
    pub interface: &'a PackageInterface,
}

impl From<VersionRequest<'_>> for Version {
    fn from(request: VersionRequest<'_>) -> Self {
        let Some(highest) = request.highest else {
            return Self::new(1, 0, 0);
        };
        let from = highest.version;
        match request.interface.required_step(highest.interface) {
            VersionStep::Major => Self::new(from.major + 1, 0, 0),
            VersionStep::Minor => Self::new(from.major, from.minor + 1, 0),
            VersionStep::Patch => Self::new(from.major, from.minor, from.patch + 1),
        }
    }
}

#[cfg(test)]
#[path = "source_release_test.rs"]
mod tests;
