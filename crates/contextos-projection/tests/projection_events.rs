//! Projection driven by completed vault writes: real vaults, real host folders, and a real state root.

mod common;

use std::fs;

use common::{REVIEWER, TestResult, codes, reviewer, skill, world};
use contextos_core::{OpKind, ProjectsDefinitions};

#[test]
fn a_created_agent_is_projected_to_both_hosts_and_recorded() -> TestResult {
    let world = world(1)?;
    let service = world.service("config-one")?;
    world.write_vault(0, REVIEWER, &reviewer("Review carefully."))?;
    let warnings = world.project(&service, OpKind::Create, &[(0, REVIEWER)])?;
    assert_eq!(codes(&warnings), Vec::<&str>::new());
    let claude = fs::read_to_string(world.claude_agent("reviewer"))?;
    assert!(
        claude.contains("name: reviewer") && claude.contains("model: sonnet") && claude.contains("Review carefully.")
    );
    let codex = fs::read_to_string(world.codex_agent("reviewer"))?;
    assert!(codex.contains("name = \"reviewer\"") && codex.contains("Review carefully."));
    assert_eq!(world.recorded_vault("reviewer")?.as_deref(), Some("alpha"));
    Ok(())
}

#[test]
fn a_changed_agent_is_rewritten() -> TestResult {
    let world = world(1)?;
    let service = world.service("config-one")?;
    world.write_vault(0, REVIEWER, &reviewer("First."))?;
    world.project(&service, OpKind::Create, &[(0, REVIEWER)])?;
    world.write_vault(0, REVIEWER, &reviewer("Second."))?;
    let warnings = world.project(&service, OpKind::Modify, &[(0, REVIEWER)])?;
    assert_eq!(codes(&warnings), Vec::<&str>::new());
    assert!(fs::read_to_string(world.claude_agent("reviewer"))?.contains("Second."));
    assert!(fs::read_to_string(world.codex_agent("reviewer"))?.contains("Second."));
    Ok(())
}

#[test]
fn a_moved_agent_is_withdrawn_under_its_old_slug_and_projected_under_the_new_one() -> TestResult {
    let world = world(1)?;
    let service = world.service("config-one")?;
    let renamed = "registry/agents/critic.md";
    world.write_vault(0, REVIEWER, &reviewer("Review."))?;
    world.project(&service, OpKind::Create, &[(0, REVIEWER)])?;
    fs::rename(world.vault_file(0, REVIEWER), world.vault_file(0, renamed))?;
    let warnings = world.project(&service, OpKind::Move, &[(0, REVIEWER), (0, renamed)])?;
    assert_eq!(codes(&warnings), Vec::<&str>::new());
    assert!(!world.claude_agent("reviewer").exists() && !world.codex_agent("reviewer").exists());
    assert!(world.claude_agent("critic").exists() && world.codex_agent("critic").exists());
    assert_eq!(world.recorded_vault("reviewer")?, None);
    assert_eq!(world.recorded_vault("critic")?.as_deref(), Some("alpha"));
    Ok(())
}

#[test]
fn a_deleted_agent_is_withdrawn_from_both_hosts() -> TestResult {
    let world = world(1)?;
    let service = world.service("config-one")?;
    world.write_vault(0, REVIEWER, &reviewer("Review."))?;
    world.project(&service, OpKind::Create, &[(0, REVIEWER)])?;
    world.remove_vault(0, REVIEWER)?;
    let warnings = world.project(&service, OpKind::Delete, &[(0, REVIEWER)])?;
    assert_eq!(codes(&warnings), Vec::<&str>::new());
    assert!(!world.claude_agent("reviewer").exists() && !world.codex_agent("reviewer").exists());
    assert_eq!(world.recorded_vault("reviewer")?, None);
    Ok(())
}

#[test]
fn deleting_the_winning_copy_promotes_the_next_vaults_copy() -> TestResult {
    let world = world(2)?;
    let service = world.service("config-one")?;
    world.write_vault(0, REVIEWER, &reviewer("From alpha."))?;
    world.write_vault(1, REVIEWER, &reviewer("From beta."))?;
    world.project(&service, OpKind::Create, &[(0, REVIEWER)])?;
    assert!(fs::read_to_string(world.claude_agent("reviewer"))?.contains("From alpha."));
    world.remove_vault(0, REVIEWER)?;
    let warnings = world.project(&service, OpKind::Delete, &[(0, REVIEWER)])?;
    assert_eq!(codes(&warnings), Vec::<&str>::new());
    assert!(fs::read_to_string(world.claude_agent("reviewer"))?.contains("From beta."));
    assert_eq!(world.recorded_vault("reviewer")?.as_deref(), Some("beta"));
    Ok(())
}

