//! Only the server writes under `memory/events/`: every other mutating tool is refused there with
//! `events/server-owned` before anything changes, whatever `force` says, while the folder's own `index.md` stays
//! ordinary content.
mod support;

use contextos_mcp::ContextOsServer;
use serde_json::{Value, json};
use support::git_harness::{call, harness_with};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const EXISTING_EVENT: &str = "memory/events/2026/10/2026-10-28-031542-a-024.md";
const EVENT_TEXT: &str = "---\ntype: event\nkind: task.state-changed\nseverity: info\nat: 2026-10-28T03:15:42Z\nentity: vault\n---\n\n# Moved\n";

fn vault_with_an_event() -> Result<(tempfile::TempDir, ContextOsServer), BoxError> {
    vault_with_an_event_configured(|_| {})
}

fn vault_with_an_event_configured(
    configure: impl FnOnce(&mut contextos_mcp::Config),
) -> Result<(tempfile::TempDir, ContextOsServer), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("memory/events/2026/10"))?;
    std::fs::write(vault.path().join(EXISTING_EVENT), EVENT_TEXT)?;
    std::fs::write(vault.path().join("memory/events/index.md"), "# Events\n")?;
    std::fs::write(vault.path().join("outside.md"), "# Outside\n")?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    configure(&mut config);
    let server = ContextOsServer::try_from(config)?;
    Ok((vault, server))
}

async fn refused_code(
    server: &ContextOsServer,
    tool: &'static str,
    arguments: Value,
) -> Result<Option<String>, BoxError> {
    let result = call(server, tool, arguments).await?;
    assert_eq!(result.is_error, Some(true), "{tool} must be refused: {result:?}");
    Ok(result
        .structured_content
        .as_ref()
        .and_then(|content| content.get("code"))
        .and_then(Value::as_str)
        .map(str::to_owned))
}

async fn current_hash(server: &ContextOsServer, path: &str) -> Result<String, BoxError> {
    let info = call(server, "fs_get_file_info", json!({"path": path})).await?;
    Ok(info
        .structured_content
        .as_ref()
        .and_then(|content| content.get("content_hash"))
        .and_then(Value::as_str)
        .ok_or("no content_hash")?
        .to_owned())
}

#[tokio::test]
async fn every_mutating_tool_is_refused_under_the_events_folder_and_changes_nothing() -> Result<(), BoxError> {
    let (vault, server) = vault_with_an_event()?;
    let hash = current_hash(&server, EXISTING_EVENT).await?;
    let attempts: Vec<(&'static str, Value)> = vec![
        (
            "fs_write_file",
            json!({"path": "memory/events/2026/10/new.md", "content": "x"}),
        ),
        (
            "fs_write_file",
            json!({"path": EXISTING_EVENT, "content": "x", "force": true}),
        ),
        (
            "fs_edit_file",
            json!({"path": EXISTING_EVENT, "edits": [{"old_text": "Moved", "new_text": "Forged"}]}),
        ),
        (
            "fs_move_file",
            json!({"source": EXISTING_EVENT, "destination": "elsewhere.md"}),
        ),
        (
            "fs_move_file",
            json!({"source": "outside.md", "destination": "memory/events/outside.md"}),
        ),
        ("fs_delete_file", json!({"path": EXISTING_EVENT})),
        ("fs_create_directory", json!({"path": "memory/events/2026/11"})),
        (
            "note_create",
            json!({"path": "memory/events/2026/10/note.md", "frontmatter": {}, "content": "x"}),
        ),
        (
            "note_update",
            json!({"path": EXISTING_EVENT, "expected_hash": hash, "patch": {"title": "Forged"}}),
        ),
        (
            "frontmatter_update",
            json!({"path": EXISTING_EVENT, "patch": {"severity": "error"}}),
        ),
        (
            "frontmatter_link",
            json!({"path": EXISTING_EVENT, "operations": [{"op": "add", "field": "depends_on", "target": "outside"}]}),
        ),
        (
            "canvas_create",
            json!({"path": "memory/events/map.canvas", "nodes": [], "edges": []}),
        ),
    ];
    for (tool, arguments) in attempts {
        let code = refused_code(&server, tool, arguments).await?;
        assert_eq!(code.as_deref(), Some("events/server-owned"), "{tool}");
    }
    assert_eq!(std::fs::read_to_string(vault.path().join(EXISTING_EVENT))?, EVENT_TEXT);
    assert!(!vault.path().join("memory/events/2026/10/new.md").exists());
    assert!(!vault.path().join("memory/events/2026/11").exists());
    assert!(!vault.path().join("elsewhere.md").exists());
    assert!(vault.path().join("outside.md").exists());
    Ok(())
}

#[tokio::test]
async fn the_events_index_note_stays_ordinary_content() -> Result<(), BoxError> {
    let (vault, server) = vault_with_an_event()?;
    let written = call(
        &server,
        "fs_write_file",
        json!({"path": "memory/events/index.md", "content": "# Events\n\nAbout events.\n", "force": true}),
    )
    .await?;
    assert_eq!(written.is_error, Some(false), "{written:?}");
    assert!(std::fs::read_to_string(vault.path().join("memory/events/index.md"))?.contains("About events"));
    Ok(())
}

#[tokio::test]
async fn a_folder_that_only_resembles_the_events_folder_is_not_protected() -> Result<(), BoxError> {
    let (vault, server) = vault_with_an_event()?;
    let written = call(
        &server,
        "fs_write_file",
        json!({"path": "memory/events-archive/note.md", "content": "# Archive\n"}),
    )
    .await?;
    assert_eq!(written.is_error, Some(false), "{written:?}");
    assert!(vault.path().join("memory/events-archive/note.md").exists());
    Ok(())
}

