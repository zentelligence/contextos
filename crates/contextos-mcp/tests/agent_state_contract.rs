//! Contract tests for `agent_state_change`: real `.contextos/team.toml`
//! reads/writes and `memory/events/` record creation through the actual
//! MCP tool handler, against a real temporary vault. No mocking, per
//! `testing.md`.

mod support;
use contextos_mcp::ContextOsServer;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, json};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

fn write(dir: &std::path::Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

const TEAM_TOML: &str = "[[member]]\nslug = \"atlas\"\nname = \"Atlas\"\ndomain = \"Finance\"\n";

#[tokio::test]
async fn a_first_transition_updates_the_team_file_and_writes_one_event_record_with_no_from_state()
-> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write(vault.path(), ".contextos/team.toml", TEAM_TOML)?;
    write(vault.path(), "registry/agents/atlas.md", "# Atlas\n")?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;

    let result = call_tool(
        server,
        "agent_state_change",
        serde_json::from_value(
            json!({"agent": "atlas", "state": "working", "reason": "Picked up the ledger reconciliation task"}),
        )?,
    )
    .await?;
    assert_eq!(result.is_error, Some(false));
    let content = result
        .structured_content
        .as_ref()
        .ok_or("agent_state_change omitted structured content")?;
    assert_eq!(content.get("agent"), Some(&json!("atlas")));
    assert_eq!(content.get("previous_state"), None);
    assert_eq!(content.get("state"), Some(&json!("working")));
    let event_path = content
        .get("event_path")
        .and_then(serde_json::Value::as_str)
        .ok_or("agent_state_change omitted event_path")?;
    assert!(event_path.starts_with("memory/events/"));
    assert!(event_path.ends_with("-atlas.md"));

    let team_file = std::fs::read_to_string(vault.path().join(".contextos/team.toml"))?;
    assert!(team_file.contains("state = \"working\""));
    assert!(team_file.contains("name = \"Atlas\""), "unrelated fields must survive");

    let event_content = std::fs::read_to_string(vault.path().join(event_path))?;
    assert!(
        event_content.starts_with("---\ntype: event\nkind: member.state-changed\nseverity: info\nat: "),
        "{event_content}"
    );
    assert!(event_content.contains("\nentity: 'vault'\nsubject: '[[registry/agents/atlas|atlas]]'\nsubject_path: 'registry/agents/atlas.md'\nmember: 'atlas'\n"));
    assert!(!event_content.contains("from:"), "{event_content}");
    assert!(event_content.contains("\nto: 'working'\n"), "{event_content}");
    assert!(event_content.contains("\nreason: 'Picked up the ledger reconciliation task'\n"));
    for old_field in ["agent-event", "from_state", "to_state", "kind: state-change"] {
        assert!(!event_content.contains(old_field), "{old_field}: {event_content}");
    }
    Ok(())
}

/// The real bug this test guards: an earlier version of the tool's own record type declared only the fields it
/// itself reads or writes, so any other operator-authored field (a human team member's own `type`) silently
/// vanished from `.contextos/team.toml` the moment `agent_state_change` next wrote the file.
#[tokio::test]
async fn a_transition_preserves_a_human_team_members_own_type_field() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write(
        vault.path(),
        ".contextos/team.toml",
        "[[member]]\nslug = \"piper\"\nname = \"Piper\"\ntype = \"human\"\nteam = \"business\"\n",
    )?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;

    let result = call_tool(
        server,
        "agent_state_change",
        serde_json::from_value(json!({"agent": "piper", "state": "working"}))?,
    )
    .await?;
    assert_eq!(result.is_error, Some(false));

    let team_file = std::fs::read_to_string(vault.path().join(".contextos/team.toml"))?;
    assert!(team_file.contains("type = \"human\""), "{team_file}");
    assert!(team_file.contains("team = \"business\""), "{team_file}");
    assert!(team_file.contains("state = \"working\""), "{team_file}");
    Ok(())
}

