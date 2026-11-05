//! A base's records are read from the base's own top-level filters, the same records `base_query` selects and the
//! doctor checks, whichever way the filters name their folder: `base_record_create` places and keys records by them,
//! and `base_read` reports the folder they confine records to.

mod support;
use contextos_mcp::ContextOsServer;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, json};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

async fn call_tool(
    server: ContextOsServer,
    name: &'static str,
    arguments: Map<String, serde_json::Value>,
) -> Result<CallToolResult, Box<dyn std::error::Error + Send + Sync>> {
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

fn structured(
    result: &CallToolResult,
) -> Result<&serde_json::Map<String, serde_json::Value>, Box<dyn std::error::Error + Send + Sync>> {
    result
        .structured_content
        .as_ref()
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| std::io::Error::other("tool result omitted structured content").into())
}

/// A vault with a `VD`-keyed base at `memory/decisions.base` whose records are selected by `filters`.
async fn decisions_vault(
    filters: &str,
) -> Result<(tempfile::TempDir, ContextOsServer), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    for folder in ["memory/decisions", "memory/tasks"] {
        std::fs::create_dir_all(vault.path().join(folder))?;
    }
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    let created = call_tool(
        server.clone(),
        "base_create",
        serde_json::from_value(json!({
            "path": "memory/decisions.base",
            "definition": {"filters": filters, "views": [{"type": "table", "name": "All"}]},
            "key_prefix": "VD"
        }))?,
    )
    .await?;
    assert_eq!(created.is_error, Some(false), "{created:?}");
    Ok((vault, server))
}

fn write_held_key(vault: &std::path::Path, relative: &str, id: &str) -> std::io::Result<()> {
    std::fs::write(vault.join(relative), format!("---\nid: {id}\n---\nBody.\n"))
}

async fn create_decision(server: &ContextOsServer) -> Result<CallToolResult, Box<dyn std::error::Error + Send + Sync>> {
    call_tool(
        server.clone(),
        "base_record_create",
        serde_json::from_value(json!({
            "path": "memory/decisions.base",
            "frontmatter": {},
            "content": "Body.\n"
        }))?,
    )
    .await
}

#[tokio::test]
async fn a_base_whose_filters_name_its_folder_by_equality_places_records_in_that_folder() -> TestResult {
    let (vault, server) = decisions_vault("file.folder == \"memory/decisions\" && file.basename != \"index\"").await?;

    let result = create_decision(&server).await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(
        structured(&result)?.get("path"),
        Some(&json!("memory/decisions/vd-001.md"))
    );
    assert!(vault.path().join("memory/decisions/vd-001.md").is_file());
    Ok(())
}

#[tokio::test]
async fn a_note_the_base_does_not_select_does_not_hold_one_of_its_keys() -> TestResult {
    let (vault, server) = decisions_vault("file.folder == \"memory/decisions\"").await?;
    write_held_key(vault.path(), "memory/tasks/elsewhere.md", "VD-001")?;

    let result = create_decision(&server).await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    let body = structured(&result)?;
    assert_eq!(body.get("key"), Some(&json!("VD-001")));
    assert_eq!(body.get("warnings"), Some(&json!([])));
    Ok(())
}

#[tokio::test]
async fn an_index_note_the_base_excludes_by_basename_does_not_hold_one_of_its_keys() -> TestResult {
    let (vault, server) = decisions_vault("file.inFolder(\"memory/decisions\") && file.basename != \"index\"").await?;
    write_held_key(vault.path(), "memory/decisions/index.md", "VD-001")?;

    let result = create_decision(&server).await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(structured(&result)?.get("key"), Some(&json!("VD-001")));
    Ok(())
}

#[tokio::test]
async fn a_record_the_base_selects_still_holds_its_key() -> TestResult {
    let (vault, server) = decisions_vault("file.folder == \"memory/decisions\"").await?;
    write_held_key(vault.path(), "memory/decisions/hand-authored.md", "VD-001")?;

    let result = create_decision(&server).await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(structured(&result)?.get("key"), Some(&json!("VD-002")));
    Ok(())
}

