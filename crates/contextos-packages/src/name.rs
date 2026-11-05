//! The identity of a release in a store: its package's name, its version, and its content digest, each validated before
//! any path is formed from it, and the identifier of one staging or removal operation.

use std::fmt;

use semver::Version;

use crate::error::PackagesError;
use crate::manifest::ManifestRejection;

/// A package's name as a store folder name: one plain folder name on every platform, never a staging or removal
/// folder's name.
///
/// Built only with `TryFrom<&str>`, which accepts 1 to 64 ASCII lower-case letters, digits, and hyphens, not starting
/// with a hyphen, and not a name Windows reserves for a device (`con`, `prn`, `aux`, `nul`, `com1` to `com9`, `lpt1`
/// to `lpt9`); anything else is [`crate::PackagesError::InvalidName`]. A name can therefore never start with `_`, so
/// [`crate::STAGING_PREFIX`] and [`crate::REMOVING_PREFIX`] folders are never read as packages. The manifest model's
/// own naming policy (such as the name reserved for the built-in script library) is stricter; this type is the
/// engine's own check at the point a path is formed, which it makes whatever the manifest model has already checked.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ReleaseName(String);

impl ReleaseName {
    /// The name as written in the store.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A release's version: an exact semantic version with major, minor, and patch only.
///
/// Built only with `TryFrom<&str>`, which refuses a range, a pre-release, build metadata, or anything that does not
/// parse, with [`crate::PackagesError::InvalidVersion`]. Its folder name is its `Display` form, digits and dots only,
/// so two versions can never differ only in letter case.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ReleaseVersion(Version);

impl ReleaseVersion {
    /// The version as a semantic version, for comparing steps and matching requirements.
    #[must_use]
    pub fn as_semver(&self) -> &Version {
        &self.0
    }
}

/// A release's content digest, `sha256:` followed by 64 lower-case hexadecimal characters, over every file the
/// manifest lists in path order, excluding `manifest.toml` ([`crate::ReleaseRules::digest`]).
///
/// Built with `TryFrom<&str>` (a digest a manifest or the ledger declares, refused with
/// [`crate::PackagesError::InvalidManifest`] or [`crate::PackagesError::LedgerUnreadable`] by the caller's context) or
/// by [`crate::ReleaseRules::digest`] (a digest computed from content).
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ReleaseDigest(String);

impl ReleaseDigest {
    /// The digest in its written form.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Where a release lives in a store: its package's name and its version, `{package}/{version}/`.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ReleaseKey {
    /// The package's name.
    pub name: ReleaseName,
    /// The release's version.
    pub version: ReleaseVersion,
}

/// A release's full identifier: where it lives and what it holds. Kept in every run record and ledger entry, so a
/// release's history survives its removal.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ReleaseId {
    /// The package's name and the release's version.
    pub key: ReleaseKey,
    /// The release's content digest.
    pub digest: ReleaseDigest,
}

/// The identifier of one staging or removal operation, naming its `_staging-{id}` or `_removing-{id}` folder.
///
/// Built only with `TryFrom<&str>`, which accepts 1 to 32 ASCII lower-case letters and digits; anything else is
/// [`crate::PackagesError::InvalidStorePath`]. Issued by an injected [`crate::IssuesOperationIds`], so tests name
/// their folders deterministically.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OperationId(String);

impl OperationId {
    /// The identifier as written in a folder name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

const MAX_NAME_LEN: usize = 64;
const MAX_OPERATION_ID_LEN: usize = 32;
const DIGEST_PREFIX: &str = "sha256:";
const DIGEST_HEX_LEN: usize = 64;

/// Names Windows reserves for devices, whatever their extension.
fn is_windows_device_name(name: &str) -> bool {
    match name {
        "con" | "prn" | "aux" | "nul" => true,
        _ => {
            let bytes = name.as_bytes();
            bytes.len() == 4 && (name.starts_with("com") || name.starts_with("lpt")) && matches!(bytes[3], b'1'..=b'9')
        }
    }
}

impl TryFrom<&str> for ReleaseName {
    type Error = PackagesError;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        let refuse = |reason: &'static str| PackagesError::InvalidName {
            raw: raw.to_owned(),
            reason,
        };
        if raw.is_empty() {
            return Err(refuse("a name is not empty"));
        }
        if raw.len() > MAX_NAME_LEN {
            return Err(refuse("a name is at most 64 characters"));
        }
        if raw.starts_with('-') {
            return Err(refuse("a name does not start with a hyphen"));
        }
        if !raw
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(refuse("a name holds only lower-case letters, digits, and hyphens"));
        }
        if is_windows_device_name(raw) {
            return Err(refuse("a name is not one Windows reserves for a device"));
        }
        Ok(Self(raw.to_owned()))
    }
}

impl TryFrom<&str> for ReleaseVersion {
    type Error = PackagesError;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        let refuse = |reason: String| PackagesError::InvalidVersion {
            raw: raw.to_owned(),
            reason,
        };
        let version = Version::parse(raw).map_err(|error| refuse(error.to_string()))?;
        if !version.pre.is_empty() {
            return Err(refuse("a release has no pre-release version".to_owned()));
        }
        if !version.build.is_empty() {
            return Err(refuse("a release has no build metadata".to_owned()));
        }
        Ok(Self(version))
    }
}

impl fmt::Display for ReleaseVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.0.major, self.0.minor, self.0.patch)
    }
}

impl TryFrom<&str> for ReleaseDigest {
    type Error = PackagesError;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        let well_formed = raw.strip_prefix(DIGEST_PREFIX).is_some_and(|hex| {
            hex.len() == DIGEST_HEX_LEN
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        });
        if well_formed {
            Ok(Self(raw.to_owned()))
        } else {
            Err(PackagesError::InvalidManifest(ManifestRejection {
                code: crate::codes::INVALID_MANIFEST,
                message: "a digest is `sha256:` followed by 64 lower-case hexadecimal characters".to_owned(),
            }))
        }
    }
}

impl From<[u8; 32]> for ReleaseDigest {
    fn from(hash: [u8; 32]) -> Self {
        let mut written = String::with_capacity(DIGEST_PREFIX.len() + DIGEST_HEX_LEN);
        written.push_str(DIGEST_PREFIX);
        for byte in hash {
            written.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
            written.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
        }
        Self(written)
    }
}

impl TryFrom<&str> for OperationId {
    type Error = PackagesError;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        let well_formed = !raw.is_empty()
            && raw.len() <= MAX_OPERATION_ID_LEN
            && raw
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
        if well_formed {
            Ok(Self(raw.to_owned()))
        } else {
            Err(PackagesError::InvalidStorePath {
                raw: raw.to_owned(),
                reason: "an operation identifier is 1 to 32 lower-case letters and digits",
            })
        }
    }
}

impl From<u64> for OperationId {
    /// The number in decimal, which is always a well-formed identifier: at most 20 digits.
    fn from(number: u64) -> Self {
        Self(number.to_string())
    }
}

#[cfg(test)]
#[path = "name_test.rs"]
mod tests;
