//! The authenticated streamable HTTP transport and its parity with the
//! stdio transport.
//!
//! HTTP-client choice for the parity, soak, and shutdown-flush tests: these
//! drive the server through the official rmcp streamable-HTTP reqwest client
//! (`rmcp` features `client` + `transport-streamable-http-client-reqwest`,
//! dev-dependency only) rather than hand-rolled raw HTTP. The vendored rmcp
//! client already implements the full streamable-HTTP JSON-RPC protocol
//! (revision negotiation, SSE/JSON response handling, retries), so reusing it
//! is more robust than reimplementing that protocol by hand, and it exercises
//! the server the same way a real MCP host would. The exceptions build raw
//! `http` requests against the `axum::Router` directly (via
//! `tower::ServiceExt::oneshot`). The auth-rejection matrix and the body-cap
//! test do so because those cases are rejected by our own middleware before
//! any MCP semantics apply, and a socket-free `oneshot` call is both simpler
//! and faster than a real client round trip. The body-limit test does so to
//! send one request larger than rmcp's own default limit. Which protocol
//! revision a client speaks is covered by `protocol_revision_contract.rs`.
mod support;
use std::net::SocketAddr;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use contextos_mcp::{ContextOsServer, HttpConfig, HttpTransportError};
use rmcp::model::{CallToolRequestParams, CallToolResult, ProgressNotificationParam, ProtocolVersion, Tool};
use rmcp::service::{ClientLifecycleMode, ClientServiceExt, NotificationContext};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::{ClientHandler, RoleClient, ServiceExt};
use serde_json::{Map, Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A `server/discover` request as a 2026-07-28 client sends it, its revision in `_meta`.
const DISCOVER_BODY_2026_07_28: &str = r#"{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#;

fn http_config(bind: &str, token: &str) -> HttpConfig {
    HttpConfig {
        bind: bind.to_owned(),
        token: token.to_owned(),
        token_env: String::new(),
        max_body_kb: 2048,
    }
}

/// Binds an ephemeral loopback port, serves `router` on it, and returns the
/// base MCP URL plus a handle and cancellation token the caller uses to shut
/// the server down. Binding completes synchronously (the OS backlog is
/// established before `TcpListener::bind` returns), so no sleep is needed to
/// know the server is ready to accept connections.
async fn spawn_http_server(
    server: ContextOsServer,
    http: &HttpConfig,
) -> Result<(SocketAddr, String, JoinHandle<()>, CancellationToken), BoxError> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let router = contextos_mcp::build_router(server, http)?;
    let shutdown = CancellationToken::new();
    let shutdown_for_serve = shutdown.clone();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move { shutdown_for_serve.cancelled().await })
            .await;
    });
    let url = format!("http://{addr}{}", contextos_mcp::HTTP_MOUNT_PATH);
    Ok((addr, url, handle, shutdown))
}

async fn stop_http_server(handle: JoinHandle<()>, shutdown: CancellationToken) -> Result<(), BoxError> {
    shutdown.cancel();
    handle.await?;
    Ok(())
}

async fn http_client(url: &str, token: &str) -> Result<rmcp::service::RunningService<rmcp::RoleClient, ()>, BoxError> {
    let config = StreamableHttpClientTransportConfig::with_uri(url.to_owned()).auth_header(token.to_owned());
    let transport = StreamableHttpClientTransport::from_config(config);
    Ok(().serve(transport).await?)
}

/// Calls a tool over HTTP and returns whatever result it produced, a tool
/// error included: for parity checks of a refused request.
async fn call_http_tool_raw(
    url: &str,
    token: &str,
    name: &'static str,
    arguments: Map<String, Value>,
) -> Result<CallToolResult, BoxError> {
    let client = http_client(url, token).await?;
    let result = client
        .call_tool(CallToolRequestParams::new(name).with_arguments(arguments))
        .await;
    client.cancel().await?;
    Ok(result?)
}

async fn call_http_tool(
    url: &str,
    token: &str,
    name: &'static str,
    arguments: Map<String, Value>,
) -> Result<CallToolResult, BoxError> {
    let result = call_http_tool_raw(url, token, name, arguments).await?;
    if result.is_error == Some(true) {
        return Err(
            std::io::Error::other(format!("{name} returned a tool error: {:?}", result.structured_content)).into(),
        );
    }
    Ok(result)
}

