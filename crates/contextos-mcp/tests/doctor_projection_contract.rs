//! `doctor` reports agent host projection findings: one check per finding, each under its stable kind, with its
//! auto-fixable classification and server-performed remediation, and never a full home path or any content.

mod support;
use contextos_mcp::{ConfigLocation, ContextOsServer};
use support::projection_doctor::{
    GLOBAL_INSTRUCTIONS, OPERATOR_FILE, REVIEWER, assert_hides_home, checks_of_kind, doctor, kind_set,
    projection_kinds, read_only_agent, remediation, snapshot, structured, vault_name, words,
};
use support::projection_world::{BoxError, ProjectionWorld, agent, skill};

const INVALID: &str = "---\ndescription: Reviews code\ntier: sonnet\naccess: all\n---\n\nBody.\n";

fn is_manual(check: &serde_json::Value) -> bool {
    check["auto_fixable"] == false && remediation(check).is_none() && check["status"] == "fail"
}

#[tokio::test]
async fn a_definition_changed_since_it_was_projected_is_reported_stale_for_each_host() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    world.write_vault(0, REVIEWER, &agent("Review it."))?;
    let server = world.projecting_server()?;
    world.write_vault(0, REVIEWER, &agent("Review it differently."))?;

    let report = doctor(&server).await?;

    let stale = checks_of_kind(&report, "projection_stale");
    assert_eq!(stale.len(), 2, "{report}");
    let name = vault_name(&server, 0).await?;
    for check in &stale {
        assert_eq!(check["auto_fixable"], true, "{check}");
        assert_eq!(remediation(check), Some("reproject"), "{check}");
        assert_eq!(check["status"], "fail", "{check}");
        assert_eq!(check["vault_name"], name.as_str(), "{check}");
        assert!(words(check).contains("reviewer"), "{check}");
    }
    assert!(stale.iter().any(|check| words(check).contains("claude")), "{stale:?}");
    assert!(stale.iter().any(|check| words(check).contains("codex")), "{stale:?}");
    assert_eq!(projection_kinds(&report), kind_set(&["projection_stale"]));
    assert_hides_home(&report, &world);
    Ok(())
}

#[tokio::test]
async fn a_projected_file_edited_by_hand_is_reported_hand_edited_and_says_reprojecting_replaces_it()
-> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    world.write_vault(0, REVIEWER, &agent("Review it."))?;
    let server = world.projecting_server()?;
    std::fs::write(world.claude_agent("reviewer"), "my own edit\n")?;

    let report = doctor(&server).await?;

    let edited = checks_of_kind(&report, "projection_hand_edited");
    assert_eq!(edited.len(), 1, "{report}");
    assert_eq!(edited[0]["auto_fixable"], true, "{}", edited[0]);
    assert_eq!(remediation(edited[0]), Some("reproject"), "{}", edited[0]);
    let text = words(edited[0]);
    assert!(text.contains("reviewer") && text.contains("claude"), "{text}");
    assert!(
        text.contains("replaces"),
        "the message must say reprojecting replaces the edit: {text}"
    );
    assert_eq!(projection_kinds(&report), kind_set(&["projection_hand_edited"]));
    assert_hides_home(&report, &world);
    Ok(())
}

#[tokio::test]
async fn an_entry_whose_definition_was_deleted_is_reported_orphaned_for_each_host() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    world.write_vault(0, REVIEWER, &agent("Review it."))?;
    let server = world.projecting_server()?;
    std::fs::remove_file(world.vault(0).join(REVIEWER))?;

    let report = doctor(&server).await?;

    let orphaned = checks_of_kind(&report, "projection_orphaned");
    assert_eq!(orphaned.len(), 2, "{report}");
    for check in &orphaned {
        assert_eq!(check["auto_fixable"], true, "{check}");
        assert_eq!(remediation(check), Some("withdraw_projection"), "{check}");
        assert!(words(check).contains("reviewer"), "{check}");
    }
    assert!(orphaned.iter().any(|check| words(check).contains("claude")));
    assert!(orphaned.iter().any(|check| words(check).contains("codex")));
    assert_eq!(projection_kinds(&report), kind_set(&["projection_orphaned"]));
    assert_hides_home(&report, &world);
    Ok(())
}

#[tokio::test]
async fn a_definition_that_fails_the_projection_contract_is_reported_invalid_and_not_auto_fixable()
-> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    world.write_vault(0, REVIEWER, INVALID)?;
    let server = world.projecting_server()?;

    let report = doctor(&server).await?;

    let invalid = checks_of_kind(&report, "projection_invalid_definition");
    assert_eq!(invalid.len(), 1, "{report}");
    assert!(is_manual(invalid[0]), "{}", invalid[0]);
    assert!(
        invalid[0]["action"].is_string(),
        "an operator action is required: {}",
        invalid[0]
    );
    assert!(words(invalid[0]).contains("reviewer"), "{}", invalid[0]);
    assert_eq!(invalid[0]["vault_name"], vault_name(&server, 0).await?.as_str());
    assert_eq!(projection_kinds(&report), kind_set(&["projection_invalid_definition"]));
    assert_hides_home(&report, &world);
    Ok(())
}

