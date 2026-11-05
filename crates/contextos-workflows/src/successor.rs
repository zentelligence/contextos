//! The decisions behind `jobs successor`, one rule for a vault's store and the host's: which release a successor names,
//! whether the job may have a successor on it, what the successor's key is, and how large a step the move is.
//!
//! Pure: reading the stores and writing the files are the callers'.

use semver::Version;
use thiserror::Error;

use crate::job_release::JobVersion;
use crate::lifecycle::JobLifecycle;
use crate::release_state::{ReleaseKind, WorkflowVersions};

#[cfg(test)]
#[path = "successor_test.rs"]
mod tests;

/// Which release a successor is asked to name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SuccessorTarget {
    /// The recorded `future` release.
    Future,
    /// The recorded `previous` release, which makes a return successor.
    Previous,
    /// This exact release.
    Version(Version),
}

/// Why a `--to` value names no release.
#[derive(Debug, Error, Eq, PartialEq)]
#[error("{value:?} is not `future`, `previous`, or an exact version such as 1.2.0")]
pub struct NotATarget {
    value: String,
}

impl TryFrom<&str> for SuccessorTarget {
    type Error = NotATarget;

    fn try_from(text: &str) -> Result<Self, Self::Error> {
        match text {
            "future" => Ok(Self::Future),
            "previous" => Ok(Self::Previous),
            exact => JobVersion::try_from(exact)
                .map(|version| Self::Version(version.as_semver().clone()))
                .map_err(|_| NotATarget {
                    value: exact.to_owned(),
                }),
        }
    }
}

/// Why a successor may not be made.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SuccessorRefusal {
    /// `future` was asked for and none is recorded.
    #[error("workflow/package-unavailable: the workflow has no future release installed")]
    NoFutureRelease,
    /// `previous` was asked for and none is recorded.
    #[error("workflow/transition-refused: the workflow has no previous release to return to")]
    NoPreviousRelease,
    /// The predecessor was retired without ever being enabled, which ends its chain.
    #[error(
        "workflow/transition-refused: the job was retired but never enabled, so it has nothing to succeed; make the successor of the job before it instead"
    )]
    PredecessorNeverEnabled,
    /// The predecessor has not been enabled.
    #[error(
        "workflow/transition-refused: the job is {stage}, so it has nothing to succeed; change its version in place with `jobs replace`"
    )]
    PredecessorNotStanding { stage: &'static str },
    /// The predecessor already has a successor.
    #[error("workflow/transition-refused: the job already has the successor {successor}")]
    HasSuccessor { successor: String },
    /// The release is closed to anything new.
    #[error("workflow/transition-refused: release {version} is a retired release, which takes no successor")]
    RetiredRelease { version: Version },
    /// The release is older than the predecessor's and is not the previous release.
    #[error(
        "workflow/transition-refused: release {to} is older than release {from}, which the job is on; only the previous release may be named"
    )]
    OlderRelease { from: Version, to: Version },
    /// A return was asked for by a job that is not on the current release.
    #[error(
        "workflow/transition-refused: the job is on a {kind} release, and only a job on the current release can return to the previous one"
    )]
    ReturnNeedsCurrent { kind: &'static str },
}

impl SuccessorRefusal {
    /// The stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NoFutureRelease => "workflow/package-unavailable",
            _ => "workflow/transition-refused",
        }
    }
}

/// The release a successor is to name, from what the state records.
///
/// # Errors
///
/// Returns [`SuccessorRefusal::NoFutureRelease`] or [`SuccessorRefusal::NoPreviousRelease`] when the release asked for
/// by name is not recorded.
pub fn resolve_target(
    target: &SuccessorTarget,
    versions: Option<&WorkflowVersions>,
) -> Result<Version, SuccessorRefusal> {
    match target {
        SuccessorTarget::Future => versions
            .and_then(|held| held.future.clone())
            .ok_or(SuccessorRefusal::NoFutureRelease),
        SuccessorTarget::Previous => versions
            .and_then(|held| held.previous.clone())
            .ok_or(SuccessorRefusal::NoPreviousRelease),
        SuccessorTarget::Version(version) => Ok(version.clone()),
    }
}

/// What is known about a predecessor and the release a successor would name.
#[derive(Clone, Copy, Debug)]
pub struct SuccessorCheck<'a> {
    /// The predecessor's lifecycle stage.
    pub stage: JobLifecycle,
    /// Whether the predecessor has ever been `enabled`. A `retired` job that never was ends its chain.
    pub ever_enabled: bool,
    /// The key of the predecessor's standing successor, when it has one.
    pub standing_successor: Option<&'a str>,
    /// The release the predecessor names.
    pub from: &'a Version,
    /// Whether that release is current, previous, future, or retired.
    pub from_kind: ReleaseKind,
    /// The release the successor would name.
    pub to: &'a Version,
    /// Whether that release is current, previous, future, or retired.
    pub to_kind: ReleaseKind,
}