async fn call_http_list_tools(url: &str, token: &str) -> Result<Vec<Tool>, BoxError> {
    let client = http_client(url, token).await?;
    let tools = client.list_all_tools().await;
    client.cancel().await?;
    Ok(tools?)
}

fn structured<'a>(result: &'a CallToolResult, context: &str) -> Result<&'a serde_json::Map<String, Value>, BoxError> {
    result
        .structured_content
        .as_ref()
        .and_then(Value::as_object)
        .ok_or_else(|| std::io::Error::other(format!("{context} omitted structured content")).into())
}

async fn assert_auth_rejected(router: Router, request: Request<Body>) -> Result<(), BoxError> {
    let response = router.oneshot(request).await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok()),
        Some("Bearer")
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
    let parsed: Value = serde_json::from_slice(&body)?;
    assert_eq!(parsed.get("code"), Some(&json!("auth/missing-token")));
    Ok(())
}

#[tokio::test]
async fn auth_rejection_matrix() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    let token = "matrix-token";
    let http = http_config("127.0.0.1:0", token);
    let router = contextos_mcp::build_router(server.clone(), &http)?;
    let mount = contextos_mcp::HTTP_MOUNT_PATH;

    // No Authorization header.
    assert_auth_rejected(
        router.clone(),
        Request::builder()
            .method("POST")
            .uri(mount)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .body(Body::empty())?,
    )
    .await?;

    // Wrong scheme.
    assert_auth_rejected(
        router.clone(),
        Request::builder()
            .method("POST")
            .uri(mount)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header(header::AUTHORIZATION, "Basic dXNlcjpwYXNz")
            .body(Body::empty())?,
    )
    .await?;

    // Wrong token.
    assert_auth_rejected(
        router.clone(),
        Request::builder()
            .method("POST")
            .uri(mount)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header(header::AUTHORIZATION, "Bearer wrong-token")
            .body(Body::empty())?,
    )
    .await?;

    // Token supplied only as a query parameter must not be accepted.
    assert_auth_rejected(
        router.clone(),
        Request::builder()
            .method("POST")
            .uri(format!("{mount}?token={token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .body(Body::empty())?,
    )
    .await?;

    // Correct bearer succeeds: a full 2026-07-28 discover round trip over `oneshot`.
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(mount)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json, text/event-stream")
                .header(header::HOST, "127.0.0.1")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header("MCP-Protocol-Version", "2026-07-28")
                .header("Mcp-Method", "server/discover")
                .body(Body::from(DISCOVER_BODY_2026_07_28))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let parsed = json_rpc_response(response).await?;
    assert_eq!(parsed.get("jsonrpc"), Some(&json!("2.0")));
    let result = parsed
        .get("result")
        .ok_or_else(|| std::io::Error::other("discover response omitted result"))?;
    assert!(
        result
            .get("supportedVersions")
            .and_then(Value::as_array)
            .is_some_and(|versions| versions.contains(&json!("2026-07-28"))),
        "server does not serve the required protocol revision: {result}"
    );

    // Plus at least one real-socket positive case, over an actual TCP
    // connection rather than an in-process `oneshot` call.
    let (_addr, url, handle, shutdown) = spawn_http_server(server, &http).await?;
    let tools = call_http_list_tools(&url, token).await?;
    assert!(!tools.is_empty(), "`tools` should not be empty");
    stop_http_server(handle, shutdown).await?;

    Ok(())
}

/// Configuration/build-level test: `validate_bind` is the "validate" half of
/// a deliberate validate-then-bind seam, so this never asks the operating
/// system to listen on `0.0.0.0`.
#[test]
fn non_loopback_bind_without_token_is_refused() -> Result<(), BoxError> {
    let refusal = contextos_mcp::validate_bind("0.0.0.0:3737", "");
    let Err(HttpTransportError::NonLoopbackBindWithoutToken { bind }) = refusal else {
        return Err(std::io::Error::other("expected a NonLoopbackBindWithoutToken refusal").into());
    };
    assert_eq!(bind, "0.0.0.0:3737");
    let message = HttpTransportError::NonLoopbackBindWithoutToken { bind }.to_string();
    assert!(message.contains("0.0.0.0:3737"), "message omitted the bind: {message}");
    assert!(
        message.to_lowercase().contains("token"),
        "message did not name a way to supply a token: {message}"
    );

    // The same bind with a token configured constructs without error.
    contextos_mcp::validate_bind("0.0.0.0:3737", "configured-token")?;

    Ok(())
}

#[tokio::test]
async fn body_cap_rejects_oversize_requests() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    let token = "cap-token";
    let http = HttpConfig {
        bind: "127.0.0.1:0".to_owned(),
        token: token.to_owned(),
        token_env: String::new(),
        max_body_kb: 1,
    };
    let router = contextos_mcp::build_router(server, &http)?;

    let oversized = "x".repeat(4096);
    let request = Request::builder()
        .method("POST")
        .uri(contextos_mcp::HTTP_MOUNT_PATH)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_LENGTH, oversized.len().to_string())
        .body(Body::from(oversized))?;

    let response = router.oneshot(request).await?;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    Ok(())
}

