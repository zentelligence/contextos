//! What a state file records about releases and the jobs that run them: each job's identity,
//! its successor link, and the record of its retirement by a successor; each workflow's `current` and `previous`
//! releases; and each workflow release's retired-release marker beside its library lock.
//!
//! The same keys are held by a vault's `.contextos/jobs.toml` and by the host's `shared-jobs.toml`, because one package
//! mechanism serves both stores. Current and previous versions, markers, locks, and retirement never span two state files: a store's releases
//! are used only by jobs of the same home.
//!
//! Pure schema: what moves `current` and `previous`, retires a job, or records a link is decided by the transition rules, never here.

use std::collections::BTreeMap;

use std::fmt::Write as _;

use rand::Rng;
use semver::Version;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;

use crate::library_lock::LibraryLock;
use crate::lifecycle::Actor;

/// The longest identity a state file may hold, in bytes.
const MAX_IDENTITY_BYTES: usize = 128;

/// Why a release rule refused, or a value could not be held.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum ReleaseStateError {
    /// An identity that is empty, longer than the limit, or holds a space or control character.
    #[error(
        "{0:?} is not a job identity: it must be 1 to {MAX_IDENTITY_BYTES} bytes with no space or control character"
    )]
    IdentityMalformed(String),
    /// The issuer gave an identity the state already holds, so it would no longer tell two jobs apart.
    #[error("the identity {identity} was issued for {job}, but the state file already holds it")]
    IdentityReused {
        /// The identity the issuer gave.
        identity: String,
        /// The job it was issued for.
        job: String,
    },
}

impl ReleaseStateError {
    /// The stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::IdentityMalformed(_) | Self::IdentityReused { .. } => "workflow/state-unavailable",
        }
    }
}

/// The identifier a state file gives a job when its key first enters state. It is never reused, so two jobs that held
/// the same key one after the other, the first deleted and the key used again, are told apart.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct JobIdentity(String);

impl JobIdentity {
    /// The identity as it is written in the state file.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for JobIdentity {
    type Error = ReleaseStateError;

    fn try_from(text: String) -> Result<Self, ReleaseStateError> {
        if text.is_empty()
            || text.len() > MAX_IDENTITY_BYTES
            || text.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            return Err(ReleaseStateError::IdentityMalformed(text));
        }
        Ok(Self(text))
    }
}

impl From<JobIdentity> for String {
    fn from(identity: JobIdentity) -> Self {
        identity.0
    }
}

/// Where a state file gets the identity it gives a key. Injected, so a test knows which identity a key was given and a
/// real issuer cannot repeat one.
pub trait IdentityIssuer {
    /// A new identity.
    fn issue(&self) -> JobIdentity;
}

/// The issuer a running server uses: 128 random bits, written as `job-` and 32 lower-case hexadecimal digits.
#[derive(Clone, Copy, Debug, Default)]
pub struct RandomIdentityIssuer;

impl IdentityIssuer for RandomIdentityIssuer {
    fn issue(&self) -> JobIdentity {
        let mut bytes = [0_u8; 16];
        rand::rng().fill_bytes(&mut bytes);
        let mut text = String::from("job-");
        for byte in bytes {
            let _ = write!(text, "{byte:02x}");
        }
        JobIdentity(text)
    }
}

/// `[...jobs.{job}.link]`: what a successor's state records about its predecessor at the successor's first passing
/// validation. A successor that has never passed validation has no link, only the `predecessor` key its job file names.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JobLink {
    /// The predecessor's identity when the link was recorded.
    pub identity: JobIdentity,
    /// Whether the predecessor was already `retired` then, so enabling the successor retires nothing.
    pub retired: bool,
    /// The job key the successor's file named for the predecessor when the link was recorded. A job file that names
    /// another key afterwards has been edited, which the identity alone cannot show when the predecessor has since been
    /// deleted. A link recorded without one reads as unrecorded, and its file is then held only to naming a predecessor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predecessor: Option<String>,
}

/// Why the system retired a job.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RetirementCause {
    /// The job's successor was enabled.
    SuccessorEnabled,
}

/// The job that took a retired job's place: its key within the same workflow, and its identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReplacedBy {
    /// The successor's job key.
    pub job: String,
    /// The successor's identity.
    pub identity: JobIdentity,
}

/// `[...jobs.{job}.retirement]`: the record of a job retired by its successor's enabling. A job the operator retired by
/// request has none.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JobRetirement {
    /// Who retired it: the system, for a successor's enabling.
    pub by: Actor,
    pub cause: RetirementCause,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub replaced_by: ReplacedBy,
}

