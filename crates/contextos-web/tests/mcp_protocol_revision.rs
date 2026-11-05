//! `contextos-web` speaks MCP protocol revision 2026-07-28 to every server
//! that supports it, over stdio and HTTP, and reaches a server that supports
//! only the earlier, handshake-based revisions through rmcp's own fall back to
//! `initialize`.

mod support;

use contextos_web::config::McpServerConfig;
use contextos_web::mcp_client::McpClient;
use rmcp::model::ProtocolVersion;
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

async fn reported_revision(client: &McpClient, tool: &str) -> Result<Value, BoxError> {
    let result = client.call_tool(tool.to_owned(), Map::new()).await?;
    result
        .structured_content
        .as_ref()
        .and_then(|content| content.get("protocol_version"))
        .cloned()
        .ok_or_else(|| format!("{tool} reported no protocol_version: {result:?}").into())
}

/// Serves `router` on an ephemeral loopback port until the returned token is
/// cancelled, returning its MCP endpoint.
async fn serve(router: axum::Router, mount: &str) -> Result<(String, CancellationToken), BoxError> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let shutdown = CancellationToken::new();
    let stop = shutdown.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move { stop.cancelled().await })
            .await;
    });
    Ok((format!("http://{addr}{mount}"), shutdown))
}

fn http_entry(name: &str, endpoint: String) -> McpServerConfig {
    McpServerConfig::Http {
        name: name.to_owned(),
        endpoint,
        token_env: None,
        auth: None,
        headers: std::collections::BTreeMap::new(),
    }
}

#[tokio::test]
async fn a_contextos_server_launched_over_stdio_is_spoken_to_in_2026_07_28() -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let vault = dir.path().join("vault");
    std::fs::create_dir_all(&vault)?;
    let config_path = support::write_vault_config(dir.path(), &vault)?;
    let client = McpClient::connect(&support::real_contextos_entry("contextos", &config_path)?).await?;

    assert_eq!(reported_revision(&client, "vault_info").await?, json!("2026-07-28"));
    Ok(())
}

#[tokio::test]
async fn a_contextos_server_reached_over_http_is_spoken_to_in_2026_07_28() -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let vault = dir.path().join("vault");
    std::fs::create_dir_all(&vault)?;
    let mut config = contextos_mcp::Config::try_from(vec![vault.clone()])?;
    for vault in &mut config.vaults {
        vault.state_directory = Some(std::path::PathBuf::from(".contextos"));
    }
    let server = contextos_mcp::ContextOsServer::try_from(config)?;
    let http = contextos_mcp::HttpConfig {
        bind: "127.0.0.1:0".to_owned(),
        token: String::new(),
        token_env: String::new(),
        max_body_kb: 2048,
    };
    let (endpoint, shutdown) = serve(
        contextos_mcp::build_router(server, &http)?,
        contextos_mcp::HTTP_MOUNT_PATH,
    )
    .await?;
    let client = McpClient::connect(&http_entry("contextos-http", endpoint)).await?;

    assert_eq!(reported_revision(&client, "vault_info").await?, json!("2026-07-28"));
    shutdown.cancel();
    Ok(())
}

/// How the legacy fixture rejects a method it does not know, such as
/// `server/discover`: the two common shapes among servers from before
/// revision 2026-07-28.
#[derive(Clone, Copy, Debug)]
enum UnknownMethodReply {
    /// A JSON-RPC "method not found" error answering that request.
    MethodNotFound,
    /// An HTTP 400 carrying an error tied to no request, as a session-based
    /// server sends for anything before `initialize`.
    UncorrelatedBadRequest,
    /// An HTTP 422 with a plain-text body and no JSON-RPC message at all, as
    /// rmcp 2.x's own session-based server sends for anything before
    /// `initialize`.
    PlainTextUnprocessable,
    /// An HTTP 500, as a server that fails on a request it cannot parse sends.
    ServerError,
    /// An empty HTTP 202, as a server that takes any message it does not know
    /// for a notification sends.
    AcceptedEmpty,
}

/// The handshake revisions the legacy fixture knows.
const LEGACY_REVISIONS: [&str; 4] = ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];

