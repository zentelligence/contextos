//! Start-up refresh, ownership, and assessment: real vaults, real host folders, and a real state root.

mod common;

use std::fs;

use common::{REVIEWER, TestResult, World, agent, codes, reviewer, world};
use contextos_core::OpKind;
use contextos_projection::{FindingKind, ProjectionReport};

fn kinds(report: &ProjectionReport) -> Vec<&'static str> {
    report
        .findings
        .iter()
        .map(|finding| <&str>::from(finding.kind))
        .collect()
}

fn projected_once(
    world: &World,
    body: &str,
) -> Result<contextos_projection::ProjectionService, Box<dyn std::error::Error>> {
    let service = world.service("config-one")?;
    world.write_vault(0, REVIEWER, &reviewer(body))?;
    world.project(&service, OpKind::Create, &[(0, REVIEWER)])?;
    Ok(service)
}

#[test]
fn refresh_projects_every_definition_already_in_the_vaults() -> TestResult {
    let world = world(2)?;
    world.write_vault(0, REVIEWER, &reviewer("From alpha."))?;
    world.write_vault(1, "registry/agents/critic.md", &reviewer("From beta."))?;
    world.write_vault(1, "registry/skills/writing/SKILL.md", &common::skill("writing"))?;
    let warnings = world.service("config-one")?.refresh();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(world.claude_agent("reviewer").exists() && world.codex_agent("critic").exists());
    assert!(world.claude_skill("writing").join("SKILL.md").exists());
    assert_eq!(world.recorded_vault("critic")?.as_deref(), Some("beta"));
    Ok(())
}

#[test]
fn refresh_picks_up_a_source_edited_out_of_band() -> TestResult {
    let world = world(1)?;
    let service = projected_once(&world, "Before.")?;
    world.write_vault(0, REVIEWER, &reviewer("After."))?;
    assert_eq!(codes(&service.refresh()), Vec::<&str>::new());
    assert!(fs::read_to_string(world.claude_agent("reviewer"))?.contains("After."));
    assert!(fs::read_to_string(world.codex_agent("reviewer"))?.contains("After."));
    Ok(())
}

#[test]
fn refresh_withdraws_what_a_removed_definition_left_behind() -> TestResult {
    let world = world(2)?;
    let service = projected_once(&world, "Review.")?;
    world.remove_vault(0, REVIEWER)?;
    assert_eq!(codes(&service.refresh()), Vec::<&str>::new());
    assert!(!world.claude_agent("reviewer").exists() && !world.codex_agent("reviewer").exists());
    assert_eq!(world.recorded_vault("reviewer")?, None);
    Ok(())
}

#[test]
fn refresh_replaces_a_vanished_projection() -> TestResult {
    let world = world(1)?;
    let service = projected_once(&world, "Review.")?;
    fs::remove_file(world.claude_agent("reviewer"))?;
    assert_eq!(codes(&service.refresh()), Vec::<&str>::new());
    assert!(fs::read_to_string(world.claude_agent("reviewer"))?.contains("Review."));
    Ok(())
}

#[test]
fn refresh_leaves_a_hand_edited_projection_whose_source_is_unchanged() -> TestResult {
    let world = world(1)?;
    let service = projected_once(&world, "Review.")?;
    fs::write(world.claude_agent("reviewer"), "my own edit")?;
    assert_eq!(codes(&service.refresh()), Vec::<&str>::new());
    assert_eq!(fs::read_to_string(world.claude_agent("reviewer"))?, "my own edit");
    let assessment = service.assess();
    assert_eq!(kinds(&assessment), ["projection_hand_edited"]);
    assert_eq!(assessment.findings[0].name, "reviewer");
    assert_eq!(assessment.findings[0].vault.as_deref(), Some("alpha"));
    assert!(assessment.findings[0].auto_fixable());
    // Assessing performs nothing.
    assert_eq!(fs::read_to_string(world.claude_agent("reviewer"))?, "my own edit");
    let resolved = service.resolve();
    assert_eq!(codes(&resolved.warnings), Vec::<&str>::new());
    assert!(fs::read_to_string(world.claude_agent("reviewer"))?.contains("Review."));
    assert_eq!(kinds(&service.assess()), Vec::<&str>::new());
    Ok(())
}

