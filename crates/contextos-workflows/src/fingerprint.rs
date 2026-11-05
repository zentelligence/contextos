//! Which version of a job facts were recorded for.
//!
//! A job's version is a digest of the artefacts that decide what it runs: for a job that names a release, that release
//! with its verified content digest and manifest, the job file, and each library release in the release's lock, in the
//! host's store or a vault's. A validation result and a completed dry run are each stored against the version they were made for, and stop applying
//! when it changes. The `ready` key a job file carries to say authoring is finished is not part of the version,
//! and neither are the formatting or comments of a manifest, so setting the key or tidying a file is not a new version.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Which store a job's release is kept in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreKind {
    /// The host package store, for a shared job.
    Host,
    /// A vault's package store, for a vault job.
    Vault,
}

impl StoreKind {
    /// The word the version is hashed with.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Vault => "vault",
        }
    }
}

/// The artefacts a job's version is made of, one form for a vault job and a shared job: the store kind, the package, the
/// release the job names and its verified digest, the job file, the release's manifest, and each library release in the
/// release's lock.
pub struct ReleaseFingerprintInput<'a> {
    pub store: StoreKind,
    /// The package the release belongs to: the workflow's own package in a vault's store.
    pub package: &'a str,
    /// The release the job file names, so another release is another version.
    pub release: &'a str,
    /// The release's verified content digest, which covers every file it lists.
    pub digest: &'a str,
    pub job_toml: &'a str,
    /// The release's `manifest.toml` as installed. The content digest leaves it out, so it is part of the version here,
    /// without its `digest` key and normalised, and a release reinstalled with another manifest is a new version.
    pub release_manifest: &'a str,
    /// The library releases in the release's lock, whether or not the workflow imports from each. They are hashed in
    /// name order.
    pub libraries: &'a [FingerprintLibrary<'a>],
}

/// One resolved library release, as a shared job's version records it.
pub struct FingerprintLibrary<'a> {
    pub name: &'a str,
    pub version: &'a str,
    /// The release's content digest.
    pub digest: &'a str,
    /// The release's `manifest.toml` as installed.
    pub manifest: &'a str,
}

/// A job's version: `sha256:` followed by 64 lowercase hexadecimal characters.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct JobFingerprint(String);

impl JobFingerprint {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn from_digest(digest: &[u8]) -> Self {
        Self(digest.iter().fold(String::from("sha256:"), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        }))
    }
}

impl From<&ReleaseFingerprintInput<'_>> for JobFingerprint {
    fn from(input: &ReleaseFingerprintInput<'_>) -> Self {
        let mut hasher = Sha256::new();
        // The store kind first, so a vault job is never taken for a host job, whatever the parts hold.
        let mut parts = vec![
            input.store.as_str().to_owned(),
            input.package.to_owned(),
            input.release.to_owned(),
            input.digest.to_owned(),
            normalised(input.job_toml, &["ready"]),
            normalised(input.release_manifest, &["digest"]),
        ];
        // A job that resolves no library adds nothing here, so only the manifest part changed its version. The tag keeps
        // a library's parts from being taken for anything else.
        let mut libraries: Vec<&FingerprintLibrary<'_>> = input.libraries.iter().collect();
        libraries.sort_by(|left, right| left.name.cmp(right.name));
        if !libraries.is_empty() {
            parts.push("libraries".to_owned());
        }
        for library in libraries {
            parts.extend([
                library.name.to_owned(),
                library.version.to_owned(),
                library.digest.to_owned(),
                normalised(library.manifest, &["digest"]),
            ]);
        }
        // Each part is length-prefixed, so a byte moved from the end of one part to the start of the next is a new
        // version.
        for part in parts {
            hasher.update(u64::try_from(part.len()).unwrap_or(u64::MAX).to_be_bytes());
            hasher.update(part.as_bytes());
        }
        Self::from_digest(&hasher.finalize())
    }
}

/// `manifest` parsed and written back without the keys in `dropped`, so its formatting, comments, and key order do not
/// matter. A manifest that does not parse is its own text, so it still has a version.
fn normalised(manifest: &str, dropped: &[&str]) -> String {
    toml::from_str::<toml::Table>(manifest)
        .ok()
        .and_then(|mut table| {
            for key in dropped {
                table.remove(*key);
            }
            toml::to_string(&table).ok()
        })
        .unwrap_or_else(|| manifest.to_owned())
}

#[derive(Debug, Error, Eq, PartialEq)]
#[error("{0:?} is not a job fingerprint: expected sha256: followed by 64 lowercase hexadecimal characters")]
pub struct FingerprintError(String);

impl TryFrom<String> for JobFingerprint {
    type Error = FingerprintError;

    fn try_from(text: String) -> Result<Self, FingerprintError> {
        let well_formed = text.strip_prefix("sha256:").is_some_and(|hex| {
            hex.len() == 64 && hex.chars().all(|character| matches!(character, '0'..='9' | 'a'..='f'))
        });
        if well_formed {
            Ok(Self(text))
        } else {
            Err(FingerprintError(text))
        }
    }
}

impl From<JobFingerprint> for String {
    fn from(fingerprint: JobFingerprint) -> Self {
        fingerprint.0
    }
}

#[cfg(test)]
#[path = "fingerprint_test.rs"]
mod tests;
