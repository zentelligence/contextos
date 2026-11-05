//! A base scan builds each note's path from the folders it walked, not from the vault root: every note in nested
//! folders is still found, a note linked from elsewhere in the vault is read through its link, and a file name that
//! only looks like the start of a Windows path is an ordinary note.

mod support;
use contextos_mcp::ContextOsServer;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, Value, json};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

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

/// The `id` of every row `base_query` returns for notes in `records`, sorted.
async fn record_ids(server: ContextOsServer) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
    let result = call_tool(
        server,
        "base_query",
        json!({
            "definition": {"filters": "file.inFolder(\"records\")", "order": ["id"]},
            "format": "json"
        }),
    )
    .await?;
    assert_eq!(result.is_error, Some(false), "{result:?}");
    let text = result
        .structured_content
        .as_ref()
        .and_then(|value| value.get("content"))
        .and_then(Value::as_str)
        .ok_or("base_query omitted content")?;
    let rows: Vec<Value> = serde_json::from_str(text)?;
    let mut ids = rows
        .iter()
        .filter_map(|row| row.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect::<Vec<_>>();
    ids.sort();
    Ok(ids)
}

fn record(path: &std::path::Path, id: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format!("---\nid: {id}\n---\n\n# {id}\n"))
}

#[tokio::test]
async fn every_record_in_nested_folders_is_found() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    record(&vault.path().join("records/r-1.md"), "rec1")?;
    record(&vault.path().join("records/a/r-2.md"), "rec2")?;
    record(&vault.path().join("records/a/b/r-3.md"), "rec3")?;
    record(&vault.path().join("records/a/b/r-4.md"), "rec4")?;
    record(&vault.path().join("records/c/r-5.md"), "rec5")?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;

    assert_eq!(record_ids(server).await?, ["rec1", "rec2", "rec3", "rec4", "rec5"]);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_record_linked_from_elsewhere_in_the_vault_and_one_named_like_a_windows_prefix_are_both_found() -> TestResult
{
    use std::os::unix::fs::symlink;
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    record(&vault.path().join("records/r-1.md"), "rec1")?;
    record(&vault.path().join("archive/r-2.md"), "rec2")?;
    symlink(vault.path().join("archive/r-2.md"), vault.path().join("records/r-2.md"))?;
    record(&vault.path().join(r"records/\\r-3.md"), "rec3")?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;

    assert_eq!(record_ids(server).await?, ["rec1", "rec2", "rec3"]);
    Ok(())
}