/// How far a move from one release to another goes, by the first number that differs.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ReleaseStep {
    /// The same release.
    Same,
    /// Only the patch number differs.
    Patch,
    /// The minor number differs.
    Minor,
    /// The major number differs.
    Major,
}

impl From<(&Version, &Version)> for ReleaseStep {
    fn from((from, to): (&Version, &Version)) -> Self {
        if from.major != to.major {
            Self::Major
        } else if from.minor != to.minor {
            Self::Minor
        } else if from.patch != to.patch {
            Self::Patch
        } else {
            Self::Same
        }
    }
}

impl ReleaseStep {
    /// The word a step is reported with.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Same => "same",
            Self::Patch => "patch",
            Self::Minor => "minor",
            Self::Major => "major",
        }
    }
}

/// Whether a job may have a successor naming `check.to`, and the step the move is.
///
/// # Errors
///
/// Returns the [`SuccessorRefusal`] for the first rule the request breaks.
pub fn check_successor(check: &SuccessorCheck<'_>) -> Result<ReleaseStep, SuccessorRefusal> {
    if !matches!(
        check.stage,
        JobLifecycle::Enabled | JobLifecycle::Suspended | JobLifecycle::Retired
    ) {
        return Err(SuccessorRefusal::PredecessorNotStanding {
            stage: check.stage.as_str(),
        });
    }
    if check.stage == JobLifecycle::Retired && !check.ever_enabled {
        return Err(SuccessorRefusal::PredecessorNeverEnabled);
    }
    if let Some(successor) = check.standing_successor {
        return Err(SuccessorRefusal::HasSuccessor {
            successor: successor.to_owned(),
        });
    }
    match check.to_kind {
        ReleaseKind::Retired => {
            return Err(SuccessorRefusal::RetiredRelease {
                version: check.to.clone(),
            });
        }
        ReleaseKind::Previous if check.from_kind != ReleaseKind::Current => {
            return Err(SuccessorRefusal::ReturnNeedsCurrent {
                kind: check.from_kind.as_str(),
            });
        }
        ReleaseKind::Current | ReleaseKind::Future if check.to < check.from => {
            return Err(SuccessorRefusal::OlderRelease {
                from: check.from.clone(),
                to: check.to.clone(),
            });
        }
        ReleaseKind::Previous | ReleaseKind::Current | ReleaseKind::Future => {}
    }
    Ok(ReleaseStep::from((check.from, check.to)))
}

/// Whether `--all` makes a successor for a job at `stage` on a release that is `kind`.
#[must_use]
pub const fn selected_for_all(stage: JobLifecycle, kind: ReleaseKind) -> bool {
    matches!(stage, JobLifecycle::Enabled | JobLifecycle::Suspended) && matches!(kind, ReleaseKind::Current)
}

/// The key of a successor made by `--all`: the predecessor's key without any trailing `-v{major}-{minor}-{patch}`,
/// then that suffix for the release the successor names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SuccessorKey(String);

impl SuccessorKey {
    /// The key as a job slug.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What a bulk successor's key is made from.
#[derive(Clone, Copy, Debug)]
pub struct SuccessorKeyInput<'a> {
    /// The predecessor's key.
    pub predecessor: &'a str,
    /// The release the successor names.
    pub to: &'a Version,
}

impl From<SuccessorKeyInput<'_>> for SuccessorKey {
    fn from(input: SuccessorKeyInput<'_>) -> Self {
        let base = without_release_suffix(input.predecessor);
        let suffix = format!("v{}-{}-{}", input.to.major, input.to.minor, input.to.patch);
        if base.is_empty() {
            Self(suffix)
        } else {
            Self(format!("{base}-{suffix}"))
        }
    }
}

/// `key` without a trailing `-v{major}-{minor}-{patch}`, or `key` itself when it does not end in one.
fn without_release_suffix(key: &str) -> &str {
    let mut parts = key.rsplitn(4, '-');
    let (Some(patch), Some(minor), Some(major)) = (parts.next(), parts.next(), parts.next()) else {
        return key;
    };
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    if major.strip_prefix('v').is_some_and(digits) && digits(minor) && digits(patch) {
        parts.next().unwrap_or("")
    } else {
        key
    }
}
