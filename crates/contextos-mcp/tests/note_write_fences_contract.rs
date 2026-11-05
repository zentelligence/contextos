//! A note whose text holds an unclosed `[[` inside a code fence is a valid note, wherever the fence sits: inside a list
//! item, indented by a tab or spaces, or inside a callout. A note write refuses an unclosed link in prose only.

mod support;

use contextos_mcp::ContextOsServer;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, Value, json};

type TestError = Box<dyn std::error::Error + Send + Sync>;

async fn call_tool(server: ContextOsServer, name: &'static str, arguments: Value) -> Result<CallToolResult, TestError> {
    let arguments: Map<String, Value> = serde_json::from_value(arguments)?;
    let (server_transport, client_transport) = tokio::io::duplex(8192);
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

const FENCED_BODIES: [&str; 4] = [
    "- step\n\t```\n\t[[unclosed\n\t```\n",
    "- step\n    ```sh\n    echo [[unclosed\n    ```\n",
    "> [!example]\n> ```\n> [[unclosed\n> ```\n",
    "1. step\n   ```\n   [[unclosed\n   ```\n",
];

#[tokio::test]
async fn note_create_accepts_an_unclosed_link_inside_a_fence_in_a_list_item_or_callout() -> Result<(), TestError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;

    for (index, body) in FENCED_BODIES.iter().enumerate() {
        let path = format!("fenced-{index}.md");
        let result = call_tool(server.clone(), "note_create", json!({"path": path, "content": body})).await?;
        assert_eq!(result.is_error, Some(false), "{body:?}: {result:?}");
    }
    Ok(())
}

#[tokio::test]
async fn note_create_still_refuses_an_unclosed_link_in_prose_after_such_a_fence() -> Result<(), TestError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    let body = "- step\n\t```\n\t[[inside\n\t```\nthen [[open in prose\n";

    let result = call_tool(server, "note_create", json!({"path": "prose.md", "content": body})).await?;

    assert_eq!(result.is_error, Some(true), "{result:?}");
    assert!(!vault.path().join("prose.md").exists());
    Ok(())
}

#[tokio::test]
async fn note_update_accepts_an_unclosed_link_inside_a_fence_in_a_list_item_or_callout() -> Result<(), TestError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(
        vault.path().join("note.md"),
        "---\ntitle: Note\ncreated: 2026-08-07T09:00+10:00\nupdated: 2026-08-07T09:00+10:00\n---\n\nOld body.\n",
    )?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    for body in FENCED_BODIES {
        let read = call_tool(server.clone(), "frontmatter_read", json!({"path": "note.md"})).await?;
        let hash = read
            .structured_content
            .as_ref()
            .and_then(|content| content.get("content_hash"))
            .and_then(Value::as_str)
            .ok_or_else(|| std::io::Error::other("no content_hash"))?
            .to_owned();

        let result = call_tool(
            server.clone(),
            "note_update",
            json!({"path": "note.md", "expected_hash": hash, "body": body}),
        )
        .await?;

        assert_eq!(result.is_error, Some(false), "{body:?}: {result:?}");
    }
    Ok(())
}
