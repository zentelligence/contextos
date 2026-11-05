//! `fs_write_file`'s binary content support: `content_base64`, capped at
//! the same fixed 10 MiB `fs_attach_file`'s own binary read path already
//! uses. Exercised through a real MCP transport
//! (`.claude/rules/mcp-contracts.md`: tests invoke the adapter, not the
//! underlying service). Shares its `call_tool`/`assert_tool_success`/
//! `assert_tool_error` helpers with every sibling `*_contract.rs` file in
//! this directory (each keeps its own copy, matching this codebase's
//! existing per-file pattern).

mod support;
use base64::Engine;
use contextos_mcp::ContextOsServer;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use serde_json::{Map, json};

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

async fn assert_tool_success(
    vault: &std::path::Path,
    name: &'static str,
    arguments: Map<String, serde_json::Value>,
) -> Result<serde_json::Map<String, serde_json::Value>, Box<dyn std::error::Error + Send + Sync>> {
    let config = support::scoped_config(vec![vault.to_path_buf()])?;
    let server = ContextOsServer::try_from(config)?;
    let result = call_tool(server, name, arguments).await?;
    let structured = result
        .structured_content
        .ok_or_else(|| std::io::Error::other(format!("{name} did not return structured content")))?;
    assert_eq!(result.is_error, Some(false), "{name} returned a tool error");
    let object = structured
        .as_object()
        .ok_or_else(|| std::io::Error::other("expected a JSON object"))?
        .clone();
    Ok(object)
}

async fn assert_tool_error(
    vault: &std::path::Path,
    name: &'static str,
    arguments: Map<String, serde_json::Value>,
    expected_code: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = support::scoped_config(vec![vault.to_path_buf()])?;
    let server = ContextOsServer::try_from(config)?;
    let result = call_tool(server, name, arguments).await?;
    let structured = result
        .structured_content
        .ok_or_else(|| std::io::Error::other(format!("{name} did not return structured error content")))?;
    assert_eq!(result.is_error, Some(true), "{name} did not mark its error");
    assert_eq!(structured.get("code"), Some(&json!(expected_code)));
    Ok(())
}

#[tokio::test]
async fn fs_write_file_writes_exact_bytes_from_content_base64() -> Result<(), Box<dyn std::error::Error + Send + Sync>>
{
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let bytes = [0_u8, 1, 2, 3, 255, 254, 128];
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);

    let fields = assert_tool_success(
        vault.path(),
        "fs_write_file",
        serde_json::from_value(json!({"path": "image.png", "content_base64": encoded}))?,
    )
    .await?;
    assert_eq!(fields.get("bytes_written"), Some(&json!(bytes.len())));
    assert_eq!(std::fs::read(vault.path().join("image.png"))?, bytes);
    Ok(())
}

#[tokio::test]
async fn fs_write_file_rejects_both_content_and_content_base64() -> Result<(), Box<dyn std::error::Error + Send + Sync>>
{
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    assert_tool_error(
        vault.path(),
        "fs_write_file",
        serde_json::from_value(json!({
            "path": "note.md",
            "content": "text",
            "content_base64": "AAA=",
        }))?,
        "io/invalid-argument",
    )
    .await?;
    assert!(!vault.path().join("note.md").exists());
    Ok(())
}

#[tokio::test]
async fn fs_write_file_rejects_neither_content_nor_content_base64()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    assert_tool_error(
        vault.path(),
        "fs_write_file",
        serde_json::from_value(json!({"path": "note.md"}))?,
        "io/invalid-argument",
    )
    .await?;
    assert!(!vault.path().join("note.md").exists());
    Ok(())
}

#[tokio::test]
async fn fs_write_file_rejects_invalid_base64() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    assert_tool_error(
        vault.path(),
        "fs_write_file",
        serde_json::from_value(json!({"path": "note.md", "content_base64": "not valid base64!!"}))?,
        "io/invalid-argument",
    )
    .await?;
    assert!(!vault.path().join("note.md").exists());
    Ok(())
}