#[test]
fn an_earlier_vault_wins_even_when_a_later_vault_writes_last() -> TestResult {
    let world = world(2)?;
    let service = world.service("config-one")?;
    world.write_vault(0, REVIEWER, &reviewer("From alpha."))?;
    world.write_vault(1, REVIEWER, &reviewer("From beta."))?;
    world.project(&service, OpKind::Create, &[(0, REVIEWER)])?;
    world.project(&service, OpKind::Modify, &[(1, REVIEWER)])?;
    assert!(fs::read_to_string(world.claude_agent("reviewer"))?.contains("From alpha."));
    Ok(())
}

#[test]
fn identical_copies_in_two_vaults_raise_nothing() -> TestResult {
    let world = world(2)?;
    let service = world.service("config-one")?;
    world.write_vault(0, REVIEWER, &reviewer("Same."))?;
    world.write_vault(1, REVIEWER, &reviewer("Same."))?;
    let warnings = world.project(&service, OpKind::Create, &[(1, REVIEWER)])?;
    assert_eq!(codes(&warnings), Vec::<&str>::new());
    assert_eq!(world.recorded_vault("reviewer")?.as_deref(), Some("alpha"));
    let assessment = service.assess();
    assert!(assessment.findings.is_empty(), "{:?}", assessment.findings);
    Ok(())
}

#[test]
fn a_divergent_later_copy_is_shadowed_and_changes_nothing() -> TestResult {
    let world = world(2)?;
    let service = world.service("config-one")?;
    world.write_vault(0, REVIEWER, &reviewer("From alpha."))?;
    world.write_vault(1, REVIEWER, &reviewer("From beta."))?;
    let warnings = world.project(&service, OpKind::Create, &[(0, REVIEWER), (1, REVIEWER)])?;
    assert_eq!(codes(&warnings), Vec::<&str>::new());
    let findings = service.assess().findings;
    let shadowed: Vec<_> = findings
        .iter()
        .filter(|finding| <&str>::from(finding.kind) == "projection_shadowed")
        .collect();
    assert_eq!(shadowed.len(), 2, "one per host: {findings:?}");
    assert!(shadowed.iter().all(|finding| {
        finding.name == "reviewer"
            && finding.vault.as_deref() == Some("alpha")
            && finding.other_vault.as_deref() == Some("beta")
    }));
    assert!(fs::read_to_string(world.claude_agent("reviewer"))?.contains("From alpha."));
    Ok(())
}

#[test]
fn an_invalid_winner_keeps_the_earlier_projection_and_never_falls_through_to_a_later_vault() -> TestResult {
    let world = world(2)?;
    let service = world.service("config-one")?;
    world.write_vault(0, REVIEWER, &reviewer("From alpha."))?;
    world.write_vault(1, REVIEWER, &reviewer("From beta."))?;
    world.project(&service, OpKind::Create, &[(0, REVIEWER)])?;
    let before = fs::read(world.claude_agent("reviewer"))?;
    world.write_vault(
        0,
        REVIEWER,
        &common::agent("Reviews", "gpt-9", "all", "Typo in the model."),
    )?;
    let warnings = world.project(&service, OpKind::Modify, &[(0, REVIEWER)])?;
    assert_eq!(codes(&warnings), ["projection/invalid-definition"]);
    assert!(warnings[0].message.contains("tier") && warnings[0].message.contains("alpha"));
    assert_eq!(fs::read(world.claude_agent("reviewer"))?, before);
    assert_eq!(world.recorded_vault("reviewer")?.as_deref(), Some("alpha"));
    Ok(())
}

#[test]
fn an_invalid_agent_is_not_projected_at_all() -> TestResult {
    let world = world(1)?;
    let service = world.service("config-one")?;
    world.write_vault(
        0,
        REVIEWER,
        &common::agent("Reviews", "balanced", "everything", "Body."),
    )?;
    let warnings = world.project(&service, OpKind::Create, &[(0, REVIEWER)])?;
    assert_eq!(codes(&warnings), ["projection/invalid-definition"]);
    assert!(!world.claude_agent("reviewer").exists() && !world.codex_agent("reviewer").exists());
    Ok(())
}

#[test]
fn a_definition_with_an_invalid_name_warns_and_projects_nothing() -> TestResult {
    let world = world(1)?;
    let service = world.service("config-one")?;
    let path = "registry/agents/Bad Name.md";
    world.write_vault(0, path, &reviewer("Body."))?;
    let warnings = world.project(&service, OpKind::Create, &[(0, path)])?;
    assert_eq!(codes(&warnings), ["projection/invalid-definition"]);
    assert!(warnings[0].message.contains("alpha") && warnings[0].message.contains("Bad Name"));
    assert_eq!(fs::read_dir(world.hosts.claude_agents.path())?.count(), 0);
    Ok(())
}

