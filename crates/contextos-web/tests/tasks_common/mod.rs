//! Shared fixtures and request helpers for the `tasks_*` integration tests. Not a test target itself: a `tests/`
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

/// A clock fixed at one instant, so a test reading "today" never depends on when it runs.
pub struct FixedClock(pub time::OffsetDateTime);

impl contextos_workflows::Clock for FixedClock {
    fn now(&self) -> time::OffsetDateTime {
        self.0
    }
}

/// [`router_over`], with the contextos server keeping `zone` and the clock fixed at `at`.
pub async fn router_over_at(
    vault_dir: &Path,
    config_dir: &Path,
    zone: &str,
    at: time::OffsetDateTime,
) -> Result<Router, BoxError> {
    let config_path = support::write_vault_config(config_dir, vault_dir)?;
    let config =
        std::fs::read_to_string(&config_path)?.replace("timezone = \"UTC\"", &format!("timezone = \"{zone}\""));
    std::fs::write(&config_path, config)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    Ok(contextos_web::build_router_at(
        clients,
        Some(config_dir),
        &config_dir.join("web.toml"),
        &contextos_web::config::WorkflowsConfig::default(),
        None,
        Arc::new(FixedClock(at)),
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
    method_with_body(router, "PATCH", path, json_body).await
}

pub async fn method_with_body(
    router: &Router,
    method: &str,
    path: &str,
    json_body: &str,
) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
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

pub fn task_note(id: &str, state: &str, entity: &str, parent: Option<&str>) -> String {
    let parent_line = parent.map_or_else(String::new, |p| format!("parent: '[[{p}]]'\n"));
    format!(
        "---\n\
         id: {id}\n\
         type: task\n\
         title: Title for {id}\n\
         state: {state}\n\
         task_type: action\n\
         entity: {entity}\n\
         {parent_line}\
         ---\n\n# Title for {id}\n"
    )
}

/// Like [`task_note`], with an `assignee` and a `priority` set, for the
/// Focus view's own assignee/priority-driven behaviour.
pub fn task_note_with_assignee_and_priority(
    id: &str,
    state: &str,
    entity: &str,
    assignee: &str,
    priority: Option<&str>,
) -> String {
    let priority_line = priority.map_or_else(String::new, |p| format!("priority: {p}\n"));
    format!(
        "---\n\
         id: {id}\n\
         type: task\n\
         title: Title for {id}\n\
         state: {state}\n\
         task_type: action\n\
         entity: {entity}\n\
         assignee: {assignee}\n\
         {priority_line}\
         ---\n\n# Title for {id}\n"
    )
}

pub fn journal_path_for_today(vault_dir: &Path) -> std::path::PathBuf {
    let date = contextos_web::journal::format_date(time::OffsetDateTime::now_utc().date());
    vault_dir.join(format!("journal/{}/{}/{date}.md", &date[0..4], &date[5..7]))
}