#[tokio::test]
async fn tool_catalogue_identical_across_transports() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    let token = "parity-token";
    let http = http_config("127.0.0.1:0", token);

    // This instance's actual dispatch router (varies with `[server] astro`,
    // unlike the always-complete `ContextOsServer::catalogue()`), read
    // before `server` moves into `spawn_http_server` below.
    let stdio_tools = server.effective_catalogue().list_all();
    let mut stdio_names: Vec<String> = stdio_tools.iter().map(|tool| tool.name.clone().into_owned()).collect();
    stdio_names.sort_unstable();

    let (_addr, url, handle, shutdown) = spawn_http_server(server, &http).await?;
    let http_tools = call_http_list_tools(&url, token).await?;
    let mut http_names: Vec<String> = http_tools.iter().map(|tool| tool.name.clone().into_owned()).collect();
    http_names.sort_unstable();

    assert_eq!(
        stdio_names, http_names,
        "tool catalogue names drifted between transports"
    );

    for tool in &stdio_tools {
        let counterpart = http_tools
            .iter()
            .find(|candidate| candidate.name == tool.name)
            .ok_or_else(|| std::io::Error::other(format!("HTTP catalogue omitted {}", tool.name)))?;
        assert_eq!(
            *counterpart.input_schema, *tool.input_schema,
            "{} input schema drifted between transports",
            tool.name
        );
        assert_eq!(
            counterpart.output_schema, tool.output_schema,
            "{} output schema drifted between transports",
            tool.name
        );
        assert_eq!(
            counterpart.description, tool.description,
            "{} description drifted between transports",
            tool.name
        );
    }

    stop_http_server(handle, shutdown).await?;
    Ok(())
}

#[tokio::test]
async fn mixed_readwrite_soak_across_ten_clients() -> Result<(), BoxError> {
    const WRITER_COUNT: usize = 10;

    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let vault_path = vault.path().to_path_buf();
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault_path.clone()])?)?;
    let token = "soak-token";
    let http = http_config("127.0.0.1:0", token);
    let (_addr, url, handle, shutdown) = spawn_http_server(server, &http).await?;

    let mut tasks = tokio::task::JoinSet::new();
    for writer in 0..WRITER_COUNT {
        let url = url.clone();
        let token = token.to_owned();
        tasks.spawn(async move {
            let path = format!("soak/writer-{writer}.md");
            let marker = format!("marker-soak-{writer}");
            let content = format!("# Writer {writer}\n\n{marker}\n");

            call_http_tool(
                &url,
                &token,
                "fs_write_file",
                serde_json::from_value(json!({"path": path, "content": content}))?,
            )
            .await?;

            let read = call_http_tool(
                &url,
                &token,
                "fs_read_text_file",
                serde_json::from_value(json!({"path": path}))?,
            )
            .await?;
            let read_content = structured(&read, "fs_read_text_file")?
                .get("content")
                .and_then(Value::as_str)
                .ok_or_else(|| std::io::Error::other("fs_read_text_file omitted content"))?;
            if read_content != content {
                return Err(
                    std::io::Error::other("fs_read_text_file returned content that did not match the write").into(),
                );
            }

            call_http_tool(
                &url,
                &token,
                "query_text",
                serde_json::from_value(json!({"query": marker}))?,
            )
            .await?;

            Ok::<(String, String), BoxError>((path, marker))
        });
    }

    let mut written = Vec::with_capacity(WRITER_COUNT);
    while let Some(joined) = tasks.join_next().await {
        written.push(joined??);
    }
    assert_eq!(written.len(), WRITER_COUNT);

    for (path, marker) in &written {
        let on_disk = std::fs::read_to_string(vault_path.join(path))?;
        if !on_disk.contains(marker.as_str()) {
            return Err(std::io::Error::other(format!("file on disk at {path} is missing its marker")).into());
        }
    }

    for (_, marker) in &written {
        let hits = call_http_tool(
            &url,
            token,
            "query_text",
            serde_json::from_value(json!({"query": marker}))?,
        )
        .await?;
        let hit_count = structured(&hits, "query_text")?
            .get("hits")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or_default();
        if hit_count == 0 {
            return Err(std::io::Error::other(format!("query_text found no hit for {marker}")).into());
        }
    }

    stop_http_server(handle, shutdown).await?;
    Ok(())
}

