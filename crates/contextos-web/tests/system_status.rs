//! The header badge and the footer's MCP servers item report real connectivity, on the page and when polled, against
//! a real `contextos` session.

mod support;

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use contextos_web::McpClientSet;
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT: &str = "contract-fixture";

async fn router_over(vault_dir: &Path, config_dir: &Path) -> Result<Router, BoxError> {
    let config_path = support::write_vault_config(config_dir, vault_dir)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    Ok(contextos_web::build_router(
        clients,
        Some(config_dir),
        &config_dir.join("web.toml"),
    ))
}

async fn get(router: &Router, path: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty())?)
        .await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

#[tokio::test]
async fn a_connected_server_shows_a_live_badge_and_an_unflagged_footer_dot_on_the_page() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    let router = router_over(vault.path(), config.path()).await?;

    let (status, page) = get(&router, &format!("/{VAULT}/settings/")).await?;

    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("live-badge status-ok"), "{page}");
    assert!(page.contains(">LIVE<"), "{page}");
    assert!(page.contains("1 of 1 connected"), "{page}");
    assert!(!page.contains("status-dot error"), "{page}");
    Ok(())
}

#[tokio::test]
async fn the_page_polls_for_connectivity_so_a_later_failure_is_shown_without_a_reload() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    let router = router_over(vault.path(), config.path()).await?;

    let (_, page) = get(&router, &format!("/{VAULT}/settings/")).await?;

    assert!(
        page.contains("hx-get=\"/shell/status\" hx-trigger=\"every 30s\" hx-swap=\"none\""),
        "{page}"
    );
    Ok(())
}

#[tokio::test]
async fn the_poll_answers_with_both_elements_marked_to_replace_their_counterparts() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    let router = router_over(vault.path(), config.path()).await?;

    let (status, fragment) = get(&router, "/shell/status").await?;

    assert_eq!(status, StatusCode::OK);
    assert!(fragment.contains("id=\"system-status-badge\""), "{fragment}");
    assert!(fragment.contains("id=\"system-status-mcp\""), "{fragment}");
    assert_eq!(fragment.matches("hx-swap-oob=\"outerHTML\"").count(), 2, "{fragment}");
    assert!(!fragment.contains("<html"), "a fragment carries no page chrome");
    Ok(())
}
