//! What the MCP surface reports for a vault whose link graph store is damaged. A server opening the vault moves the
//! damaged store aside and rebuilds it, reporting where it went. While another instance still holds it,
//! `query_index_status` and `vault_info` still answer, naming the failure, `query_graph` refuses with that failure's
//! own code, `query_index_rebuild` refuses as `index/in-use` rather than failing on the damage, and a vault write
//! still succeeds, reporting the failure as a warning.

mod support;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use contextos_mcp::ContextOsServer;
use contextos_search::{GraphBackend, LinkGraph, LinkGraphConfig};
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

fn server_for(vault: &Path) -> Result<ContextOsServer, Box<dyn std::error::Error + Send + Sync>> {
    Ok(ContextOsServer::try_from(support::scoped_config(vec![
        vault.to_path_buf(),
    ])?)?)
}

fn graph_status(result: &CallToolResult) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    result
        .structured_content
        .as_ref()
        .and_then(|value| value.get("graph"))
        .cloned()
        .ok_or_else(|| format!("query_index_status omitted the graph status: {result:?}").into())
}

/// A populated `sqlite` link graph store for `vault`, then a damaged page in the middle of its file: what `SQLite`
/// reports as a malformed database image.
fn damaged_graph_store(vault: &Path) -> TestResult {
    let store_directory = vault.join(".contextos").join("graph");
    {
        let mut graph = LinkGraph::try_from(LinkGraphConfig {
            store_directory: store_directory.clone(),
            backend: GraphBackend::Sqlite,
            write_wait: contextos_search::DEFAULT_WRITE_WAIT,
        })?;
        let notes = (0..500)
            .map(|i| {
                (
                    format!("note-{i}-with-a-reasonably-long-path-to-grow-the-store-file.md"),
                    format!("Note {i} also has a reasonably long title, to grow the store file further still"),
                    Vec::new(),
                )
            })
            .collect::<Vec<_>>();
        graph.rebuild(&notes)?;
    }
    let file = store_directory.join("graph.sqlite3");
    let length = std::fs::metadata(&file)?.len();
    let mut handle = std::fs::OpenOptions::new().write(true).open(&file)?;
    handle.seek(SeekFrom::Start(length / 2))?;
    handle.write_all(&[0xFF_u8; 4096])?;
    Ok(())
}

#[tokio::test]
async fn a_vault_whose_graph_was_rebuilt_reports_a_ready_graph() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("a.md"), "# A\n")?;
    let server = server_for(vault.path())?;
    let rebuilt = call_tool(server.clone(), "query_index_rebuild", json!({"index": "graph"})).await?;
    assert_eq!(rebuilt.is_error, Some(false), "{rebuilt:?}");

    let status = call_tool(server, "query_index_status", json!({})).await?;
    assert_eq!(status.is_error, Some(false), "{status:?}");
    let graph = graph_status(&status)?;
    assert_eq!(graph.get("health"), Some(&json!("ready")), "{graph}");
    assert_eq!(graph.get("failure"), None, "{graph}");
    Ok(())
}

/// Registers a holder of the vault's link graph store for a process other than this one, as another live instance
/// would; it holds the store until dropped.
fn another_instance_holding_the_graph(vault: &Path) -> Result<std::fs::File, Box<dyn std::error::Error + Send + Sync>> {
    let holders = vault.join(".contextos").join("holders").join("graph");
    std::fs::create_dir_all(&holders)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(holders.join(format!("{}-0.holder", std::process::id().wrapping_add(1))))?;
    file.lock()?;
    Ok(file)
}

fn structured(result: &CallToolResult) -> Result<&Value, Box<dyn std::error::Error + Send + Sync>> {
    result
        .structured_content
        .as_ref()
        .ok_or_else(|| format!("the result omitted its structured content: {result:?}").into())
}

#[tokio::test]
async fn a_server_opening_a_vault_with_a_damaged_graph_store_rebuilds_it_and_reports_where_the_damage_went()
-> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    damaged_graph_store(vault.path())?;
    let server = server_for(vault.path())?;

    let status = call_tool(server, "query_index_status", json!({})).await?;

    assert_eq!(status.is_error, Some(false), "{status:?}");
    let graph = graph_status(&status)?;
    assert_eq!(graph.get("health"), Some(&json!("ready")), "{graph}");
    let moved_to = graph
        .pointer("/last_recovery/moved_to")
        .and_then(Value::as_str)
        .ok_or("the recovery must name where the damaged store went")?;
    assert!(
        Path::new(moved_to).join("graph").join("graph.sqlite3").is_file(),
        "{graph}"
    );
    assert!(graph.pointer("/last_recovery/at").is_some(), "{graph}");
    Ok(())
}

