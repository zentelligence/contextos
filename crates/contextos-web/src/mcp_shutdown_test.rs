use super::*;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::http::StatusCode;
use axum::response::IntoResponse;

async fn http_fixture(
    hang_on_delete: bool,
) -> Result<(McpClientSet, Arc<AtomicBool>, tokio::task::JoinHandle<()>), Box<dyn std::error::Error>> {
    let deleted = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&deleted);
    let router = axum::Router::new().route(
        "/mcp",
        axum::routing::post(|axum::Json(request): axum::Json<serde_json::Value>| async move {
            if request["method"] == "initialize" {
                (
                    [("mcp-session-id", "fixture")],
                    axum::Json(serde_json::json!({
                        "jsonrpc": "2.0", "id": request["id"], "result": {
                            "protocolVersion": "2025-11-25", "capabilities": {},
                            "serverInfo": {"name": "fixture", "version": "1"}
                        }
                    })),
                )
                    .into_response()
            } else if request.get("id").is_some() {
                // A legacy session-based server knows no `server/discover`, the probe a client
                // speaking protocol revision 2026-07-28 sends first, and says so, which is the
                // signal for that client to fall back to `initialize`.
                axum::Json(serde_json::json!({
                    "jsonrpc": "2.0", "id": request["id"],
                    "error": {"code": -32601, "message": "Method not found"}
                }))
                .into_response()
            } else {
                StatusCode::ACCEPTED.into_response()
            }
        })
        .delete(move || {
            let observed = Arc::clone(&observed);
            async move {
                observed.store(true, Ordering::SeqCst);
                if hang_on_delete {
                    std::future::pending::<()>().await;
                }
                StatusCode::OK
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/mcp", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    let set = McpClientSet::connect(&[McpServerConfig::Http {
        name: "http".into(),
        endpoint,
        token_env: None,
        auth: None,
        headers: std::collections::BTreeMap::new(),
    }])
    .await?;
    assert!(set.get("http").is_some());
    Ok((set, deleted, server))
}

#[tokio::test]
async fn shutdown_deletes_the_http_session_before_returning() -> Result<(), Box<dyn std::error::Error>> {
    let (set, deleted, server) = http_fixture(false).await?;
    set.shutdown().await;
    assert!(deleted.load(Ordering::SeqCst));
    set.shutdown().await; // Closing twice is harmless.
    server.abort();
    Ok(())
}

#[tokio::test]
async fn an_unresponsive_http_delete_cannot_block_shutdown() -> Result<(), Box<dyn std::error::Error>> {
    let (set, deleted, server) = http_fixture(true).await?;
    let session = set.get("http").ok_or("missing session")?;
    let set = Arc::new(set);
    let closing = Arc::clone(&set);
    let shutdown = tokio::spawn(async move {
        closing.shutdown().await;
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !deleted.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    tokio::time::pause();
    let start = tokio::time::Instant::now();
    shutdown.await?;
    assert!(tokio::time::Instant::now() - start <= Duration::from_secs(10));
    assert!(session.running.lock().await.is_closed());
    server.abort();
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_stdio_server_ignoring_eof_receives_sigterm_before_forced_kill() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let marker = dir.path().join("eof");
    let term_marker = dir.path().join("term");
    // A legacy server: it answers `initialize`, and refuses any other request (such as the
    // `server/discover` probe a client speaking revision 2026-07-28 sends first) as unknown.
    // After EOF the fixture blocks in `wait`, which POSIX requires to return as soon as a trapped signal arrives. A
    // shell blocked in a foreground command instead (such as `read` opening a FIFO) may defer the trap until the command
    // finishes, and on macOS, whose `sh` is bash, the trap intermittently never ran before the forced kill.
    let script = r#"
trap 'echo term > "$2"; kill "$!" 2>/dev/null; exit 0' TERM
while IFS= read -r request; do
    id=$(printf '%s' "$request" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
    case "$request" in
        *'"initialize"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-11-25","capabilities":{},"serverInfo":{"name":"fixture","version":"1"}}}\n' "$id" ;;
        *'"id":'[0-9]*) printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"Method not found"}}\n' "$id" ;;
    esac
done
sleep 600 &
echo eof > "$1"
wait "$!"
"#;
    let set = Arc::new(
        McpClientSet::connect(&[McpServerConfig::Stdio {
            name: "stubborn".into(),
            command: "sh".into(),
            args: vec![
                "-c".into(),
                script.into(),
                "fixture".into(),
                marker.to_string_lossy().into_owned(),
                term_marker.to_string_lossy().into_owned(),
            ],
        }])
        .await?,
    );
    let pid = set
        .get("stubborn")
        .and_then(|client| client.pid())
        .ok_or("missing child pid")?;
    let closing = Arc::clone(&set);
    let shutdown = tokio::spawn(async move {
        closing.shutdown().await;
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !marker.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(3)).await;
    tokio::time::resume();
    shutdown.await?;
    assert_eq!(std::fs::read_to_string(term_marker)?.trim(), "term");
    // Waiting for SDK cleanup must reap the child, not merely send a kill signal.
    assert!(
        !std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()?
            .success()
    );
    Ok(())
}

#[tokio::test]
async fn startup_stops_when_shutdown_was_already_requested() {
    let (store, clock) = no_oauth_dependencies();
    let shutdown = tokio_util::sync::CancellationToken::new();
    shutdown.cancel();
    let result = McpClientSet::connect_until_shutdown(&[], &store, &clock, &shutdown).await;
    assert!(result.is_err(), "cancelled startup must not proceed to serve HTTP");
}
