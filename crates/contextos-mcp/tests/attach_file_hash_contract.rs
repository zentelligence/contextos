//! `fs_attach_file` reports the content hash of the exact bytes it embedded, so a caller that read a file whole can
//! overwrite it with `expected_hash` without a second read, and without a window in which the file could change between
//! the read and the hash being taken. Exercised through a real MCP transport, as every sibling `*_contract.rs` file is;
//! each keeps its own `call_tool` copy, matching this directory's existing pattern.

mod support;
use contextos_mcp::ContextOsServer;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
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

fn server_for(vault: &tempfile::TempDir) -> Result<ContextOsServer, Box<dyn std::error::Error + Send + Sync>> {
    Ok(ContextOsServer::try_from(support::scoped_config(vec![
        vault.path().to_path_buf(),
    ])?)?)
}

/// The hash the attachment reports, read from the text block that follows the embedded resource.
fn reported_hash(result: &CallToolResult) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let block = result
        .content
        .get(1)
        .and_then(ContentBlock::as_text)
        .ok_or_else(|| std::io::Error::other("no text block follows the embedded resource"))?;
    let value: Value = serde_json::from_str(&block.text)?;
    Ok(value
        .get("content_hash")
        .and_then(Value::as_str)
        .ok_or_else(|| std::io::Error::other("the text block carries no content_hash"))?
        .to_owned())
}

fn file_info_hash(result: &CallToolResult) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    Ok(result
        .structured_content
        .as_ref()
        .and_then(|value| value.get("content_hash"))
        .and_then(Value::as_str)
        .ok_or_else(|| std::io::Error::other("fs_get_file_info returned no content_hash"))?
        .to_owned())
}

#[tokio::test]
async fn attached_text_and_binary_files_report_the_hash_fs_get_file_info_reports() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("plain.txt"), "hello\nworld\n")?;
    std::fs::write(vault.path().join("image.png"), [0_u8, 1, 2, 3, 255])?;
    let server = server_for(&vault)?;

    for path in ["plain.txt", "image.png"] {
        let attached = call_tool(server.clone(), "fs_attach_file", json!({ "path": path })).await?;
        let info = call_tool(server.clone(), "fs_get_file_info", json!({ "path": path })).await?;
        assert_eq!(attached.is_error, Some(false), "{path}");
        assert!(
            attached.content.first().and_then(ContentBlock::as_resource).is_some(),
            "the embedded resource must remain the first content block for {path}"
        );
        assert_eq!(reported_hash(&attached)?, file_info_hash(&info)?, "{path}");
    }
    Ok(())
}

#[tokio::test]
async fn the_attached_hash_authorises_an_overwrite_and_a_stale_one_is_refused() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let target = vault.path().join("note.md");
    std::fs::write(&target, "first\n")?;
    let server = server_for(&vault)?;

    let attached = call_tool(server.clone(), "fs_attach_file", json!({ "path": "note.md" })).await?;
    let hash = reported_hash(&attached)?;
    let written = call_tool(
        server.clone(),
        "fs_write_file",
        json!({ "path": "note.md", "content": "second\n", "expected_hash": hash }),
    )
    .await?;
    assert_eq!(written.is_error, Some(false));
    assert_eq!(std::fs::read_to_string(&target)?, "second\n");

    let stale = call_tool(
        server,
        "fs_write_file",
        json!({ "path": "note.md", "content": "third\n", "expected_hash": hash }),
    )
    .await?;
    assert_eq!(stale.is_error, Some(true));
    assert_eq!(
        stale.structured_content.as_ref().and_then(|value| value.get("code")),
        Some(&json!("io/conflict"))
    );
    assert_eq!(std::fs::read_to_string(&target)?, "second\n");
    Ok(())
}

#[tokio::test]
async fn a_failed_attachment_carries_no_hash() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let oversized = std::fs::File::create(vault.path().join("oversized.bin"))?;
    oversized.set_len(10 * 1024 * 1024 + 1)?;
    let server = server_for(&vault)?;

    let refused = call_tool(server, "fs_attach_file", json!({ "path": "oversized.bin" })).await?;

    assert_eq!(refused.is_error, Some(true));
    assert!(
        refused
            .content
            .iter()
            .filter_map(ContentBlock::as_text)
            .all(|block| !block.text.contains("content_hash")),
        "a refused attachment must not report a hash"
    );
    Ok(())
}
