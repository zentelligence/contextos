//! A proxy call logs method/server/tool/duration/outcome at `INFO`, with no
//! argument or result content present in the log line.

mod support;

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::Request;
use serde_json::json;
use support::logs::capture;
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[tokio::test]
async fn a_proxy_call_logs_fields_but_never_argument_or_result_content() -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let vault = dir.path().join("vault");
    std::fs::create_dir_all(&vault)?;
    let argument_path = "secret-note.md";
    std::fs::write(vault.join(argument_path), "TOP-SECRET-CONTENT-Q7F3")?;
    let config_path = support::write_vault_config(dir.path(), &vault)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(contextos_web::mcp_client::McpClientSet::connect(&[entry]).await?);
    let router = contextos_web::build_router(clients, Some(dir.path()), &dir.path().join("web.toml"));

    let (writer, _guard) = capture();

    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp/contextos/fs_read_text_file")
                .header("content-type", "application/json")
                .body(Body::from(json!({"path": argument_path}).to_string()))?,
        )
        .await?;
    // The response body legitimately carries the file content
    // (`TOP-SECRET-CONTENT-Q7F3`); only the tracing log line, asserted
    // below, must never carry it.
    let _ = to_bytes(response.into_body(), usize::MAX).await?;

    let log = writer.lines().join("\n");
    assert!(log.contains("MCP proxy call"), "log line missing: {log:?}");
    assert!(
        log.contains("server=\"contextos\""),
        "log missing server field: {log:?}"
    );
    assert!(
        log.contains("tool=\"fs_read_text_file\""),
        "log missing tool field: {log:?}"
    );
    assert!(log.contains("duration_ms="), "log missing duration_ms field: {log:?}");
    assert!(log.contains("outcome="), "log missing outcome field: {log:?}");
    assert!(
        !log.contains("TOP-SECRET-CONTENT-Q7F3"),
        "log leaked result content: {log:?}"
    );
    assert!(!log.contains(argument_path), "log leaked argument content: {log:?}");
    Ok(())
}
