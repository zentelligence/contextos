//! What a `query_graph` node says beyond its path: the kind of thing it stands for, the files an ambiguous link
//! target answers to, and why a note's links could not be read.

mod support;

use contextos_mcp::ContextOsServer;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, Value, json};

type TestError = Box<dyn std::error::Error + Send + Sync>;

async fn call_tool(
    server: ContextOsServer,
    name: &'static str,
    arguments: Map<String, Value>,
) -> Result<CallToolResult, TestError> {
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server_handle = tokio::spawn(async move {
        server.serve(server_transport).await?.waiting().await?;
        Ok::<(), TestError>(())
    });
    let mut client = ().serve(client_transport).await?;
    let result = client
        .call_tool(CallToolRequestParams::new(name).with_arguments(arguments))
        .await;
    client.close().await?;
    server_handle.await??;
    Ok(result?)
}

fn nodes(result: &CallToolResult) -> Result<Vec<Value>, TestError> {
    result
        .structured_content
        .as_ref()
        .and_then(|content| content.get("nodes"))
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| std::io::Error::other("query_graph omitted nodes").into())
}

fn server_over(vault: &tempfile::TempDir) -> Result<ContextOsServer, TestError> {
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].index_md.enabled = false;
    Ok(ContextOsServer::try_from(config)?)
}

#[tokio::test]
async fn an_ambiguous_target_node_names_the_files_it_answers_to() -> Result<(), TestError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("x"))?;
    std::fs::create_dir_all(vault.path().join("y"))?;
    std::fs::write(vault.path().join("x/t.md"), "x\n")?;
    std::fs::write(vault.path().join("y/t.md"), "y\n")?;
    std::fs::write(vault.path().join("a.md"), "[[t]]\n")?;
    let server = server_over(&vault)?;
    call_tool(
        server.clone(),
        "query_index_rebuild",
        serde_json::from_value(json!({"index": "graph"}))?,
    )
    .await?;

    let result = call_tool(
        server,
        "query_graph",
        serde_json::from_value(json!({"operation": "neighbours", "from": "a.md"}))?,
    )
    .await?;

    let nodes = nodes(&result)?;
    let ambiguous = nodes
        .iter()
        .find(|node| node.get("kind") == Some(&json!("ambiguous")))
        .ok_or_else(|| std::io::Error::other("no ambiguous node"))?;
    assert_eq!(ambiguous.get("path"), Some(&json!("t")));
    assert_eq!(ambiguous.get("candidates"), Some(&json!(["x/t.md", "y/t.md"])));
    assert_eq!(ambiguous.get("unreadable"), None);
    let note = nodes
        .iter()
        .find(|node| node.get("path") == Some(&json!("a.md")))
        .ok_or_else(|| std::io::Error::other("no note node"))?;
    assert_eq!(note.get("candidates"), None);
    Ok(())
}

#[tokio::test]
async fn a_note_whose_links_cannot_be_read_says_why() -> Result<(), TestError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("good.md"), "[[bad]]\n")?;
    std::fs::write(vault.path().join("bad.md"), "text\n[[unclosed\n")?;
    let server = server_over(&vault)?;
    call_tool(
        server.clone(),
        "query_index_rebuild",
        serde_json::from_value(json!({"index": "graph"}))?,
    )
    .await?;

    let result = call_tool(
        server,
        "query_graph",
        serde_json::from_value(json!({"operation": "neighbours", "from": "good.md"}))?,
    )
    .await?;

    let nodes = nodes(&result)?;
    let bad = nodes
        .iter()
        .find(|node| node.get("path") == Some(&json!("bad.md")))
        .ok_or_else(|| std::io::Error::other("no node for the unreadable note"))?;
    assert_eq!(bad.get("kind"), Some(&json!("note")));
    assert!(
        bad.get("unreadable")
            .and_then(Value::as_str)
            .is_some_and(|reason| reason.contains("line 2")),
        "{bad}"
    );
    Ok(())
}

#[tokio::test]
async fn a_link_added_outside_the_server_is_in_the_next_graph_answer_without_a_rebuild() -> Result<(), TestError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("a.md"), "# A\n")?;
    std::fs::write(vault.path().join("b.md"), "# B\n")?;
    let server = server_over(&vault)?;
    call_tool(
        server.clone(),
        "query_index_rebuild",
        serde_json::from_value(json!({"index": "graph"}))?,
    )
    .await?;
    std::fs::write(vault.path().join("a.md"), "# A\n\nnow links [[b]] and a ![[new.png]]\n")?;
    std::fs::write(vault.path().join("new.png"), [1_u8])?;

    let result = call_tool(
        server,
        "query_graph",
        serde_json::from_value(json!({"operation": "neighbours", "from": "a.md"}))?,
    )
    .await?;

    let nodes = nodes(&result)?;
    let kind_of = |path: &str| {
        nodes
            .iter()
            .find(|node| node.get("path") == Some(&json!(path)))
            .and_then(|node| node.get("kind"))
    };
    assert_eq!(kind_of("b.md"), Some(&json!("note")));
    assert_eq!(kind_of("new.png"), Some(&json!("file")));
    Ok(())
}
