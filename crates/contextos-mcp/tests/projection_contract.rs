//! Agent host projection through the MCP tools: a definition written, moved, or deleted in a vault reaches the Claude
//! Code and Codex folders under a temporary home, a failure arrives as a warning on a write that still succeeded, a
//! configuration that does not project touches nothing, and the server projects what changed while it was stopped.

mod support;
use contextos_mcp::{ConfigLocation, ContextOsServer, ProjectionConfig};
use rmcp::ServerHandler;
use serde_json::{Value, json};
use support::git_harness::call;
use support::projection_world::{BoxError, ProjectionWorld, agent, skill};

const REVIEWER: &str = "registry/agents/reviewer.md";

async fn structured(server: &ContextOsServer, tool: &'static str, arguments: Value) -> Result<Value, BoxError> {
    let result = call(server, tool, arguments).await?;
    assert_eq!(result.is_error, Some(false), "{tool}: {result:?}");
    result
        .structured_content
        .ok_or_else(|| format!("{tool} omitted its structured result").into())
}

fn warnings(result: &Value) -> Vec<String> {
    result
        .get("warnings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|warning| warning.as_str().map(str::to_owned))
        .collect()
}

#[tokio::test]
async fn writing_an_agent_definition_projects_it_into_both_hosts() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    let server = world.projecting_server()?;

    let result = structured(
        &server,
        "fs_write_file",
        json!({"path": REVIEWER, "content": agent("Review it.")}),
    )
    .await?;

    assert_eq!(warnings(&result), Vec::<String>::new());
    let claude = std::fs::read_to_string(world.claude_agent("reviewer"))?;
    assert!(
        claude.contains("model: sonnet") && claude.contains("Review it."),
        "{claude}"
    );
    let codex = std::fs::read_to_string(world.codex_agent("reviewer"))?;
    assert!(
        codex.contains("name = \"reviewer\"") && codex.contains("Review it."),
        "{codex}"
    );
    Ok(())
}

#[tokio::test]
async fn writing_a_skill_definition_projects_its_folder_into_both_hosts() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    let server = world.projecting_server()?;

    let result = structured(
        &server,
        "fs_write_file",
        json!({"path": "registry/skills/writing/SKILL.md", "content": skill("writing")}),
    )
    .await?;

    assert_eq!(warnings(&result), Vec::<String>::new());
    assert!(world.claude_skill("writing").join("SKILL.md").is_file());
    assert!(world.codex_skill("writing").join("SKILL.md").is_file());
    Ok(())
}

#[tokio::test]
async fn editing_a_definition_replaces_what_the_hosts_hold() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    let server = world.projecting_server()?;
    structured(
        &server,
        "fs_write_file",
        json!({"path": REVIEWER, "content": agent("First.")}),
    )
    .await?;
    let read = structured(&server, "fs_read_text_file", json!({"path": REVIEWER})).await?;
    let hash = read["content_hash"].as_str().ok_or("no content_hash")?.to_owned();

    structured(
        &server,
        "fs_write_file",
        json!({"path": REVIEWER, "content": agent("Second."), "expected_hash": hash}),
    )
    .await?;

    assert!(std::fs::read_to_string(world.claude_agent("reviewer"))?.contains("Second."));
    assert!(std::fs::read_to_string(world.codex_agent("reviewer"))?.contains("Second."));
    Ok(())
}

#[tokio::test]
async fn moving_a_definition_withdraws_the_old_name_and_projects_the_new() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    let server = world.projecting_server()?;
    structured(
        &server,
        "fs_write_file",
        json!({"path": REVIEWER, "content": agent("Review it.")}),
    )
    .await?;

    let result = structured(
        &server,
        "fs_move_file",
        json!({"source": REVIEWER, "destination": "registry/agents/critic.md"}),
    )
    .await?;

    assert_eq!(warnings(&result), Vec::<String>::new());
    assert!(!world.claude_agent("reviewer").exists() && !world.codex_agent("reviewer").exists());
    assert!(world.claude_agent("critic").is_file() && world.codex_agent("critic").is_file());
    Ok(())
}

