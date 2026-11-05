use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use contextos_core::ContentHash;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{ManifestRejection, NameRejection, ProjectionError, ProjectionTarget, Slug, TargetRootRejection};

/// The manifest document version this build reads and writes.
const VERSION: u32 = 1;

/// Identifies the configuration that projected an entry, so one configuration never claims another's entries. The
/// caller supplies it; this crate only validates and stores it.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ConfigurationId(String);

/// The longest configuration identifier, in bytes.
const MAX_CONFIGURATION_BYTES: usize = 128;

impl TryFrom<&str> for ConfigurationId {
    type Error = ProjectionError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let reason = if value.is_empty() {
            Some(NameRejection::Empty)
        } else if value.len() > MAX_CONFIGURATION_BYTES {
            Some(NameRejection::TooLong {
                maximum: MAX_CONFIGURATION_BYTES,
            })
        } else if value.chars().any(char::is_control) {
            Some(NameRejection::ControlCharacter)
        } else {
            None
        };
        match reason {
            Some(reason) => Err(ProjectionError::InvalidConfigurationId { reason }),
            None => Ok(Self(value.to_owned())),
        }
    }
}

impl<'a> From<&'a ConfigurationId> for &'a str {
    fn from(value: &'a ConfigurationId) -> Self {
        &value.0
    }
}

/// One projected entry the server owns.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManifestEntry {
    source_vault: String,
    source_path: String,
    source_hash: ContentHash,
    projected_hash: ContentHash,
    configuration: ConfigurationId,
}

impl ManifestEntry {
    /// Records an entry projected from `source_path` in the vault named `source_vault`.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::ManifestMalformed`] when the vault name or path is empty, since such an entry could
    /// never be read back.
    pub fn new(
        source_vault: &str,
        source_path: &str,
        source_hash: ContentHash,
        projected_hash: ContentHash,
        configuration: ConfigurationId,
    ) -> Result<Self, ProjectionError> {
        for (field, text) in [("source_vault", source_vault), ("source_path", source_path)] {
            if text.is_empty() {
                return Err(ProjectionError::ManifestMalformed {
                    reason: ManifestRejection::EmptyField { field },
                });
            }
        }
        Ok(Self {
            source_vault: source_vault.to_owned(),
            source_path: source_path.to_owned(),
            source_hash,
            projected_hash,
            configuration,
        })
    }

    /// The source vault's name.
    #[must_use]
    pub fn source_vault(&self) -> &str {
        &self.source_vault
    }

    /// The source path, relative to its vault.
    #[must_use]
    pub fn source_path(&self) -> &str {
        &self.source_path
    }

    /// The hash of the source definition when it was projected.
    #[must_use]
    pub const fn source_hash(&self) -> &ContentHash {
        &self.source_hash
    }

    /// The hash of what was written to the target.
    #[must_use]
    pub const fn projected_hash(&self) -> &ContentHash {
        &self.projected_hash
    }

    /// The configuration that projected this entry.
    #[must_use]
    pub const fn configuration(&self) -> &ConfigurationId {
        &self.configuration
    }
}

/// A stable name for one target: the SHA-256 of its resolved root path.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TargetKey(String);

impl From<&ProjectionTarget> for TargetKey {
    fn from(value: &ProjectionTarget) -> Self {
        let root: &Path = value.into();
        let digest = Sha256::digest(root.as_os_str().as_encoded_bytes());
        Self(<&str>::from(&ContentHash::from(<[u8; 32]>::from(digest))).to_owned())
    }
}

impl<'a> From<&'a TargetKey> for &'a str {
    fn from(value: &'a TargetKey) -> Self {
        &value.0
    }
}

/// Where a target's manifest lives: host-owned storage the caller supplies, never a folder the host reads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManifestLocation(PathBuf);

impl ManifestLocation {
    /// The manifest for `target` beneath `state_root`: `<state_root>/projection/<target key>.json`.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::TargetRootRejected`] when `state_root` is not absolute.
    pub fn for_target(state_root: &Path, target: &ProjectionTarget) -> Result<Self, ProjectionError> {
        let location = Self::try_from(state_root)?;
        let key = TargetKey::from(target);
        Ok(Self(
            location
                .0
                .join("projection")
                .join(format!("{}.json", <&str>::from(&key))),
        ))
    }
}

impl TryFrom<&Path> for ManifestLocation {
    type Error = ProjectionError;

    fn try_from(value: &Path) -> Result<Self, Self::Error> {
        if !value.is_absolute() {
            return Err(ProjectionError::TargetRootRejected {
                reason: TargetRootRejection::NotAbsolute,
            });
        }
        Ok(Self(value.to_path_buf()))
    }
}

impl<'a> From<&'a ManifestLocation> for &'a Path {
    fn from(value: &'a ManifestLocation) -> Self {
        &value.0
    }
}

const fn malformed(reason: ManifestRejection) -> ProjectionError {
    ProjectionError::ManifestMalformed { reason }
}

