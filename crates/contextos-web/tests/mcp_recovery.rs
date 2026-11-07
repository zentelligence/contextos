//! `contextos-web` starts and serves setup with no vault and no working MCP server, shows each configured server's
//! readiness with its cause, and recovers a server, or picks up a vault added or removed, without restarting: against
//! a real `contextos` child process.

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use axum::response::IntoResponse as _;
use contextos_web::McpClientSet;
use contextos_web::mcp_readiness::Readiness;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT: &str = "contract-fixture";
const MISSING_COMMAND: &str = "contextos-web-test-missing-command-9f3c";

/// A loopback URL nothing listens on: its port was free a moment ago, so a connection to it is refused at once.
fn unreachable_loopback_url() -> std::io::Result<String> {
    let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    Ok(format!("http://127.0.0.1:{port}/mcp"))
}

/// A `web.toml` with `entries` as its `[[mcp_server]]` tables.
fn write_web_toml(dir: &Path, entries: &str) -> std::io::Result<PathBuf> {
    let path = dir.join("web.toml");
    std::fs::write(&path, format!("[server]\nbind = \"127.0.0.1:7373\"\n{entries}"))?;
    Ok(path)
}

/// A stdio `[[mcp_server]]` table running `command` with `args`.
fn stdio_table(name: &str, command: &str, args: &[String]) -> String {
    // `Debug` quotes and escapes each string the way a TOML basic string requires, including a Windows path.
    let args = args.iter().map(|arg| format!("{arg:?}")).collect::<Vec<_>>().join(", ");
    format!("\n[[mcp_server]]\nname = {name:?}\ntransport = \"stdio\"\ncommand = {command:?}\nargs = [{args}]\n")
}

/// The stdio table for a real `contextos` server reading `config_path`.
fn contextos_table(config_path: &Path) -> Result<String, BoxError> {
    let binary = support::contextos_mcp_binary()?;
    Ok(stdio_table(
        "contextos",
        &binary.to_string_lossy(),
        &["--config".to_owned(), config_path.to_string_lossy().into_owned()],
    ))
}

/// Starts the sessions `web.toml` names as the long-running server does, and a router over them.
async fn serve(web_toml: &Path) -> Result<(Arc<McpClientSet>, Router), BoxError> {
    let config = contextos_web::config::load_web_config(web_toml)?;
    let clients = contextos_web::app::start_until_shutdown(&config, web_toml, &CancellationToken::new()).await?;
    let config_dir = web_toml.parent().ok_or("web.toml has a directory")?;
    let router = contextos_web::build_router(Arc::clone(&clients), Some(config_dir), web_toml);
    Ok((clients, router))
}

async fn send(router: &Router, method: Method, path: &str, body: &str) -> Result<(StatusCode, String), BoxError> {
    let mut request = Request::builder()
        .method(method.clone())
        .uri(path)
        .header("content-type", "application/json");
    // Every change a settings page makes is an HTMX request; a page load is not.
    if method != Method::GET {
        request = request.header("hx-request", "true");
    }
    let response = router
        .clone()
        .oneshot(request.body(Body::from(body.to_owned()))?)
        .await?;
    let status = response.status();
    let location = response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((
        status,
        location.unwrap_or_else(|| String::from_utf8_lossy(&bytes).into_owned()),
    ))
}

async fn get(router: &Router, path: &str) -> Result<(StatusCode, String), BoxError> {
    send(router, Method::GET, path, "").await
}

/// Waits for `server`'s connection attempt to end, bounded so a regression fails rather than hangs.
async fn settled(clients: &McpClientSet, server: &str) -> Result<Readiness, BoxError> {
    let readiness = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            match clients.readiness_of(server).await.map(|server| server.readiness) {
                Some(Readiness::Connecting) => tokio::task::yield_now().await,
                other => return other,
            }
        }
    })
    .await?;
    readiness.ok_or_else(|| format!("{server} is not configured").into())
}