#[tokio::test]
async fn deleting_a_definition_withdraws_it_from_both_hosts() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    let server = world.projecting_server()?;
    structured(
        &server,
        "fs_write_file",
        json!({"path": REVIEWER, "content": agent("Review it.")}),
    )
    .await?;
    assert!(world.claude_agent("reviewer").is_file());

    let result = structured(&server, "fs_delete_file", json!({"path": REVIEWER})).await?;

    assert_eq!(warnings(&result), Vec::<String>::new());
    assert!(!world.claude_agent("reviewer").exists() && !world.codex_agent("reviewer").exists());
    Ok(())
}

#[tokio::test]
async fn an_invalid_definition_warns_on_a_write_that_still_succeeds() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    let server = world.projecting_server()?;
    let invalid = "---\ndescription: Reviews code\ntier: sonnet\naccess: all\n---\n\nBody.\n";

    let result = structured(&server, "fs_write_file", json!({"path": REVIEWER, "content": invalid})).await?;

    let warnings = warnings(&result);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].starts_with("projection/invalid-definition:"),
        "{warnings:?}"
    );
    assert_eq!(std::fs::read_to_string(world.vault(0).join(REVIEWER))?, invalid);
    assert!(!world.claude_agent("reviewer").exists() && !world.codex_agent("reviewer").exists());
    Ok(())
}

#[tokio::test]
async fn the_first_configured_vault_wins_a_slug_two_vaults_define() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(2)?;
    world.install_hosts()?;
    let server = world.projecting_server()?;
    let second = format!("{}://{REVIEWER}", server_vault_name(&server, 1).await?);
    let first = format!("{}://{REVIEWER}", server_vault_name(&server, 0).await?);
    structured(
        &server,
        "fs_write_file",
        json!({"path": second, "content": agent("From the second.")}),
    )
    .await?;
    assert!(std::fs::read_to_string(world.claude_agent("reviewer"))?.contains("From the second."));

    structured(
        &server,
        "fs_write_file",
        json!({"path": first, "content": agent("From the first.")}),
    )
    .await?;

    assert!(std::fs::read_to_string(world.claude_agent("reviewer"))?.contains("From the first."));
    Ok(())
}

async fn server_vault_name(server: &ContextOsServer, index: usize) -> Result<String, BoxError> {
    let info = structured(server, "vault_info", json!({})).await?;
    info["vaults"][index]["name"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "vault_info omitted a vault name".into())
}

#[tokio::test]
async fn a_named_configuration_without_opt_in_never_touches_a_host_folder() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.write_vault(0, REVIEWER, &agent("Before start."))?;
    let server = ContextOsServer::try_from(world.config(ConfigLocation::Named, None)?)?;

    structured(
        &server,
        "fs_write_file",
        json!({"path": "registry/agents/other.md", "content": agent("Other.")}),
    )
    .await?;

    assert!(
        world.home_is_empty()?,
        "a configuration that does not project created something under the home"
    );
    Ok(())
}

#[tokio::test]
async fn a_named_configuration_that_opts_in_projects() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    let projection = ProjectionConfig {
        enabled: Some(true),
        ..ProjectionConfig::default()
    };
    let server = ContextOsServer::try_from(world.config(ConfigLocation::Named, Some(projection))?)?;

    structured(
        &server,
        "fs_write_file",
        json!({"path": REVIEWER, "content": agent("Review it.")}),
    )
    .await?;

    assert!(world.claude_agent("reviewer").is_file());
    Ok(())
}

#[tokio::test]
async fn a_host_turned_off_in_configuration_is_left_alone() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    let mut projection = ProjectionConfig::default();
    projection.codex.enabled = false;
    let server = ContextOsServer::try_from(world.config(ConfigLocation::PlatformDefault, Some(projection))?)?;

    structured(
        &server,
        "fs_write_file",
        json!({"path": REVIEWER, "content": agent("Review it.")}),
    )
    .await?;

    assert!(world.claude_agent("reviewer").is_file());
    assert!(!world.home().join(".codex/agents").exists() && !world.home().join(".agents").exists());
    Ok(())
}

#[tokio::test]
async fn a_host_that_is_not_installed_is_never_given_a_home_folder() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.write_vault(0, REVIEWER, &agent("Review it."))?;
    std::fs::create_dir_all(world.home().join(".claude"))?;

    let _server = world.projecting_server()?;

    assert!(world.claude_agent("reviewer").is_file());
    assert!(!world.home().join(".codex").exists() && !world.home().join(".agents").exists());
    Ok(())
}