#[tokio::test]
async fn a_later_vaults_divergent_definition_is_reported_shadowed_and_not_auto_fixable() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(2)?;
    world.install_hosts()?;
    world.write_vault(0, REVIEWER, &agent("From the first."))?;
    world.write_vault(1, REVIEWER, &agent("From the second."))?;
    let server = world.projecting_server()?;

    let report = doctor(&server).await?;

    let shadowed = checks_of_kind(&report, "projection_shadowed");
    assert!(!shadowed.is_empty(), "{report}");
    for check in &shadowed {
        assert!(is_manual(check), "{check}");
        assert!(words(check).contains("reviewer"), "{check}");
        assert!(
            check["vault_name"]
                .as_str()
                .is_some_and(|name| name.starts_with("vault")),
            "a finding about a source definition names a source vault: {check}"
        );
    }
    assert_eq!(projection_kinds(&report), kind_set(&["projection_shadowed"]));
    assert_hides_home(&report, &world);
    Ok(())
}

#[tokio::test]
async fn a_file_the_server_did_not_project_at_a_projected_slug_is_reported_as_a_collision_and_left_alone()
-> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    world.write_vault(0, REVIEWER, &agent("Review it."))?;
    std::fs::create_dir_all(world.home().join(".claude/agents"))?;
    std::fs::write(world.claude_agent("reviewer"), OPERATOR_FILE)?;
    let server = world.projecting_server()?;
    let before = snapshot(&world.home().join(".claude"))?;

    let report = doctor(&server).await?;

    let collisions = checks_of_kind(&report, "projection_unowned_collision");
    assert_eq!(collisions.len(), 1, "{report}");
    assert!(is_manual(collisions[0]), "{}", collisions[0]);
    let text = words(collisions[0]);
    assert!(text.contains("reviewer") && text.contains("claude"), "{text}");
    assert_eq!(collisions[0]["vault_name"], vault_name(&server, 0).await?.as_str());
    assert_eq!(projection_kinds(&report), kind_set(&["projection_unowned_collision"]));
    assert_eq!(
        snapshot(&world.home().join(".claude"))?,
        before,
        "doctor must change nothing"
    );
    assert_hides_home(&report, &world);
    Ok(())
}

#[tokio::test]
async fn an_entry_recorded_as_another_configurations_is_reported_foreign_and_not_auto_fixable() -> Result<(), BoxError>
{
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    world.write_vault(0, REVIEWER, &agent("Review it."))?;
    drop(world.projecting_server()?);
    let other = world.projecting_server_for("other.toml")?;

    let report = doctor(&other).await?;

    let foreign = checks_of_kind(&report, "projection_foreign_owner");
    assert_eq!(foreign.len(), 2, "one per host: {report}");
    for check in &foreign {
        assert!(is_manual(check), "{check}");
        assert!(words(check).contains("reviewer"), "{check}");
    }
    assert!(foreign.iter().any(|check| words(check).contains("claude")));
    assert!(foreign.iter().any(|check| words(check).contains("codex")));
    assert_eq!(projection_kinds(&report), kind_set(&["projection_foreign_owner"]));
    assert_hides_home(&report, &world);
    Ok(())
}

#[tokio::test]
async fn global_instructions_that_changed_since_the_block_was_placed_are_reported_for_each_host() -> Result<(), BoxError>
{
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    world.write_vault(0, GLOBAL_INSTRUCTIONS, "First orientation.\n")?;
    let server = world.projecting_server()?;
    world.write_vault(0, GLOBAL_INSTRUCTIONS, "Second orientation.\n")?;

    let report = doctor(&server).await?;

    let blocks = checks_of_kind(&report, "orientation_block");
    assert_eq!(blocks.len(), 2, "{report}");
    let name = vault_name(&server, 0).await?;
    for check in &blocks {
        assert_eq!(check["auto_fixable"], true, "{check}");
        assert_eq!(remediation(check), Some("restore_orientation"), "{check}");
        assert_eq!(check["vault_name"], name.as_str(), "{check}");
    }
    assert!(blocks.iter().any(|check| words(check).contains("claude")));
    assert!(blocks.iter().any(|check| words(check).contains("codex")));
    assert_eq!(projection_kinds(&report), kind_set(&["orientation_block"]));
    assert_hides_home(&report, &world);
    Ok(())
}