#[tokio::test]
async fn a_second_transition_reports_the_prior_state_and_carries_it_into_the_event_record() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write(vault.path(), ".contextos/team.toml", TEAM_TOML)?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;

    call_tool(
        server.clone(),
        "agent_state_change",
        serde_json::from_value(json!({"agent": "atlas", "state": "working"}))?,
    )
    .await?;
    let second = call_tool(
        server,
        "agent_state_change",
        serde_json::from_value(json!({"agent": "atlas", "state": "blocked"}))?,
    )
    .await?;
    assert_eq!(second.is_error, Some(false));
    let content = second
        .structured_content
        .as_ref()
        .ok_or("agent_state_change omitted structured content")?;
    assert_eq!(content.get("previous_state"), Some(&json!("working")));
    assert_eq!(content.get("state"), Some(&json!("blocked")));

    let event_path = content
        .get("event_path")
        .and_then(serde_json::Value::as_str)
        .ok_or("agent_state_change omitted event_path")?;
    let event_content = std::fs::read_to_string(vault.path().join(event_path))?;
    assert!(
        event_content.contains("\nseverity: warning\n"),
        "becoming blocked draws attention"
    );
    assert!(event_content.contains("\nfrom: 'working'\n"), "{event_content}");
    assert!(event_content.contains("\nto: 'blocked'\n"), "{event_content}");
    Ok(())
}

#[tokio::test]
async fn a_member_with_no_definition_note_gets_an_event_with_no_subject() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write(vault.path(), ".contextos/team.toml", TEAM_TOML)?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;

    let result = call_tool(
        server,
        "agent_state_change",
        serde_json::from_value(json!({"agent": "atlas", "state": "idle"}))?,
    )
    .await?;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    let event_path = result
        .structured_content
        .as_ref()
        .and_then(|content| content.get("event_path"))
        .and_then(serde_json::Value::as_str)
        .ok_or("agent_state_change omitted event_path")?
        .to_owned();
    assert!(event_path.ends_with("-member-state-changed.md"), "{event_path}");
    let event_content = std::fs::read_to_string(vault.path().join(event_path))?;
    assert!(!event_content.contains("subject"), "{event_content}");
    Ok(())
}

#[tokio::test]
async fn an_unknown_agent_slug_is_rejected_and_the_team_file_is_left_unchanged() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    write(vault.path(), ".contextos/team.toml", TEAM_TOML)?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;

    let result = call_tool(
        server,
        "agent_state_change",
        serde_json::from_value(json!({"agent": "nonexistent", "state": "working"}))?,
    )
    .await?;
    assert_eq!(result.is_error, Some(true));
    let content = result
        .structured_content
        .as_ref()
        .ok_or("agent_state_change omitted structured content")?;
    assert_eq!(content.get("code"), Some(&json!("agent/not-found")));

    let team_file = std::fs::read_to_string(vault.path().join(".contextos/team.toml"))?;
    assert_eq!(team_file, TEAM_TOML);
    Ok(())
}

#[tokio::test]
async fn a_missing_team_file_is_rejected_as_not_configured_not_a_generic_read_error() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;

    let result = call_tool(
        server,
        "agent_state_change",
        serde_json::from_value(json!({"agent": "atlas", "state": "working"}))?,
    )
    .await?;
    assert_eq!(result.is_error, Some(true));
    let content = result
        .structured_content
        .as_ref()
        .ok_or("agent_state_change omitted structured content")?;
    assert_eq!(content.get("code"), Some(&json!("agent/not-configured")));
    Ok(())
}

async fn call_tool(
    server: ContextOsServer,
    name: &'static str,
    arguments: Map<String, serde_json::Value>,
) -> Result<CallToolResult, BoxError> {
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server_handle = tokio::spawn(async move {
        server.serve(server_transport).await?.waiting().await?;
        Ok::<(), BoxError>(())
    });
    let mut client = ().serve(client_transport).await?;
    let result = client
        .call_tool(CallToolRequestParams::new(name).with_arguments(arguments))
        .await;
    client.close().await?;
    server_handle.await??;
    Ok(result?)
}