#[test]
fn paths_outside_the_projected_definitions_are_ignored() -> TestResult {
    let world = world(1)?;
    let service = world.service("config-one")?;
    for path in [
        "notes/reviewer.md",
        "registry/agents/index.md",
        "registry/instructions/global.md",
        "registry/roles/reviewer.md",
    ] {
        world.write_vault(0, path, "text")?;
        assert!(
            world.project(&service, OpKind::Create, &[(0, path)])?.is_empty(),
            "{path}"
        );
    }
    assert_eq!(fs::read_dir(world.hosts.claude_agents.path())?.count(), 0);
    assert_eq!(fs::read_dir(world.hosts.state.path())?.count(), 0);
    Ok(())
}

#[test]
fn removing_a_whole_registry_folder_withdraws_what_it_defined() -> TestResult {
    let world = world(1)?;
    let service = world.service("config-one")?;
    world.write_vault(0, REVIEWER, &reviewer("Review."))?;
    world.write_vault(0, "registry/skills/writing/SKILL.md", &skill("writing"))?;
    world.project(&service, OpKind::Create, &[(0, REVIEWER)])?;
    world.project(&service, OpKind::Create, &[(0, "registry/skills/writing/SKILL.md")])?;
    world.remove_vault(0, "registry")?;
    let warnings = world.project(&service, OpKind::Delete, &[(0, "registry")])?;
    assert_eq!(codes(&warnings), Vec::<&str>::new());
    assert!(!world.claude_agent("reviewer").exists() && !world.claude_skill("writing").exists());
    assert!(!world.codex_agent("reviewer").exists() && !world.codex_skill("writing").exists());
    Ok(())
}

#[test]
fn a_skill_folder_is_projected_to_both_hosts_and_follows_its_files() -> TestResult {
    let world = world(1)?;
    let service = world.service("config-one")?;
    let main = "registry/skills/writing/SKILL.md";
    let extra = "registry/skills/writing/references/style.md";
    world.write_vault(0, main, &skill("writing"))?;
    world.write_vault(0, extra, "Style notes.")?;
    assert_eq!(
        codes(&world.project(&service, OpKind::Create, &[(0, extra)])?),
        Vec::<&str>::new()
    );
    for folder in [world.claude_skill("writing"), world.codex_skill("writing")] {
        assert_eq!(fs::read_to_string(folder.join("SKILL.md"))?, skill("writing"));
        assert_eq!(fs::read_to_string(folder.join("references/style.md"))?, "Style notes.");
    }
    world.remove_vault(0, extra)?;
    assert_eq!(
        codes(&world.project(&service, OpKind::Delete, &[(0, extra)])?),
        Vec::<&str>::new()
    );
    for folder in [world.claude_skill("writing"), world.codex_skill("writing")] {
        assert!(folder.join("SKILL.md").exists() && !folder.join("references").exists());
    }
    world.remove_vault(0, "registry/skills/writing")?;
    let warnings = world.project(&service, OpKind::Delete, &[(0, "registry/skills/writing")])?;
    assert_eq!(codes(&warnings), Vec::<&str>::new());
    assert!(!world.claude_skill("writing").exists() && !world.codex_skill("writing").exists());
    Ok(())
}

