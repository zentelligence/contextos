//! Which MCP protocol revision a client speaks with this server, over stdio
//! and streamable HTTP alike.
//!
//! The server speaks revision 2026-07-28, where every request carries its
//! revision in `_meta` and no `initialize` handshake exists, and accepts the
//! earlier, handshake-based revisions only as far as rmcp provides them: a
//! legacy client negotiates through `initialize`, which can reach at most the
//! newest revision that still has one. `vault_info` reports the revision of
//! the session that called it.
//!
//! Most cases drive the server through rmcp's own client, the way a real host
//! would. The HTTP cases that need a request rmcp's client never sends (an
//! unknown revision, a missing header, a malformed modern request) build raw
//! requests against the router through `tower::ServiceExt::oneshot`.

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use contextos_mcp::{ContextOsServer, HttpConfig};
use rmcp::model::{CallToolRequestParams, CallToolResult, ClientConfig, ProtocolVersion};
use rmcp::service::{ClientLifecycleMode, ClientServiceExt, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::{RoleClient, Service, ServiceExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const TOKEN: &str = "revision-token";

fn server() -> Result<(ContextOsServer, tempfile::TempDir), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    Ok((server, vault))
}

fn http_config(token: &str) -> HttpConfig {
    HttpConfig {
        bind: "127.0.0.1:0".to_owned(),
        token: token.to_owned(),
        token_env: String::new(),
        max_body_kb: 2048,
    }
}

/// Serves `server` over HTTP on an ephemeral loopback port until the returned
/// token is cancelled.
async fn serve_http(server: ContextOsServer) -> Result<(String, CancellationToken), BoxError> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let router = contextos_mcp::build_router(server, &http_config(TOKEN))?;
    let shutdown = CancellationToken::new();
    let stop = shutdown.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move { stop.cancelled().await })
            .await;
    });
    Ok((format!("http://{addr}{}", contextos_mcp::HTTP_MOUNT_PATH), shutdown))
}

/// The client transport configuration for `url`, carrying the bearer token.
fn http_client_config(url: &str) -> StreamableHttpClientTransportConfig {
    StreamableHttpClientTransportConfig::with_uri(url.to_owned()).auth_header(TOKEN.to_owned())
}

/// The lifecycle of a client that speaks 2026-07-28 only.
fn modern() -> ClientLifecycleMode {
    ClientLifecycleMode::Discover {
        preferred_versions: vec![ProtocolVersion::V_2026_07_28],
    }
}

/// A legacy client asking `initialize` for `revision`.
fn legacy(revision: ProtocolVersion) -> ClientConfig {
    ClientConfig::default().with_protocol_version(revision)
}

/// Connects `client` to `server` over an in-process stdio pipe with the given
/// lifecycle.
async fn stdio_client<C: Service<RoleClient>>(
    server: ContextOsServer,
    client: C,
    lifecycle: ClientLifecycleMode,
) -> Result<RunningService<RoleClient, C>, BoxError> {
    let (server_transport, client_transport) = tokio::io::duplex(65536);
    tokio::spawn(async move {
        if let Ok(running) = server.serve(server_transport).await {
            let _ = running.waiting().await;
        }
    });
    Ok(client.serve_with_lifecycle(client_transport, lifecycle).await?)
}

async fn reported_revision<C: Service<RoleClient>>(client: &RunningService<RoleClient, C>) -> Result<Value, BoxError> {
    let result: CallToolResult = client.call_tool(CallToolRequestParams::new("vault_info")).await?;
    result
        .structured_content
        .as_ref()
        .and_then(|content| content.get("protocol_version"))
        .cloned()
        .ok_or_else(|| std::io::Error::other("vault_info omitted protocol_version").into())
}

#[tokio::test]
async fn a_2026_07_28_client_calls_tools_without_initialize_over_stdio() -> Result<(), BoxError> {
    let (server, _vault) = server()?;
    let client = stdio_client(server, (), modern()).await?;
    assert_eq!(reported_revision(&client).await?, json!("2026-07-28"));
    client.cancel().await?;
    Ok(())
}

