//! What a job file says about the release it runs: the exact `version` it names and, for a
//! successor, the `predecessor` it succeeds. One form for a vault job and a shared job.
//!
//! A job names one release of its workflow, never a range and never the newest, so a job validated, dry-run, and
//! enabled for a release runs that release and nothing else. A `[libraries]` table is an unknown key: library releases
//! are the workflow release's lock, never a job's choice.

use semver::Version;
use thiserror::Error;

use crate::package_name::is_device_name;

#[cfg(test)]
#[path = "job_release_test.rs"]
mod tests;

/// The exact release a job file's `version` names: major, minor, and patch, with no range and no build metadata.
///
/// Built only from a job file's text through the conversion that checks it, so a held value is always exact.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct JobVersion(Version);

/// Why a job file's `version` or `predecessor` is not usable. Every case is a defect in the authored file, so every case
/// carries the `validation/artefact-invalid` code, which authoring reports as the `cause` of `workflow/job-file-invalid`.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum JobReleaseError {
    /// Not three numeric parts: a range, a partial version, or text that is not a version.
    #[error("version {value:?} is not an exact version: write major, minor, and patch, such as 1.2.0, and no range")]
    VersionNotExact { value: String },
    /// An exact version that also carries build metadata, which two releases of one version could differ by.
    #[error("version {value:?} carries build metadata: write major, minor, and patch only")]
    VersionBuildMetadata { value: String },
    /// An exact version with a pre-release part, which no release can carry.
    #[error("version {value:?} is a pre-release: a release is major, minor, and patch only")]
    VersionPreRelease { value: String },
    /// A predecessor that is not a job slug.
    #[error("predecessor {value:?} is not a job key: expected lowercase letters, digits, and hyphens")]
    PredecessorNotSlug { value: String },
    /// A predecessor that is a name Windows reserves for a device.
    #[error("predecessor {value:?} is a name Windows reserves for a device")]
    PredecessorReserved { value: String },
}

impl JobReleaseError {
    /// The stable machine-readable code a file with this defect carries.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        "validation/artefact-invalid"
    }
}

impl TryFrom<&str> for JobVersion {
    type Error = JobReleaseError;

    fn try_from(text: &str) -> Result<Self, Self::Error> {
        let version = Version::parse(text).map_err(|_| JobReleaseError::VersionNotExact { value: text.to_owned() })?;
        if !version.pre.is_empty() {
            return Err(JobReleaseError::VersionPreRelease { value: text.to_owned() });
        }
        if !version.build.is_empty() {
            return Err(JobReleaseError::VersionBuildMetadata { value: text.to_owned() });
        }
        Ok(Self(version))
    }
}

impl TryFrom<&str> for PredecessorKey {
    type Error = JobReleaseError;

    fn try_from(text: &str) -> Result<Self, Self::Error> {
        let slug = !text.is_empty()
            && text
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !slug {
            return Err(JobReleaseError::PredecessorNotSlug { value: text.to_owned() });
        }
        if is_device_name(text) {
            return Err(JobReleaseError::PredecessorReserved { value: text.to_owned() });
        }
        Ok(Self(text.to_owned()))
    }
}

impl JobVersion {
    /// The version as a semantic version, for finding the release and comparing it with `current` and `previous`.
    #[must_use]
    pub const fn as_semver(&self) -> &Version {
        &self.0
    }
}

/// The three numbers of a release version, which make an exact version by construction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JobVersionNumbers {
    /// The major number.
    pub major: u64,
    /// The minor number.
    pub minor: u64,
    /// The patch number.
    pub patch: u64,
}

impl From<JobVersionNumbers> for JobVersion {
    fn from(numbers: JobVersionNumbers) -> Self {
        Self(Version::new(numbers.major, numbers.minor, numbers.patch))
    }
}

/// The key of the job a successor succeeds: a job slug of the same workflow, written by `jobs successor`.
///
/// Built only through the conversion that checks it is a slug and not a Windows reserved device name.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PredecessorKey(String);

impl PredecessorKey {
    /// The key as the job file writes it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The `version` and `predecessor` a job file's text holds, before they are checked.
#[derive(Clone, Copy, Debug)]
pub struct JobFileText<'a> {
    /// The `version` value.
    pub version: &'a str,
    /// The `predecessor` value, when the file has one.
    pub predecessor: Option<&'a str>,
}

impl TryFrom<JobFileText<'_>> for JobFileRelease {
    type Error = JobReleaseError;

    fn try_from(text: JobFileText<'_>) -> Result<Self, Self::Error> {
        Ok(Self {
            version: JobVersion::try_from(text.version)?,
            predecessor: text.predecessor.map(PredecessorKey::try_from).transpose()?,
        })
    }
}

/// The release fields of a loaded job file, in either home.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobFileRelease {
    /// The release the job runs.
    pub version: JobVersion,
    /// The job this one succeeds, for a successor.
    pub predecessor: Option<PredecessorKey>,
}

#[path = "job_release_authoring.rs"]
mod authoring;
pub use authoring::{InstalledReleases, JobPlace, Presence, ReleaseRefusal, check_new_job, check_replacement};
