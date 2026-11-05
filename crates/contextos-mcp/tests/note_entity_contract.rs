//! The `entity` a created note carries is derived from where the note sits in the vault, against the vault's own
//! `.contextos/entities.toml`: the entity whose top-level directory holds it, and `vault` for every other path,
//! `memory/` included. Covers `note_create` and `base_record_create`, the two tools that write a
//! new note with default frontmatter.

mod support;
use contextos_mcp::ContextOsServer;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, Value, json};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

const ENTITIES: &str = "[[entities]]\nslug = \"zenith\"\nname = \"Zenith\"\naccent = 1\n\n\
                        [[entities]]\nslug = \"personal\"\nname = \"Personal\"\naccent = 2\n";

async fn call_tool(
    server: ContextOsServer,
    name: &'static str,
    arguments: Value,
) -> Result<CallToolResult, Box<dyn std::error::Error + Send + Sync>> {
    let arguments: Map<String, Value> = serde_json::from_value(arguments)?;
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server_handle = tokio::spawn(async move {
        server.serve(server_transport).await?.waiting().await?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });
    let mut client = ().serve(client_transport).await?;
    let result = client
        .call_tool(CallToolRequestParams::new(name).with_arguments(arguments))
        .await;
    client.close().await?;
    server_handle.await??;
    Ok(result?)
}

/// A vault with the given `.contextos/entities.toml` (none when `entities` is `None`).
fn vault_with(
    entities: Option<&str>,
) -> Result<(tempfile::TempDir, ContextOsServer), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    if let Some(entities) = entities {
        std::fs::create_dir_all(vault.path().join(".contextos"))?;
        std::fs::write(vault.path().join(".contextos/entities.toml"), entities)?;
    }
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    Ok((vault, server))
}

async fn entity_of(
    server: &ContextOsServer,
    path: &str,
) -> Result<Option<Value>, Box<dyn std::error::Error + Send + Sync>> {
    let read = call_tool(server.clone(), "frontmatter_read", json!({"path": path})).await?;
    assert_eq!(read.is_error, Some(false), "{read:?}");
    Ok(read
        .structured_content
        .as_ref()
        .and_then(|value| value.get("frontmatter"))
        .and_then(|frontmatter| frontmatter.get("entity"))
        .cloned())
}

async fn create_note(
    server: &ContextOsServer,
    path: &str,
) -> Result<CallToolResult, Box<dyn std::error::Error + Send + Sync>> {
    call_tool(
        server.clone(),
        "note_create",
        json!({"path": path, "content": "Body.\n"}),
    )
    .await
}

#[tokio::test]
async fn a_note_inside_an_entity_directory_carries_that_entity() -> TestResult {
    let (_vault, server) = vault_with(Some(ENTITIES))?;
    let created = create_note(&server, "zenith/projects/plan.md").await?;
    assert_eq!(created.is_error, Some(false), "{created:?}");
    assert_eq!(
        entity_of(&server, "zenith/projects/plan.md").await?,
        Some(json!("zenith"))
    );
    Ok(())
}

#[tokio::test]
async fn note_create_below_a_memory_folder_named_for_an_entity_writes_vault() -> TestResult {
    let (_vault, server) = vault_with(Some(ENTITIES))?;
    for path in [
        "memory/tasks/zenith/renew-licence.md",
        "memory/zenith/x.md",
        "projects/personal/x.md",
    ] {
        let created = create_note(&server, path).await?;
        assert_eq!(created.is_error, Some(false), "{created:?}");
        assert_eq!(entity_of(&server, path).await?, Some(json!("vault")), "{path}");
    }
    Ok(())
}

#[tokio::test]
async fn base_record_create_below_a_memory_folder_named_for_an_entity_writes_vault() -> TestResult {
    let (vault, server) = vault_with(Some(ENTITIES))?;
    std::fs::create_dir_all(vault.path().join("memory/tasks/zenith"))?;
    let base = call_tool(
        server.clone(),
        "base_create",
        json!({
            "path": "memory/tasks.base",
            "definition": {
                "filters": "file.inFolder(\"memory/tasks/zenith\") && file.basename != \"index\"",
                "views": [{"type": "table", "name": "All"}]
            },
            "key_prefix": "A"
        }),
    )
    .await?;
    assert_eq!(base.is_error, Some(false), "{base:?}");
    let record = call_tool(
        server.clone(),
        "base_record_create",
        json!({"path": "memory/tasks.base", "content": "Body.\n"}),
    )
    .await?;
    assert_eq!(record.is_error, Some(false), "{record:?}");
    let path = record
        .structured_content
        .as_ref()
        .and_then(|value| value.get("path"))
        .and_then(Value::as_str)
        .ok_or_else(|| std::io::Error::other("base_record_create omitted its path"))?
        .to_owned();
    assert!(path.starts_with("memory/tasks/zenith/"), "{path}");
    assert_eq!(entity_of(&server, &path).await?, Some(json!("vault")));
    Ok(())
}