#[tokio::test]
async fn fs_write_file_rejects_binary_content_over_the_ten_mebibyte_cap()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let oversized = vec![0_u8; 10 * 1024 * 1024 + 1];
    let encoded = base64::engine::general_purpose::STANDARD.encode(oversized);

    assert_tool_error(
        vault.path(),
        "fs_write_file",
        serde_json::from_value(json!({"path": "oversized.bin", "content_base64": encoded}))?,
        "io/too-large-to-write",
    )
    .await?;
    assert!(!vault.path().join("oversized.bin").exists());
    Ok(())
}

/// The cap is inclusive and exact: content of precisely 10 MiB is written,
/// whatever padding its base64 encoding carries, and content one byte over is
/// refused naming its true size, not an estimate from its encoded length.
#[tokio::test]
async fn fs_write_file_accepts_binary_content_of_exactly_the_cap_and_refuses_one_byte_more()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let cap = 10 * 1024 * 1024;
    // 10 MiB leaves a remainder of one byte in three, so its encoding ends in
    // "==", the padding an encoded-length estimate overcounts by two bytes.
    let exact = vec![0x5A_u8; cap];
    assert_tool_success(
        vault.path(),
        "fs_write_file",
        serde_json::from_value(json!({
            "path": "exact.bin",
            "content_base64": base64::engine::general_purpose::STANDARD.encode(&exact),
        }))?,
    )
    .await?;
    assert_eq!(std::fs::read(vault.path().join("exact.bin"))?, exact);

    let over = vec![0x5A_u8; cap + 1];
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    let result = call_tool(
        server,
        "fs_write_file",
        serde_json::from_value(json!({
            "path": "over.bin",
            "content_base64": base64::engine::general_purpose::STANDARD.encode(&over),
        }))?,
    )
    .await?;
    let content = result
        .structured_content
        .ok_or("the refusal carried no structured content")?;
    assert_eq!(content["code"], json!("io/too-large-to-write"));
    let message = content["message"].as_str().ok_or("the refusal carried no message")?;
    assert!(message.contains(&format!("{} bytes", cap + 1)), "{message}");
    assert!(!vault.path().join("over.bin").exists());
    Ok(())
}

/// `fs_write_file`'s binary write and `fs_attach_file`'s binary read agree
/// on the same bytes: the round trip a client actually cares about.
#[tokio::test]
async fn fs_write_file_and_fs_attach_file_round_trip_binary_content()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    let server = ContextOsServer::try_from(config)?;
    let bytes = [10_u8, 20, 30, 255, 0];
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);

    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let server_handle = tokio::spawn(async move {
        server.serve(server_transport).await?.waiting().await?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });
    let mut client = ().serve(client_transport).await?;

    let write_arguments: Map<String, serde_json::Value> =
        serde_json::from_value(json!({"path": "blob.bin", "content_base64": encoded}))?;
    let write_result = client
        .call_tool(CallToolRequestParams::new("fs_write_file").with_arguments(write_arguments))
        .await?;
    assert_eq!(write_result.is_error, Some(false));

    let attach_arguments: Map<String, serde_json::Value> = serde_json::from_value(json!({"path": "blob.bin"}))?;
    let attach_result = client
        .call_tool(CallToolRequestParams::new("fs_attach_file").with_arguments(attach_arguments))
        .await?;
    let attached_resource = attach_result
        .content
        .first()
        .and_then(rmcp::model::ContentBlock::as_resource)
        .ok_or_else(|| std::io::Error::other("fs_attach_file did not embed a resource"))?;
    match &attached_resource.resource {
        rmcp::model::ResourceContents::BlobResourceContents { blob, .. } => {
            assert_eq!(
                blob, &encoded,
                "fs_attach_file must read back exactly what fs_write_file wrote"
            );
        }
        other => {
            return Err(std::io::Error::other(format!("expected blob resource contents, got {other:?}")).into());
        }
    }

    client.close().await?;
    server_handle.await??;
    Ok(())
}