const EVENT_BASE: &str = "memory/events/2026/10/map.base";
const EVENT_CANVAS: &str = "memory/events/2026/10/map.canvas";

#[tokio::test]
async fn base_and_canvas_tools_are_refused_under_the_events_folder() -> Result<(), BoxError> {
    let (vault, server) = vault_with_an_event()?;
    std::fs::write(
        vault.path().join(EVENT_BASE),
        "views:\n  - type: table\n    name: All\n",
    )?;
    std::fs::write(vault.path().join(EVENT_CANVAS), "{\"nodes\":[],\"edges\":[]}")?;
    let base_hash = current_hash(&server, EVENT_BASE).await?;
    let canvas_hash = current_hash(&server, EVENT_CANVAS).await?;
    let attempts: Vec<(&'static str, Value)> = vec![
        (
            "base_create",
            json!({"path": "memory/events/new.base", "definition": {"views": [{"type": "table", "name": "All"}]}}),
        ),
        (
            "base_apply",
            json!({
                "path": EVENT_BASE,
                "expected_hash": base_hash,
                "operations": [{"op": "set_property", "name": "status", "property": {"displayName": "Status"}}]
            }),
        ),
        (
            "canvas_apply",
            json!({
                "path": EVENT_CANVAS,
                "expected_hash": canvas_hash,
                "operations": [{"op": "add_node", "node": {"id": "n", "type": "text", "text": "x", "x": 0, "y": 0, "width": 10, "height": 10}}]
            }),
        ),
    ];
    for (tool, arguments) in attempts {
        let code = refused_code(&server, tool, arguments).await?;
        assert_eq!(code.as_deref(), Some("events/server-owned"), "{tool}");
    }
    assert!(!vault.path().join("memory/events/new.base").exists());
    assert_eq!(
        std::fs::read_to_string(vault.path().join(EVENT_CANVAS))?,
        "{\"nodes\":[],\"edges\":[]}"
    );
    Ok(())
}

#[tokio::test]
async fn moving_or_deleting_a_parent_of_the_events_folder_is_refused_and_changes_nothing() -> Result<(), BoxError> {
    let (vault, server) = vault_with_an_event_configured(|config| config.vaults[0].git.destructive_delete = true)?;
    std::fs::create_dir_all(vault.path().join("prepared/events/2026/10"))?;
    std::fs::write(vault.path().join("prepared/events/2026/10/forged.md"), EVENT_TEXT)?;
    let attempts: Vec<(&'static str, Value)> = vec![
        ("fs_move_file", json!({"source": "memory", "destination": "moved"})),
        ("fs_move_file", json!({"source": "prepared", "destination": "memory"})),
        (
            "fs_move_file",
            json!({"source": "prepared/events", "destination": "memory/events"}),
        ),
        ("fs_delete_file", json!({"path": "memory"})),
        ("fs_delete_file", json!({"path": "memory", "hard": true})),
    ];
    for (tool, arguments) in attempts {
        let code = refused_code(&server, tool, arguments.clone()).await?;
        assert_eq!(code.as_deref(), Some("events/server-owned"), "{tool} {arguments}");
    }
    let batch = call(
        &server,
        "fs_delete_file",
        json!({"paths": ["memory", "outside.md"], "hard": true}),
    )
    .await?;
    let first_code = batch
        .structured_content
        .as_ref()
        .and_then(|content| content.pointer("/results/0/error/code"))
        .and_then(Value::as_str);
    assert_eq!(first_code, Some("events/server-owned"), "{batch:?}");
    assert_eq!(std::fs::read_to_string(vault.path().join(EXISTING_EVENT))?, EVENT_TEXT);
    assert!(!vault.path().join("moved").exists());
    assert!(vault.path().join("prepared/events/2026/10/forged.md").exists());
    Ok(())
}

#[tokio::test]
async fn git_restore_leaves_the_events_folder_alone_even_when_the_configuration_does_not_exclude_it()
-> Result<(), BoxError> {
    let harness = harness_with(
        |config| {
            config.vaults[0].git.restore_exclude = vec!["journal".into()];
            config.vaults[0].git.destructive_delete = true;
        },
        |root| {
            std::fs::create_dir_all(root.join("memory/events/2026/10"))?;
            std::fs::write(root.join(EXISTING_EVENT), EVENT_TEXT)?;
            Ok(())
        },
    )
    .await?;
    let log = harness.structured("git_log", json!({"limit": 1})).await?;
    let baseline = log
        .get("entries")
        .or_else(|| log.get("commits"))
        .and_then(Value::as_array)
        .and_then(|entries| entries.first())
        .and_then(|entry| entry.get("id").or_else(|| entry.get("commit_id")))
        .and_then(Value::as_str)
        .ok_or("no baseline commit")?
        .to_owned();
    std::fs::write(harness.vault.path().join("alpha/note.md"), "# Damaged\n")?;
    std::fs::write(harness.vault.path().join(EXISTING_EVENT), "changed by the server\n")?;
    std::fs::write(harness.vault.path().join("memory/events/2026/10/later.md"), "later\n")?;

    let restored = harness
        .call("git_restore", json!({"path": ".", "ref": baseline}))
        .await?;

    assert_eq!(restored.is_error, Some(false), "{restored:?}");
    assert_eq!(
        std::fs::read_to_string(harness.vault.path().join("alpha/note.md"))?,
        "# Note\n"
    );
    assert_eq!(
        std::fs::read_to_string(harness.vault.path().join(EXISTING_EVENT))?,
        "changed by the server\n"
    );
    assert!(harness.vault.path().join("memory/events/2026/10/later.md").exists());
    Ok(())
}
