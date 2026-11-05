//! A write leaves a hand-edited projected agent alone unless the write changes that agent's own source, in which case
//! the edit is replaced and the write's result says so with the warning `projection/hand-edited-replaced`.

mod support;
use serde_json::json;
use support::git_harness::call;
use support::projection_world::{BoxError, ProjectionWorld, agent};

const CRITIC: &str = "registry/agents/critic.md";

/// Projects the critic, then edits its Claude Code file by hand.
fn hand_edited() -> Result<(ProjectionWorld, contextos_mcp::ContextOsServer), BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    world.write_vault(0, CRITIC, &agent("Criticise it."))?;
    let server = world.projecting_server()?;
    assert!(world.claude_agent("critic").is_file(), "the fixture was not projected");
    std::fs::write(world.claude_agent("critic"), "edited by hand\n")?;
    Ok((world, server))
}

/// Writes `content` at `path`, replacing the file when it exists, and returns the write's warnings.
async fn write(server: &contextos_mcp::ContextOsServer, path: &str, content: &str) -> Result<Vec<String>, BoxError> {
    let mut arguments = json!({"path": path, "content": content});
    let existing = call(server, "fs_read_text_file", json!({"path": path})).await?;
    if let Some(hash) = existing
        .structured_content
        .as_ref()
        .and_then(|read| read["content_hash"].as_str())
    {
        arguments["expected_hash"] = json!(hash);
    }
    let result = call(server, "fs_write_file", arguments).await?;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    let structured = result.structured_content.ok_or("no structured result")?;
    Ok(structured["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|warning| warning.as_str().map(str::to_owned))
        .collect())
}

#[tokio::test]
async fn a_write_that_changes_the_source_replaces_the_hand_edit_and_warns_naming_the_host_and_slug()
-> Result<(), BoxError> {
    let (world, server) = hand_edited()?;

    let warnings = write(&server, CRITIC, &agent("Criticise it differently.")).await?;

    let replaced: Vec<&String> = warnings
        .iter()
        .filter(|warning| warning.starts_with("projection/hand-edited-replaced:"))
        .collect();
    assert_eq!(replaced.len(), 1, "{warnings:?}");
    assert!(
        replaced[0].contains("claude") && replaced[0].contains("critic"),
        "{}",
        replaced[0]
    );
    let home = world.home().to_string_lossy().into_owned();
    assert!(
        !replaced[0].contains(&home) && !replaced[0].contains("edited by hand"),
        "{}",
        replaced[0]
    );
    assert!(std::fs::read_to_string(world.claude_agent("critic"))?.contains("Criticise it differently."));
    Ok(())
}

#[tokio::test]
async fn a_write_elsewhere_in_the_registry_leaves_the_hand_edit_in_place_without_a_warning() -> Result<(), BoxError> {
    let (world, server) = hand_edited()?;

    let warnings = write(&server, "registry/agents/other.md", &agent("Do something else.")).await?;

    assert!(
        !warnings
            .iter()
            .any(|warning| warning.starts_with("projection/hand-edited-replaced")),
        "{warnings:?}"
    );
    assert_eq!(
        std::fs::read_to_string(world.claude_agent("critic"))?,
        "edited by hand\n"
    );
    assert!(world.claude_agent("other").is_file(), "the new agent was not projected");
    Ok(())
}
