//! What the first live run of a new version decides.
//!
//! An enabled job whose new version passed validation keeps its schedule, but its first live run is on trial. This
//! module is the pure rule that turns how that run ended into what happens to the job: a success or a degraded run ends
//! the trial, a failure suspends the job with the failure as the reason, and a cancelled run decides nothing.

use thiserror::Error;

use crate::fingerprint::JobFingerprint;
use crate::lifecycle::{Suspension, SuspensionCause, SuspensionReason, Transition};
use crate::script::RunOutcome;
use crate::validation::ValidationReport;

/// How a trial run ended, as far as the job is concerned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FirstRunVerdict {
    /// The run succeeded or degraded, so the version works.
    Passed,
    /// The run failed, including by timing out, and the job is suspended with this reason. The run is not retried.
    Failed {
        reason: SuspensionReason,
        /// The stable code the run failed with, when it had one.
        code: Option<String>,
    },
}

/// A run that ended without deciding anything, because it was cancelled.
#[derive(Debug, Error, Eq, PartialEq)]
#[error("a cancelled run says nothing about whether the version works")]
pub struct NoVerdict;

impl TryFrom<&RunOutcome> for FirstRunVerdict {
    type Error = NoVerdict;

    fn try_from(outcome: &RunOutcome) -> Result<Self, NoVerdict> {
        match outcome {
            RunOutcome::Succeeded | RunOutcome::Degraded(_) => Ok(Self::Passed),
            RunOutcome::Failed(failure) => Ok(Self::Failed {
                reason: SuspensionReason::from(failure),
                code: failure.kind.map(|kind| kind.code().to_owned()),
            }),
            RunOutcome::Cancelled => Err(NoVerdict),
        }
    }
}

impl From<(FirstRunVerdict, JobFingerprint, Option<u64>)> for Transition {
    /// The transition that settles the trial of `version`, by the run numbered `run`, when it has a number.
    fn from((verdict, version, run): (FirstRunVerdict, JobFingerprint, Option<u64>)) -> Self {
        match verdict {
            FirstRunVerdict::Passed => Self::ProbationPassed(version),
            FirstRunVerdict::Failed { reason, code } => Self::ProbationFailed(
                version,
                Suspension {
                    cause: SuspensionCause::ProbationFailed { run, failure: code },
                    reason,
                },
            ),
        }
    }
}

/// What validating an enabled job's new version decides.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NewVersionVerdict {
    /// The version passed, so the job stays enabled and its first live run is on trial.
    OnProbation(JobFingerprint),
    /// The version failed, or could not be determined, so the job is suspended with the failures as the reason.
    Suspend(SuspensionReason),
    /// Nothing failed, but a check could not be made, so the version is neither accepted nor condemned. The job's state
    /// is left alone and the version is looked at again on a later pass, because an outage is not a defect in the job.
    Wait,
}

impl From<(&ValidationReport, Option<JobFingerprint>)> for NewVersionVerdict {
    /// The verdict on a validation `report` made for `version`, which is `None` when the job's files could not all be
    /// read.
    fn from((report, version): (&ValidationReport, Option<JobFingerprint>)) -> Self {
        match version {
            Some(version) if report.failures.is_empty() => {
                if report.unverifiable.is_empty() {
                    Self::OnProbation(version)
                } else {
                    Self::Wait
                }
            }
            _ => Self::Suspend(SuspensionReason::from(report)),
        }
    }
}

#[cfg(test)]
#[path = "probation_test.rs"]
mod tests;