#[tokio::test]
async fn a_first_launch_with_no_server_and_no_vault_serves_setup_diagnostics_and_packages() -> Result<(), BoxError> {
    let config_dir = tempfile::tempdir()?;
    let app_data = tempfile::tempdir()?;
    let web_toml = write_web_toml(config_dir.path(), "")?;
    let config = contextos_web::config::load_web_config(&web_toml)?;
    let clients = contextos_web::app::start_until_shutdown(&config, &web_toml, &CancellationToken::new()).await?;
    let roots = contextos_web::package_store::AppDataRoots::from(app_data.path());
    let router = contextos_web::build_router_in(
        clients,
        Some(config_dir.path()),
        &web_toml,
        &config.workflows,
        Some(&roots),
    );

    let (settings, page) = get(&router, "/server/settings/").await?;
    let (root, location) = get(&router, "/").await?;
    let (status, _) = get(&router, "/shell/status").await?;
    let (packages, _) = get(&router, "/api/v1/server/packages").await?;

    assert_eq!(settings, StatusCode::OK);
    assert!(
        page.contains("No <code>[[mcp_server]]</code> entries are configured."),
        "{page}"
    );
    assert_eq!((root, location.as_str()), (StatusCode::SEE_OTHER, "/server/settings/"));
    assert_eq!(status, StatusCode::OK);
    assert_eq!(packages, StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn a_stdio_server_that_cannot_start_leaves_setup_reachable_with_its_cause() -> Result<(), BoxError> {
    let config_dir = tempfile::tempdir()?;
    let web_toml = write_web_toml(config_dir.path(), &stdio_table("contextos", MISSING_COMMAND, &[]))?;

    let (_clients, router) = serve(&web_toml).await?;

    let (status, page) = get(&router, "/server/settings/").await?;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("Not connected"), "{page}");
    assert!(page.contains("Check the command and its arguments"), "{page}");
    assert!(page.contains(MISSING_COMMAND), "the cause names the command: {page}");
    assert!(
        page.contains("0 of 1 connected"),
        "the footer counts the configured server: {page}"
    );
    let (root, location) = get(&router, "/").await?;
    assert_eq!((root, location.as_str()), (StatusCode::SEE_OTHER, "/server/settings/"));
    let (proxied, body) = send(&router, Method::POST, "/mcp/contextos/vault_info", "{}").await?;
    assert_eq!(proxied, StatusCode::BAD_GATEWAY);
    assert!(body.contains("mcp/unreachable"), "{body}");
    Ok(())
}

#[tokio::test]
async fn an_unreachable_http_server_is_listed_with_its_cause_beside_a_working_one() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    let config_path = support::write_vault_config(config_dir.path(), vault_dir.path())?;
    let endpoint = unreachable_loopback_url()?;
    let entries = format!(
        "{}\n[[mcp_server]]\nname = \"remote\"\ntransport = \"http\"\nendpoint = {endpoint:?}\n",
        contextos_table(&config_path)?
    );
    let web_toml = write_web_toml(config_dir.path(), &entries)?;

    let (clients, router) = serve(&web_toml).await?;

    assert!(matches!(
        settled(&clients, "contextos").await?,
        Readiness::Connected { .. }
    ));
    assert!(matches!(settled(&clients, "remote").await?, Readiness::Failed(_)));
    let (_, page) = get(&router, "/server/settings/").await?;
    assert!(page.contains("1 of 2 connected"), "{page}");
    let (root, location) = get(&router, "/").await?;
    assert_eq!((root, location.as_str()), (StatusCode::SEE_OTHER, "/contract-fixture/"));
    Ok(())
}

#[tokio::test]
async fn saving_a_corrected_entry_connects_it_without_a_restart() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    let config_path = support::write_vault_config(config_dir.path(), vault_dir.path())?;
    let web_toml = write_web_toml(config_dir.path(), &stdio_table("contextos", MISSING_COMMAND, &[]))?;
    let (clients, router) = serve(&web_toml).await?;
    assert!(clients.primary().is_none());

    let binary = support::contextos_mcp_binary()?;
    let patch = serde_json::json!({
        "target": "mcp_server",
        "name": "contextos",
        "patch": {
            "command": binary.to_string_lossy(),
            "args": ["--config", config_path.to_string_lossy()],
        },
    });
    let (status, _) = send(&router, Method::PATCH, "/server/settings/", &patch.to_string()).await?;

    assert_eq!(status, StatusCode::OK);
    assert!(matches!(
        settled(&clients, "contextos").await?,
        Readiness::Connected { .. }
    ));
    let (root, location) = get(&router, "/").await?;
    assert_eq!((root, location.as_str()), (StatusCode::SEE_OTHER, "/contract-fixture/"));
    Ok(())
}