/// A minimal third-party server from before revision 2026-07-28: it speaks
/// only the handshake-based revisions and knows nothing of `server/discover`.
/// Its one tool reports the revision the session's requests name.
async fn legacy_endpoint(
    axum::extract::State(reply): axum::extract::State<UnknownMethodReply>,
    headers: axum::http::HeaderMap,
    axum::Json(message): axum::Json<Value>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    match message.get("method").and_then(Value::as_str).unwrap_or_default() {
        "initialize" => {
            let requested = message["params"]["protocolVersion"].as_str().unwrap_or_default();
            let revision = if LEGACY_REVISIONS.contains(&requested) {
                requested
            } else {
                "2025-11-25"
            };
            axum::Json(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": revision,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "legacy-fixture", "version": "1.0"},
                },
            }))
            .into_response()
        }
        method if method.starts_with("notifications/") => axum::http::StatusCode::ACCEPTED.into_response(),
        "tools/call" => {
            let revision = headers
                .get("MCP-Protocol-Version")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default();
            axum::Json(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {"content": [], "structuredContent": {"protocol_version": revision}, "isError": false},
            }))
            .into_response()
        }
        _ => match reply {
            UnknownMethodReply::MethodNotFound => axum::Json(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": "Method not found"},
            }))
            .into_response(),
            UnknownMethodReply::UncorrelatedBadRequest => (
                axum::http::StatusCode::BAD_REQUEST,
                axum::Json(json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": {"code": -32000, "message": "Bad Request: Server not initialized"},
                })),
            )
                .into_response(),
            UnknownMethodReply::PlainTextUnprocessable => (
                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                "Unexpected message, expect initialize request",
            )
                .into_response(),
            UnknownMethodReply::ServerError => {
                (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "unknown method").into_response()
            }
            UnknownMethodReply::AcceptedEmpty => axum::http::StatusCode::ACCEPTED.into_response(),
        },
    }
}

async fn connect_to_legacy_fixture(reply: UnknownMethodReply) -> Result<(McpClient, CancellationToken), BoxError> {
    let router = axum::Router::new()
        .route("/mcp", axum::routing::post(legacy_endpoint))
        .with_state(reply);
    let (endpoint, shutdown) = serve(router, "/mcp").await?;
    // Bounded well under rmcp's ten-second discover timeout, so the test proves the fall back
    // followed the server's refusal rather than the timeout.
    let client = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        McpClient::connect(&http_entry("legacy", endpoint)),
    )
    .await??;
    Ok((client, shutdown))
}

#[tokio::test]
async fn a_legacy_server_answering_discover_with_method_not_found_is_reached_through_initialize() -> Result<(), BoxError>
{
    let (client, shutdown) = connect_to_legacy_fixture(UnknownMethodReply::MethodNotFound).await?;
    assert_eq!(
        reported_revision(&client, "report_revision").await?,
        json!(ProtocolVersion::LATEST_WITH_INITIALIZE.as_str())
    );
    shutdown.cancel();
    Ok(())
}

#[tokio::test]
async fn a_legacy_server_refusing_discover_with_an_uncorrelated_bad_request_is_reached_through_initialize()
-> Result<(), BoxError> {
    let (client, shutdown) = connect_to_legacy_fixture(UnknownMethodReply::UncorrelatedBadRequest).await?;
    assert_eq!(
        reported_revision(&client, "report_revision").await?,
        json!(ProtocolVersion::LATEST_WITH_INITIALIZE.as_str())
    );
    shutdown.cancel();
    Ok(())
}

#[tokio::test]
async fn a_legacy_server_refusing_discover_with_a_plain_text_error_is_reached_through_initialize()
-> Result<(), BoxError> {
    let (client, shutdown) = connect_to_legacy_fixture(UnknownMethodReply::PlainTextUnprocessable).await?;
    assert_eq!(
        reported_revision(&client, "report_revision").await?,
        json!(ProtocolVersion::LATEST_WITH_INITIALIZE.as_str())
    );
    shutdown.cancel();
    Ok(())
}

/// Closing a 2026-07-28 HTTP connection, which has no session to delete, is
/// prompt: shutdown does not wait on a deletion that can never be made.
#[tokio::test]
async fn shutting_down_a_2026_07_28_http_connection_is_prompt() -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let vault = dir.path().join("vault");
    std::fs::create_dir_all(&vault)?;
    let mut config = contextos_mcp::Config::try_from(vec![vault.clone()])?;
    for vault in &mut config.vaults {
        vault.state_directory = Some(std::path::PathBuf::from(".contextos"));
    }
    let server = contextos_mcp::ContextOsServer::try_from(config)?;
    let http = contextos_mcp::HttpConfig {
        bind: "127.0.0.1:0".to_owned(),
        token: String::new(),
        token_env: String::new(),
        max_body_kb: 2048,
    };
    let (endpoint, stop) = serve(
        contextos_mcp::build_router(server, &http)?,
        contextos_mcp::HTTP_MOUNT_PATH,
    )
    .await?;
    let set = contextos_web::mcp_client::McpClientSet::connect(&[http_entry("contextos-http", endpoint)]).await?;
    let client = set.get("contextos-http").ok_or("the server did not connect")?;
    assert_eq!(reported_revision(&client, "vault_info").await?, json!("2026-07-28"));

    tokio::time::timeout(std::time::Duration::from_secs(2), set.shutdown()).await?;
    stop.cancel();
    Ok(())
}

