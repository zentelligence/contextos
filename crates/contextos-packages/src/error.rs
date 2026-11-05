//! The package store's refusals and failures, each with a stable code.
//!
//! Every variant maps to exactly one code in [`codes`], through an inherent `PackagesError::code`, which returns
//! [`ManifestRejection::code`] unchanged for [`PackagesError::InvalidManifest`]:
//!
//! | Variant | Code |
//! | --- | --- |
//! | [`PackagesError::LedgerMissing`] | [`codes::LEDGER_UNREADABLE`] |
//! | [`PackagesError::LedgerUnreadable`] | [`codes::LEDGER_UNREADABLE`] |
//! | [`PackagesError::LedgerPresent`] | [`codes::LEDGER_PRESENT`] |
//! | [`PackagesError::DuplicateIdentity`] | [`codes::DUPLICATE_IDENTITY`] |
//! | [`PackagesError::NotFound`] | [`codes::NOT_FOUND`] |
//! | [`PackagesError::VersionStepInvalid`] | [`codes::VERSION_STEP_INVALID`] |
//! | [`PackagesError::InvalidName`] | [`codes::INVALID_NAME`] |
//! | [`PackagesError::InvalidVersion`] | [`codes::INVALID_VERSION`] |
//! | [`PackagesError::InvalidManifest`] | the rejection's own code |
//! | [`PackagesError::InvalidStorePath`] | [`codes::INVALID_PATH`] |
//! | [`PackagesError::LimitExceeded`] | [`codes::FOLDER_TOO_LARGE`] |
//! | [`PackagesError::StoreLocked`] | [`codes::STORE_LOCKED`] |
//! | [`PackagesError::ReleaseIncomplete`] | [`codes::RELEASE_INCOMPLETE`] |
//! | [`PackagesError::Store`] | [`codes::STORE_UNAVAILABLE`] |

use thiserror::Error;

use crate::check::ReadLimits;
use crate::io::{StoreFailure, StoreFailureKind};
use crate::manifest::ManifestRejection;
use crate::name::{ReleaseDigest, ReleaseKey, ReleaseVersion};
use crate::step::VersionStep;

/// The stable codes of [`PackagesError`]. Codes shared with the store's existing surface keep their existing text.
pub mod codes {
    /// The ledger is missing in a store that holds releases, or cannot be read: installs and removals are refused.
    pub const LEDGER_UNREADABLE: &str = "packages/ledger-unreadable";
    /// A ledger rebuild was asked of a store that has a ledger.
    pub const LEDGER_PRESENT: &str = "packages/ledger-present";
    /// The ledger records the version with another digest. A version is never reused with another digest.
    pub const DUPLICATE_IDENTITY: &str = "packages/duplicate-identity";
    /// A bundle's version steps less from the highest the ledger records than its interface changes need.
    pub const VERSION_STEP_INVALID: &str = "packages/version-step-invalid";
    /// The release is not in the store.
    pub const NOT_FOUND: &str = "packages/not-found";
    /// A package name is not one plain store folder name.
    pub const INVALID_NAME: &str = "packages/invalid-name";
    /// A version is not an exact semantic version without pre-release or build metadata.
    pub const INVALID_VERSION: &str = "packages/invalid-version";
    /// The manifest model's default code for a manifest it refuses, or that does not describe its files.
    pub const INVALID_MANIFEST: &str = "packages/invalid-manifest";
    /// A store path or operation identifier is not well formed.
    pub const INVALID_PATH: &str = "packages/invalid-path";
    /// A release holds more than the store's read limits allow.
    pub const FOLDER_TOO_LARGE: &str = "packages/folder-too-large";
    /// Another operation holds the store's lock.
    pub const STORE_LOCKED: &str = "packages/store-locked";
    /// A ledger rebuild was refused because a release is incomplete or unverifiable.
    pub const RELEASE_INCOMPLETE: &str = "packages/release-incomplete";
    /// The store could not be read or changed.
    pub const STORE_UNAVAILABLE: &str = "packages/store-unavailable";
}

/// A bundle's version and the step its interface changes need, when the version steps less.
#[derive(Debug)]
pub struct VersionStepRefusal {
    /// The release offered.
    pub release: ReleaseKey,
    /// The lower version of the pair whose step is too small: the release the offered one follows, or the offered one
    /// itself when a recorded release above it would step too little.
    pub from: ReleaseVersion,
    /// The higher version of that pair: the offered release, or the recorded release that follows it.
    pub to: ReleaseVersion,
    /// The step `to` takes from `from`.
    pub taken: VersionStep,
    /// The step its interface changes need.
    pub required: VersionStep,
    /// The interface changes found.
    pub changes: Vec<String>,
}

