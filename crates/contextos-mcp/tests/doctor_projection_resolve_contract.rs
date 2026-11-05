//! `doctor_resolve` repairs the auto-fixable agent host projection findings and only those: it re-projects stale and
//! hand-edited entries, withdraws orphaned ones, restores the orientation block, previews the same outcomes on a dry
//! run, and is idempotent. The tool's description discloses what each repair replaces or removes.

mod support;
use contextos_mcp::{ConfigLocation, ContextOsServer};
use support::projection_doctor::{
    GLOBAL_INSTRUCTIONS, OPERATOR_FILE, OVERRIDE_FILE, REVIEWER, assert_hides_home, checks_of_kind, doctor,
    doctor_resolve, drifted_world, kind_set, outcomes_of_kind, projection_kinds, projection_outcomes, snapshot,
    vault_name,
};
use support::projection_world::{BoxError, ProjectionWorld, agent, skill};

const AUTO: [&str; 4] = [
    "projection_stale",
    "projection_hand_edited",
    "projection_orphaned",
    "orientation_block",
];
const MANUAL: [&str; 3] = [
    "projection_invalid_definition",
    "projection_unowned_collision",
    "codex_override_hides_agents",
];

#[tokio::test]
async fn the_tool_description_names_the_projection_remediations_and_what_each_replaces_or_removes()
-> Result<(), BoxError> {
    let tools = ContextOsServer::catalogue().list_all();
    let description = tools
        .iter()
        .find(|tool| tool.name == "doctor_resolve")
        .and_then(|tool| tool.description.as_deref())
        .ok_or("doctor_resolve has no description")?
        .to_lowercase();

    for remediation in ["reproject", "withdraw_projection", "restore_orientation"] {
        assert!(
            description.contains(remediation),
            "missing {remediation}: {description}"
        );
    }
    assert!(
        description.contains("replaces a hand edit"),
        "re-projecting must disclose that it replaces a hand edit: {description}"
    );
    assert!(
        description.contains("removes a projected"),
        "withdrawing must disclose that it removes a projected entry: {description}"
    );
    Ok(())
}

#[tokio::test]
async fn resolving_repairs_every_auto_fixable_projection_finding_and_never_a_manual_one() -> Result<(), BoxError> {
    let drifted = drifted_world()?;
    let (world, server) = (&drifted.world, &drifted.server);
    let before = doctor(server).await?;
    assert_eq!(
        projection_kinds(&before),
        kind_set(&[
            "projection_stale",
            "projection_hand_edited",
            "projection_orphaned",
            "orientation_block",
            "projection_invalid_definition",
            "projection_unowned_collision",
            "codex_override_hides_agents",
        ]),
        "{before}"
    );
    let operator_before = std::fs::read(world.claude_agent("mine"))?;

    let result = doctor_resolve(server, false).await?;

    let outcomes = projection_outcomes(&result);
    for (kind, count) in [
        ("projection_stale", 2),
        ("projection_hand_edited", 1),
        ("projection_orphaned", 2),
        ("orientation_block", 2),
    ] {
        assert_eq!(outcomes_of_kind(&result, kind).len(), count, "{kind}: {result}");
    }
    assert_eq!(outcomes.len(), 7, "one outcome per auto-fixable finding: {outcomes:?}");
    for outcome in &outcomes {
        assert_eq!(outcome["resolved"], true, "{outcome}");
        assert!(
            outcome["message"].as_str().is_some_and(|text| !text.is_empty()),
            "{outcome}"
        );
    }
    for kind in MANUAL {
        assert!(
            outcomes_of_kind(&result, kind).is_empty(),
            "{kind} must never be acted on"
        );
    }
    let name = vault_name(server, 0).await?;
    for outcome in outcomes_of_kind(&result, "projection_stale") {
        assert_eq!(outcome["vault_name"], name.as_str(), "{outcome}");
        assert!(outcome["vault_path"].is_string(), "{outcome}");
    }

    assert!(std::fs::read_to_string(world.claude_agent("reviewer"))?.contains("Review it differently."));
    assert!(std::fs::read_to_string(world.codex_agent("reviewer"))?.contains("Review it differently."));
    assert_eq!(std::fs::read(world.claude_agent("critic"))?, drifted.critic_projected);
    assert!(!world.claude_agent("retired").exists() && !world.codex_agent("retired").exists());
    for instructions in [".claude/CLAUDE.md", ".codex/AGENTS.md"] {
        let text = std::fs::read_to_string(world.home().join(instructions))?;
        assert!(text.contains("Second orientation."), "{instructions}: {text}");
        assert!(!text.contains("First orientation."), "{instructions}: {text}");
    }
    assert_eq!(std::fs::read(world.claude_agent("mine"))?, operator_before);
    assert_eq!(std::fs::read_to_string(world.claude_agent("mine"))?, OPERATOR_FILE);
    assert_eq!(
        std::fs::read_to_string(world.home().join(".codex/AGENTS.override.md"))?,
        OVERRIDE_FILE
    );
    assert!(
        std::fs::read_to_string(world.vault(0).join("registry/agents/broken.md"))?.contains("Body."),
        "no vault file may change"
    );

    let report = &result["report"];
    assert_eq!(
        projection_kinds(report),
        kind_set(&MANUAL),
        "the fresh report lists only what needs the operator: {report}"
    );
    for kind in MANUAL {
        for check in checks_of_kind(report, kind) {
            assert_eq!(check["auto_fixable"], false, "{check}");
        }
    }
    assert_hides_home(&result, world);
    Ok(())
}

