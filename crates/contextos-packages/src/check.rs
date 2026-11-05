//! The read check every read of a release makes, and what it finds.
//!
//! A read lists the release folder and compares it with `manifest.toml`; reads and hashes every listed file against the
//! manifest, and the whole against its digest; then reads `manifest.toml` and lists the folder again. A release that
//! passes is [`ReadCheck::Verified`] and carries the bytes it was verified with, so nothing in it is read twice. One
//! that fails is classified by what is present, never by cause: removed, incomplete, damaged, or unverifiable.

use std::fmt;

use crate::io::StoreFailure;
use crate::manifest::{ManifestRejection, ReleaseFiles};
use crate::name::{ReleaseDigest, ReleaseId, ReleaseKey};
use crate::path::ReleaseFilePath;

/// The most a read of one release may hold, checked before content is read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadLimits {
    /// The most entries, files and folders, a release folder may hold.
    pub max_entries: usize,
    /// The most bytes all of a release's files, `manifest.toml` included, may hold together.
    pub max_bytes: u64,
}

/// A release that passed the read check, with the bytes it was verified with.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedRelease<M> {
    /// The release, its digest the one its manifest declares and its files hash to.
    pub release: ReleaseId,
    /// The parsed manifest.
    pub manifest: M,
    /// The text of `manifest.toml` as read.
    pub manifest_text: String,
    /// Every listed file's bytes, each matching the hash the manifest lists.
    pub files: ReleaseFiles,
}

/// What makes a release incomplete: something listed is missing or something unlisted is present.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IncompleteFinding {
    /// The release folder has no `manifest.toml`.
    ManifestMissing,
    /// A file the manifest lists is absent.
    ListedFileAbsent(ReleaseFilePath),
    /// An entry in the release folder that the manifest does not list, as the store reported its path. A folder is
    /// listed implicitly by any listed file below it; an empty folder is unlisted.
    UnlistedEntry(String),
}

/// What makes a release damaged: it is complete but wrong.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DamageFinding {
    /// `manifest.toml` does not parse, or breaks the model's schema.
    ManifestInvalid(ManifestRejection),
    /// `manifest.toml` names another release than the folder it stands in.
    ManifestForAnotherRelease {
        /// The release the folder stands for.
        folder: ReleaseKey,
        /// The release the manifest names.
        manifest: ReleaseKey,
    },
    /// A listed file's bytes do not hash to what the manifest lists.
    FileHashMismatch(ReleaseFilePath),
    /// The listed files do not hash to the manifest's digest.
    DigestMismatch {
        /// The digest the manifest declares.
        declared: ReleaseDigest,
        /// The digest of the files as read.
        computed: ReleaseDigest,
    },
    /// A link or reparse point stands in the release, at the path the store reported.
    LinkInRelease(String),
    /// An entry that is neither a regular file nor a folder stands in the release, at the path the store reported.
    NotARegularFile(String),
}

/// Why a release could not be verified, which says nothing about whether it is whole.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UnverifiableReason {
    /// `manifest.toml` or the folder's listing differed between the start and the end of the read.
    ChangedWhileRead,
    /// The store could not be reached, or a read failed part way.
    StoreUnavailable(StoreFailure),
    /// The release holds more than the read's limits allow.
    LimitExceeded(ReadLimits),
}

/// The outcome of reading one release.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReadCheck<M> {
    /// The release is whole and verified.
    Verified(VerifiedRelease<M>),
    /// There is no folder for the release.
    Removed,
    /// Something listed is absent, something unlisted is present, or there is no `manifest.toml`. Every finding is
    /// reported, so an operator can restore or remove each.
    Incomplete(Vec<IncompleteFinding>),
    /// The release is complete but wrong. Every finding is reported.
    Damaged(Vec<DamageFinding>),
    /// The release could not be verified.
    Unverifiable(UnverifiableReason),
}

/// A read check's outcome without its findings or content, for listings and reports. `From<&ReadCheck<M>>`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReleaseClassification {
    /// See [`ReadCheck::Verified`].
    Verified,
    /// See [`ReadCheck::Removed`].
    Removed,
    /// See [`ReadCheck::Incomplete`].
    Incomplete,
    /// See [`ReadCheck::Damaged`].
    Damaged,
    /// See [`ReadCheck::Unverifiable`].
    Unverifiable,
}

impl<M> From<&ReadCheck<M>> for ReleaseClassification {
    fn from(check: &ReadCheck<M>) -> Self {
        match check {
            ReadCheck::Verified(_) => Self::Verified,
            ReadCheck::Removed => Self::Removed,
            ReadCheck::Incomplete(_) => Self::Incomplete,
            ReadCheck::Damaged(_) => Self::Damaged,
            ReadCheck::Unverifiable(_) => Self::Unverifiable,
        }
    }
}

impl fmt::Display for IncompleteFinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ManifestMissing => formatter.write_str("manifest.toml is missing"),
            Self::ListedFileAbsent(path) => write!(formatter, "{path} is listed but absent"),
            Self::UnlistedEntry(path) => write!(formatter, "{path} is present but not listed"),
        }
    }
}

impl fmt::Display for DamageFinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ManifestInvalid(rejection) => write!(formatter, "manifest.toml is invalid: {}", rejection.message),
            Self::ManifestForAnotherRelease { folder, manifest } => write!(
                formatter,
                "manifest.toml names {} {}, not {} {}",
                manifest.name.as_str(),
                manifest.version,
                folder.name.as_str(),
                folder.version
            ),
            Self::FileHashMismatch(path) => write!(formatter, "{path} does not hash to what the manifest lists"),
            Self::DigestMismatch { .. } => {
                formatter.write_str("the files do not hash to the digest the manifest declares")
            }
            Self::LinkInRelease(path) => write!(formatter, "{path} is a link, which a release never holds"),
            Self::NotARegularFile(path) => write!(formatter, "{path} is neither a file nor a folder"),
        }
    }
}

impl fmt::Display for UnverifiableReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ChangedWhileRead => formatter.write_str("the release changed while it was read"),
            Self::StoreUnavailable(failure) => write!(formatter, "the store could not be read: {}", failure.detail),
            Self::LimitExceeded(limits) => write!(
                formatter,
                "the release holds more than {} entries or {} bytes",
                limits.max_entries, limits.max_bytes
            ),
        }
    }
}

#[cfg(test)]
#[path = "check_test.rs"]
mod tests;
