//! How `doctor_resolve` plans and reports the agent host projection repairs.
//!
//! The repairs themselves are the projection service's: one `resolve` pass under the host-level projection lock
//! repairs every auto-fixable finding at once. This module words what `doctor_resolve` then says about each finding,
//! and whether the repair took, judged by reassessing rather than trusting the pass.

use contextos_core::VaultRoot;
use contextos_projection::{Failure, ProjectionReport};

use super::doctor::{DoctorResolveOutcome, Remedy};
use crate::doctor::{DoctorReport, Remediation};
use crate::doctor_kind::DoctorKind;
use crate::doctor_projection::ProjectionSubject;
use crate::projection_lock::ProjectionLockError;

/// What a dry run reports for a projection remedy: the repair a real resolution would make.
pub(super) fn planned(remedy: &Remedy<'_>) -> DoctorResolveOutcome {
    let message = match remedy.remediation {
        Remediation::Reproject => {
            "would re-project it from the vault's definition, replacing the projected copy and any hand edit of it"
        }
        Remediation::WithdrawProjection => "would remove the projected entry the server owns, whose definition is gone",
        Remediation::RestoreOrientation => "would rewrite only the orientation block in the global instructions",
        _ => "would not repair a finding that is not a projection finding",
    };
    outcome(remedy, false, message.to_owned())
}

/// What the resolution reports for each projection remedy once its pass has run: repaired when the finding is gone
/// from a fresh assessment, and otherwise unresolved with the reason the pass gave.
pub(super) fn performed(
    remedies: &[Remedy<'_>],
    pass: &Result<ProjectionReport, ProjectionLockError>,
    after: &DoctorReport,
) -> Vec<DoctorResolveOutcome> {
    // A reassessment that could not read the host folders says nothing about whether any repair took, so none of
    // them is reported resolved.
    let unverified = after.checks.iter().find_map(|check| match check.kind {
        DoctorKind::ProjectionBusy => Some("another process held the projection lock"),
        DoctorKind::ProjectionLockFailed => Some("the projection lock could not be opened"),
        _ => None,
    });
    remedies
        .iter()
        .map(|remedy| {
            let still_present = after.checks.iter().any(|check| {
                check.kind == remedy.check.kind
                    && check.subject == remedy.check.subject
                    && check.message == remedy.check.message
            });
            match pass {
                Err(error) => outcome(remedy, false, format!("not repaired: {error}; retry")),
                Ok(_) if unverified.is_some() => outcome(
                    remedy,
                    false,
                    format!(
                        "the repair ran but its result could not be verified because {}; retry, or run doctor again",
                        unverified.unwrap_or_default()
                    ),
                ),
                Ok(report) => match report.failures.iter().find(|failure| failed(remedy, failure)) {
                    Some(failure) => outcome(
                        remedy,
                        false,
                        format!("not repaired: the repair failed with {}", failure.code),
                    ),
                    None if still_present => outcome(remedy, false, not_repaired(report)),
                    None => outcome(remedy, true, repaired(remedy.remediation).to_owned()),
                },
            }
        })
        .collect()
}

/// Whether `failure` is the entry `remedy` set out to repair: the same subject the finding is reported under.
fn failed(remedy: &Remedy<'_>, failure: &Failure) -> bool {
    remedy.check.subject == ProjectionSubject::from((remedy.check.kind, failure)).to_string()
}

fn repaired(remediation: Remediation) -> &'static str {
    match remediation {
        Remediation::Reproject => "re-projected from the vault's definition",
        Remediation::WithdrawProjection => "removed the projected entry",
        Remediation::RestoreOrientation => "rewrote the orientation block",
        _ => "repaired",
    }
}

/// Why a finding survived the pass: the stable codes of the warnings the pass raised, which name no path.
fn not_repaired(report: &ProjectionReport) -> String {
    if report.warnings.is_empty() {
        return "not repaired: the finding is still present after the repair".to_owned();
    }
    let codes = report
        .warnings
        .iter()
        .map(|warning| warning.code.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!("not repaired: the repair raised {codes}")
}

fn outcome(remedy: &Remedy<'_>, resolved: bool, message: String) -> DoctorResolveOutcome {
    let root: Option<&VaultRoot> = remedy.root();
    DoctorResolveOutcome {
        subject: remedy.check.subject.clone(),
        kind: remedy.check.kind,
        vault_path: root.map(|root| root.path().display().to_string()),
        vault_name: remedy.check.vault_name.clone(),
        resolved,
        message,
    }
}

#[cfg(test)]
#[path = "doctor_projection_test.rs"]
mod tests;
