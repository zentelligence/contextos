//! Shared fixtures and request helpers for the `entity_docs_*` integration tests. Not a test target itself: a `tests/`
//! subdirectory module is compiled once into each including test crate, and each of those uses only some helpers,
//! so `#![allow(dead_code)]` keeps the rest from warning (the same arrangement as `support`).
#![allow(dead_code)]

use tower::ServiceExt as _;

use crate::support;

pub use axum::Router;
pub use axum::body::{Body, to_bytes};
pub use axum::http::{HeaderMap, Request, StatusCode};
pub use contextos_web::McpClientSet;
pub use std::path::Path;
pub use std::sync::Arc;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

pub const VAULT_NAME: &str = "contract-fixture";

pub async fn router_over(vault_dir: &Path, config_dir: &Path) -> Result<Router, BoxError> {
    let config_path = support::write_vault_config(config_dir, vault_dir)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    Ok(contextos_web::build_router(
        clients,
        Some(config_dir),
        &config_dir.join("web.toml"),
    ))
}

pub async fn get(router: &Router, path: &str) -> Result<(StatusCode, String, HeaderMap), BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty())?)
        .await?;
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned(), headers))
}

/// A `GET` carrying `HX-Request: true`, returning just the
/// `.entity-docs-screen` fragment with no full-page shell around it -
/// matching `tests/wiki.rs::get_fragment`'s identical reasoning: shape
/// assertions about the fragment's own internal structure (the accordion
/// tree in particular) should not have to fight the shell's own separate
/// breadcrumb repeating the same folder label outside the tree.
pub async fn get_fragment(router: &Router, path: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header("hx-request", "true")
                .body(Body::empty())?,
        )
        .await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

pub async fn post(router: &Router, path: &str, json_body: &str) -> Result<(StatusCode, String, HeaderMap), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(json_body.to_owned()))?,
        )
        .await?;
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned(), headers))
}

pub async fn patch(router: &Router, path: &str, json_body: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(json_body.to_owned()))?,
        )
        .await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

pub async fn delete(router: &Router, path: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().method("DELETE").uri(path).body(Body::empty())?)
        .await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

pub fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

/// A newly created folder's own path from an `HX-Redirect` response header
/// (`/{{vault}}/entity/{{entity_slug}}/{{path}}`). Creating a
/// markdown file lands in its own edit form in the same response instead
/// (see `tests/entity_docs_create.rs::pushed_path`), so only "New folder"
/// still uses a plain full-navigation redirect.
pub fn redirected_path(headers: &HeaderMap) -> Option<String> {
    let target = headers.get("HX-Redirect")?.to_str().ok()?;
    let (_, path) = target.split_once("/entity/zenith/")?;
    Some(path.to_owned())
}

/// The rendered edit form's own current `expected_hash` hidden field,
/// matching `tests/wiki.rs::extract_expected_hash`'s identical contract.
pub fn extract_expected_hash(body: &str) -> Option<String> {
    let marker = "name=\"expected_hash\" value=\"";
    let after = body.split(marker).nth(1)?;
    let (hash, _) = after.split_once('"')?;
    Some(hash.to_owned())
}

/// The rendered metadata panel's own current `expected_hash`, embedded in its own `hx-vals`
/// script rather than a hidden field, matching `tests/wiki.rs::extract_metadata_expected_hash`'s
/// identical contract.
pub fn extract_metadata_expected_hash(body: &str) -> Option<String> {
    let marker = "expected_hash: '";
    let after = body.split(marker).nth(1)?;
    let (hash, _) = after.split_once('\'')?;
    Some(hash.to_owned())
}

pub fn seed_one_entity(vault_dir: &Path) -> std::io::Result<()> {
    write(
        vault_dir,
        "memory/entities/index.md",
        "# Entities\n\n| File | Entity |\n| --- | --- |\n| zenith.md | Zenith |\n",
    )
}
