//! Agent host projection findings as `doctor` checks.
//!
//! The projection service decides what is wrong and whether the server can repair it; this module only words each
//! finding as a check: its stable kind, the configured vault it concerns (when one does), and the remediation code
//! `doctor_resolve` applies. Nothing here reads or writes a host folder.

use contextos_core::VaultSet;
use std::fmt;

use contextos_projection::{Failure, Finding, FindingKind, HostKind};

use crate::doctor::{DoctorCheck, DoctorError, DoctorStatus, Remediation};
use crate::doctor_kind::DoctorKind;
use crate::projection_lock::ProjectionLockError;
use crate::projection_wiring::Projection;

/// What to assess and which findings to keep.
pub(crate) struct ProjectionChecks<'a> {
    /// The composed projection; a disabled one yields no check.
    pub(crate) projection: &'a Projection,
    /// The configured vaults, which findings name by their configured name.
    pub(crate) roots: &'a VaultSet,
    /// When set, only findings that concern this configured vault are kept.
    pub(crate) target_vault_index: Option<usize>,
}

impl TryFrom<ProjectionChecks<'_>> for Vec<DoctorCheck> {
    type Error = DoctorError;

    fn try_from(value: ProjectionChecks<'_>) -> Result<Self, Self::Error> {
        let report = match value.projection.assess() {
            Ok(report) => report,
            // Another process holds a host-level lock; that says nothing about the vaults, so they are still
            // reported and projection alone is named as unassessed.
            Err(ProjectionLockError::Busy { seconds }) => return Ok(vec![busy_check(seconds)]),
            // The same for a lock that could not be opened at all: projection alone is unassessed.
            Err(ProjectionLockError::Failed) => return Ok(vec![lock_failed_check()]),
        };
        for warning in &report.warnings {
            // The warning names a host and a stable code, never a path under the home folder.
            tracing::warn!(code = %warning.code, "agent host projection assessment: {}", warning.message);
        }
        let mut checks = Vec::<DoctorCheck>::new();
        for finding in &report.findings {
            let check = DoctorCheck::from(&ProjectionFinding {
                finding,
                roots: value.roots,
            });
            // A finding concerns the vault its source came from and, for a shadowing, the later vault whose different
            // definition lost; scoped to either, it is listed.
            if let Some(target) = value.target_vault_index {
                let target_name = value.roots.iter().nth(target).map(contextos_core::VaultRoot::name);
                let shadowed_here =
                    finding.other_vault.as_deref().is_some() && finding.other_vault.as_deref() == target_name;
                if check.vault_index != Some(target) && !shadowed_here {
                    continue;
                }
            }
            // A definition's own problem is the same whichever host it was meant for, and is said once.
            let repeats_for_another_host = finding.host.is_some()
                && !concerns_one_host(finding.kind)
                && checks
                    .iter()
                    .any(|earlier| earlier.subject == check.subject && earlier.message == check.message);
            if !repeats_for_another_host {
                checks.push(check);
            }
        }
        Ok(checks)
    }
}

/// The one check that stands for a projection that could not be assessed because another process held its lock.
fn busy_check(seconds: u64) -> DoctorCheck {
    DoctorCheck {
        subject: DoctorKind::ProjectionBusy.to_string(),
        kind: DoctorKind::ProjectionBusy,
        status: DoctorStatus::Fail,
        message: format!(
            "projection/busy: another contextos process held the agent host projection lock for more than {seconds} \
             seconds, so agent host projection was not assessed."
        ),
        action: Some(
            "Retry shortly. If it persists, another contextos process is stuck mid-projection: stop it.".to_owned(),
        ),
        auto_fixable: false,
        remediation: None,
        vault_index: None,
        vault_name: None,
    }
}

/// The one check that stands for a projection that could not be assessed because its lock could not be opened.
fn lock_failed_check() -> DoctorCheck {
    DoctorCheck {
        subject: DoctorKind::ProjectionLockFailed.to_string(),
        kind: DoctorKind::ProjectionLockFailed,
        status: DoctorStatus::Fail,
        message:
            "projection/lock-failed: the agent host projection lock could not be opened, so agent host projection \
                  was not assessed."
                .to_owned(),
        action: Some(
            "Check that the server's state folder under the home directory is on a local disk it can write, and that \
             projection.lock in it is a file, then rerun the doctor."
                .to_owned(),
        ),
        auto_fixable: false,
        remediation: None,
        vault_index: None,
        vault_name: None,
    }
}

/// Whether a finding of `kind` is about one host's folder, so that the same finding for each host is a distinct fact.
const fn concerns_one_host(kind: FindingKind) -> bool {
    !matches!(
        kind,
        FindingKind::ProjectionInvalidDefinition | FindingKind::ProjectionShadowed
    )
}

/// How a projection check names what it is about: the kind, the definition or file, and the host when it is one
/// host's copy. A repair's failure is matched to its remedy by building the same subject, never by reading prose.
pub(crate) struct ProjectionSubject<'a> {
    kind: DoctorKind,
    name: &'a str,
    host: Option<HostKind>,
}