#[tokio::test]
async fn start_up_projects_a_definition_added_while_the_server_was_stopped() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.write_vault(0, REVIEWER, &agent("Added in Obsidian."))?;
    world.write_vault(0, "registry/skills/writing/SKILL.md", &skill("writing"))?;
    assert!(world.home_is_empty()?);
    world.install_hosts()?;

    let _server = world.projecting_server()?;

    assert!(std::fs::read_to_string(world.claude_agent("reviewer"))?.contains("Added in Obsidian."));
    assert!(world.codex_agent("reviewer").is_file());
    assert!(world.claude_skill("writing").join("SKILL.md").is_file());
    Ok(())
}

#[tokio::test]
async fn start_up_withdraws_what_a_definition_removed_while_stopped_left_behind() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    world.write_vault(0, REVIEWER, &agent("Short lived."))?;
    let first = world.projecting_server()?;
    assert!(world.claude_agent("reviewer").is_file());
    drop(first);
    std::fs::remove_file(world.vault(0).join(REVIEWER))?;

    let _second = world.projecting_server()?;

    assert!(!world.claude_agent("reviewer").exists() && !world.codex_agent("reviewer").exists());
    Ok(())
}

#[tokio::test]
async fn start_up_survives_a_host_folder_that_cannot_be_prepared() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    world.write_vault(0, REVIEWER, &agent("Review it."))?;
    std::fs::write(world.home().join(".claude/agents"), b"a file in the way")?;

    let server = world.projecting_server()?;

    assert!(world.codex_agent("reviewer").is_file(), "the other host still projects");
    structured(&server, "vault_info", json!({})).await?;
    Ok(())
}

#[tokio::test]
async fn vault_info_says_vault_order_decides_which_definitions_win() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(2)?;
    world.install_hosts()?;
    let server = world.projecting_server()?;

    let tools = ContextOsServer::catalogue().list_all();
    let description = tools
        .iter()
        .find(|tool| tool.name == "vault_info")
        .and_then(|tool| tool.description.as_deref())
        .ok_or("vault_info has no description")?
        .to_owned();
    let info = structured(&server, "vault_info", json!({})).await?;

    assert!(description.contains("configuration order"), "{description}");
    assert!(description.contains("agent and skill definitions win"), "{description}");
    assert!(
        description.contains("host projection") && description.contains("plugin"),
        "{description}"
    );
    let names = info["vaults"]
        .as_array()
        .ok_or("no vaults")?
        .iter()
        .filter_map(|vault| vault["name"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 2);
    assert!(
        names[0].starts_with("vault0") && names[1].starts_with("vault1"),
        "{names:?}"
    );
    Ok(())
}

#[tokio::test]
async fn the_instructions_keep_their_sentence_and_point_at_vault_info_and_agents_md() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    let server = world.projecting_server()?;

    let instructions = server.get_info().instructions.ok_or("no instructions")?;

    assert!(
        instructions.starts_with("Safe filesystem operations over configured ContextOS vault roots"),
        "{instructions}"
    );
    assert!(
        instructions.contains("vault_info") && instructions.contains("AGENTS.md"),
        "{instructions}"
    );
    assert!(instructions.contains("before working in it"), "{instructions}");
    Ok(())
}

const GLOBAL_INSTRUCTIONS: &str = "registry/instructions/global.md";
const ORIENTATION_BEGIN: &str = "<!-- contextos:orientation:begin -->";
const ORIENTATION_END: &str = "<!-- contextos:orientation:end -->";

fn orientation_block(content: &str) -> String {
    format!("{ORIENTATION_BEGIN}\n{content}{ORIENTATION_END}\n")
}

