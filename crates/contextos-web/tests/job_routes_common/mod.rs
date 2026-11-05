//! What the job route tests share: a router over a real `contextos` session and a real temporary vault, requests against
//! it, and the files of a workflow and its jobs. Not a test target.
#![allow(dead_code)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use contextos_web::McpClientSet;
use tower::ServiceExt as _;

use super::support;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

pub const VAULT_NAME: &str = "contract-fixture";

pub async fn router_over(vault_dir: &Path, config_dir: &Path) -> Result<Router, BoxError> {
    support::release_fixture::install_vault_releases(vault_dir).map_err(|error| error.to_string())?;
    let config_path = support::write_vault_config(config_dir, vault_dir)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    Ok(contextos_web::build_router_in(
        clients,
        Some(config_dir),
        &config_dir.join("web.toml"),
        &contextos_web::config::WorkflowsConfig::default(),
        Some(&contextos_web::package_store::AppDataRoots::from(config_dir)),
    ))
}

/// The claim of one job, in the app-data directory [`router_over`] gave the router.
pub fn claim_of(config_dir: &Path, workflow: &str, job: &str) -> Result<contextos_web::run_claim::ClaimSlot, BoxError> {
    Ok(contextos_web::vault_runs::claim_slot(
        &support::host(config_dir),
        VAULT_NAME,
        workflow,
        workflow,
        job,
    )?)
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

pub async fn post(router: &Router, path: &str) -> Result<StatusCode, BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().method("POST").uri(path).body(Body::empty())?)
        .await?;
    Ok(response.status())
}

/// `PATCH` with a `{"favourite": ...}` JSON body, matching the star toggle's own real request shape
/// (`hx-ext="json-enc"`, `docs/http-routing-response-contract.md`'s PATCH "submit partial update" row).
pub async fn patch_favourite(router: &Router, path: &str, favourite: bool) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(path)
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!("{{\"favourite\":{favourite}}}")))?,
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

/// Registers a workflow: its own manifest plus entry script, under
/// `registry/workflows/{{package}}/{{workflow}}/`.
pub fn write_workflow(
    vault_dir: &Path,
    workflow_slug: &str,
    workflow_toml: &str,
    entry_name: &str,
    entry_content: &str,
) -> std::io::Result<()> {
    write(
        vault_dir,
        &format!("registry/workflows/{workflow_slug}/workflows.toml"),
        &format!("workflows = [{workflow_slug:?}]\n"),
    )?;
    write(
        vault_dir,
        &format!("registry/workflows/{workflow_slug}/{workflow_slug}/workflow.toml"),
        workflow_toml,
    )?;
    write(
        vault_dir,
        &format!("registry/workflows/{workflow_slug}/{workflow_slug}/{entry_name}"),
        entry_content,
    )
}

/// Registers a saved job nested under its parent workflow's own
/// `jobs/` directory.
pub fn write_job(vault_dir: &Path, workflow_slug: &str, job_slug: &str, job_toml: &str) -> std::io::Result<()> {
    write(
        vault_dir,
        &format!("registry/workflows/{workflow_slug}/{workflow_slug}/jobs/{job_slug}.toml"),
        job_toml,
    )
}

/// Records a job's lifecycle state (and optionally its schedule) in the vault's `.contextos/jobs.toml`, as the Web-owned
/// state store holds it.
pub fn set_state(
    vault_dir: &Path,
    workflow_slug: &str,
    job_slug: &str,
    lifecycle: &str,
    schedule: Option<&str>,
) -> std::io::Result<()> {
    let schedule = schedule.map_or_else(String::new, |expression| format!("schedule = \"{expression}\"\n"));
    write(
        vault_dir,
        ".contextos/jobs.toml",
        &format!("[{workflow_slug}.{workflow_slug}.{job_slug}]\nlifecycle = \"{lifecycle}\"\n{schedule}"),
    )
}

/// Polls `GET /{{vault_name}}/jobs/{{workflow_slug}}/{{slug}}/` until its
/// rendered run history contains `needle` (a manually triggered run
/// completes asynchronously, `scheduler::execute_run` runs detached),
/// bounded by a real deadline rather than looping forever: this is the
/// same "wait for genuine async completion, bounded" pattern
/// `mcp_client_test.rs`'s own `tokio::time::timeout` use establishes, not
/// an arbitrary pacing sleep (`testing.md`).
pub async fn wait_for_detail_page_to_contain(router: &Router, path: &str, needle: &str) -> Result<String, BoxError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let (status, body) = get(router, path).await?;
        if status == StatusCode::OK && body.contains(needle) {
            return Ok(body);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("timed out waiting for {path:?} to contain {needle:?}; last body:\n{body}").into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