/// Which of current, previous, future, or retired release one workflow release is, by the first rule that matches.
/// None of these is a lifecycle stage: only a job has a lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReleaseKind {
    /// The recorded `current`: what runs.
    Current,
    /// The recorded `previous`: the way back.
    Previous,
    /// Newer than `current`, or any release of a workflow with no `current`: what is being tried. There can be several; the
    /// recorded `future` is the newest of them.
    Future,
    /// A release with a recorded marker, or one older than `current` that was never `current` or `previous`.
    Retired,
}

impl ReleaseKind {
    /// The words a release is shown with, in refusals, listings, and pages.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Previous => "previous",
            Self::Future => "future",
            Self::Retired => "retired release",
        }
    }
}

impl std::fmt::Display for ReleaseKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// What a release is asked to allow.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReleaseUse {
    /// A new job naming the release.
    NewJob,
    /// A job's `version` changed in place to name the release.
    VersionChangedInPlace,
    /// A successor naming the release.
    Successor,
    /// A job on the release going from `registered` to `enabled`.
    Enable,
    /// A job on the release going from `suspended` to `enabled`.
    Resume,
    /// A `retired` job on the release re-activated.
    Reactivate,
    /// A new library lock for the release.
    NewLock,
}

/// What a release says to a [`ReleaseUse`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReleaseAllowance {
    /// Yes.
    Allowed,
    /// Yes, and `current` and `previous` move with it.
    AllowedMovingCurrent,
    /// Only a return successor: a successor of a job on `current` that names `previous`.
    ReturnSuccessorOnly,
    /// Only a return successor, and `current` and `previous` move with it.
    ReturnSuccessorMovingCurrent,
    /// No.
    Refused,
    /// No job can be in the position the use describes, so there is nothing to allow.
    NotReachable,
    /// No, because what the use would do is already done and never done again.
    AlreadyFixed,
}

impl ReleaseAllowance {
    /// Whether the use goes ahead, given whether the job is a return successor.
    #[must_use]
    pub const fn permits(self, return_successor: bool) -> bool {
        match self {
            Self::Allowed | Self::AllowedMovingCurrent => true,
            Self::ReturnSuccessorOnly | Self::ReturnSuccessorMovingCurrent => return_successor,
            Self::Refused | Self::NotReachable | Self::AlreadyFixed => false,
        }
    }

    /// Whether the use, when it goes ahead, moves the workflow's `current` and `previous`.
    #[must_use]
    pub const fn moves_current(self) -> bool {
        matches!(self, Self::AllowedMovingCurrent | Self::ReturnSuccessorMovingCurrent)
    }
}

impl ReleaseKind {
    /// What this release allows for `usage`.
    #[must_use]
    pub const fn allows(self, usage: ReleaseUse) -> ReleaseAllowance {
        use ReleaseAllowance::{
            Allowed, AllowedMovingCurrent, AlreadyFixed, NotReachable, Refused, ReturnSuccessorMovingCurrent,
            ReturnSuccessorOnly,
        };
        match (self, usage) {
            (Self::Future, ReleaseUse::Enable) => AllowedMovingCurrent,
            (Self::Future, ReleaseUse::Resume) => NotReachable,
            (Self::Previous, ReleaseUse::Successor) => ReturnSuccessorOnly,
            (Self::Previous, ReleaseUse::Enable) => ReturnSuccessorMovingCurrent,
            (Self::Previous, ReleaseUse::NewLock) => AlreadyFixed,
            (Self::Current | Self::Future, _) | (Self::Previous | Self::Retired, ReleaseUse::Resume) => Allowed,
            (
                Self::Previous | Self::Retired,
                ReleaseUse::NewJob
                | ReleaseUse::VersionChangedInPlace
                | ReleaseUse::Successor
                | ReleaseUse::Enable
                | ReleaseUse::Reactivate
                | ReleaseUse::NewLock,
            ) => Refused,
        }
    }
}

/// Whether `slug` is one of the words a workflow's table in the state file uses for its own entries, so no job may have it
/// as its key.
#[must_use]
pub fn is_reserved_job_slug(slug: &str) -> bool {
    matches!(slug, "current" | "previous" | "future")
}