#[tokio::test]
async fn shutdown_flushes_pending_commits() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    // A long debounce means only the explicit graceful-shutdown flush below
    // produces a commit, matching how
    // `graceful_flush_commits_pending_paths_before_shutdown` isolates
    // the flush path from the automatic debounced commit.
    config.vaults[0].git.commit_debounce_s = 3600;
    let server = ContextOsServer::try_from(config)?;
    let token = "flush-token";
    let http = http_config("127.0.0.1:0", token);
    let (_addr, url, handle, shutdown) = spawn_http_server(server.clone(), &http).await?;

    call_http_tool(&url, token, "git_init", Map::new()).await?;
    call_http_tool(
        &url,
        token,
        "fs_write_file",
        serde_json::from_value(json!({"path": "shutdown-http.md", "content": "safe via http\n"}))?,
    )
    .await?;

    // The same graceful-shutdown flush path main.rs runs on SIGINT/SIGTERM,
    // triggered here programmatically rather than via a real OS signal.
    let commits = server.flush_git()?;

    assert_eq!(commits.len(), 1);
    assert!(commits[0].commit_id.is_some());
    assert!(
        commits[0]
            .committed_paths
            .iter()
            .any(|path| path == std::path::Path::new("shutdown-http.md"))
    );

    stop_http_server(handle, shutdown).await?;
    Ok(())
}

async fn call_stdio_tool(
    server: ContextOsServer,
    name: &'static str,
    arguments: Map<String, Value>,
) -> Result<CallToolResult, BoxError> {
    let (server_transport, client_transport) = tokio::io::duplex(65536);
    let server_handle = tokio::spawn(async move {
        server.serve(server_transport).await?.waiting().await?;
        Ok::<(), BoxError>(())
    });
    let mut client = ().serve(client_transport).await?;
    let result = client
        .call_tool(CallToolRequestParams::new(name).with_arguments(arguments))
        .await;
    client.close().await?;
    server_handle.await??;
    Ok(result?)
}

/// The fields of a search result that must agree across transports. Index
/// freshness is left out: it reports how much reindexing the first query
/// against a vault did, which legitimately differs between two queries.
fn comparable(result: &CallToolResult, context: &str) -> Result<Value, BoxError> {
    let mut fields = structured(result, context)?.clone();
    fields.remove("index_freshness");
    Ok(Value::Object(fields))
}

#[tokio::test]
async fn search_tools_return_identical_results_http_result_and_stdio() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    for (name, body) in [
        ("one", "widget widget widget"),
        ("two", "widget widget filler"),
        ("three", "widget filler filler"),
    ] {
        std::fs::write(
            vault.path().join(format!("{name}.md")),
            format!("---\ntitle: {name}\ntype: task\ndescription: About {name}\n---\n\n{body}\n"),
        )?;
    }
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].index_md.enabled = false;
    let server = ContextOsServer::try_from(config)?;
    let token = "search-parity-token";
    let (_addr, url, handle, shutdown) = spawn_http_server(server.clone(), &http_config("127.0.0.1:0", token)).await?;

    let requests: [(&'static str, Value); 3] = [
        (
            "query_text",
            json!({"query": "widget", "limit": 2, "offset": 1, "sort": "title", "note_type": "task"}),
        ),
        (
            "query_blended",
            json!({"query": "widget", "mode": "text", "limit": 10, "min_relevance": 0.4}),
        ),
        ("query_blended", json!({"query": "widget", "limit": 10})),
    ];
    for (name, request) in requests {
        let arguments: Map<String, Value> = serde_json::from_value(request)?;
        let stdio_result = call_stdio_tool(server.clone(), name, arguments.clone()).await?;
        let http_result = call_http_tool(&url, token, name, arguments).await?;
        assert_eq!(
            comparable(&stdio_result, name)?,
            comparable(&http_result, name)?,
            "{name} results differ between transports"
        );
    }

    stop_http_server(handle, shutdown).await?;
    Ok(())
}