#[tokio::test]
async fn base_read_reports_the_folder_its_filters_confine_records_to() -> TestResult {
    let (vault, server) = decisions_vault("file.folder == \"memory/decisions\" && file.basename != \"index\"").await?;
    std::fs::write(
        vault.path().join("loose.base"),
        "filters: type == \"task\"\nviews:\n  - type: table\n    name: All\n",
    )?;

    let scoped = call_tool(
        server.clone(),
        "base_read",
        serde_json::from_value(json!({"path": "memory/decisions.base"}))?,
    )
    .await?;
    let loose = call_tool(
        server.clone(),
        "base_read",
        serde_json::from_value(json!({"path": "loose.base"}))?,
    )
    .await?;

    assert_eq!(
        structured(&scoped)?.get("records_folder"),
        Some(&json!("memory/decisions"))
    );
    assert_eq!(structured(&loose)?.get("records_folder"), None);
    Ok(())
}

#[tokio::test]
async fn a_base_whose_records_sit_directly_in_its_folder_refuses_a_relative_sub_folder() -> TestResult {
    let (vault, server) = decisions_vault("file.folder == \"memory/decisions\"").await?;

    let refused = call_tool(
        server.clone(),
        "base_record_create",
        serde_json::from_value(json!({
            "path": "memory/decisions.base",
            "folder": "2026",
            "frontmatter": {},
            "content": "Body.\n"
        }))?,
    )
    .await?;

    assert_eq!(refused.is_error, Some(true), "{refused:?}");
    assert_eq!(structured(&refused)?.get("code"), Some(&json!("io/invalid-argument")));
    assert!(!vault.path().join("memory/decisions/2026").exists());
    let sidecar = std::fs::read_to_string(vault.path().join("memory/decisions.md"))?;
    assert!(!sidecar.contains("VD: 1"), "no key is reserved: {sidecar}");
    Ok(())
}

#[tokio::test]
async fn base_read_says_whether_a_bases_records_may_sit_beneath_its_folder() -> TestResult {
    let (vault, server) = decisions_vault("file.folder == \"memory/decisions\"").await?;
    std::fs::write(
        vault.path().join("tree.base"),
        "filters: file.inFolder(\"memory/tasks\")\nviews:\n  - type: table\n    name: All\n",
    )?;

    let exact = call_tool(
        server.clone(),
        "base_read",
        serde_json::from_value(json!({"path": "memory/decisions.base"}))?,
    )
    .await?;
    let tree = call_tool(
        server,
        "base_read",
        serde_json::from_value(json!({"path": "tree.base"}))?,
    )
    .await?;

    assert_eq!(
        structured(&exact)?.get("records_include_subfolders"),
        Some(&json!(false))
    );
    assert_eq!(structured(&tree)?.get("records_include_subfolders"), Some(&json!(true)));
    Ok(())
}

#[tokio::test]
async fn a_record_its_filters_hide_still_holds_its_key_within_the_bases_folder() -> TestResult {
    // A status condition in the top-level filters hides closed records from every view, but an id they hold is
    // still taken: ids are unique within the base's folder, whatever else its filters say.
    let (vault, server) =
        decisions_vault("file.inFolder(\"memory/decisions\") && file.basename != \"index\" && status != \"closed\"")
            .await?;
    std::fs::write(
        vault.path().join("memory/decisions/old.md"),
        "---\nid: VD-001\nstatus: closed\n---\nBody.\n",
    )?;

    let result = create_decision(&server).await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(structured(&result)?.get("key"), Some(&json!("VD-002")));
    Ok(())
}

#[tokio::test]
async fn a_base_filtering_on_backlinks_still_creates_records_without_the_link_graph() -> TestResult {
    let (_vault, server) =
        decisions_vault("file.inFolder(\"memory/decisions\") && file.backlinks.contains(\"index.md\")").await?;

    let result = create_decision(&server).await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    assert_eq!(structured(&result)?.get("key"), Some(&json!("VD-001")));
    Ok(())
}