#[tokio::test]
async fn a_damaged_graph_store_another_instance_holds_is_reported_by_status_naming_the_failure() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    damaged_graph_store(vault.path())?;
    let _other = another_instance_holding_the_graph(vault.path())?;
    let server = server_for(vault.path())?;

    let status = call_tool(server, "query_index_status", json!({})).await?;

    assert_eq!(status.is_error, Some(false), "{status:?}");
    let graph = graph_status(&status)?;
    assert_eq!(graph.get("health"), Some(&json!("unreadable")), "{graph}");
    assert_eq!(graph.get("needs_rebuild"), Some(&json!(true)), "{graph}");
    let failure = graph
        .get("failure")
        .ok_or("an unreadable graph must name its failure")?;
    assert_eq!(failure.get("code"), Some(&json!("index/corrupt")), "{failure}");
    assert!(
        failure
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains("damaged")),
        "{failure}"
    );
    assert!(
        failure
            .get("remediation")
            .and_then(Value::as_str)
            .is_some_and(|remediation| remediation.contains("moves the damaged files aside")),
        "{failure}"
    );
    Ok(())
}

#[tokio::test]
async fn vault_info_still_answers_for_a_vault_whose_graph_store_is_damaged_and_held() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("a.md"), "# A\n\nSearchable prose.\n")?;
    damaged_graph_store(vault.path())?;
    let _other = another_instance_holding_the_graph(vault.path())?;
    let server = server_for(vault.path())?;

    let info = call_tool(server, "vault_info", json!({})).await?;

    assert_eq!(
        info.is_error,
        Some(false),
        "vault_info must not fail on one damaged store: {info:?}"
    );
    let indexes = structured(&info)?
        .pointer("/vaults/0/indexes")
        .ok_or("vault_info omitted the vault's indexes")?;
    assert_eq!(
        indexes.pointer("/graph/health"),
        Some(&json!("unreadable")),
        "{indexes}"
    );
    assert_eq!(
        indexes.pointer("/graph/failure/code"),
        Some(&json!("index/corrupt")),
        "{indexes}"
    );
    assert_eq!(indexes.pointer("/text/health"), Some(&json!("ready")), "{indexes}");
    assert_eq!(indexes.pointer("/text/enabled"), Some(&json!(true)), "{indexes}");
    Ok(())
}

#[tokio::test]
async fn a_rebuild_of_a_damaged_graph_store_another_instance_holds_is_refused_as_in_use() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    damaged_graph_store(vault.path())?;
    let _other = another_instance_holding_the_graph(vault.path())?;
    let server = server_for(vault.path())?;

    let refused = call_tool(server, "query_index_rebuild", json!({"index": "graph"})).await?;

    assert_eq!(refused.is_error, Some(true), "{refused:?}");
    let error = structured(&refused)?;
    assert_eq!(error.get("code"), Some(&json!("index/in-use")), "{error}");
    assert!(
        error
            .get("remediation")
            .and_then(Value::as_str)
            .is_some_and(|remediation| remediation.contains("stop the other contextos processes")),
        "{error}"
    );
    assert!(vault.path().join(".contextos/graph/graph.sqlite3").is_file());
    Ok(())
}

#[tokio::test]
async fn a_rebuild_recovers_a_damaged_graph_store_once_no_other_instance_holds_it_and_reports_where_it_went()
-> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    damaged_graph_store(vault.path())?;
    let other = another_instance_holding_the_graph(vault.path())?;
    let server = server_for(vault.path())?;
    drop(other);

    let rebuilt = call_tool(server, "query_index_rebuild", json!({"index": "graph"})).await?;

    assert_eq!(rebuilt.is_error, Some(false), "{rebuilt:?}");
    let result = structured(&rebuilt)?;
    assert_eq!(result.pointer("/recovered/0/store"), Some(&json!("graph")), "{result}");
    let moved_to = result
        .pointer("/recovered/0/moved_to")
        .and_then(Value::as_str)
        .ok_or("the rebuild must name where the damaged store went")?;
    assert!(Path::new(moved_to).join("graph").join("graph.sqlite3").is_file());
    Ok(())
}

#[tokio::test]
async fn a_vault_write_succeeds_with_a_warning_when_its_graph_store_is_damaged_and_held() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    damaged_graph_store(vault.path())?;
    let _other = another_instance_holding_the_graph(vault.path())?;
    let server = server_for(vault.path())?;

    let written = call_tool(
        server,
        "fs_write_file",
        json!({"path": "new.md", "content": "# New\n\n[[note-1-with-a-reasonably-long-path-to-grow-the-store-file]]\n"}),
    )
    .await?;

    assert_eq!(
        written.is_error,
        Some(false),
        "the write itself must succeed: {written:?}"
    );
    assert!(vault.path().join("new.md").is_file());
    let warnings = structured(&written)?
        .get("warnings")
        .and_then(Value::as_array)
        .ok_or("the write must report the graph failure as a warning")?;
    assert!(
        warnings
            .iter()
            .filter_map(Value::as_str)
            .any(|warning| warning.starts_with("index/corrupt:")),
        "{warnings:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_graph_query_against_a_damaged_store_another_instance_holds_is_refused_with_the_failures_own_code()
-> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    damaged_graph_store(vault.path())?;
    let _other = another_instance_holding_the_graph(vault.path())?;
    let server = server_for(vault.path())?;

    let refused = call_tool(server, "query_graph", json!({"operation": "orphans"})).await?;

    assert_eq!(refused.is_error, Some(true), "{refused:?}");
    let error = structured(&refused)?;
    assert_eq!(error.get("code"), Some(&json!("index/in-use")), "{error}");
    Ok(())
}
