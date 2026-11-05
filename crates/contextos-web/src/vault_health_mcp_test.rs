use serde_json::json;

use super::*;
use crate::vault_health::HealthSummary;

fn step(kind: &str, message: &str) -> RepairStep {
    RepairStep {
        kind: CheckKind::from(kind),
        message: message.to_owned(),
        resolved: false,
    }
}

#[test]
fn the_same_plan_always_has_the_same_digest() {
    let steps = [step("managed_indexes", "would call vault_index_rebuild")];

    assert_eq!(PlanDigest::from(("v", &steps[..])), PlanDigest::from(("v", &steps[..])));
}

#[test]
fn a_plan_with_different_steps_has_a_different_digest() {
    let rebuild = [step("managed_indexes", "would call vault_index_rebuild")];
    let init = [step("git_recovery", "would call git_init")];
    let both = [rebuild[0].clone(), init[0].clone()];

    assert_ne!(
        PlanDigest::from(("v", &rebuild[..])),
        PlanDigest::from(("v", &init[..]))
    );
    assert_ne!(
        PlanDigest::from(("v", &rebuild[..])),
        PlanDigest::from(("v", &both[..]))
    );
}

#[test]
fn the_digest_separates_a_kind_from_the_message_that_follows_it() {
    // Without a delimiter these two would hash the same bytes.
    let split_early = [step("git_recovery", "xwould call git_init")];
    let split_late = [step("git_recoveryx", "would call git_init")];

    assert_ne!(
        PlanDigest::from(("v", &split_early[..])),
        PlanDigest::from(("v", &split_late[..]))
    );
}

#[test]
fn an_empty_plan_has_a_digest_too() {
    let none: [RepairStep; 0] = [];

    assert_eq!(PlanDigest::from(("v", &none[..])), PlanDigest::from(("v", &none[..])));
    assert!(
        !PlanDigest::from(("v", &none[..])).as_str().is_empty(),
        "`PlanDigest::from((\"v\", &none[..])).as_str()` should not be empty"
    );
}

fn wire_report() -> WireReport {
    WireReport {
        checks: vec![
            json!({"kind": "configuration", "status": "pass", "message": "ok", "auto_fixable": false}),
            json!({"kind": "managed_indexes", "vault_name": "alpha", "status": "pass", "message": "ok", "auto_fixable": false}),
            json!({"kind": "managed_indexes", "vault_name": "beta", "status": "fail", "message": "stale", "auto_fixable": true}),
        ],
    }
}

#[test]
fn a_resolve_report_keeps_the_vault_and_the_global_checks_and_drops_other_vaults() -> Result<(), HealthError> {
    let report = health_of(wire_report(), "alpha")?;

    let findings = report.groups().iter().map(|group| group.checks.len()).sum::<usize>();
    assert_eq!(findings, 2, "this vault's check and the global one");
    assert_eq!(report.summary(), HealthSummary::Healthy);
    Ok(())
}

#[test]
fn another_vaults_failure_never_reaches_this_vaults_report() -> Result<(), HealthError> {
    let report = health_of(wire_report(), "beta")?;

    assert_eq!(
        report.summary(),
        HealthSummary::NeedsAttention {
            failures: 1,
            fixable: 1
        }
    );
    Ok(())
}

#[test]
fn a_scope_names_the_vault_by_its_configured_name() {
    let arguments = scope("alpha", false);

    assert_eq!(arguments.get("path"), Some(&json!("alpha://.")));
    assert_eq!(arguments.get("dry_run"), None);
    assert_eq!(scope("alpha", true).get("dry_run"), Some(&json!(true)));
}

#[test]
fn a_plan_reviewed_for_one_vault_never_matches_another_vaults_plan() {
    let steps = [step("managed_indexes", "would call vault_index_rebuild")];

    assert_ne!(
        PlanDigest::from(("alpha", &steps[..])),
        PlanDigest::from(("beta", &steps[..]))
    );
}
