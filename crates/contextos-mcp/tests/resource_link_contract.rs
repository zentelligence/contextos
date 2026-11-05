//! `resource_link` attachment on oversized tool results: `fs_read_text_file`
//! and `fs_read_multiple_files` return an inline preview plus a
//! `resource_link` content block once a file crosses
//! `server.resource_link_threshold_kb`, and that link's URI reads back the
//! complete, untruncated content. Exercised through a real MCP transport
//! (`.claude/rules/mcp-contracts.md`: tests invoke the adapter, not the
//! underlying service).

mod resource_support;

use contextos_mcp::{Config, ContextOsServer};
use rmcp::model::{CallToolRequestParams, ReadResourceRequestParams, ResourceContents};

use resource_support::{BoxError, connect};

// Debug-formatting the path (rather than `Display`/`.display()`) is
// required here, not just conventional: TOML string values need the
// surrounding quotes and escaping `Debug` provides, which `Display` would
// strip, breaking the generated config for any path with special
// characters.
#[expect(
    clippy::unnecessary_debug_formatting,
    reason = "Debug quoting/escaping is required to embed the path as a valid TOML string"
)]
fn threshold_config(vault: &std::path::Path, threshold_kb: u64) -> Result<Config, BoxError> {
    let source = format!(
        "[[vault]]\npath = {vault:?}\nname = \"vault\"\nstate_directory = \".contextos\"\n\
         [server]\nresource_link_threshold_kb = {threshold_kb}\n"
    );
    Ok(Config::try_from(source.as_str())?)
}

fn call_tool_arguments(value: &serde_json::Value) -> Result<serde_json::Map<String, serde_json::Value>, BoxError> {
    value
        .as_object()
        .cloned()
        .ok_or_else(|| std::io::Error::other("expected a JSON object").into())
}

#[tokio::test]
async fn fs_read_text_file_below_threshold_returns_full_content_with_no_resource_link() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    // Exactly one byte under the 1 KB threshold, so this is the tightest
    // possible "still below" boundary case.
    let content = "a".repeat(1024 - 1);
    std::fs::write(vault.path().join("note.md"), &content)?;

    let config = threshold_config(vault.path(), 1)?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let result = client
        .call_tool(
            CallToolRequestParams::new("fs_read_text_file")
                .with_arguments(call_tool_arguments(&serde_json::json!({ "path": "note.md" }))?),
        )
        .await?;

    assert_eq!(result.is_error, Some(false));
    assert_eq!(
        result
            .structured_content
            .as_ref()
            .and_then(|value| value.get("content"))
            .and_then(serde_json::Value::as_str),
        Some(content.as_str()),
        "below threshold, content must be returned in full, unchanged"
    );
    assert_eq!(
        result
            .structured_content
            .as_ref()
            .and_then(|value| value.get("truncated"))
            .and_then(serde_json::Value::as_bool),
        Some(false)
    );
    assert_eq!(
        result.content.len(),
        1,
        "below threshold, no resource_link content block is attached"
    );
    assert!(rmcp::model::ContentBlock::as_resource_link(&result.content[0]).is_none());

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn fs_read_text_file_above_threshold_attaches_a_resource_link_whose_uri_reads_back_identical_content()
-> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let content = "line one\n".repeat(200); // well over 1 KB
    std::fs::write(vault.path().join("note.md"), &content)?;

    let config = threshold_config(vault.path(), 1)?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let result = client
        .call_tool(
            CallToolRequestParams::new("fs_read_text_file")
                .with_arguments(call_tool_arguments(&serde_json::json!({ "path": "note.md" }))?),
        )
        .await?;

    assert_eq!(result.is_error, Some(false));
    let preview = result
        .structured_content
        .as_ref()
        .and_then(|value| value.get("content"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| std::io::Error::other("missing content field"))?
        .to_owned();
    assert!(
        preview.len() < content.len(),
        "at or above threshold, the inline preview must be shorter than the full file"
    );
    assert!(
        content.starts_with(&preview),
        "the preview must be a genuine prefix of the real content, not a stub"
    );
    assert_eq!(
        result
            .structured_content
            .as_ref()
            .and_then(|value| value.get("truncated"))
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );

    let link = result
        .content
        .iter()
        .find_map(rmcp::model::ContentBlock::as_resource_link)
        .ok_or_else(|| std::io::Error::other("no resource_link content block was attached"))?;
    assert!(link.uri.starts_with("vault://"));

    let read = client
        .read_resource(ReadResourceRequestParams::new(link.uri.clone()))
        .await?;
    match &read.contents[0] {
        ResourceContents::TextResourceContents { text, .. } => {
            assert_eq!(
                text, &content,
                "the resource_link must resolve to the complete, untruncated content"
            );
        }
        other => {
            return Err(std::io::Error::other(format!("expected text resource contents, got {other:?}")).into());
        }
    }

    client.close().await?;
    server_handle.await??;
    Ok(())
}

#[tokio::test]
async fn fs_read_multiple_files_attaches_per_file_resource_links_independently() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let small_content = "small";
    let large_content = "line one\n".repeat(200);
    std::fs::write(vault.path().join("small.md"), small_content)?;
    std::fs::write(vault.path().join("large.md"), &large_content)?;

    let config = threshold_config(vault.path(), 1)?;
    let server = ContextOsServer::try_from(config)?;
    let (mut client, server_handle) = connect(server).await?;

    let result = client
        .call_tool(
            CallToolRequestParams::new("fs_read_multiple_files").with_arguments(call_tool_arguments(
                &serde_json::json!({ "paths": ["small.md", "large.md"] }),
            )?),
        )
        .await?;

    assert_eq!(result.is_error, Some(false));
    let files = result
        .structured_content
        .as_ref()
        .and_then(|value| value.get("files"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| std::io::Error::other("missing files field"))?;
    let small = files
        .iter()
        .find(|file| file.get("path").and_then(serde_json::Value::as_str) == Some("small.md"))
        .ok_or_else(|| std::io::Error::other("small.md missing from batch result"))?;
    let large = files
        .iter()
        .find(|file| file.get("path").and_then(serde_json::Value::as_str) == Some("large.md"))
        .ok_or_else(|| std::io::Error::other("large.md missing from batch result"))?;

    assert_eq!(
        small.get("content").and_then(serde_json::Value::as_str),
        Some(small_content)
    );
    assert_eq!(small.get("truncated").and_then(serde_json::Value::as_bool), Some(false));
    assert_eq!(large.get("truncated").and_then(serde_json::Value::as_bool), Some(true));
    let large_preview = large
        .get("content")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| std::io::Error::other("large.md missing content"))?;
    assert!(large_preview.len() < large_content.len());

    // Exactly one resource_link, for the one file that crossed the
    // threshold: the small file's own headroom must not spuriously
    // attach a second link.
    let links: Vec<_> = result
        .content
        .iter()
        .filter_map(rmcp::model::ContentBlock::as_resource_link)
        .collect();
    assert_eq!(links.len(), 1);

    let read = client
        .read_resource(ReadResourceRequestParams::new(links[0].uri.clone()))
        .await?;
    match &read.contents[0] {
        ResourceContents::TextResourceContents { text, .. } => {
            assert_eq!(text, &large_content);
        }
        other => {
            return Err(std::io::Error::other(format!("expected text resource contents, got {other:?}")).into());
        }
    }

    client.close().await?;
    server_handle.await??;
    Ok(())
}