#[tokio::test]
async fn resolving_a_second_time_changes_nothing_and_reports_no_projection_outcome() -> Result<(), BoxError> {
    let drifted = drifted_world()?;
    let (world, server) = (&drifted.world, &drifted.server);
    let first = doctor_resolve(server, false).await?;
    assert_eq!(
        projection_outcomes(&first).len(),
        7,
        "the first run must repair: {first}"
    );
    let after_first = snapshot(world.home())?;

    let second = doctor_resolve(server, false).await?;

    assert!(projection_outcomes(&second).is_empty(), "{second}");
    assert_eq!(snapshot(world.home())?, after_first, "a repeat run must be a no-op");
    assert_eq!(projection_kinds(&second["report"]), kind_set(&MANUAL));
    Ok(())
}

#[tokio::test]
async fn a_dry_run_plans_the_outcomes_a_resolution_performs_and_changes_nothing_on_disk() -> Result<(), BoxError> {
    let drifted = drifted_world()?;
    let (world, server) = (&drifted.world, &drifted.server);
    let host_files = snapshot(world.home())?;

    let planned = doctor_resolve(server, true).await?;

    assert_eq!(
        snapshot(world.home())?,
        host_files,
        "a dry run must not change the home"
    );
    let plan = projection_outcomes(&planned);
    assert_eq!(plan.len(), 7, "{planned}");
    for outcome in &plan {
        assert_eq!(outcome["resolved"], false, "a plan has resolved nothing: {outcome}");
        assert!(
            outcome["message"].as_str().is_some_and(|text| text.contains("would")),
            "{outcome}"
        );
    }
    let kinds_after_plan = projection_kinds(&planned["report"]);
    assert!(
        AUTO.iter().all(|kind| kinds_after_plan.contains(*kind)),
        "a dry run repairs nothing, so every finding remains: {kinds_after_plan:?}"
    );

    let performed = doctor_resolve(server, false).await?;

    let identify = |outcome: &&serde_json::Value| {
        (
            outcome["kind"].clone(),
            outcome["subject"].clone(),
            outcome["vault_name"].clone(),
            outcome["vault_path"].clone(),
        )
    };
    assert_eq!(
        plan.iter().map(identify).collect::<Vec<_>>(),
        projection_outcomes(&performed).iter().map(identify).collect::<Vec<_>>(),
        "the dry run plans exactly the outcomes the resolution then reports"
    );
    Ok(())
}

#[tokio::test]
async fn with_no_vault_configured_resolving_withdraws_every_recorded_entry_and_names_no_vault() -> Result<(), BoxError>
{
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    world.write_vault(0, REVIEWER, &agent("Review it."))?;
    world.write_vault(0, "registry/skills/writing/SKILL.md", &skill("writing"))?;
    world.write_vault(0, GLOBAL_INSTRUCTIONS, "Orient.\n")?;
    drop(world.projecting_server()?);
    let mut config = world.config(ConfigLocation::PlatformDefault, None)?;
    config.vaults.clear();
    let server = ContextOsServer::try_from(config)?;
    let claude_instructions = std::fs::read(world.home().join(".claude/CLAUDE.md"))?;

    let result = doctor_resolve(&server, false).await?;

    let withdrawn = outcomes_of_kind(&result, "projection_orphaned");
    assert_eq!(withdrawn.len(), 4, "{result}");
    for outcome in &withdrawn {
        assert_eq!(outcome["resolved"], true, "{outcome}");
        assert!(outcome.get("vault_name").is_none(), "{outcome}");
        assert!(outcome.get("vault_path").is_none(), "{outcome}");
    }
    assert!(!world.claude_agent("reviewer").exists() && !world.codex_agent("reviewer").exists());
    assert!(!world.claude_skill("writing").exists() && !world.codex_skill("writing").exists());
    assert_eq!(
        std::fs::read(world.home().join(".claude/CLAUDE.md"))?,
        claude_instructions,
        "with no vault configured nothing can be said of the orientation, so its block stays"
    );
    assert_eq!(checks_of_kind(&result["report"], "projection_orphaned").len(), 0);
    assert_eq!(projection_outcomes(&doctor_resolve(&server, false).await?).len(), 0);
    Ok(())
}

#[tokio::test]
async fn a_configuration_that_does_not_project_resolves_no_projection_finding_and_touches_no_host_folder()
-> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    world.write_vault(0, REVIEWER, &agent("Review it."))?;
    let projecting = world.projecting_server()?;
    std::fs::write(world.claude_agent("reviewer"), "my own edit\n")?;
    let enabled = doctor_resolve(&projecting, true).await?;
    assert_eq!(
        outcomes_of_kind(&enabled, "projection_hand_edited").len(),
        1,
        "the fixture must plan a repair when projection is on: {enabled}"
    );
    drop(projecting);
    let before = snapshot(world.home())?;
    let server = ContextOsServer::try_from(world.config(ConfigLocation::Named, None)?)?;

    let result = doctor_resolve(&server, false).await?;

    assert!(projection_outcomes(&result).is_empty(), "{result}");
    assert_eq!(projection_kinds(&result["report"]), kind_set(&[]));
    assert_eq!(snapshot(world.home())?, before, "nothing in the home may change");
    Ok(())
}
