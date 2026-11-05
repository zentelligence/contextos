//! `vault_index_rebuild` completes managed index frontmatter from the vault's entities, and, only when asked to
//! `repair`, corrects the values an earlier defective rebuild wrote, reporting each correction.

mod support;
use contextos_mcp::ContextOsServer;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, Value, json};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const ENTITIES: &str = "[[entities]]\nslug = \"personal\"\nname = \"Personal\"\naccent = 1\n";

const DEFECTIVE_INDEX: &str = "---\ntype: index\ntitle: Journal\nentity: vault\nstatus: new\n\
    created: 2026-09-27 10:00:00.0 +10:00:00\nupdated: 2026-09-27 10:00:00.0 +10:00:00\ntags: []\naliases: []\n\
    description: ''\nsummary: ''\nexcerpt: ''\n---\n\n# Journal: Index\n\n";

fn vault_with_a_defective_index() -> Result<tempfile::TempDir, BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join(".contextos"))?;
    std::fs::write(vault.path().join(".contextos/entities.toml"), ENTITIES)?;
    std::fs::create_dir_all(vault.path().join("personal/journal"))?;
    std::fs::write(vault.path().join("personal/journal/day.md"), "# Day\n")?;
    std::fs::write(vault.path().join("personal/journal/index.md"), DEFECTIVE_INDEX)?;
    Ok(vault)
}

fn structured(result: CallToolResult) -> Result<Value, BoxError> {
    result
        .structured_content
        .ok_or_else(|| std::io::Error::other("the tool omitted its structured result").into())
}

#[tokio::test]
async fn an_ordinary_rebuild_reports_no_repairs_and_keeps_existing_values() -> Result<(), BoxError> {
    let vault = vault_with_a_defective_index()?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;

    let result = call_tool(
        server,
        "vault_index_rebuild",
        serde_json::from_value(json!({"path": "personal/journal"}))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false));
    let structured = structured(result)?;
    assert_eq!(structured.get("repairs"), Some(&json!([])));
    assert_eq!(structured.get("warnings"), Some(&json!([])));
    let source = std::fs::read_to_string(vault.path().join("personal/journal/index.md"))?;
    assert!(source.contains("entity: vault\n"), "{source}");
    assert!(source.contains("status: new\n"), "{source}");
    Ok(())
}

#[tokio::test]
async fn a_repairing_rebuild_corrects_the_index_and_lists_every_correction() -> Result<(), BoxError> {
    let vault = vault_with_a_defective_index()?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;

    let result = call_tool(
        server,
        "vault_index_rebuild",
        serde_json::from_value(json!({"path": "personal/journal", "repair": true}))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false));
    let structured = structured(result)?;
    let repairs = structured
        .get("repairs")
        .and_then(Value::as_array)
        .ok_or_else(|| std::io::Error::other("repairs missing"))?;
    assert_eq!(repairs.len(), 1);
    assert_eq!(repairs[0].get("path"), Some(&json!("personal/journal/index.md")));
    let actions = repairs[0]
        .get("actions")
        .and_then(Value::as_array)
        .ok_or_else(|| std::io::Error::other("actions missing"))?;
    assert!(
        actions.contains(&json!({"property": "entity", "from": "vault", "to": "personal"})),
        "{actions:?}"
    );
    assert!(
        actions.contains(&json!({"property": "status", "from": "new", "to": null})),
        "{actions:?}"
    );
    let source = std::fs::read_to_string(vault.path().join("personal/journal/index.md"))?;
    assert!(source.contains("entity: personal\n"), "{source}");
    assert!(!source.contains("status:"), "{source}");
    Ok(())
}

#[tokio::test]
async fn a_repairing_dry_run_lists_the_corrections_and_writes_nothing() -> Result<(), BoxError> {
    let vault = vault_with_a_defective_index()?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;

    let result = call_tool(
        server,
        "vault_index_rebuild",
        serde_json::from_value(json!({"path": "personal/journal", "repair": true, "dry_run": true}))?,
    )
    .await?;

    let structured = structured(result)?;
    assert_eq!(
        structured.get("repairs").and_then(Value::as_array).map(Vec::len),
        Some(1)
    );
    assert_eq!(
        std::fs::read_to_string(vault.path().join("personal/journal/index.md"))?,
        DEFECTIVE_INDEX
    );
    Ok(())
}

#[tokio::test]
async fn a_malformed_entity_list_is_rejected_with_a_stable_code_and_nothing_is_written() -> Result<(), BoxError> {
    let vault = vault_with_a_defective_index()?;
    std::fs::write(vault.path().join(".contextos/entities.toml"), "[[entities]\nslug = ")?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;

    let result = call_tool(
        server,
        "vault_index_rebuild",
        serde_json::from_value(json!({"path": "personal/journal"}))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.structured_content.as_ref().and_then(|value| value.get("code")),
        Some(&json!("index/entities"))
    );
    assert_eq!(
        std::fs::read_to_string(vault.path().join("personal/journal/index.md"))?,
        DEFECTIVE_INDEX
    );
    Ok(())
}

#[tokio::test]
async fn frontmatter_that_cannot_be_edited_is_kept_and_reported_as_a_warning() -> Result<(), BoxError> {
    let vault = vault_with_a_defective_index()?;
    let flow_style = "---\n{type: index, title: Flow Style}\n---\n# Journal\n";
    std::fs::write(vault.path().join("personal/journal/index.md"), flow_style)?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;

    let result = call_tool(
        server,
        "vault_index_rebuild",
        serde_json::from_value(json!({"path": "personal/journal"}))?,
    )
    .await?;

    assert_eq!(result.is_error, Some(false));
    let structured = structured(result)?;
    let warnings = structured
        .get("warnings")
        .and_then(Value::as_array)
        .ok_or_else(|| std::io::Error::other("warnings missing"))?;
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].get("path"), Some(&json!("personal/journal/index.md")));
    let source = std::fs::read_to_string(vault.path().join("personal/journal/index.md"))?;
    assert!(
        source.starts_with("---\n{type: index, title: Flow Style}\n---\n"),
        "{source}"
    );
    assert!(source.contains("[[personal/journal/day.md|Day]]"), "{source}");
    Ok(())
}

#[tokio::test]
async fn an_unknown_rebuild_option_is_still_rejected() -> Result<(), BoxError> {
    let vault = vault_with_a_defective_index()?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;

    let outcome = call_tool(
        server,
        "vault_index_rebuild",
        serde_json::from_value(json!({"path": "personal/journal", "fix_everything": true}))?,
    )
    .await;

    assert!(outcome.is_err() || outcome.is_ok_and(|result| result.is_error == Some(true)));
    Ok(())
}

async fn call_tool(
    server: ContextOsServer,
    name: &'static str,
    arguments: Map<String, Value>,
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