/// `[packages.{package}.workflows.{workflow}]`: a workflow's `current` and `previous` versions, each with the time it took effect,
/// and its `future` version. A workflow with no job ever enabled has no `current` or `previous`; one with no installed release
/// newer than `current` has no `future`.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkflowVersions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<Version>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub current_at: Option<OffsetDateTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<Version>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "time::serde::rfc3339::option"
    )]
    pub previous_at: Option<OffsetDateTime>,
    /// The newest installed release newer than `current`, or the newest installed release when there is no `current`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub future: Option<Version>,
}

/// Why a workflow release was marked retired.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RetiredReleaseCause {
    /// An enabling on a newer release left it neither `current` nor `previous`.
    Superseded,
}

/// `[packages.{package}.releases."{version}".workflows.{workflow}.retired]`: the retired-release marker. A marker is
/// never removed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetiredRelease {
    pub cause: RetiredReleaseCause,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

/// `[packages.{package}.releases."{version}".workflows.{workflow}]`: what is recorded about one workflow of one release.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkflowReleaseState {
    /// The library releases this workflow release runs with, fixed at the first passing validation of any job on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock: Option<LibraryLock>,
    /// The retired-release marker, once one is recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired: Option<RetiredRelease>,
}

/// `[packages.{package}.releases."{version}"]`: what is recorded about one release, per workflow it holds, since a host
/// package release can hold several workflows.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReleaseState {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub workflows: BTreeMap<String, WorkflowReleaseState>,
}

/// `[packages.{package}]`: one package's workflows' versions and release records.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PackageReleases {
    /// Each workflow's `current` and `previous`, by workflow slug.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub workflows: BTreeMap<String, WorkflowVersions>,
    /// What is recorded about each release, by version.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub releases: BTreeMap<Version, ReleaseState>,
}

impl PackageReleases {
    /// What is recorded about `workflow` in `release`, when anything is.
    #[must_use]
    pub fn workflow_release(&self, release: &Version, workflow: &str) -> Option<&WorkflowReleaseState> {
        self.releases.get(release)?.workflows.get(workflow)
    }

    /// Records `workflow`'s `future` from the releases installed now: the newest installed release newer than `current`,
    /// or the newest installed release when there is no `current`, leaving out the `previous` release and any release
    /// with a retired marker, which are those kinds before they could be future. Records none when there is no such release.
    /// Returns whether the recorded value changed.
    pub fn record_future(&mut self, workflow: &str, installed: &[Version]) -> bool {
        let held = self.workflows.get(workflow);
        let current = held.and_then(|held| held.current.as_ref());
        let previous = held.and_then(|held| held.previous.as_ref());
        let future = installed
            .iter()
            .filter(|release| current.is_none_or(|current| *release > current))
            .filter(|release| Some(*release) != previous)
            .filter(|release| {
                self.workflow_release(release, workflow)
                    .is_none_or(|recorded| recorded.retired.is_none())
            })
            .max()
            .cloned();
        if held.and_then(|held| held.future.as_ref()) == future.as_ref() {
            return false;
        }
        let versions = self.workflows.entry(workflow.to_owned()).or_default();
        versions.future = future;
        if *versions == WorkflowVersions::default() {
            self.workflows.remove(workflow);
        }
        true
    }

    /// Which of current, previous, future, or retired `release` of `workflow` is, by the first rule that matches.
    #[must_use]
    pub fn kind_of(&self, workflow: &str, release: &Version) -> ReleaseKind {
        let versions = self.workflows.get(workflow);
        let current = versions.and_then(|versions| versions.current.as_ref());
        if current == Some(release) {
            ReleaseKind::Current
        } else if versions.and_then(|versions| versions.previous.as_ref()) == Some(release) {
            ReleaseKind::Previous
        } else if self
            .workflow_release(release, workflow)
            .is_some_and(|recorded| recorded.retired.is_some())
        {
            ReleaseKind::Retired
        } else if current.is_none_or(|current| release > current) {
            ReleaseKind::Future
        } else {
            ReleaseKind::Retired
        }
    }
}

/// A state file that records which releases are current and previous: a vault's `.contextos/jobs.toml` and the host's `shared-jobs.toml`. Callers
/// that only ask which of them a release is are written once for both homes against this trait.
pub trait ReleaseKinds {
    /// Which of current, previous, future, or retired `release` of `workflow` in `package` is, by the first rule that matches.
    fn release_kind(&self, package: &str, workflow: &str, release: &Version) -> ReleaseKind;
}

#[cfg(test)]
#[path = "release_state_test.rs"]
mod tests;