#[tokio::test]
async fn link_integrity_reports_are_identical_over_http_and_stdio() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("wiki"))?;
    std::fs::write(vault.path().join("wiki/a.md"), "[[gone]] [x](gone.md)\n[[b]]\n")?;
    std::fs::write(vault.path().join("b.md"), "# B\n")?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].index_md.enabled = false;
    let server = ContextOsServer::try_from(config)?;
    let token = "link-integrity-parity-token";
    let (_addr, url, handle, shutdown) = spawn_http_server(server.clone(), &http_config("127.0.0.1:0", token)).await?;

    for request in [
        json!({"operation": "unresolved", "limit": 1}),
        json!({"operation": "unresolved", "path": "wiki"}),
        json!({"operation": "unlinked"}),
        json!({"operation": "unresolved", "limit": 5000}),
    ] {
        let arguments: Map<String, Value> = serde_json::from_value(request.clone())?;
        let stdio_result = call_stdio_tool(server.clone(), "query_graph", arguments.clone()).await?;
        let http_result = call_http_tool_raw(&url, token, "query_graph", arguments).await?;
        assert_eq!(stdio_result.is_error, http_result.is_error, "{request}");
        assert_eq!(
            structured(&stdio_result, "query_graph")?,
            structured(&http_result, "query_graph")?,
            "query_graph {request} differs between transports"
        );
    }

    stop_http_server(handle, shutdown).await?;
    Ok(())
}

/// Choosing an id series behaves identically over both transports: a refused
/// prefix returns the same structured error, and a chosen prefix mints from
/// its own counter whichever transport made the call.
#[tokio::test]
async fn id_series_selection_is_identical_over_http_and_stdio() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("tasks"))?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].index_md.enabled = false;
    let server = ContextOsServer::try_from(config)?;
    let token = "series-parity-token";
    let (_addr, url, handle, shutdown) = spawn_http_server(server.clone(), &http_config("127.0.0.1:0", token)).await?;

    let created = call_stdio_tool(
        server.clone(),
        "base_create",
        serde_json::from_value(json!({
            "path": "tasks.base",
            "definition": {
                "filters": "file.inFolder(\"tasks\") && file.basename != \"index\"",
                "views": [{"type": "table", "name": "All"}]
            },
            "key_prefix": "TASK",
            "key_prefixes": ["DEC"]
        }))?,
    )
    .await?;
    assert_eq!(created.is_error, Some(false), "{created:?}");

    let record = |prefix: Option<&str>| -> Result<Map<String, Value>, serde_json::Error> {
        let mut request = json!({"path": "tasks.base", "frontmatter": {}, "content": "Body.\n"});
        if let (Some(request), Some(prefix)) = (request.as_object_mut(), prefix) {
            request.insert("prefix".to_owned(), json!(prefix));
        }
        serde_json::from_value(request)
    };

    let refused_over_stdio = call_stdio_tool(server.clone(), "base_record_create", record(Some("NOPE"))?).await?;
    let refused_over_http = call_http_tool_raw(&url, token, "base_record_create", record(Some("NOPE"))?).await?;
    assert_eq!(refused_over_stdio.is_error, Some(true));
    assert_eq!(refused_over_http.is_error, Some(true));
    assert_eq!(
        structured(&refused_over_stdio, "stdio refusal")?,
        structured(&refused_over_http, "http refusal")?,
        "a refused prefix differs between transports"
    );
    assert_eq!(
        structured(&refused_over_http, "http refusal")?.get("code"),
        Some(&json!("io/invalid-key-prefix"))
    );

    let mut keys = Vec::new();
    let mut field_names = Vec::new();
    for (over_http, prefix) in [(true, Some("DEC")), (false, Some("DEC")), (false, None), (true, None)] {
        let result = if over_http {
            call_http_tool(&url, token, "base_record_create", record(prefix)?).await?
        } else {
            call_stdio_tool(server.clone(), "base_record_create", record(prefix)?).await?
        };
        let body = structured(&result, "base_record_create")?;
        keys.push(body.get("key").cloned().ok_or("no key")?);
        let mut names: Vec<&String> = body.keys().collect();
        names.sort();
        field_names.push(format!("{names:?}"));
    }
    assert_eq!(
        keys,
        vec![json!("DEC-001"), json!("DEC-002"), json!("TASK-001"), json!("TASK-002")]
    );
    assert!(
        field_names.windows(2).all(|pair| pair[0] == pair[1]),
        "result shapes differ between transports: {field_names:?}"
    );

    stop_http_server(handle, shutdown).await?;
    Ok(())
}