#[test]
fn a_skill_without_the_required_frontmatter_is_not_projected() -> TestResult {
    let world = world(1)?;
    let service = world.service("config-one")?;
    let main = "registry/skills/writing/SKILL.md";
    world.write_vault(0, main, "---\nname: writing\n---\n\nNo description.\n")?;
    let warnings = world.project(&service, OpKind::Create, &[(0, main)])?;
    assert_eq!(codes(&warnings), ["projection/invalid-definition"]);
    assert!(!world.claude_skill("writing").exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_link_out_of_the_vault_in_a_source_skill_folder_is_refused_as_a_link() -> TestResult {
    let world = world(1)?;
    let service = world.service("config-one")?;
    let destination = tempfile::tempdir()?;
    fs::write(destination.path().join("secret.txt"), "secret")?;
    world.write_vault(0, "registry/skills/triage/SKILL.md", &skill("triage"))?;
    std::os::unix::fs::symlink(destination.path(), world.vault_file(0, "registry/skills/triage/inner"))?;

    let warnings = world.project(&service, OpKind::Create, &[(0, "registry/skills/triage/SKILL.md")])?;

    assert_eq!(codes(&warnings), ["projection/path-rejected"], "{warnings:?}");
    assert!(!world.claude_skill("triage").exists());
    assert!(!world.codex_skill("triage").exists());
    Ok(())
}

#[test]
fn one_unwritable_host_does_not_stop_the_other_projecting() -> TestResult {
    let world = world(1)?;
    let service = world.service("config-one")?;
    fs::create_dir(world.codex_agent("reviewer"))?;
    world.write_vault(0, REVIEWER, &reviewer("Review."))?;
    world.write_vault(0, "registry/skills/writing/SKILL.md", &skill("writing"))?;
    let warnings = world.project(
        &service,
        OpKind::Create,
        &[(0, REVIEWER), (0, "registry/skills/writing/SKILL.md")],
    )?;
    assert_eq!(codes(&warnings), ["projection/path-rejected"]);
    assert!(warnings[0].message.contains("codex") && warnings[0].message.contains("reviewer"));
    assert!(fs::read_to_string(world.claude_agent("reviewer"))?.contains("Review."));
    assert!(world.codex_skill("writing").join("SKILL.md").exists());
    assert!(world.claude_skill("writing").join("SKILL.md").exists());
    assert!(world.codex_agent("reviewer").is_dir());
    Ok(())
}

#[test]
fn warnings_never_carry_definition_content_or_host_paths() -> TestResult {
    let world = world(1)?;
    let service = world.service("config-one")?;
    fs::create_dir(world.codex_agent("reviewer"))?;
    world.write_vault(0, REVIEWER, &reviewer("A secret instruction."))?;
    let warnings = world.project(&service, OpKind::Create, &[(0, REVIEWER)])?;
    let host_root = world.hosts.codex_agents.path().to_string_lossy().into_owned();
    for warning in &warnings {
        assert!(
            !warning.message.contains("secret") && !warning.message.contains(&host_root),
            "{warning:?}"
        );
    }
    Ok(())
}

#[test]
fn an_unreadable_vault_leaves_the_slug_untouched_with_a_warning() -> TestResult {
    let world = world(2)?;
    let service = world.service("config-one")?;
    world.write_vault(0, REVIEWER, &reviewer("From alpha."))?;
    world.project(&service, OpKind::Create, &[(0, REVIEWER)])?;
    let before = fs::read(world.claude_agent("reviewer"))?;
    // A file where the second vault's agents folder should be makes it unlistable and its definitions unreadable.
    world.write_vault(1, "registry/agents", "not a folder")?;
    world.write_vault(0, REVIEWER, &reviewer("Edited in alpha."))?;
    let warnings = world.project(&service, OpKind::Modify, &[(0, REVIEWER)])?;
    assert_eq!(codes(&warnings), ["projection/read-failed"]);
    assert_eq!(fs::read(world.claude_agent("reviewer"))?, before);
    Ok(())
}

#[test]
fn the_service_is_usable_as_the_routing_port() {
    fn takes_port(_port: &dyn ProjectsDefinitions) {}
    if let Ok(world) = world(1)
        && let Ok(service) = world.service("config-one")
    {
        takes_port(&service);
    }
}

#[test]
fn a_definition_carrying_a_host_tools_field_warns_naming_it_and_is_not_projected() -> TestResult {
    let world = world(1)?;
    let service = world.service("config-one")?;
    world.write_vault(
        0,
        REVIEWER,
        "---\ndescription: Reviews\ntier: fast\ntools: all\n---\n\nBody.\n",
    )?;

    let warnings = world.project(&service, OpKind::Create, &[(0, REVIEWER)])?;

    assert_eq!(codes(&warnings), ["projection/invalid-definition"]);
    assert!(warnings[0].message.contains("(field `tools`"), "{warnings:?}");
    assert!(!world.claude_agent("reviewer").exists() && !world.codex_agent("reviewer").exists());
    Ok(())
}

#[test]
fn a_definition_without_access_is_projected_to_both_hosts_with_no_tool_restriction() -> TestResult {
    let world = world(1)?;
    let service = world.service("config-one")?;
    world.write_vault(
        0,
        REVIEWER,
        "---\ndescription: Reviews\ntier: balanced\n---\n\nReview carefully.\n",
    )?;

    let warnings = world.project(&service, OpKind::Create, &[(0, REVIEWER)])?;

    assert_eq!(codes(&warnings), Vec::<&str>::new());
    let claude = fs::read_to_string(world.claude_agent("reviewer"))?;
    assert!(!claude.contains("\ntools:"), "{claude}");
    let codex = fs::read_to_string(world.codex_agent("reviewer"))?;
    assert!(!codex.contains("shell_tool"), "{codex}");
    Ok(())
}