/// A stdio server built on an SDK from before revision 2026-07-28 that, like
/// rmcp before 3.5, insists the first message is `initialize` and exits on
/// anything else, such as the `server/discover` probe. Each launch appends a
/// line to `$1`, so a test can count launches. With `$2` set to `always-exit`
/// it exits on every first message, `initialize` included; with `$2` set to
/// `uncorrelated` it answers a first message other than `initialize` with an
/// error tied to no request and keeps waiting for `initialize`, as some SDKs
/// do, instead of exiting.
#[cfg(unix)]
const EXITING_LEGACY_SERVER: &str = r#"
echo launch >> "$1"
IFS= read -r first || exit 1
case "$first" in
    *'"initialize"'*) ;;
    *)
        if [ "$2" != "uncorrelated" ]; then echo "expect initialize request" >&2; exit 1; fi
        printf '{"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"Received request before initialization was complete"}}\n'
        while IFS= read -r first; do
            case "$first" in *'"initialize"'*) break ;; esac
        done
        ;;
esac
if [ "$2" = "always-exit" ]; then exit 1; fi
id=$(printf '%s' "$first" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"legacy-stdio","version":"1"}}}\n' "$id"
while IFS= read -r request; do
    id=$(printf '%s' "$request" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
    case "$request" in
        *'"tools/call"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[],"structuredContent":{"protocol_version":"2025-11-25"},"isError":false}}\n' "$id" ;;
        *'"id":'[0-9]*) printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"Method not found"}}\n' "$id" ;;
    esac
done
"#;

#[cfg(unix)]
fn exiting_legacy_entry(launches: &std::path::Path, mode: &str) -> McpServerConfig {
    McpServerConfig::Stdio {
        name: "legacy-stdio".to_owned(),
        command: "sh".to_owned(),
        args: vec![
            "-c".to_owned(),
            EXITING_LEGACY_SERVER.to_owned(),
            "legacy-stdio".to_owned(),
            launches.to_string_lossy().into_owned(),
            mode.to_owned(),
        ],
    }
}

#[cfg(unix)]
fn launch_count(launches: &std::path::Path) -> Result<usize, BoxError> {
    Ok(std::fs::read_to_string(launches)?.lines().count())
}

/// A legacy stdio server that exits on the `server/discover` probe is
/// launched once more and reached through `initialize`.
#[cfg(unix)]
#[tokio::test]
async fn a_legacy_stdio_server_that_exits_on_discover_is_relaunched_and_reached_through_initialize()
-> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let launches = dir.path().join("launches");
    let client = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        McpClient::connect(&exiting_legacy_entry(&launches, "")),
    )
    .await??;

    assert_eq!(
        reported_revision(&client, "report_revision").await?,
        json!("2025-11-25")
    );
    assert_eq!(
        launch_count(&launches)?,
        2,
        "one probe launch, then one initialize launch"
    );
    Ok(())
}

/// The relaunch happens once: a server that also exits on `initialize` fails
/// to connect after its second launch rather than being launched again.
#[cfg(unix)]
#[tokio::test]
async fn a_stdio_server_that_exits_on_initialize_too_is_launched_only_twice() -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let launches = dir.path().join("launches");
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        McpClient::connect(&exiting_legacy_entry(&launches, "always-exit")),
    )
    .await?;

    assert!(
        result.is_err(),
        "a server that exits on every first message cannot connect"
    );
    assert_eq!(launch_count(&launches)?, 2);
    Ok(())
}

/// A legacy stdio server that answers the probe with an error tied to no
/// request, which rmcp cannot read as a legacy reply, is reached the same way.
#[cfg(unix)]
#[tokio::test]
async fn a_legacy_stdio_server_answering_discover_with_an_uncorrelated_error_is_reached_through_initialize()
-> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let launches = dir.path().join("launches");
    let client = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        McpClient::connect(&exiting_legacy_entry(&launches, "uncorrelated")),
    )
    .await??;

    assert_eq!(
        reported_revision(&client, "report_revision").await?,
        json!("2025-11-25")
    );
    assert!(launch_count(&launches)? <= 2);
    Ok(())
}

/// rmcp's own fall back follows only a refusal it can read as a legacy reply.
/// A legacy HTTP server that answers the probe with a server error, or with an
/// empty 202, is reached by connecting again through `initialize`.
#[tokio::test]
async fn a_legacy_http_server_answering_discover_with_a_server_error_or_an_empty_reply_is_reached_through_initialize()
-> Result<(), BoxError> {
    for reply in [UnknownMethodReply::ServerError, UnknownMethodReply::AcceptedEmpty] {
        let (client, shutdown) = connect_to_legacy_fixture(reply).await?;
        assert_eq!(
            reported_revision(&client, "report_revision").await?,
            json!(ProtocolVersion::LATEST_WITH_INITIALIZE.as_str()),
            "{reply:?}"
        );
        shutdown.cancel();
    }
    Ok(())
}
