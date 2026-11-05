//! A managed `index.md` inside a skill folder is not part of the skill: it is never projected, counted, or compared.

mod common;

use std::fs;

use common::{TestResult, skill, world};
use contextos_core::OpKind;
use contextos_projection::{FindingKind, HostKind};

const SKILL: &str = "registry/skills/triage/SKILL.md";

#[test]
fn a_skills_managed_indexes_at_every_level_are_not_projected() -> TestResult {
    let world = world(1)?;
    let service = world.service("config-one")?;
    world.write_vault(0, SKILL, &skill("triage"))?;
    world.write_vault(0, "registry/skills/triage/index.md", "managed top")?;
    world.write_vault(0, "registry/skills/triage/scripts/run.sh", "echo")?;
    world.write_vault(0, "registry/skills/triage/scripts/index.md", "managed nested")?;

    let warnings = world.project(&service, OpKind::Create, &[(0, SKILL)])?;

    assert!(warnings.is_empty(), "{warnings:?}");
    for host in [world.claude_skill("triage"), world.codex_skill("triage")] {
        assert!(host.join("SKILL.md").is_file());
        assert!(host.join("scripts/run.sh").is_file());
        assert!(!host.join("index.md").exists(), "the top-level index was projected");
        assert!(
            !host.join("scripts/index.md").exists(),
            "the nested index was projected"
        );
    }
    Ok(())
}

#[test]
fn an_index_already_projected_into_a_host_skill_disappears_the_next_time_the_skill_is_projected() -> TestResult {
    let world = world(1)?;
    let service = world.service("config-one")?;
    world.write_vault(0, SKILL, &skill("triage"))?;
    world.project(&service, OpKind::Create, &[(0, SKILL)])?;
    fs::write(
        world.claude_skill("triage").join("index.md"),
        "projected by an older version",
    )?;
    fs::create_dir_all(world.claude_skill("triage").join("scripts"))?;
    fs::write(world.claude_skill("triage").join("scripts/index.md"), "older nested")?;
    world.write_vault(0, SKILL, &skill("triage").replace("How to", "How best to"))?;

    let warnings = world.project(&service, OpKind::Modify, &[(0, SKILL)])?;

    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(world.claude_skill("triage").join("SKILL.md").is_file());
    assert!(!world.claude_skill("triage").join("index.md").exists());
    assert!(!world.claude_skill("triage").join("scripts/index.md").exists());
    Ok(())
}

#[test]
fn an_index_in_a_host_skill_folder_is_not_a_hand_edit_or_a_difference() -> TestResult {
    let world = world(1)?;
    let service = world.service("config-one")?;
    world.write_vault(0, SKILL, &skill("triage"))?;
    world.project(&service, OpKind::Create, &[(0, SKILL)])?;
    fs::write(
        world.claude_skill("triage").join("index.md"),
        "left by an older version",
    )?;

    let assessment = service.assess();

    let skill_findings: Vec<_> = assessment
        .findings
        .iter()
        .filter(|finding| finding.host == Some(HostKind::ClaudeCode) && finding.name == "triage")
        .map(|finding| finding.kind)
        .collect();
    assert!(skill_findings.is_empty(), "{skill_findings:?}");
    assert!(!skill_findings.contains(&FindingKind::ProjectionHandEdited));
    Ok(())
}

#[test]
fn adding_or_changing_a_managed_index_leaves_the_projected_skill_exactly_as_it_was() -> TestResult {
    let world = world(1)?;
    let service = world.service("config-one")?;
    world.write_vault(0, SKILL, &skill("triage"))?;
    world.project(&service, OpKind::Create, &[(0, SKILL)])?;
    let before = fs::metadata(world.claude_skill("triage").join("SKILL.md"))?.modified()?;
    world.write_vault(0, "registry/skills/triage/index.md", "managed")?;

    let assessment = service.assess();

    assert!(assessment.findings.is_empty(), "{:?}", assessment.findings);
    let resolved = service.resolve();
    assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);
    assert_eq!(
        fs::metadata(world.claude_skill("triage").join("SKILL.md"))?.modified()?,
        before
    );
    assert!(!world.claude_skill("triage").join("index.md").exists());
    Ok(())
}