#[tokio::test]
async fn a_supplied_entity_is_written_unchanged_wherever_the_note_sits() -> TestResult {
    // Guard: the supplied value always wins over the derived one, before and after the entity rule changes.
    let (_vault, server) = vault_with(Some(ENTITIES))?;
    for (path, supplied) in [
        ("zenith/projects/plan.md", "personal"),
        ("memory/tasks/zenith/x.md", "zenith"),
        ("memory/wiki/topic.md", "Not A Configured Entity"),
        ("root-note.md", "personal"),
    ] {
        let created = call_tool(
            server.clone(),
            "note_create",
            json!({"path": path, "frontmatter": {"entity": supplied}, "content": "Body.\n"}),
        )
        .await?;
        assert_eq!(created.is_error, Some(false), "{created:?}");
        assert_eq!(entity_of(&server, path).await?, Some(json!(supplied)), "{path}");
    }
    Ok(())
}

#[tokio::test]
async fn a_note_in_the_vaults_shared_areas_carries_the_vault_entity() -> TestResult {
    let (_vault, server) = vault_with(Some(ENTITIES))?;
    for path in ["memory/wiki/topic.md", "registry/prompts/draft.md", "root-note.md"] {
        let created = create_note(&server, path).await?;
        assert_eq!(created.is_error, Some(false), "{created:?}");
        assert_eq!(entity_of(&server, path).await?, Some(json!("vault")), "{path}");
    }
    Ok(())
}

#[tokio::test]
async fn a_note_in_a_vault_with_no_entity_list_carries_no_entity_rather_than_a_guess() -> TestResult {
    let (_vault, server) = vault_with(None)?;
    let created = create_note(&server, "zenith/plan.md").await?;
    assert_eq!(created.is_error, Some(false), "{created:?}");
    assert_eq!(entity_of(&server, "zenith/plan.md").await?, None);
    Ok(())
}

#[tokio::test]
async fn an_entity_the_caller_supplies_is_kept_over_the_derived_one() -> TestResult {
    let (_vault, server) = vault_with(Some(ENTITIES))?;
    let created = call_tool(
        server.clone(),
        "note_create",
        json!({"path": "memory/wiki/topic.md", "frontmatter": {"entity": "personal"}, "content": "Body.\n"}),
    )
    .await?;
    assert_eq!(created.is_error, Some(false), "{created:?}");
    assert_eq!(
        entity_of(&server, "memory/wiki/topic.md").await?,
        Some(json!("personal"))
    );
    Ok(())
}

#[tokio::test]
async fn a_malformed_entity_list_refuses_the_note_and_writes_nothing() -> TestResult {
    let (vault, server) = vault_with(Some("entities = \"not a list\"\n"))?;
    let created = create_note(&server, "zenith/plan.md").await?;
    assert_eq!(created.is_error, Some(true), "{created:?}");
    let code = created
        .structured_content
        .as_ref()
        .and_then(|value| value.get("code"))
        .cloned();
    assert_eq!(code, Some(json!("format/entities")));
    assert!(!vault.path().join("zenith/plan.md").exists());
    Ok(())
}

#[tokio::test]
async fn a_base_record_note_carries_the_entity_of_the_folder_it_is_created_in() -> TestResult {
    let (vault, server) = vault_with(Some(ENTITIES))?;
    std::fs::create_dir_all(vault.path().join("zenith/requirements"))?;
    let base = call_tool(
        server.clone(),
        "base_create",
        json!({
            "path": "zenith/requirements.base",
            "definition": {
                "filters": "file.inFolder(\"zenith/requirements\") && file.basename != \"index\"",
                "views": [{"type": "table", "name": "All"}]
            },
            "key_prefix": "REQ"
        }),
    )
    .await?;
    assert_eq!(base.is_error, Some(false), "{base:?}");
    let record = call_tool(
        server.clone(),
        "base_record_create",
        json!({"path": "zenith/requirements.base", "content": "Body.\n"}),
    )
    .await?;
    assert_eq!(record.is_error, Some(false), "{record:?}");
    let path = record
        .structured_content
        .as_ref()
        .and_then(|value| value.get("path"))
        .and_then(Value::as_str)
        .ok_or_else(|| std::io::Error::other("base_record_create omitted its path"))?
        .to_owned();
    assert_eq!(entity_of(&server, &path).await?, Some(json!("zenith")));
    Ok(())
}
