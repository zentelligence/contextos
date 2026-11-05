//! The doctor's derived store checks over MCP. A server running over a link graph store damaged underneath it (its
//! own handle still reading cached pages, as on 2026-09-27) has `doctor` report the damage as a distinct failure, and
//! `doctor_resolve` move the store aside and rebuild it when no other instance holds it; while another instance does,
//! the finding names it and is left to the operator.

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

fn graph_store_check(result: &CallToolResult) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    result
        .structured_content
        .as_ref()
        .and_then(|content| content.get("checks"))
        .and_then(Value::as_array)
        .and_then(|checks| {
            checks
                .iter()
                .find(|check| check.get("kind") == Some(&json!("link_graph_store")))
        })
        .cloned()
        .ok_or_else(|| format!("no link graph store check in {result:?}").into())
}

/// A vault with a built `sqlite` link graph store, closed so its content is in the main database file.
fn vault_with_a_graph_store() -> Result<tempfile::TempDir, Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let notes = (0..500)
        .map(|i| {
            (
                format!("note-{i}-with-a-reasonably-long-path-to-grow-the-store-file.md"),
                format!("Note {i} also has a reasonably long title, to grow the store file further still"),
                Vec::new(),
            )
        })
        .collect::<Vec<_>>();
    for (path, title, _) in &notes {
        std::fs::write(vault.path().join(path), format!("# {title}\n"))?;
    }
    LinkGraph::try_from(LinkGraphConfig {
        store_directory: vault.path().join(".contextos").join("graph"),
        backend: GraphBackend::Sqlite,
        write_wait: contextos_search::DEFAULT_WRITE_WAIT,
    })?
    .rebuild(&notes)?;
    Ok(vault)
}

fn damage_the_graph_store(vault: &Path) -> TestResult {
    let file = vault.join(".contextos").join("graph").join("graph.sqlite3");
    let length = std::fs::metadata(&file)?.len();
    let mut handle = std::fs::OpenOptions::new().write(true).open(&file)?;
    handle.seek(SeekFrom::Start(length / 2))?;
    handle.write_all(&[0xFF_u8; 4096])?;
    Ok(())
}

fn server_for(vault: &Path) -> Result<ContextOsServer, Box<dyn std::error::Error + Send + Sync>> {
    Ok(ContextOsServer::try_from(support::scoped_config(vec![
        vault.to_path_buf(),
    ])?)?)
}

#[tokio::test]
async fn doctor_reports_a_healthy_graph_store_as_passing() -> TestResult {
    let vault = vault_with_a_graph_store()?;
    let server = server_for(vault.path())?;

    let check = graph_store_check(&call_tool(server, "doctor", json!({})).await?)?;

    assert_eq!(check.get("status"), Some(&json!("pass")), "{check}");
    assert!(
        check
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains("500 notes")),
        "{check}"
    );
    Ok(())
}

#[tokio::test]
async fn doctor_reports_a_graph_store_damaged_under_a_running_server_and_doctor_resolve_repairs_it() -> TestResult {
    let vault = vault_with_a_graph_store()?;
    let server = server_for(vault.path())?;
    damage_the_graph_store(vault.path())?;

    let check = graph_store_check(&call_tool(server.clone(), "doctor", json!({})).await?)?;
    assert_eq!(check.get("status"), Some(&json!("fail")), "{check}");
    assert_eq!(check.get("auto_fixable"), Some(&json!(true)), "{check}");
    assert_eq!(
        check.get("remediation_tool"),
        Some(&json!("query_index_rebuild")),
        "{check}"
    );
    assert!(
        check
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains("index/corrupt")),
        "{check}"
    );

    let resolved = call_tool(server.clone(), "doctor_resolve", json!({})).await?;
    assert_eq!(resolved.is_error, Some(false), "{resolved:?}");
    let outcome = resolved
        .structured_content
        .as_ref()
        .and_then(|content| content.get("outcomes"))
        .and_then(Value::as_array)
        .and_then(|outcomes| {
            outcomes
                .iter()
                .find(|outcome| outcome.get("kind") == Some(&json!("link_graph_store")))
        })
        .ok_or("doctor_resolve must act on the damaged store")?;
    assert_eq!(outcome.get("resolved"), Some(&json!(true)), "{outcome}");
    let after = graph_store_check(&resolved.structured_content.map_or_else(
        || CallToolResult::success(Vec::new()),
        |content| CallToolResult::structured(content.get("report").cloned().unwrap_or(Value::Null)),
    ))?;
    assert_eq!(after.get("status"), Some(&json!("pass")), "{after}");

    let status = call_tool(server, "query_index_status", json!({})).await?;
    let graph = status
        .structured_content
        .as_ref()
        .and_then(|content| content.get("graph"))
        .ok_or("query_index_status omitted the graph")?;
    assert_eq!(graph.get("health"), Some(&json!("ready")), "{graph}");
    assert!(
        graph
            .get("nodes")
            .and_then(Value::as_u64)
            .is_some_and(|nodes| nodes >= 500),
        "the graph must be rebuilt from every note in the vault: {graph}"
    );
    assert!(graph.pointer("/last_recovery/moved_to").is_some(), "{graph}");
    Ok(())
}

#[tokio::test]
async fn a_damaged_graph_store_another_instance_holds_is_named_and_left_to_the_operator() -> TestResult {
    let vault = vault_with_a_graph_store()?;
    let server = server_for(vault.path())?;
    damage_the_graph_store(vault.path())?;
    let holders = vault.path().join(".contextos").join("holders").join("graph");
    std::fs::create_dir_all(&holders)?;
    let other = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(holders.join(format!("{}-0.holder", std::process::id().wrapping_add(1))))?;
    other.lock()?;

    let check = graph_store_check(&call_tool(server.clone(), "doctor", json!({})).await?)?;
    assert_eq!(check.get("status"), Some(&json!("fail")), "{check}");
    assert_eq!(check.get("auto_fixable"), Some(&json!(false)), "{check}");
    assert!(
        check
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(|message| message.contains("open in 1 other contextos process")),
        "{check}"
    );

    let resolved = call_tool(server, "doctor_resolve", json!({})).await?;
    let acted_on_the_store = resolved
        .structured_content
        .as_ref()
        .and_then(|content| content.get("outcomes"))
        .and_then(Value::as_array)
        .is_some_and(|outcomes| {
            outcomes
                .iter()
                .any(|outcome| outcome.get("kind") == Some(&json!("link_graph_store")))
        });
    assert!(!acted_on_the_store, "{resolved:?}");
    assert!(vault.path().join(".contextos/graph/graph.sqlite3").is_file());
    Ok(())
}
