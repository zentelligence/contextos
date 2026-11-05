use contextos_core::OperationWarning;
use contextos_projection::{DefinitionKind, Failure, HostKind};

use super::*;
use crate::doctor::{DoctorCheck, DoctorStatus};
use crate::doctor_kind::DoctorKind;

fn check(kind: DoctorKind, subject: &str, message: &str) -> DoctorCheck {
    DoctorCheck {
        subject: subject.to_owned(),
        kind,
        status: DoctorStatus::Fail,
        message: message.to_owned(),
        action: None,
        auto_fixable: kind == DoctorKind::ProjectionStale,
        remediation: (kind == DoctorKind::ProjectionStale).then_some(Remediation::Reproject),
        vault_index: None,
        vault_name: None,
    }
}

fn stale(host: &str) -> DoctorCheck {
    check(
        DoctorKind::ProjectionStale,
        &format!("Projection stale: reviewer ({host})"),
        "The projected copy differs from the definition.",
    )
}

fn failure(slug: &str) -> Failure {
    Failure {
        host: HostKind::ClaudeCode,
        definition: Some(DefinitionKind::Agent),
        name: slug.to_owned(),
        code: "projection/write-failed".to_owned(),
    }
}

fn remedy(check: &DoctorCheck) -> Remedy<'_> {
    Remedy {
        check,
        remediation: Remediation::Reproject,
        scope: super::super::doctor::RemedyScope::Host { root: None },
    }
}

fn after(checks: Vec<DoctorCheck>) -> DoctorReport {
    DoctorReport { checks }
}

fn clean_pass() -> ProjectionReport {
    ProjectionReport::default()
}

#[test]
fn a_repair_whose_finding_is_gone_from_a_clean_reassessment_is_resolved() {
    let finding = stale("claude");

    let outcomes = performed(&[remedy(&finding)], &Ok(clean_pass()), &after(Vec::new()));

    assert!(outcomes[0].resolved, "{}", outcomes[0].message);
}

#[test]
fn a_reassessment_that_found_the_lock_busy_leaves_every_projection_remedy_unresolved_and_says_why() {
    let first = stale("claude");
    let second = stale("codex");
    let busy = check(
        DoctorKind::ProjectionBusy,
        "Projection busy",
        "projection/busy: another contextos process held the agent host projection lock",
    );

    let outcomes = performed(
        &[remedy(&first), remedy(&second)],
        &Ok(clean_pass()),
        &after(vec![busy]),
    );

    assert_eq!(outcomes.len(), 2);
    for outcome in outcomes {
        assert!(
            !outcome.resolved,
            "an unverified repair was reported resolved: {}",
            outcome.message
        );
        assert!(
            outcome.message.contains("could not be verified") && outcome.message.contains("another process"),
            "{}",
            outcome.message
        );
    }
}

#[test]
fn a_reassessment_that_could_not_open_the_lock_leaves_every_projection_remedy_unresolved_and_says_why() {
    let finding = stale("claude");
    let failed = check(
        DoctorKind::ProjectionLockFailed,
        "Projection lock failed",
        "projection/lock-failed: the agent host projection lock could not be opened",
    );

    let outcomes = performed(&[remedy(&finding)], &Ok(clean_pass()), &after(vec![failed]));

    assert!(!outcomes[0].resolved, "{}", outcomes[0].message);
    assert!(
        outcomes[0].message.contains("could not be verified") && outcomes[0].message.contains("could not be opened"),
        "{}",
        outcomes[0].message
    );
}

#[test]
fn a_remedy_whose_repair_returned_a_warning_for_its_host_and_slug_is_never_resolved() {
    let claude = stale("claude");
    let codex = stale("codex");
    let pass = Ok(ProjectionReport {
        findings: Vec::new(),
        warnings: vec![OperationWarning {
            code: "projection/write-failed".to_owned(),
            message: "Could not project the agent `reviewer` to claude. Check the folder is writable.".to_owned(),
        }],
        failures: vec![failure("reviewer")],
    });

    let outcomes = performed(&[remedy(&claude), remedy(&codex)], &pass, &after(Vec::new()));

    assert!(!outcomes[0].resolved, "{}", outcomes[0].message);
    assert!(
        outcomes[0].message.contains("projection/write-failed"),
        "{}",
        outcomes[0].message
    );
    assert!(
        outcomes[1].resolved,
        "a warning for another host must not unresolve this one"
    );
}

#[test]
fn a_warning_for_another_slug_does_not_unresolve_a_remedy() {
    let finding = stale("claude");
    let pass = Ok(ProjectionReport {
        findings: Vec::new(),
        warnings: vec![OperationWarning {
            code: "projection/write-failed".to_owned(),
            message: "Could not project the agent `critic` to claude. Check the folder is writable.".to_owned(),
        }],
        failures: vec![failure("critic")],
    });

    let outcomes = performed(&[remedy(&finding)], &pass, &after(Vec::new()));

    assert!(outcomes[0].resolved, "{}", outcomes[0].message);
}
