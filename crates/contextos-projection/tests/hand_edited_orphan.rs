//! A projected entry the operator edited by hand is never discarded behind their back: when its definition goes away,
//! only `doctor_resolve` withdraws it.

mod common;

use std::fs;

use common::{REVIEWER, TestResult, World, codes, reviewer, world};
use contextos_core::OpKind;
use contextos_projection::{FindingKind, HostKind, ProjectionService};

const EDITED: &str = "# My own take on the reviewer\n";

/// Projects the reviewer to both hosts, edits Claude's copy by hand, then removes the definition from the vault.
fn orphaned_with_a_hand_edit() -> Result<(World, ProjectionService), Box<dyn std::error::Error>> {
    let world = world(1)?;
    let service = world.service("config-one")?;
    world.write_vault(0, REVIEWER, &reviewer("Review."))?;
    world.project(&service, OpKind::Create, &[(0, REVIEWER)])?;
    fs::write(world.claude_agent("reviewer"), EDITED)?;
    world.remove_vault(0, REVIEWER)?;
    Ok((world, service))
}

#[test]
fn a_delete_event_leaves_a_hand_edited_entry_and_withdraws_the_untouched_one() -> TestResult {
    let (world, service) = orphaned_with_a_hand_edit()?;

    let warnings = world.project(&service, OpKind::Delete, &[(0, REVIEWER)])?;

    assert!(
        world.claude_agent("reviewer").exists(),
        "the hand-edited entry was withdrawn"
    );
    assert_eq!(fs::read_to_string(world.claude_agent("reviewer"))?, EDITED);
    assert!(
        !world.codex_agent("reviewer").exists(),
        "the unedited Codex copy should be withdrawn"
    );
    assert_eq!(world.recorded_vault("reviewer")?.as_deref(), Some("alpha"));
    assert!(
        codes(&warnings).iter().all(|code| *code != "projection/write-failed"),
        "{warnings:?}"
    );
    Ok(())
}

#[test]
fn a_start_up_refresh_leaves_a_hand_edited_orphan_in_place() -> TestResult {
    let (world, service) = orphaned_with_a_hand_edit()?;

    let _ = service.refresh();

    assert!(
        world.claude_agent("reviewer").exists(),
        "the hand-edited entry was withdrawn"
    );
    assert_eq!(fs::read_to_string(world.claude_agent("reviewer"))?, EDITED);
    assert_eq!(world.recorded_vault("reviewer")?.as_deref(), Some("alpha"));
    Ok(())
}

#[test]
fn doctor_reports_a_hand_edited_orphan_as_both_orphaned_and_hand_edited_and_changes_nothing() -> TestResult {
    let (world, service) = orphaned_with_a_hand_edit()?;

    let assessment = service.assess();

    let claude: Vec<FindingKind> = assessment
        .findings
        .iter()
        .filter(|finding| finding.host == Some(HostKind::ClaudeCode) && finding.name == "reviewer")
        .map(|finding| finding.kind)
        .collect();
    assert!(claude.contains(&FindingKind::ProjectionOrphaned), "{claude:?}");
    assert!(claude.contains(&FindingKind::ProjectionHandEdited), "{claude:?}");
    assert_eq!(fs::read_to_string(world.claude_agent("reviewer"))?, EDITED);
    Ok(())
}

#[test]
fn only_resolve_withdraws_a_hand_edited_orphan() -> TestResult {
    let (world, service) = orphaned_with_a_hand_edit()?;
    let _ = service.refresh();
    assert!(
        world.claude_agent("reviewer").exists(),
        "the refresh withdrew the hand-edited entry"
    );

    let resolved = service.resolve();

    assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
    assert!(!world.claude_agent("reviewer").exists());
    assert_eq!(world.recorded_vault("reviewer")?, None);
    Ok(())
}
