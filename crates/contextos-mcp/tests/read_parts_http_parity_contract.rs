//! A byte part of `fs_read_text_file`, and each way a part or a continued read is refused, are identical over
//! stdio and streamable HTTP. Kept apart from `http_transport.rs` to keep that file under the project's file-size
//! limit; the HTTP harness here follows it.

mod support;

use contextos_mcp::{ContextOsServer, HttpConfig};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Map, Value, json};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const TOKEN: &str = "read-parts-parity-token";

async fn call_over_http(url: &str, arguments: Map<String, Value>) -> Result<CallToolResult, BoxError> {
    let config = StreamableHttpClientTransportConfig::with_uri(url.to_owned()).auth_header(TOKEN.to_owned());
    let client = ().serve(StreamableHttpClientTransport::from_config(config)).await?;
    let result = client
        .call_tool(CallToolRequestParams::new("fs_read_text_file").with_arguments(arguments))
        .await;
    client.cancel().await?;
    Ok(result?)
}

async fn call_over_stdio(server: ContextOsServer, arguments: Map<String, Value>) -> Result<CallToolResult, BoxError> {
    let (server_transport, client_transport) = tokio::io::duplex(65536);
    let server_handle = tokio::spawn(async move {
        server.serve(server_transport).await?.waiting().await?;
        Ok::<(), BoxError>(())
    });
    let client = ().serve(client_transport).await?;
    let result = client
        .call_tool(CallToolRequestParams::new("fs_read_text_file").with_arguments(arguments))
        .await;
    client.cancel().await?;
    server_handle.await??;
    Ok(result?)
}

fn structured(result: &CallToolResult, context: &str) -> Result<Value, BoxError> {
    result
        .structured_content
        .clone()
        .ok_or_else(|| std::io::Error::other(format!("{context}: no structured content: {result:?}")).into())
}

#[tokio::test]
async fn a_part_and_its_refusals_are_identical_over_stdio_and_http() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("note.md"), "alpha\nbeta\ngamma é")?;
    std::fs::write(vault.path().join("bad.md"), [0xE2, 0x80, 0x80, 0x80, 0x80])?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].index_md.enabled = false;
    let server = ContextOsServer::try_from(config)?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let router = contextos_mcp::build_router(
        server.clone(),
        &HttpConfig {
            bind: "127.0.0.1:0".to_owned(),
            token: TOKEN.to_owned(),
            token_env: String::new(),
            max_body_kb: 2048,
        },
    )?;
    let shutdown = CancellationToken::new();
    let serving = shutdown.clone();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move { serving.cancelled().await })
            .await;
    });
    let url = format!("http://{address}{}", contextos_mcp::HTTP_MOUNT_PATH);

    // A well-formed version no file has, so the read is refused as changed.
    let stale = "0123456789abcdef0123456789abcdef";
    let requests: [(&str, Value, Option<&str>); 10] = [
        ("a part", json!({ "path": "note.md", "offset": 6, "length": 4 }), None),
        ("a part at the end", json!({ "path": "note.md", "offset": 17 }), None),
        (
            "a short length",
            json!({ "path": "note.md", "offset": 0, "length": 2 }),
            Some("io/invalid-range"),
        ),
        (
            "an offset beyond the end",
            json!({ "path": "note.md", "offset": 99 }),
            Some("io/invalid-range"),
        ),
        (
            "a stale version",
            json!({ "path": "note.md", "offset": 0, "expected_version": stale }),
            Some("io/conflict"),
        ),
        (
            "an offset inside a character",
            json!({ "path": "note.md", "offset": 18 }),
            Some("io/invalid-range"),
        ),
        (
            "a part refused as binary",
            json!({ "path": "bad.md", "offset": 0, "length": 4 }),
            Some("io/binary"),
        ),
        (
            "a part combined with head",
            json!({ "path": "note.md", "offset": 0, "head": 1 }),
            Some("io/invalid-argument"),
        ),
        (
            "a stale version with a short length",
            json!({ "path": "note.md", "length": 2, "expected_version": stale }),
            Some("io/conflict"),
        ),
        (
            "a malformed version",
            json!({ "path": "note.md", "offset": 0, "expected_version": "nope" }),
            Some("io/invalid-version"),
        ),
    ];
    for (context, request, refused_with) in requests {
        let arguments: Map<String, Value> = serde_json::from_value(request)?;
        let over_stdio = call_over_stdio(server.clone(), arguments.clone()).await?;
        let over_http = call_over_http(&url, arguments).await?;
        let stdio_value = structured(&over_stdio, context)?;
        let http_value = structured(&over_http, context)?;

        assert_eq!(stdio_value, http_value, "{context} differs between transports");
        assert_eq!(over_stdio.is_error, over_http.is_error, "{context}");
        if let Some(code) = refused_with {
            assert_eq!(over_http.is_error, Some(true), "{context}: {http_value}");
            assert_eq!(http_value.get("code"), Some(&json!(code)), "{context}: {http_value}");
        } else {
            assert_eq!(over_http.is_error, Some(false), "{context}: {http_value}");
            assert!(http_value.get("next_offset").is_some(), "{context}: {http_value}");
            assert!(http_value.get("version").is_some(), "{context}: {http_value}");
        }
    }

    shutdown.cancel();
    handle.await?;
    Ok(())
}