impl<'a> From<(DoctorKind, &'a Failure)> for ProjectionSubject<'a> {
    fn from((kind, failure): (DoctorKind, &'a Failure)) -> Self {
        Self {
            kind,
            name: &failure.name,
            host: Some(failure.host),
        }
    }
}

impl fmt::Display for ProjectionSubject<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.kind, self.name)?;
        if let Some(host) = self.host {
            write!(formatter, " ({host})")?;
        }
        Ok(())
    }
}

/// One finding, with the vaults its source vault name is looked up in.
pub(crate) struct ProjectionFinding<'a> {
    pub(crate) finding: &'a Finding,
    pub(crate) roots: &'a VaultSet,
}

/// The remediation of a finding the server can repair.
impl TryFrom<&Finding> for Remediation {
    type Error = NotAutoFixable;

    fn try_from(value: &Finding) -> Result<Self, Self::Error> {
        if !value.auto_fixable() {
            return Err(NotAutoFixable);
        }
        match value.kind {
            FindingKind::ProjectionStale | FindingKind::ProjectionHandEdited => Ok(Self::Reproject),
            FindingKind::ProjectionOrphaned => Ok(Self::WithdrawProjection),
            FindingKind::OrientationBlock => Ok(Self::RestoreOrientation),
            FindingKind::ProjectionInvalidDefinition
            | FindingKind::ProjectionShadowed
            | FindingKind::ProjectionUnownedCollision
            | FindingKind::ProjectionForeignOwner
            | FindingKind::CodexReadOnlyUnenforced
            | FindingKind::CodexOverrideHidesAgents => Err(NotAutoFixable),
        }
    }
}

/// The finding needs the operator, or is of a kind no server repair exists for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct NotAutoFixable;

impl From<&ProjectionFinding<'_>> for DoctorCheck {
    fn from(value: &ProjectionFinding<'_>) -> Self {
        let finding = value.finding;
        let kind = DoctorKind::from(finding.kind);
        let remediation = Remediation::try_from(finding).ok();
        // Only a vault that is configured is named: an entry recorded against a vault since removed concerns none the
        // operator can address.
        let vault = finding.vault.as_deref().and_then(|name| {
            value
                .roots
                .iter()
                .enumerate()
                .find(|(_, root)| root.name() == name)
                .map(|(index, root)| (index, root.name().to_owned()))
        });
        Self {
            subject: ProjectionSubject {
                kind,
                name: &finding.name,
                host: finding.host.filter(|_| concerns_one_host(finding.kind)),
            }
            .to_string(),
            kind,
            status: DoctorStatus::Fail,
            message: finding.message.clone(),
            action: Some(action(finding.kind, remediation)),
            auto_fixable: remediation.is_some(),
            remediation,
            vault_index: vault.as_ref().map(|(index, _)| *index),
            vault_name: vault.map(|(_, name)| name),
        }
    }
}

/// What the operator or `doctor_resolve` should do about a finding.
fn action(kind: FindingKind, remediation: Option<Remediation>) -> String {
    match (kind, remediation) {
        (FindingKind::ProjectionHandEdited, Some(_)) => "Call doctor_resolve (remediation reproject) to replace the hand edit \
             with what the vault defines, or run contextos doctor --resolve. To keep the edit, copy it into the vault's \
             definition first."
            .to_owned(),
        (FindingKind::OrientationBlock, Some(_)) => "Call doctor_resolve (remediation restore_orientation) to rewrite only the \
             lines between the orientation block's markers, or run contextos doctor --resolve."
            .to_owned(),
        (_, Some(remediation)) => format!(
            "Call doctor_resolve (remediation {}) or run contextos doctor --resolve.",
            remediation.code()
        ),
        (FindingKind::ProjectionInvalidDefinition, None) => {
            "Correct the definition in the vault so it meets the projection contract, then rerun the doctor.".to_owned()
        }
        (FindingKind::ProjectionShadowed, None) => {
            "Rename, align, or remove one of the definitions so each slug has one, then rerun the doctor.".to_owned()
        }
        (FindingKind::ProjectionUnownedCollision, None) => {
            "Move or rename the file or folder that sits there, or rename the definition, then rerun the doctor. \
             ContextOS leaves it untouched."
                .to_owned()
        }
        (FindingKind::ProjectionForeignOwner, None) => "Resolve it from the configuration that owns the entry, or disable \
             projection in one of the two configurations, then rerun the doctor."
            .to_owned(),
        (FindingKind::CodexReadOnlyUnenforced, None) => "Review the agent's tools: Codex can only switch off shell, apps, \
             and plugins for a read-only agent. No repair exists."
            .to_owned(),
        (FindingKind::OrientationBlock, None) => {
            "Correct the markers or the source by hand, as the message says, then rerun the doctor. The file is left \
             unchanged."
                .to_owned()
        }
        (FindingKind::CodexOverrideHidesAgents, None) => {
            "Remove AGENTS.override.md, or merge its content into the instructions file, then rerun the doctor.".to_owned()
        }
        (FindingKind::ProjectionStale | FindingKind::ProjectionHandEdited | FindingKind::ProjectionOrphaned, None) => {
            "Inspect the projected entry and the vault's definition, then rerun the doctor.".to_owned()
        }
    }
}

#[cfg(test)]
#[path = "doctor_projection_test.rs"]
mod tests;
