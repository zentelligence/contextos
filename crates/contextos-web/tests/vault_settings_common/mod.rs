//! Shared fixtures and request helpers for the `vault_settings_*` integration tests. Not a test target itself: a `tests/`
//! subdirectory module is compiled once into each including test crate, and each of those uses only some helpers,
//! so `#![allow(dead_code)]` keeps the rest from warning (the same arrangement as `support`).
#![allow(dead_code)]

use tower::ServiceExt as _;

use crate::support;

pub use axum::Router;
pub use axum::body::{Body, to_bytes};
pub use axum::http::{Request, StatusCode};
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

pub async fn get(router: &Router, path: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty())?)
        .await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
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

pub fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

/// Slices one category's own `<section id="pane-{name}">...</section>`
/// fragment out of a rendered `/{{vault_name}}/settings/` body, so an
/// assertion about that category's own markup cannot be satisfied by a
/// different category's identically-shaped field on the same page (every
/// category renders in one response, `vault_settings.rs`'s own module doc).
pub fn section_panel<'a>(body: &'a str, name: &str) -> Result<&'a str, BoxError> {
    let marker = format!("id=\"pane-{name}\"");
    let start = body.find(&marker).ok_or_else(|| format!("no {name:?} panel in body"))?;
    let rest = &body[start..];
    let end = rest.find("</section>").unwrap_or(rest.len());
    Ok(&rest[..end])
}

pub fn inbox_outbox_body(curator_agent: &str, inbox_dir: &str, outbox_dir: &str, processed_dir: &str) -> String {
    format!(
        r#"{{"target": "inbox_outbox", "curator_agent": "{curator_agent}", "inbox_dir": "{inbox_dir}", "outbox_dir": "{outbox_dir}", "processed_dir": "{processed_dir}"}}"#
    )
}

pub fn journal_body(journal_dir: &str, template_path: &str) -> String {
    format!(
        r#"{{"target": "journal", "journal_dir": "{journal_dir}", "template_path": "{template_path}", "date_format": "YYYY/MM/YYYY-MM-DD", "date_structure_side": "right"}}"#
    )
}

pub fn notes_settings_body(notes_dir: &str, template_path: &str) -> String {
    format!(
        r#"{{"target": "notes", "notes_dir": "{notes_dir}", "template_path": "{template_path}", "date_format": "YYYY/MM/YYYY-MM-DD-HHmm", "date_structure_active": true, "date_structure_side": "right", "metadata_panel_side": "right"}}"#
    )
}

pub fn wiki_settings_body(wiki_dir: &str, tree_side: &str) -> String {
    format!(r#"{{"target": "wiki", "wiki_dir": "{wiki_dir}", "date_structure_side": "{tree_side}"}}"#)
}

pub fn wiki_settings_body_with_metadata_side(wiki_dir: &str, tree_side: &str, metadata_panel_side: &str) -> String {
    format!(
        r#"{{"target": "wiki", "wiki_dir": "{wiki_dir}", "date_structure_side": "{tree_side}", "metadata_panel_side": "{metadata_panel_side}"}}"#
    )
}

pub fn insights_settings_body(insights_dir: &str, shard_side: &str) -> String {
    format!(r#"{{"target": "insights", "insights_dir": "{insights_dir}", "date_structure_side": "{shard_side}"}}"#)
}

pub fn insights_settings_body_with_metadata_side(
    insights_dir: &str,
    shard_side: &str,
    metadata_panel_side: &str,
) -> String {
    format!(
        r#"{{"target": "insights", "insights_dir": "{insights_dir}", "date_structure_side": "{shard_side}", "metadata_panel_side": "{metadata_panel_side}"}}"#
    )
}

pub fn entity_docs_settings_body(folder_side: &str) -> String {
    format!(r#"{{"target": "entity_docs", "date_structure_side": "{folder_side}"}}"#)
}

pub fn entity_docs_settings_body_with_metadata_side(folder_side: &str, metadata_panel_side: &str) -> String {
    format!(
        r#"{{"target": "entity_docs", "date_structure_side": "{folder_side}", "metadata_panel_side": "{metadata_panel_side}"}}"#
    )
}

pub fn profile_settings_body(side: &str, entity_template_path: &str) -> String {
    format!(
        r#"{{"target": "profile", "date_structure_side": "{side}", "entity_template_path": "{entity_template_path}"}}"#
    )
}