/// The target root as the manifest records it, so a manifest copied to another target is recognised.
struct RecordedRoot(String);

impl From<&ProjectionTarget> for RecordedRoot {
    fn from(value: &ProjectionTarget) -> Self {
        let root: &Path = value.into();
        Self(root.to_string_lossy().into_owned())
    }
}

impl TryFrom<EntryDocument> for ManifestEntry {
    type Error = ProjectionError;

    fn try_from(value: EntryDocument) -> Result<Self, Self::Error> {
        let hash = |field: &'static str, text: &str| {
            ContentHash::try_from(text).map_err(|_| malformed(ManifestRejection::InvalidHash { field }))
        };
        Self::new(
            &value.source_vault,
            &value.source_path,
            hash("source_hash", &value.source_hash)?,
            hash("projected_hash", &value.projected_hash)?,
            ConfigurationId::try_from(value.configuration.as_str())
                .map_err(|_| malformed(ManifestRejection::InvalidConfiguration))?,
        )
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EntryDocument {
    source_vault: String,
    source_path: String,
    source_hash: String,
    projected_hash: String,
    configuration: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ManifestDocument {
    version: u32,
    target_root: String,
    entries: BTreeMap<String, EntryDocument>,
}

/// The record of which entries in one target the server projected, and therefore may rewrite or remove.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OwnershipManifest {
    entries: BTreeMap<Slug, ManifestEntry>,
}

impl OwnershipManifest {
    /// An empty manifest: the server owns nothing in the target.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Reads the manifest at `location` for `target`. A missing file is an empty manifest.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::ManifestMalformed`] for any content that is not a valid manifest for this target,
    /// and [`ProjectionError::ManifestUnreadable`] when the file exists but cannot be read. Nothing is repaired.
    pub fn read(location: &ManifestLocation, target: &ProjectionTarget) -> Result<Self, ProjectionError> {
        let bytes = match fs::read(<&Path>::from(location)) {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Self::empty()),
            Err(source) => return Err(ProjectionError::ManifestUnreadable { source }),
        };
        let document: ManifestDocument = serde_json::from_slice(&bytes).map_err(|error| {
            malformed(ManifestRejection::InvalidDocument {
                line: error.line(),
                column: error.column(),
            })
        })?;
        if document.version != VERSION {
            return Err(malformed(ManifestRejection::UnsupportedVersion {
                found: document.version,
                supported: VERSION,
            }));
        }
        if document.target_root != RecordedRoot::from(target).0 {
            return Err(malformed(ManifestRejection::TargetMismatch));
        }
        let mut entries = BTreeMap::new();
        for (key, entry) in document.entries {
            let slug = Slug::try_from(key.as_str()).map_err(|_| malformed(ManifestRejection::InvalidSlug))?;
            entries.insert(slug, ManifestEntry::try_from(entry)?);
        }
        Ok(Self { entries })
    }

    /// Writes the manifest to `location` for `target` with the shared atomic writer.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::Write`] when the file cannot be written.
    pub fn write(&self, location: &ManifestLocation, target: &ProjectionTarget) -> Result<(), ProjectionError> {
        let document = ManifestDocument {
            version: VERSION,
            target_root: RecordedRoot::from(target).0,
            entries: self
                .entries
                .iter()
                .map(|(slug, entry)| {
                    (
                        <&str>::from(slug).to_owned(),
                        EntryDocument {
                            source_vault: entry.source_vault.clone(),
                            source_path: entry.source_path.clone(),
                            source_hash: <&str>::from(&entry.source_hash).to_owned(),
                            projected_hash: <&str>::from(&entry.projected_hash).to_owned(),
                            configuration: <&str>::from(&entry.configuration).to_owned(),
                        },
                    )
                })
                .collect(),
        };
        let mut bytes =
            serde_json::to_vec_pretty(&document).map_err(|source| ProjectionError::ManifestSerialise { source })?;
        bytes.push(b'\n');
        contextos_fs::host_file::write_file_atomically(<&Path>::from(location), &bytes)?;
        Ok(())
    }

    /// The entry recorded for `slug`.
    #[must_use]
    pub fn get(&self, slug: &Slug) -> Option<&ManifestEntry> {
        self.entries.get(slug)
    }

    /// Records or replaces the entry for `slug`, returning the one it replaced.
    pub fn insert(&mut self, slug: Slug, entry: ManifestEntry) -> Option<ManifestEntry> {
        self.entries.insert(slug, entry)
    }

    /// Forgets `slug`, returning its entry.
    pub fn remove(&mut self, slug: &Slug) -> Option<ManifestEntry> {
        self.entries.remove(slug)
    }

    /// Every entry in slug order.
    pub fn iter(&self) -> impl Iterator<Item = (&Slug, &ManifestEntry)> {
        self.entries.iter()
    }

    /// How many entries the manifest records.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the manifest records nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
#[path = "manifest_test.rs"]
mod tests;