#[tokio::test]
async fn a_malformed_orientation_block_is_reported_and_not_auto_fixable() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    world.write_vault(0, GLOBAL_INSTRUCTIONS, "Orient.\n")?;
    let server = world.projecting_server()?;
    let claude = world.home().join(".claude/CLAUDE.md");
    std::fs::write(&claude, "# Mine\n<!-- contextos:orientation:begin -->\nno end marker\n")?;

    let report = doctor(&server).await?;

    let blocks = checks_of_kind(&report, "orientation_block");
    assert_eq!(blocks.len(), 1, "only the malformed host is reported: {report}");
    assert!(is_manual(blocks[0]), "{}", blocks[0]);
    assert!(words(blocks[0]).contains("claude"), "{}", blocks[0]);
    assert_eq!(
        std::fs::read_to_string(&claude)?,
        "# Mine\n<!-- contextos:orientation:begin -->\nno end marker\n",
        "doctor must change nothing"
    );
    assert_hides_home(&report, &world);
    Ok(())
}

#[tokio::test]
async fn a_read_only_agent_projected_to_codex_is_reported_as_unenforced_there() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    world.write_vault(0, REVIEWER, &read_only_agent("Look only."))?;
    let server = world.projecting_server()?;

    let report = doctor(&server).await?;

    let unenforced = checks_of_kind(&report, "codex_read_only_unenforced");
    assert_eq!(unenforced.len(), 1, "{report}");
    assert!(is_manual(unenforced[0]), "{}", unenforced[0]);
    let text = words(unenforced[0]);
    assert!(text.contains("reviewer") && text.contains("codex"), "{text}");
    assert_eq!(projection_kinds(&report), kind_set(&["codex_read_only_unenforced"]));
    assert_hides_home(&report, &world);
    Ok(())
}

#[tokio::test]
async fn an_override_file_beside_codexs_instructions_is_reported_and_names_no_single_vault() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    std::fs::write(world.home().join(".codex/AGENTS.override.md"), "Override.\n")?;
    let server = world.projecting_server()?;

    let report = doctor(&server).await?;

    let hidden = checks_of_kind(&report, "codex_override_hides_agents");
    assert_eq!(hidden.len(), 1, "{report}");
    assert!(is_manual(hidden[0]), "{}", hidden[0]);
    assert!(words(hidden[0]).contains("agents.override.md"), "{}", hidden[0]);
    assert!(
        hidden[0].get("vault_name").is_none(),
        "a finding that concerns no single vault omits vault_name: {}",
        hidden[0]
    );
    assert_eq!(projection_kinds(&report), kind_set(&["codex_override_hides_agents"]));
    assert_hides_home(&report, &world);
    Ok(())
}

#[tokio::test]
async fn with_no_vault_configured_every_recorded_entry_is_reported_orphaned_without_a_vault_name()
-> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    world.write_vault(0, REVIEWER, &agent("Review it."))?;
    world.write_vault(0, "registry/skills/writing/SKILL.md", &skill("writing"))?;
    drop(world.projecting_server()?);
    let mut config = world.config(ConfigLocation::PlatformDefault, None)?;
    config.vaults.clear();
    let server = ContextOsServer::try_from(config)?;

    let report = doctor(&server).await?;

    let orphaned = checks_of_kind(&report, "projection_orphaned");
    assert_eq!(
        orphaned.len(),
        4,
        "an agent and a skill for each of two hosts: {report}"
    );
    for check in &orphaned {
        assert_eq!(check["auto_fixable"], true, "{check}");
        assert_eq!(remediation(check), Some("withdraw_projection"), "{check}");
        assert!(
            check.get("vault_name").is_none(),
            "no vault is configured, so none can be named: {check}"
        );
    }
    assert_eq!(projection_kinds(&report), kind_set(&["projection_orphaned"]));
    assert_hides_home(&report, &world);
    Ok(())
}

#[tokio::test]
async fn a_configuration_that_does_not_project_reports_no_projection_kind_and_touches_no_host_folder()
-> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    world.write_vault(0, REVIEWER, &agent("Review it."))?;
    world.write_vault(0, GLOBAL_INSTRUCTIONS, "Orient.\n")?;
    let projecting = world.projecting_server()?;
    std::fs::write(world.claude_agent("reviewer"), "my own edit\n")?;
    let enabled = doctor(&projecting).await?;
    assert_eq!(
        checks_of_kind(&enabled, "projection_hand_edited").len(),
        1,
        "the fixture must show a finding when projection is on: {enabled}"
    );
    drop(projecting);
    let before = snapshot(world.home())?;
    let server = ContextOsServer::try_from(world.config(ConfigLocation::Named, None)?)?;

    let report = doctor(&server).await?;

    assert_eq!(projection_kinds(&report), kind_set(&[]), "{report}");
    assert_eq!(snapshot(world.home())?, before, "nothing in the home may change");
    let again = structured(&server, "doctor", serde_json::json!({})).await?;
    assert_eq!(projection_kinds(&again), kind_set(&[]));
    Ok(())
}
