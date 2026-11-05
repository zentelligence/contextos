//! A workflow release's library lock: each library release it runs with,
//! transitively, by name, version, and digest.
//!
//! The lock is fixed at the first passing validation of any job on the workflow release and recorded in the same state
//! write that records the job `validated`. Every later validation, job version, dry run, and run of every job on that
//! release reads it and resolves nothing. A store's releases stay immutable, so the lock lives in the state file, beside
//! whether the release is current, previous, or future.
//!
//! Pure schema: choosing the releases and recording the lock are the validation's and the state's, never this module's.

use std::collections::BTreeMap;

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::job_release::PredecessorKey;

/// One library release a lock names.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LockedLibrary {
    pub version: Version,
    /// The release's content digest, `sha256:` followed by 64 lower-case hexadecimal characters.
    pub digest: String,
}

/// The library releases one workflow release runs with, by library name, written as a table per library holding its
/// `version` and `digest`. A lock with no library is a lock all the same: the release
/// was validated and needs none.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct LibraryLock {
    pub libraries: BTreeMap<String, LockedLibrary>,
}

/// What a job's version was made from: the package and the exact release its job file names, and the library lock it
/// ran with, whether read from the state file or resolved by this validation as a release's first.
///
/// Carried from a validation to the state change that keeps its result, so the lock a first passing validation resolved
/// is recorded in the same write that records the job `validated`, and the job's entry records the release and lock its
/// validated version was made from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionBasis {
    /// The package the release belongs to: the workflow's own package in a vault's store, the job's package on the host.
    pub package: String,
    /// The release the job file names.
    pub release: Version,
    /// The library lock the version was made from.
    pub lock: LibraryLock,
    /// The job a successor's job file names as its predecessor, so the successor's first passing validation records
    /// its link in the same write.
    pub predecessor: Option<PredecessorKey>,
}