/// A package store operation's refusal or failure. Messages name releases and store-relative paths only.
#[derive(Debug, Error)]
pub enum PackagesError {
    /// The store holds releases and has no ledger. See [`codes::LEDGER_UNREADABLE`].
    #[error(
        "the package store holds releases but has no ledger, so nothing can be installed or removed; restore \
         ledger.toml, or rebuild it with `packages inspect <vault> --rebuild-ledger` (or `--shared`)"
    )]
    LedgerMissing,
    /// The ledger does not parse, breaks its schema, or records one version with two digests. See
    /// [`codes::LEDGER_UNREADABLE`].
    #[error(
        "the package store's ledger cannot be read, so nothing can be installed or removed: {reason}; restore \
         ledger.toml from a copy"
    )]
    LedgerUnreadable {
        /// What is wrong with the ledger, naming no content beyond the entry at fault.
        reason: String,
    },
    /// See [`codes::LEDGER_PRESENT`].
    #[error("the package store has a ledger, so it is not rebuilt; restore it from a copy if it cannot be read")]
    LedgerPresent,
    /// See [`codes::DUPLICATE_IDENTITY`].
    #[error(
        "{} {} is recorded with digest {}, not {}; a version is never reused with another digest",
        .release.name.as_str(),
        .release.version.as_semver(),
        .recorded.as_str(),
        .offered.as_str()
    )]
    DuplicateIdentity {
        /// The release.
        release: ReleaseKey,
        /// The digest the ledger records for it.
        recorded: ReleaseDigest,
        /// The digest offered.
        offered: ReleaseDigest,
    },
    /// See [`codes::VERSION_STEP_INVALID`].
    #[error(
        "{} {} is a {} step from {}, and its interface changes need a {} step; interface changes found: {}; version \
         it by what its interface changes, or declare a larger step",
        .0.release.name.as_str(),
        .0.to,
        .0.taken,
        .0.from,
        .0.required,
        if .0.changes.is_empty() { "none".to_owned() } else { .0.changes.join("; ") }
    )]
    VersionStepInvalid(Box<VersionStepRefusal>),
    /// See [`codes::NOT_FOUND`].
    #[error("{} {} is not in the package store", .release.name.as_str(), .release.version.as_semver())]
    NotFound {
        /// The release.
        release: ReleaseKey,
    },
    /// See [`codes::INVALID_NAME`].
    #[error("{raw:?} is not a package name: {reason}")]
    InvalidName {
        /// The name as given.
        raw: String,
        /// Why it is refused.
        reason: &'static str,
    },
    /// See [`codes::INVALID_VERSION`].
    #[error("{raw:?} is not an exact version: {reason}")]
    InvalidVersion {
        /// The version as given.
        raw: String,
        /// Why it is refused.
        reason: String,
    },
    /// The manifest model refused the manifest, or it does not describe the files given.
    #[error("{}", .0.message)]
    InvalidManifest(ManifestRejection),
    /// See [`codes::INVALID_PATH`].
    #[error("{raw:?} is not a valid store path: {reason}")]
    InvalidStorePath {
        /// The path as given.
        raw: String,
        /// Why it is refused.
        reason: &'static str,
    },
    /// See [`codes::FOLDER_TOO_LARGE`].
    #[error(
        "the release holds more than the store allows: at most {} entries and {} bytes",
        .limits.max_entries,
        .limits.max_bytes
    )]
    LimitExceeded {
        /// The limits exceeded.
        limits: ReadLimits,
    },
    /// See [`codes::STORE_LOCKED`].
    #[error("another package store operation is in progress; try again when it has finished")]
    StoreLocked,
    /// See [`codes::RELEASE_INCOMPLETE`].
    #[error(
        "{} {} is incomplete or cannot be verified, so the ledger is not rebuilt; restore or remove its entries first",
        .release.name.as_str(),
        .release.version.as_semver()
    )]
    ReleaseIncomplete {
        /// The first release found incomplete or unverifiable.
        release: ReleaseKey,
    },
    /// See [`codes::STORE_UNAVAILABLE`].
    #[error("the package store could not be read or changed: {}", .0.detail)]
    Store(StoreFailure),
}

impl PackagesError {
    /// The stable machine-readable code of this error. For [`PackagesError::InvalidManifest`] it is the manifest
    /// model's own code, unchanged.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::LedgerMissing | Self::LedgerUnreadable { .. } => codes::LEDGER_UNREADABLE,
            Self::LedgerPresent => codes::LEDGER_PRESENT,
            Self::DuplicateIdentity { .. } => codes::DUPLICATE_IDENTITY,
            Self::VersionStepInvalid(_) => codes::VERSION_STEP_INVALID,
            Self::NotFound { .. } => codes::NOT_FOUND,
            Self::InvalidName { .. } => codes::INVALID_NAME,
            Self::InvalidVersion { .. } => codes::INVALID_VERSION,
            Self::InvalidManifest(rejection) => rejection.code,
            Self::InvalidStorePath { .. } => codes::INVALID_PATH,
            Self::LimitExceeded { .. } => codes::FOLDER_TOO_LARGE,
            Self::StoreLocked => codes::STORE_LOCKED,
            Self::ReleaseIncomplete { .. } => codes::RELEASE_INCOMPLETE,
            Self::Store(_) => codes::STORE_UNAVAILABLE,
        }
    }
}

impl From<StoreFailure> for PackagesError {
    /// A busy lock is [`PackagesError::StoreLocked`]; every other failure is [`PackagesError::Store`].
    fn from(failure: StoreFailure) -> Self {
        match failure.kind {
            StoreFailureKind::LockBusy => Self::StoreLocked,
            _ => Self::Store(failure),
        }
    }
}

#[cfg(test)]
#[path = "error_test.rs"]
mod tests;