#[tokio::test]
async fn writing_the_global_instructions_puts_the_block_in_both_hosts_and_keeps_the_operators_content()
-> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    let claude = world.home().join(".claude/CLAUDE.md");
    let codex = world.home().join(".codex/AGENTS.md");
    std::fs::write(&claude, "# My Claude rules\nBe brief.\n")?;
    std::fs::write(&codex, "# My Codex rules\n")?;
    let server = world.projecting_server()?;

    let result = structured(
        &server,
        "fs_write_file",
        json!({"path": GLOBAL_INSTRUCTIONS, "content": "Call vault_info, then read AGENTS.md.\n"}),
    )
    .await?;

    assert_eq!(warnings(&result), Vec::<String>::new());
    assert_eq!(
        std::fs::read_to_string(&claude)?,
        format!(
            "# My Claude rules\nBe brief.\n\n{}",
            orientation_block("Call vault_info, then read AGENTS.md.\n")
        )
    );
    assert_eq!(
        std::fs::read_to_string(&codex)?,
        format!(
            "# My Codex rules\n\n{}",
            orientation_block("Call vault_info, then read AGENTS.md.\n")
        )
    );
    Ok(())
}

#[tokio::test]
async fn the_global_instructions_create_a_missing_instructions_file_in_an_installed_host() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    let server = world.projecting_server()?;

    structured(
        &server,
        "fs_write_file",
        json!({"path": GLOBAL_INSTRUCTIONS, "content": "Orient.\n"}),
    )
    .await?;

    assert_eq!(
        std::fs::read_to_string(world.home().join(".claude/CLAUDE.md"))?,
        orientation_block("Orient.\n")
    );
    assert_eq!(
        std::fs::read_to_string(world.home().join(".codex/AGENTS.md"))?,
        orientation_block("Orient.\n")
    );
    Ok(())
}

#[tokio::test]
async fn the_global_instructions_never_create_the_instructions_file_of_an_uninstalled_host() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    std::fs::create_dir_all(world.home().join(".claude"))?;
    let server = world.projecting_server()?;

    structured(
        &server,
        "fs_write_file",
        json!({"path": GLOBAL_INSTRUCTIONS, "content": "Orient.\n"}),
    )
    .await?;

    assert_eq!(
        std::fs::read_to_string(world.home().join(".claude/CLAUDE.md"))?,
        orientation_block("Orient.\n")
    );
    assert!(
        !world.home().join(".codex").exists(),
        "an uninstalled host must not gain a folder"
    );
    assert!(!world.home().join(".agents").exists());
    Ok(())
}

#[tokio::test]
async fn a_definition_carrying_a_host_model_field_warns_naming_it_and_is_not_projected() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    let server = world.projecting_server()?;
    let carries_model = "---\ndescription: Reviews code\ntier: fast\nmodel: fast\n---\n\nBody.\n";

    let result = structured(
        &server,
        "fs_write_file",
        json!({"path": REVIEWER, "content": carries_model}),
    )
    .await?;

    let warnings = warnings(&result);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].starts_with("projection/invalid-definition:") && warnings[0].contains("(field `model`"),
        "{warnings:?}"
    );
    assert!(!world.claude_agent("reviewer").exists() && !world.codex_agent("reviewer").exists());
    Ok(())
}

#[tokio::test]
async fn a_definition_without_access_is_projected_with_every_tool() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    let server = world.projecting_server()?;
    let without_access = "---\ndescription: Reviews code\ntier: balanced\n---\n\nReview it.\n";

    let result = structured(
        &server,
        "fs_write_file",
        json!({"path": REVIEWER, "content": without_access}),
    )
    .await?;

    assert_eq!(warnings(&result), Vec::<String>::new());
    let claude = std::fs::read_to_string(world.claude_agent("reviewer"))?;
    assert!(
        claude.contains("model: sonnet") && !claude.contains("\ntools:"),
        "an agent with access to everything inherits the session's tools: {claude}"
    );
    Ok(())
}

#[tokio::test]
async fn the_orientation_pointer_is_a_line_of_its_own_after_the_existing_sentence() -> Result<(), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    let server = world.projecting_server()?;

    let instructions = server.get_info().instructions.ok_or("no instructions")?;

    let lines: Vec<&str> = instructions.lines().collect();
    assert!(
        lines.len() >= 2,
        "the pointer is not on a line of its own: {instructions:?}"
    );
    assert!(
        lines[0].starts_with("Safe filesystem operations over configured ContextOS vault roots")
            && !lines[0].contains("vault_info")
            && !lines[0].contains("AGENTS.md"),
        "{instructions:?}"
    );
    assert!(
        lines[1].contains("vault_info") && lines[1].contains("AGENTS.md") && lines[1].contains("before working in it"),
        "{instructions:?}"
    );
    Ok(())
}
