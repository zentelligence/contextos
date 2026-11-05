use super::*;

fn finding(kind: FindingKind, repair: Repair) -> Finding {
    Finding {
        kind,
        host: Some(HostKind::Codex),
        definition: None,
        name: "global.md".to_owned(),
        vault: None,
        other_vault: None,
        repair,
        message: String::new(),
    }
}

#[test]
fn a_finding_is_auto_fixable_only_when_its_kind_can_be_and_its_repair_is_automatic() {
    assert!(finding(FindingKind::OrientationBlock, Repair::Automatic).auto_fixable());
    assert!(!finding(FindingKind::OrientationBlock, Repair::Manual).auto_fixable());
    assert!(!finding(FindingKind::CodexOverrideHidesAgents, Repair::Manual).auto_fixable());
}

#[test]
fn an_orientation_block_is_a_kind_the_server_can_repair_and_the_override_file_is_never() {
    assert_eq!(Repair::from(FindingKind::OrientationBlock), Repair::Automatic);
    assert_eq!(Repair::from(FindingKind::CodexOverrideHidesAgents), Repair::Manual);
}