#[tokio::test]
async fn reconnect_connects_an_entry_corrected_by_hand_and_keeps_the_other_servers_diagnostics() -> Result<(), BoxError>
{
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    let config_path = support::write_vault_config(config_dir.path(), vault_dir.path())?;
    let broken_other = stdio_table("other", MISSING_COMMAND, &[]);
    let web_toml = write_web_toml(
        config_dir.path(),
        &(stdio_table("contextos", MISSING_COMMAND, &[]) + broken_other.as_str()),
    )?;
    let (clients, router) = serve(&web_toml).await?;
    write_web_toml(
        config_dir.path(),
        &(contextos_table(&config_path)? + broken_other.as_str()),
    )?;

    let (status, panel) = send(
        &router,
        Method::POST,
        "/server/settings/mcp-servers/contextos/reconnect",
        "",
    )
    .await?;

    assert_eq!(status, StatusCode::OK);
    assert!(panel.contains("conn-pending"), "the panel reads as connecting: {panel}");
    assert!(matches!(
        settled(&clients, "contextos").await?,
        Readiness::Connected { .. }
    ));
    let (_, panel) = get(&router, "/server/settings/mcp-servers/contextos/connection").await?;
    assert!(panel.contains("Connected"), "{panel}");
    let (_, other) = get(&router, "/server/settings/mcp-servers/other/connection").await?;
    assert!(
        other.contains(MISSING_COMMAND),
        "another server's cause survives the recovery: {other}"
    );
    Ok(())
}

#[tokio::test]
async fn reconnecting_an_unknown_server_is_refused() -> Result<(), BoxError> {
    let config_dir = tempfile::tempdir()?;
    let web_toml = write_web_toml(config_dir.path(), "")?;
    let (_clients, router) = serve(&web_toml).await?;

    let (status, body) = send(
        &router,
        Method::POST,
        "/server/settings/mcp-servers/nowhere/reconnect",
        "",
    )
    .await?;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("settings/unknown-mcp-server"), "{body}");
    Ok(())
}

#[tokio::test]
async fn the_first_vault_added_is_picked_up_by_reconnecting_the_contextos_server() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    let config_path = support::write_zero_vault_config(config_dir.path())?;
    let web_toml = write_web_toml(config_dir.path(), &contextos_table(&config_path)?)?;
    let (clients, router) = serve(&web_toml).await?;
    let (_, before) = get(&router, "/").await?;
    assert_eq!(before, "/server/settings/");

    support::write_vault_config(config_dir.path(), vault_dir.path())?;
    send(
        &router,
        Method::POST,
        "/server/settings/mcp-servers/contextos/reconnect",
        "",
    )
    .await?;

    assert!(matches!(
        settled(&clients, "contextos").await?,
        Readiness::Connected { .. }
    ));
    let (_, after) = get(&router, "/").await?;
    assert_eq!(after, format!("/{VAULT}/"));
    Ok(())
}

#[tokio::test]
async fn the_last_vault_removed_is_picked_up_by_reconnecting_the_contextos_server() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    let config_path = support::write_vault_config(config_dir.path(), vault_dir.path())?;
    let web_toml = write_web_toml(config_dir.path(), &contextos_table(&config_path)?)?;
    let (clients, router) = serve(&web_toml).await?;
    let (_, before) = get(&router, "/").await?;
    assert_eq!(before, format!("/{VAULT}/"));

    support::write_zero_vault_config(config_dir.path())?;
    send(
        &router,
        Method::POST,
        "/server/settings/mcp-servers/contextos/reconnect",
        "",
    )
    .await?;

    assert!(matches!(
        settled(&clients, "contextos").await?,
        Readiness::Connected { .. }
    ));
    let (_, after) = get(&router, "/").await?;
    assert_eq!(after, "/server/settings/");
    let (settings, _) = get(&router, "/server/settings/").await?;
    assert_eq!(settings, StatusCode::OK);
    Ok(())
}