#[tokio::test]
async fn a_2026_07_28_client_calls_tools_without_initialize_over_http() -> Result<(), BoxError> {
    let (server, _vault) = server()?;
    let (url, shutdown) = serve_http(server).await?;
    let client = ()
        .serve_with_lifecycle(
            StreamableHttpClientTransport::from_config(http_client_config(&url)),
            modern(),
        )
        .await?;
    assert_eq!(reported_revision(&client).await?, json!("2026-07-28"));
    client.cancel().await?;
    shutdown.cancel();
    Ok(())
}

#[tokio::test]
async fn a_legacy_client_keeps_the_known_revision_it_asks_initialize_for_over_stdio_and_http() -> Result<(), BoxError> {
    let (server, _vault) = server()?;
    let (url, shutdown) = serve_http(server.clone()).await?;
    for revision in [ProtocolVersion::V_2025_11_25, ProtocolVersion::V_2025_06_18] {
        let over_stdio = stdio_client(
            server.clone(),
            legacy(revision.clone()),
            ClientLifecycleMode::Initialize,
        )
        .await?;
        assert_eq!(reported_revision(&over_stdio).await?, json!(revision.as_str()));
        over_stdio.cancel().await?;

        let over_http = legacy(revision.clone())
            .serve(StreamableHttpClientTransport::from_config(http_client_config(&url)))
            .await?;
        assert_eq!(reported_revision(&over_http).await?, json!(revision.as_str()));
        over_http.cancel().await?;
    }
    shutdown.cancel();
    Ok(())
}

/// Revision 2026-07-28 has no `initialize` handshake, so a client asking
/// `initialize` for it is a legacy client and negotiates the newest revision
/// that has one.
#[tokio::test]
async fn initialize_asking_for_2026_07_28_negotiates_the_newest_handshake_revision_over_stdio_and_http()
-> Result<(), BoxError> {
    let (server, _vault) = server()?;
    let (url, shutdown) = serve_http(server.clone()).await?;
    let handshake = json!(ProtocolVersion::LATEST_WITH_INITIALIZE.as_str());

    let over_stdio = stdio_client(
        server,
        legacy(ProtocolVersion::V_2026_07_28),
        ClientLifecycleMode::Initialize,
    )
    .await?;
    assert_eq!(reported_revision(&over_stdio).await?, handshake);
    over_stdio.cancel().await?;

    let over_http = legacy(ProtocolVersion::V_2026_07_28)
        .serve(StreamableHttpClientTransport::from_config(http_client_config(&url)))
        .await?;
    assert_eq!(reported_revision(&over_http).await?, handshake);
    over_http.cancel().await?;
    shutdown.cancel();
    Ok(())
}

/// Sends one raw request to the router and returns its status and JSON-RPC
/// message, whichever encoding the server chose: a JSON body or one SSE
/// `data:` event.
async fn raw_request(headers: &[(&str, &str)], body: Value) -> Result<(StatusCode, Value), BoxError> {
    let (server, _vault) = server()?;
    let router = contextos_mcp::build_router(server, &http_config(""))?;
    let mut request = Request::builder()
        .method("POST")
        .uri(contextos_mcp::HTTP_MOUNT_PATH)
        .header(header::HOST, "127.0.0.1")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = router.oneshot(request.body(Body::from(body.to_string()))?).await?;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
    let text = std::str::from_utf8(&bytes)?;
    let payload = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .find(|data| !data.is_empty())
        .unwrap_or(text);
    Ok((status, serde_json::from_str(payload)?))
}

fn modern_meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {},
    })
}

#[tokio::test]
async fn server_discover_lists_2026_07_28_among_the_supported_revisions() -> Result<(), BoxError> {
    let (status, message) = raw_request(
        &[
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "server/discover"),
        ],
        json!({"jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": {"_meta": modern_meta()}}),
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    let supported = message["result"]["supportedVersions"]
        .as_array()
        .ok_or_else(|| std::io::Error::other(format!("discover listed no supported versions: {message}")))?;
    assert!(supported.contains(&json!("2026-07-28")), "{message}");
    Ok(())
}

