//! Every `contextos-web` command that connects to an MCP server ends its session before the process exits. The server
//! here is a fixture over HTTP that records the session deletion a client sends when it closes, which is observable
//! once the command has exited, so a command that returns without closing fails on an assertion and not on a race in
//! what the process logs while it ends.
#![cfg(target_os = "linux")]

mod support;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::http::StatusCode;
use axum::response::IntoResponse;
use tokio::process::Command;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A server that opens a session and answers every request after that with an error, so each command ends in a refusal
/// or a failure, and counts the deletions of its session.
struct Fixture {
    deleted: Arc<AtomicUsize>,
    server: tokio::task::JoinHandle<()>,
    root: tempfile::TempDir,
}

impl Fixture {
    async fn start() -> Result<Self, BoxError> {
        let deleted = Arc::new(AtomicUsize::new(0));
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
                    observed.fetch_add(1, Ordering::SeqCst);
                    StatusCode::OK
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}/mcp", listener.local_addr()?);
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        let root = tempfile::tempdir()?;
        std::fs::write(
            root.path().join("web.toml"),
            format!("[[mcp_server]]\nname = \"contextos\"\ntransport = \"http\"\nendpoint = {endpoint:?}\n"),
        )?;
        std::fs::write(root.path().join("job.toml"), "name = \"Nightly\"\n")?;
        Ok(Self { deleted, server, root })
    }

    /// Runs the command with `arguments`, and says how many sessions were deleted by the time it had exited.
    async fn deletions_after(&self, arguments: &[&str]) -> Result<usize, BoxError> {
        let data = self.root.path().join("data");
        let output = Command::new(env!("CARGO_BIN_EXE_contextos-web"))
            .arg("--config")
            .arg(self.root.path().join("web.toml"))
            .args(arguments)
            .env("XDG_DATA_HOME", &data)
            .env("HOME", &data)
            .env_remove("CONTEXTOS_WEB_CONFIG")
            .env_remove("RUST_LOG")
            .output()
            .await?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains("fail to close sink"), "stderr: {stderr}");
        Ok(self.deleted.load(Ordering::SeqCst))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn assert_closes(arguments: &[&str]) -> Result<(), BoxError> {
    let fixture = Fixture::start().await?;
    let file = fixture.root.path().join("job.toml");
    let file: &Path = &file;
    let arguments: Vec<String> = arguments
        .iter()
        .map(|part| part.replace("{file}", &file.to_string_lossy()))
        .collect();
    let borrowed: Vec<&str> = arguments.iter().map(String::as_str).collect();
    let deleted = fixture.deletions_after(&borrowed).await?;
    assert_eq!(
        deleted,
        1,
        "`{}` returned without ending its MCP session",
        borrowed.join(" ")
    );
    Ok(())
}

const HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[tokio::test]
async fn workflows_releases_ends_its_session() -> Result<(), BoxError> {
    assert_closes(&["workflows", "releases", "va", "ops", "sync"]).await
}

#[tokio::test]
async fn workflows_install_ends_its_session() -> Result<(), BoxError> {
    assert_closes(&["workflows", "install", "va", "ops"]).await
}

#[tokio::test]
async fn libraries_install_ends_its_session() -> Result<(), BoxError> {
    assert_closes(&["libraries", "install", "va", "crm"]).await
}

#[tokio::test]
async fn jobs_list_ends_its_session() -> Result<(), BoxError> {
    assert_closes(&["jobs", "list", "va"]).await
}

#[tokio::test]
async fn jobs_show_ends_its_session() -> Result<(), BoxError> {
    assert_closes(&["jobs", "show", "va", "ops", "sync", "nightly"]).await
}

#[tokio::test]
async fn jobs_create_ends_its_session() -> Result<(), BoxError> {
    assert_closes(&["jobs", "create", "va", "ops", "sync", "nightly", "--file", "{file}"]).await
}

#[tokio::test]
async fn jobs_replace_ends_its_session() -> Result<(), BoxError> {
    assert_closes(&[
        "jobs",
        "replace",
        "va",
        "ops",
        "sync",
        "nightly",
        "--file",
        "{file}",
        "--expected-hash",
        HASH,
    ])
    .await
}

#[tokio::test]
async fn jobs_validate_ends_its_session() -> Result<(), BoxError> {
    assert_closes(&["jobs", "validate", "va", "ops", "sync", "nightly"]).await
}

#[tokio::test]
async fn jobs_retire_ends_its_session() -> Result<(), BoxError> {
    assert_closes(&["jobs", "retire", "va", "ops", "sync", "nightly"]).await
}

#[tokio::test]
async fn jobs_run_ends_its_session() -> Result<(), BoxError> {
    assert_closes(&["jobs", "run", "va", "ops", "sync", "nightly"]).await
}

#[tokio::test]
async fn jobs_dry_run_ends_its_session() -> Result<(), BoxError> {
    assert_closes(&["jobs", "dry-run", "va", "ops", "sync", "nightly"]).await
}

#[tokio::test]
async fn workflows_run_ends_its_session() -> Result<(), BoxError> {
    assert_closes(&["workflows", "run", "va", "ops", "sync"]).await
}