/// `frontmatter_link` behaves identically over both transports: a refused
/// call returns the same structured error, and an add followed by the same
/// add over the other transport reports the link as already present.
#[tokio::test]
async fn frontmatter_link_is_identical_over_http_and_stdio() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("tasks"))?;
    std::fs::write(
        vault.path().join("tasks.base"),
        "filters: file.inFolder(\"tasks\")\nviews:\n- type: table\n  name: All\n",
    )?;
    std::fs::write(
        vault.path().join("tasks.md"),
        "---\nkey_format: FILEPATH\nlast_id: 0\nlast_create: t0\nlast_update: t0\nlink_fields:\n  depends_on:\n    inverse: blocks\n  blocks:\n    inverse: depends_on\n---\n",
    )?;
    for id in ["Alpha", "Beta"] {
        std::fs::write(
            vault.path().join(format!("tasks/{}.md", id.to_lowercase())),
            format!("---\nid: {id}\n---\nBody.\n"),
        )?;
    }
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].index_md.enabled = false;
    let server = ContextOsServer::try_from(config)?;
    let token = "link-parity-token";
    let (_addr, url, handle, shutdown) = spawn_http_server(server.clone(), &http_config("127.0.0.1:0", token)).await?;

    let request = |field: &str, target: &str| -> Result<Map<String, Value>, serde_json::Error> {
        serde_json::from_value(json!({
            "path": "tasks/alpha.md",
            "operations": [{"op": "add", "field": field, "target": target}]
        }))
    };

    let refused_over_stdio = call_stdio_tool(server.clone(), "frontmatter_link", request("nonsense", "Beta")?).await?;
    let refused_over_http = call_http_tool_raw(&url, token, "frontmatter_link", request("nonsense", "Beta")?).await?;
    assert_eq!(refused_over_stdio.is_error, Some(true));
    assert_eq!(refused_over_http.is_error, Some(true));
    assert_eq!(
        structured(&refused_over_stdio, "stdio refusal")?,
        structured(&refused_over_http, "http refusal")?,
        "a refused link differs between transports"
    );
    assert_eq!(
        structured(&refused_over_http, "http refusal")?.get("code"),
        Some(&json!("link/undeclared-field"))
    );

    let added = call_http_tool(&url, token, "frontmatter_link", request("depends_on", "Beta")?).await?;
    let repeated = call_stdio_tool(server.clone(), "frontmatter_link", request("depends_on", "Beta")?).await?;
    let added_body = structured(&added, "http add")?;
    let repeated_body = structured(&repeated, "stdio repeat")?;
    assert_eq!(added_body["changes"][0]["outcome"], json!("added"));
    assert_eq!(repeated_body["changes"][0]["outcome"], json!("already-present"));
    assert_eq!(added_body["changes"][0]["link"], repeated_body["changes"][0]["link"]);
    assert_eq!(added_body["path"], repeated_body["path"]);
    assert_eq!(
        added_body["reciprocal"][0]["path"],
        repeated_body["reciprocal"][0]["path"]
    );
    assert_eq!(
        added_body["reciprocal"][0]["field"],
        repeated_body["reciprocal"][0]["field"]
    );
    assert_eq!(added_body["reciprocal"][0]["outcome"], json!("added"));
    assert_eq!(repeated_body["reciprocal"][0]["outcome"], json!("already-present"));
    let names = |body: &Map<String, Value>| {
        let mut names: Vec<String> = body.keys().cloned().collect();
        names.sort();
        names
    };
    assert_eq!(
        names(added_body),
        names(repeated_body),
        "result shapes differ between transports"
    );

    stop_http_server(handle, shutdown).await?;
    Ok(())
}