/// A 2026-07-28 request over HTTP must mirror its method and tool name into
/// the `Mcp-Method` and `Mcp-Name` headers; one that omits them is refused
/// with the protocol's `HeaderMismatch` error.
#[tokio::test]
async fn a_2026_07_28_http_request_without_its_standard_headers_is_refused_with_header_mismatch() -> Result<(), BoxError>
{
    let (status, message) = raw_request(
        &[("MCP-Protocol-Version", "2026-07-28")],
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "vault_info", "arguments": {}, "_meta": modern_meta()},
        }),
    )
    .await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(message["error"]["code"], json!(-32020), "{message}");
    Ok(())
}

/// The MCP Streamable HTTP specification lets a server that accepts clients
/// from before the `MCP-Protocol-Version` header existed treat a request
/// without it as 2025-03-26, which rmcp does.
#[tokio::test]
async fn an_http_request_without_a_version_header_is_served_as_2025_03_26() -> Result<(), BoxError> {
    let (status, message) = raw_request(
        &[],
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "vault_info", "arguments": {}}}),
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        message["result"]["structuredContent"]["protocol_version"],
        json!(ProtocolVersion::V_2025_03_26.as_str())
    );
    Ok(())
}

/// A legacy `initialize` naming a revision this server does not know is
/// answered with the newest revision that still has a handshake.
#[tokio::test]
async fn initialize_naming_an_unknown_revision_is_offered_the_newest_handshake_revision() -> Result<(), BoxError> {
    let (status, message) = raw_request(
        &[],
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2099-01-01",
                "capabilities": {},
                "clientInfo": {"name": "protocol-revision-test", "version": "1.0"},
            },
        }),
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        message["result"]["protocolVersion"],
        json!(ProtocolVersion::LATEST_WITH_INITIALIZE.as_str())
    );
    Ok(())
}

/// A 2026-07-28 request whose `Mcp-Name` header names another tool than its
/// body is refused with `HeaderMismatch`, so an intermediary routing on the
/// header can never be steered to a different tool than the one that runs.
#[tokio::test]
async fn a_2026_07_28_http_request_whose_headers_disagree_with_its_body_is_refused_with_header_mismatch()
-> Result<(), BoxError> {
    let (status, message) = raw_request(
        &[
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "tools/call"),
            ("Mcp-Name", "fs_delete_file"),
        ],
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "vault_info", "arguments": {}, "_meta": modern_meta()},
        }),
    )
    .await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(message["error"]["code"], json!(-32020), "{message}");
    Ok(())
}

/// A request carrying, in its `_meta`, a revision newer than any this server
/// knows is refused with `UnsupportedProtocolVersion`, which names the
/// revisions the server supports so the client can retry with one of them.
#[tokio::test]
async fn a_request_naming_an_unsupported_revision_is_refused_with_the_supported_revisions() -> Result<(), BoxError> {
    let (server, _vault) = server()?;
    let (server_transport, client_transport) = tokio::io::duplex(65536);
    tokio::spawn(async move {
        if let Ok(running) = server.serve(server_transport).await {
            let _ = running.waiting().await;
        }
    });
    let (read, mut write) = tokio::io::split(client_transport);
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "server/discover",
        "params": {"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2099-01-01",
            "io.modelcontextprotocol/clientCapabilities": {},
        }},
    });
    tokio::io::AsyncWriteExt::write_all(&mut write, format!("{request}\n").as_bytes()).await?;
    let mut lines = tokio::io::AsyncBufReadExt::lines(tokio::io::BufReader::new(read));
    let line = tokio::time::timeout(std::time::Duration::from_secs(10), lines.next_line())
        .await??
        .ok_or("the server closed stdio without answering")?;
    let message: Value = serde_json::from_str(&line)?;
    assert_eq!(message["error"]["code"], json!(-32022), "{message}");
    let supported = message["error"]["data"]["supported"]
        .as_array()
        .ok_or_else(|| std::io::Error::other(format!("no supported revisions listed: {message}")))?;
    assert!(supported.contains(&json!("2026-07-28")), "{message}");
    Ok(())
}
