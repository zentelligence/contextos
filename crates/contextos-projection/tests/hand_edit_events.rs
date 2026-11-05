//! A write never discards an operator's hand edit unless the definition it came from changed, and then it says so.

mod common;

use std::fs;

use common::{REVIEWER, TestResult, World, agent, codes, reviewer, world};
use contextos_core::{OpKind, OperationWarning};
use contextos_projection::{FindingKind, HostKind, ProjectionService};

const EDITED: &str = "# My own take on the reviewer\n";
const REPLACED: &str = "projection/hand-edited-replaced";
const OTHER: &str = "registry/agents/other.md";

/// Projects the reviewer to both hosts, then edits Claude's copy by hand.
fn hand_edited_claude_copy() -> Result<(World, ProjectionService), Box<dyn std::error::Error>> {
    let world = world(1)?;
    let service = world.service("config-one")?;
    world.write_vault(0, REVIEWER, &reviewer("Review."))?;
    world.project(&service, OpKind::Create, &[(0, REVIEWER)])?;
    fs::write(world.claude_agent("reviewer"), EDITED)?;
    Ok((world, service))
}

fn replaced(warnings: &[OperationWarning]) -> Vec<&OperationWarning> {
    warnings.iter().filter(|warning| warning.code == REPLACED).collect()
}

#[test]
fn a_write_elsewhere_in_the_registry_leaves_a_hand_edited_agent_in_place() -> TestResult {
    let (world, service) = hand_edited_claude_copy()?;
    world.write_vault(0, OTHER, &agent("Another", "fast", "all", "Other."))?;

    let warnings = world.project(&service, OpKind::Create, &[(0, OTHER)])?;

    assert_eq!(fs::read_to_string(world.claude_agent("reviewer"))?, EDITED);
    assert!(replaced(&warnings).is_empty(), "{warnings:?}");
    assert!(world.claude_agent("other").exists());
    Ok(())
}

#[test]
fn a_write_to_the_hand_edited_agents_unchanged_source_leaves_the_edit_in_place_without_a_warning() -> TestResult {
    let (world, service) = hand_edited_claude_copy()?;

    let warnings = world.project(&service, OpKind::Modify, &[(0, REVIEWER)])?;

    assert_eq!(fs::read_to_string(world.claude_agent("reviewer"))?, EDITED);
    assert!(replaced(&warnings).is_empty(), "{warnings:?}");
    Ok(())
}

#[test]
fn a_whole_registry_event_leaves_a_hand_edited_agent_whose_source_is_unchanged() -> TestResult {
    let (world, service) = hand_edited_claude_copy()?;

    let warnings = world.project(&service, OpKind::Modify, &[(0, "registry/agents")])?;

    assert_eq!(fs::read_to_string(world.claude_agent("reviewer"))?, EDITED);
    assert!(replaced(&warnings).is_empty(), "{warnings:?}");
    Ok(())
}

#[test]
fn doctor_still_reports_the_hand_edit_that_a_write_left_in_place() -> TestResult {
    let (world, service) = hand_edited_claude_copy()?;
    world.write_vault(0, OTHER, &agent("Another", "fast", "all", "Other."))?;
    world.project(&service, OpKind::Create, &[(0, OTHER)])?;

    let assessment = service.assess();

    assert!(
        assessment.findings.iter().any(|finding| {
            finding.kind == FindingKind::ProjectionHandEdited
                && finding.host == Some(HostKind::ClaudeCode)
                && finding.name == "reviewer"
        }),
        "{:?}",
        assessment.findings
    );
    Ok(())
}

#[test]
fn a_write_that_changes_the_source_replaces_the_hand_edit_and_warns_once_naming_the_host_and_slug() -> TestResult {
    let (world, service) = hand_edited_claude_copy()?;
    world.write_vault(0, REVIEWER, &reviewer("Review more carefully."))?;

    let warnings = world.project(&service, OpKind::Modify, &[(0, REVIEWER)])?;

    let rendered = fs::read_to_string(world.claude_agent("reviewer"))?;
    assert!(rendered.contains("Review more carefully."), "{rendered}");
    let found = replaced(&warnings);
    assert_eq!(found.len(), 1, "{warnings:?}");
    let message = &found[0].message;
    assert!(message.contains("claude") && message.contains("reviewer"), "{message}");
    assert!(
        !message.contains("codex"),
        "the untouched Codex copy was not hand-edited: {message}"
    );
    Ok(())
}

#[test]
fn the_replaced_warning_never_carries_a_home_path_or_the_hand_edited_content() -> TestResult {
    let (world, service) = hand_edited_claude_copy()?;
    world.write_vault(0, REVIEWER, &reviewer("Review more carefully."))?;

    let warnings = world.project(&service, OpKind::Modify, &[(0, REVIEWER)])?;

    let found = replaced(&warnings);
    assert_eq!(found.len(), 1, "{warnings:?}");
    let message = &found[0].message;
    for private in [
        world.hosts.claude_agents.path().to_string_lossy().into_owned(),
        world.hosts.state.path().to_string_lossy().into_owned(),
        EDITED.trim().to_owned(),
    ] {
        assert!(!message.contains(&private), "{message}");
    }
    Ok(())
}

#[test]
fn each_hand_edited_host_copy_replaced_by_a_changed_source_gets_its_own_warning() -> TestResult {
    let (world, service) = hand_edited_claude_copy()?;
    fs::write(world.codex_agent("reviewer"), "name = \"mine\"\n")?;
    world.write_vault(0, REVIEWER, &reviewer("Review more carefully."))?;

    let warnings = world.project(&service, OpKind::Modify, &[(0, REVIEWER)])?;

    let found = replaced(&warnings);
    assert_eq!(found.len(), 2, "{warnings:?}");
    assert!(found.iter().any(|warning| warning.message.contains("claude")));
    assert!(found.iter().any(|warning| warning.message.contains("codex")));
    Ok(())
}

#[test]
fn a_write_that_replaces_nothing_by_hand_raises_no_replaced_warning() -> TestResult {
    let world = world(1)?;
    let service = world.service("config-one")?;
    world.write_vault(0, REVIEWER, &reviewer("Review."))?;
    world.project(&service, OpKind::Create, &[(0, REVIEWER)])?;
    world.write_vault(0, REVIEWER, &reviewer("Review more carefully."))?;

    let warnings = world.project(&service, OpKind::Modify, &[(0, REVIEWER)])?;

    assert!(!codes(&warnings).contains(&REPLACED), "{warnings:?}");
    Ok(())
}

#[test]
fn doctor_resolve_replaces_a_hand_edit_whose_source_is_unchanged() -> TestResult {
    let (world, service) = hand_edited_claude_copy()?;
    world.write_vault(0, OTHER, &agent("Another", "fast", "all", "Other."))?;
    world.project(&service, OpKind::Create, &[(0, OTHER)])?;
    assert_eq!(fs::read_to_string(world.claude_agent("reviewer"))?, EDITED);

    let resolved = service.resolve();

    assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
    assert_ne!(fs::read_to_string(world.claude_agent("reviewer"))?, EDITED);
    Ok(())
}

#[test]
fn start_up_refresh_still_leaves_a_hand_edit_whose_source_is_unchanged() -> TestResult {
    let (world, service) = hand_edited_claude_copy()?;

    let _ = service.refresh();

    assert_eq!(fs::read_to_string(world.claude_agent("reviewer"))?, EDITED);
    Ok(())
}