/// Records every progress notification the server sends, so a test can tell
/// a progress-tracked call apart from one whose progress went missing.
#[derive(Clone, Default)]
struct ProgressCapturingClient {
    updates: std::sync::Arc<tokio::sync::Mutex<Vec<ProgressNotificationParam>>>,
    /// Signalled once the last phase of an `index: "all"` rebuild is
    /// recorded. rmcp runs each notification handler as its own task and a
    /// call returns when its result arrives, so a test waits on this rather
    /// than assuming the handlers ran first.
    final_phase: std::sync::Arc<tokio::sync::Notify>,
}

impl ClientHandler for ProgressCapturingClient {
    fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) -> impl std::future::Future<Output = ()> + Send + '_ {
        let updates = self.updates.clone();
        let final_phase = self.final_phase.clone();
        async move {
            let is_final = params.message.as_deref() == Some("link graph rebuilt");
            updates.lock().await.push(params);
            if is_final {
                final_phase.notify_one();
            }
        }
    }
}

/// A tool that streams progress before its result must still deliver that
/// result over HTTP. The rmcp client attaches a progress token to every
/// request, so a response that carried only the first progress notification
/// would lose the result for every real host, not just ones that ask for
/// progress deliberately.
#[tokio::test]
async fn query_index_rebuild_returns_its_result_and_streams_progress_over_http() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("a.md"), "# A\n\nFirst note.\n")?;
    std::fs::write(vault.path().join("b.md"), "# B\n\n[[a]]\n")?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].index_md.enabled = false;
    let server = ContextOsServer::try_from(config)?;
    let token = "progress-token";
    let (_addr, url, handle, shutdown) = spawn_http_server(server, &http_config("127.0.0.1:0", token)).await?;

    // A legacy client negotiating through `initialize`, and a 2026-07-28
    // client carrying its revision and progress token in each request.
    let lifecycles = [
        ClientLifecycleMode::Initialize,
        ClientLifecycleMode::Discover {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
        },
    ];
    for lifecycle in lifecycles {
        let handler = ProgressCapturingClient::default();
        let updates = handler.updates.clone();
        let final_phase = handler.final_phase.clone();
        let transport = StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(url.clone()).auth_header(token.to_owned()),
        );
        let client = handler.serve_with_lifecycle(transport, lifecycle.clone()).await?;
        let params = CallToolRequestParams::new("query_index_rebuild")
            .with_arguments(serde_json::from_value(json!({"index": "all"}))?);
        // Bounded so a response that never arrives fails the test instead of
        // hanging it.
        let result = tokio::time::timeout(std::time::Duration::from_secs(30), client.call_tool(params)).await??;
        tokio::time::timeout(std::time::Duration::from_secs(30), final_phase.notified()).await?;
        client.cancel().await?;

        assert_eq!(result.is_error, Some(false), "{lifecycle:?}");
        let body = structured(&result, "query_index_rebuild over http")?;
        assert_eq!(body["text"]["scanned"], json!(2), "{lifecycle:?}");
        assert_eq!(body["graph"]["notes_scanned"], json!(2), "{lifecycle:?}");
        let updates = updates.lock().await;
        assert!(
            updates
                .iter()
                .any(|update| update.message.as_deref() == Some("rebuilding text index")),
            "expected the rebuild's progress notifications over HTTP for {lifecycle:?}, got {updates:?}"
        );
        assert!(
            updates
                .iter()
                .any(|update| update.message.as_deref() == Some("link graph rebuilt")),
            "{lifecycle:?}"
        );
    }

    stop_http_server(handle, shutdown).await?;
    Ok(())
}

/// Reads the JSON-RPC message from a single-message response, whichever
/// encoding the server chose: a plain JSON body or one SSE `data:` event.
async fn json_rpc_response(response: axum::response::Response) -> Result<Value, BoxError> {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
    let text = std::str::from_utf8(&bytes)?;
    let payload = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .find(|data| !data.is_empty())
        .unwrap_or(text);
    Ok(serde_json::from_str(payload)?)
}