#[test]
fn an_event_replaces_a_hand_edit_with_the_new_projection_when_the_source_changed_and_says_so() -> TestResult {
    let world = world(1)?;
    let service = projected_once(&world, "Review.")?;
    fs::write(world.claude_agent("reviewer"), "my own edit")?;
    world.write_vault(0, REVIEWER, &reviewer("Changed."))?;
    assert_eq!(
        codes(&world.project(&service, OpKind::Modify, &[(0, REVIEWER)])?),
        ["projection/hand-edited-replaced"]
    );
    assert!(fs::read_to_string(world.claude_agent("reviewer"))?.contains("Changed."));
    Ok(())
}

#[test]
fn an_identical_unrecorded_file_is_adopted_not_rewritten() -> TestResult {
    let world = world(1)?;
    let service = projected_once(&world, "Review.")?;
    fs::remove_dir_all(world.hosts.state.path().join("projection"))?;
    assert_eq!(world.recorded_vault("reviewer")?, None);
    let before = fs::metadata(world.claude_agent("reviewer"))?.modified()?;
    let warnings = service.refresh();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(world.recorded_vault("reviewer")?.as_deref(), Some("alpha"));
    assert_eq!(fs::metadata(world.claude_agent("reviewer"))?.modified()?, before);
    Ok(())
}

#[test]
fn an_unrecorded_file_with_other_content_is_never_touched() -> TestResult {
    let world = world(1)?;
    let service = world.service("config-one")?;
    fs::write(world.claude_agent("reviewer"), "the operator's own agent")?;
    world.write_vault(0, REVIEWER, &reviewer("Review."))?;
    let warnings = world.project(&service, OpKind::Create, &[(0, REVIEWER)])?;
    assert_eq!(codes(&warnings), ["projection/unowned-collision"]);
    assert!(warnings[0].message.contains("claude"));
    assert_eq!(
        fs::read_to_string(world.claude_agent("reviewer"))?,
        "the operator's own agent"
    );
    assert!(world.codex_agent("reviewer").exists(), "the other host still projects");
    assert_eq!(world.recorded_vault("reviewer")?, None);
    let resolved = service.resolve();
    assert!(kinds(&resolved).contains(&"projection_unowned_collision"));
    assert_eq!(
        fs::read_to_string(world.claude_agent("reviewer"))?,
        "the operator's own agent"
    );
    Ok(())
}

#[test]
fn an_entry_recorded_by_another_configuration_is_never_touched() -> TestResult {
    let world = world(1)?;
    let _theirs = projected_once(&world, "First.")?;
    let ours = world.service("config-two")?;
    let before = fs::read(world.claude_agent("reviewer"))?;
    world.write_vault(0, REVIEWER, &reviewer("Edited."))?;
    let warnings = world.project(&ours, OpKind::Modify, &[(0, REVIEWER)])?;
    assert_eq!(
        codes(&warnings),
        ["projection/foreign-owner", "projection/foreign-owner"]
    );
    assert_eq!(fs::read(world.claude_agent("reviewer"))?, before);
    assert!(
        ours.refresh()
            .iter()
            .all(|warning| warning.code == "projection/foreign-owner")
    );
    assert!(
        ours.resolve()
            .findings
            .iter()
            .any(|finding| finding.kind == FindingKind::ProjectionForeignOwner)
    );
    assert_eq!(fs::read(world.claude_agent("reviewer"))?, before);
    // Removing the definition does not let another configuration withdraw it either.
    world.remove_vault(0, REVIEWER)?;
    let _ = ours.resolve();
    assert_eq!(fs::read(world.claude_agent("reviewer"))?, before);
    Ok(())
}

#[test]
fn no_vault_configured_withdraws_nothing_until_doctor_resolves() -> TestResult {
    let world = world(1)?;
    let _service = projected_once(&world, "Review.")?;
    let empty = World::no_roots()?;
    let service = world.service_over(&empty, "config-one")?;
    assert_eq!(codes(&service.refresh()), Vec::<&str>::new());
    assert!(world.claude_agent("reviewer").exists() && world.codex_agent("reviewer").exists());
    assert_eq!(world.recorded_vault("reviewer")?.as_deref(), Some("alpha"));
    let assessment = service.assess();
    assert_eq!(kinds(&assessment), ["projection_orphaned", "projection_orphaned"]);
    assert!(world.claude_agent("reviewer").exists());
    let resolved = service.resolve();
    assert_eq!(codes(&resolved.warnings), Vec::<&str>::new());
    assert!(!world.claude_agent("reviewer").exists() && !world.codex_agent("reviewer").exists());
    assert_eq!(world.recorded_vault("reviewer")?, None);
    Ok(())
}