/// A legacy, session-based MCP endpoint that refuses with `401` any request not carrying `Bearer {token}`.
async fn endpoint_requiring(token: &'static str) -> std::io::Result<String> {
    let router = Router::new().route(
        "/mcp",
        axum::routing::post(
            move |headers: axum::http::HeaderMap, axum::Json(request): axum::Json<serde_json::Value>| async move {
                let expected = format!("Bearer {token}");
                if headers.get("authorization").and_then(|value| value.to_str().ok()) != Some(expected.as_str()) {
                    return (StatusCode::UNAUTHORIZED, [("www-authenticate", "Bearer")]).into_response();
                }
                match request["method"].as_str() {
                    Some("initialize") => (
                        [("mcp-session-id", "fixture")],
                        axum::Json(serde_json::json!({
                            "jsonrpc": "2.0", "id": request["id"], "result": {
                                "protocolVersion": "2025-11-25", "capabilities": {"tools": {}},
                                "serverInfo": {"name": "fixture", "version": "1"}
                            }
                        })),
                    )
                        .into_response(),
                    Some("tools/list") => {
                        axum::Json(serde_json::json!({"jsonrpc": "2.0", "id": request["id"], "result": {"tools": []}}))
                            .into_response()
                    }
                    _ if request.get("id").is_some() => axum::Json(serde_json::json!({
                        "jsonrpc": "2.0", "id": request["id"],
                        "error": {"code": -32601, "message": "Method not found"}
                    }))
                    .into_response(),
                    _ => StatusCode::ACCEPTED.into_response(),
                }
            },
        )
        .delete(|| async { StatusCode::OK }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/mcp", listener.local_addr()?);
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok(endpoint)
}

#[tokio::test]
async fn pasting_the_missing_bearer_token_connects_the_server() -> Result<(), BoxError> {
    let config_dir = tempfile::tempdir()?;
    let endpoint = endpoint_requiring("good").await?;
    let entry = format!(
        "\n[[mcp_server]]\nname = \"remote\"\ntransport = \"http\"\nendpoint = {endpoint:?}\n\n\
         [mcp_server.auth]\nmode = \"bearer\"\ncredential_ref = \"remote-token\"\n"
    );
    let web_toml = write_web_toml(config_dir.path(), &entry)?;
    let (clients, router) = serve(&web_toml).await?;
    assert!(matches!(
        settled(&clients, "remote").await?,
        Readiness::LoginRequired(_)
    ));

    let patch = serde_json::json!({
        "target": "mcp_server",
        "name": "remote",
        "patch": {
            "endpoint": endpoint,
            "auth": {"mode": "bearer", "credential_ref": "remote-token", "credential_value": "good"},
        },
    });
    let (status, page) = send(&router, Method::PATCH, "/server/settings/", &patch.to_string()).await?;

    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(
        matches!(settled(&clients, "remote").await?, Readiness::Connected { .. }),
        "the pasted token is used without any other change to the entry"
    );
    Ok(())
}

#[tokio::test]
async fn removing_a_server_while_the_primary_is_not_connected_is_refused_as_unreachable() -> Result<(), BoxError> {
    let config_dir = tempfile::tempdir()?;
    let web_toml = write_web_toml(
        config_dir.path(),
        &(stdio_table("contextos", MISSING_COMMAND, &[]) + stdio_table("other", MISSING_COMMAND, &[]).as_str()),
    )?;
    let (_clients, router) = serve(&web_toml).await?;
    let before = std::fs::read_to_string(&web_toml)?;

    let (status, body) = send(&router, Method::DELETE, "/server/settings/mcp-servers/other", "").await?;

    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert!(body.contains("mcp/unreachable"), "{body}");
    assert_eq!(
        std::fs::read_to_string(&web_toml)?,
        before,
        "web.toml is left as it was"
    );
    Ok(())
}

#[tokio::test]
async fn a_reconnect_not_sent_by_the_settings_page_is_refused() -> Result<(), BoxError> {
    let config_dir = tempfile::tempdir()?;
    let web_toml = write_web_toml(config_dir.path(), &stdio_table("contextos", MISSING_COMMAND, &[]))?;
    let (clients, router) = serve(&web_toml).await?;

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/server/settings/mcp-servers/contextos/reconnect")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(
        matches!(settled(&clients, "contextos").await?, Readiness::Failed(_)),
        "no attempt was started"
    );
    Ok(())
}