/// The operator's `max_body_kb` is the one request body limit: a request
/// within it reaches the tool even when it is larger than rmcp's own default
/// limit (4 MiB).
#[tokio::test]
async fn a_request_within_a_configured_body_cap_above_rmcp_default_reaches_the_tool() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    let http = HttpConfig {
        bind: "127.0.0.1:0".to_owned(),
        token: String::new(),
        token_env: String::new(),
        max_body_kb: 8192,
    };
    let router = contextos_mcp::build_router(server, &http)?;
    let content = "x".repeat(5 * 1024 * 1024);
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "fs_write_file", "arguments": {"path": "large.md", "content": content}},
    })
    .to_string();
    let request = Request::builder()
        .method("POST")
        .uri(contextos_mcp::HTTP_MOUNT_PATH)
        .header(header::HOST, "127.0.0.1")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .body(Body::from(body))?;

    let response = router.oneshot(request).await?;
    assert_eq!(response.status(), StatusCode::OK);
    let message = json_rpc_response(response).await?;
    assert_eq!(message["result"]["isError"], json!(false), "{}", message["error"]);
    assert_eq!(std::fs::metadata(vault.path().join("large.md"))?.len(), 5 * 1024 * 1024);
    Ok(())
}

/// Sends a legacy `vault_info` call carrying `host` as its Host header to a
/// router built for `http`, returning the response status.
async fn status_for_host(http: &HttpConfig, host: &str, token: &str) -> Result<StatusCode, BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    let router = contextos_mcp::build_router(server, http)?;
    let mut request = Request::builder()
        .method("POST")
        .uri(contextos_mcp::HTTP_MOUNT_PATH)
        .header(header::HOST, host)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream");
    if !token.is_empty() {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"vault_info","arguments":{}}}"#;
    Ok(router.oneshot(request.body(Body::from(body))?).await?.status())
}

/// A non-loopback bind requires a bearer token, which a DNS-rebinding page
/// cannot know, so a client reaching the server by its network address is
/// served rather than refused for its Host header.
#[tokio::test]
async fn a_non_loopback_bind_serves_an_authenticated_client_using_the_network_address() -> Result<(), BoxError> {
    let http = http_config("0.0.0.0:3737", "lan-token");
    assert_eq!(
        status_for_host(&http, "192.168.1.20:3737", "lan-token").await?,
        StatusCode::OK
    );
    assert_eq!(
        status_for_host(&http, "192.168.1.20:3737", "").await?,
        StatusCode::UNAUTHORIZED
    );
    Ok(())
}

/// A loopback bind needs no token, so its Host check is what refuses a
/// DNS-rebinding page whose name resolves to the loopback address.
#[tokio::test]
async fn a_loopback_bind_refuses_a_request_naming_another_host() -> Result<(), BoxError> {
    let http = http_config("127.0.0.1:3737", "");
    assert_eq!(status_for_host(&http, "127.0.0.1:3737", "").await?, StatusCode::OK);
    assert_eq!(
        status_for_host(&http, "rebinding.example:3737", "").await?,
        StatusCode::FORBIDDEN
    );
    Ok(())
}

/// With the default body cap, the largest write `fs_write_file` permits (10
/// MiB of binary content, which arrives base64-encoded) fits in one HTTP
/// request, as it does over stdio.
#[tokio::test]
async fn the_default_body_cap_admits_the_largest_permitted_binary_write() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    let router = contextos_mcp::build_router(server, &HttpConfig::default())?;
    let largest = usize::try_from(contextos_fs::MAX_ATTACHMENT_BYTES)?;
    let content = vec![0xA5_u8; largest];
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &content);
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "fs_write_file", "arguments": {"path": "largest.bin", "content_base64": encoded}},
    })
    .to_string();
    let request = Request::builder()
        .method("POST")
        .uri(contextos_mcp::HTTP_MOUNT_PATH)
        .header(header::HOST, "127.0.0.1")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, text/event-stream")
        .body(Body::from(body))?;

    let response = router.oneshot(request).await?;
    assert_eq!(response.status(), StatusCode::OK);
    let message = json_rpc_response(response).await?;
    assert_eq!(
        message["result"]["isError"],
        json!(false),
        "{}",
        message["result"]["structuredContent"]
    );
    assert_eq!(std::fs::read(vault.path().join("largest.bin"))?, content);
    Ok(())
}