#[test]
fn a_projection_kept_for_an_invalid_definition_is_reported_not_repaired() -> TestResult {
    let world = world(1)?;
    let service = projected_once(&world, "Review.")?;
    world.write_vault(0, REVIEWER, &agent("Reviews", "gpt-9", "all", "Body."))?;
    let before = fs::read(world.claude_agent("reviewer"))?;
    let resolved = service.resolve();
    assert_eq!(codes(&resolved.warnings), Vec::<&str>::new());
    let kinds = kinds(&resolved);
    assert!(
        kinds.iter().all(|kind| *kind == "projection_invalid_definition"),
        "{kinds:?}"
    );
    assert!(resolved.findings[0].message.contains("tier"));
    assert_eq!(fs::read(world.claude_agent("reviewer"))?, before);
    Ok(())
}

#[test]
fn definitions_with_invalid_names_are_reported_by_assessment() -> TestResult {
    let world = world(1)?;
    let service = world.service("config-one")?;
    world.write_vault(0, "registry/agents/Bad Name.md", &reviewer("Body."))?;
    world.write_vault(0, "registry/skills/Bad_Name/SKILL.md", &common::skill("x"))?;
    let assessment = service.assess();
    assert_eq!(
        kinds(&assessment),
        ["projection_invalid_definition", "projection_invalid_definition"]
    );
    assert!(assessment.findings.iter().all(|finding| finding.host.is_none()));
    let warnings = service.refresh();
    assert_eq!(
        codes(&warnings),
        ["projection/invalid-definition", "projection/invalid-definition"]
    );
    Ok(())
}

#[test]
fn a_read_only_agent_projected_to_codex_is_reported_as_unenforced() -> TestResult {
    let world = world(1)?;
    let service = world.service("config-one")?;
    world.write_vault(0, REVIEWER, &agent("Reviews", "balanced", "read-only", "Look only."))?;
    assert_eq!(codes(&service.refresh()), Vec::<&str>::new());
    let assessment = service.assess();
    let unenforced: Vec<_> = assessment
        .findings
        .iter()
        .filter(|finding| finding.kind == FindingKind::CodexReadOnlyUnenforced)
        .collect();
    assert_eq!(unenforced.len(), 1);
    assert_eq!(<&str>::from(unenforced[0].host.ok_or("no host")?), "codex");
    assert!(!unenforced[0].auto_fixable());
    assert_eq!(assessment.findings.len(), 1);
    Ok(())
}

#[test]
fn assessing_a_stale_projection_reports_it_and_changes_nothing() -> TestResult {
    let world = world(1)?;
    let service = projected_once(&world, "Before.")?;
    world.write_vault(0, REVIEWER, &reviewer("After."))?;
    world.write_vault(0, "registry/agents/new-one.md", &reviewer("New."))?;
    let assessment = service.assess();
    assert_eq!(
        kinds(&assessment)
            .iter()
            .filter(|kind| **kind == "projection_stale")
            .count(),
        4
    );
    assert_eq!(codes(&assessment.warnings), Vec::<&str>::new());
    assert!(fs::read_to_string(world.claude_agent("reviewer"))?.contains("Before."));
    assert!(!world.claude_agent("new-one").exists());
    assert_eq!(world.hosts.state.path().join("projection").read_dir()?.count(), 2);
    Ok(())
}

#[test]
fn a_malformed_manifest_leaves_that_host_folder_untouched_and_the_other_projecting() -> TestResult {
    let world = world(1)?;
    let service = projected_once(&world, "Before.")?;
    let claude_manifest = fs::read_dir(world.hosts.state.path().join("projection"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    for path in &claude_manifest {
        let text = fs::read_to_string(path)?;
        // By the temporary folder's own name: the manifest holds its resolved path, which JSON escapes on Windows and
        // which is spelled differently there when the temporary folder is reached through a short name.
        let folder = world.hosts.claude_agents.path().file_name().ok_or("no folder name")?;
        if text.contains(&*folder.to_string_lossy()) {
            fs::write(path, "not json")?;
        }
    }
    world.write_vault(0, REVIEWER, &reviewer("After."))?;
    let warnings = world.project(&service, OpKind::Modify, &[(0, REVIEWER)])?;
    assert_eq!(codes(&warnings), ["projection/manifest-malformed"]);
    assert!(fs::read_to_string(world.claude_agent("reviewer"))?.contains("Before."));
    assert!(fs::read_to_string(world.codex_agent("reviewer"))?.contains("After."));
    Ok(())
}
