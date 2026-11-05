//! A workflow run's record is written in the operator's local time, in the zone the contextos server reports: its
//! dated folder and its `started` and `completed` stamps. Real `contextos-mcp` sessions over real temporary vaults,
//! with the clock fixed at 21:45 UTC on 31 October 2026, already 11:30 on 1 November in the Chatham Islands under
//! daylight saving time (+13:45), so a record dated in UTC would land in the October folder instead. The Chatham
//! Islands' zone is chosen because no machine running this test is likely to be set to it: a record dated in the
//! host's zone instead of the server's would then still be caught.

mod support;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use contextos_web::McpClientSet;
use time::OffsetDateTime;
use time::macros::datetime;
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT_NAME: &str = "contract-fixture";

struct FixedClock;

impl contextos_workflows::Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        datetime!(2026-10-31 21:45 UTC)
    }
}

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

async fn router_in_chatham(vault_dir: &Path, config_dir: &Path) -> Result<Router, BoxError> {
    support::release_fixture::install_vault_releases(vault_dir).map_err(|error| error.to_string())?;
    let config_path = support::write_vault_config(config_dir, vault_dir)?;
    let config = std::fs::read_to_string(&config_path)?.replace("timezone = \"UTC\"", "timezone = \"Pacific/Chatham\"");
    std::fs::write(&config_path, config)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    Ok(contextos_web::build_router_at(
        clients,
        Some(config_dir),
        &config_dir.join("web.toml"),
        &contextos_web::config::WorkflowsConfig::default(),
        Some(&contextos_web::package_store::AppDataRoots::from(config_dir)),
        Arc::new(FixedClock),
    ))
}

async fn send(router: &Router, method: &str, path: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().method(method).uri(path).body(Body::empty())?)
        .await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

#[tokio::test]
async fn a_run_record_is_filed_and_stamped_in_the_operators_local_time() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write(
        vault_dir.path(),
        "registry/workflows/ops/workflows.toml",
        "workflows = [\"sync\"]\n",
    )?;
    write(
        vault_dir.path(),
        "registry/workflows/ops/sync/workflow.toml",
        "name = \"Sync workflow\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = [\"contextos\"]\n",
    )?;
    write(
        vault_dir.path(),
        "registry/workflows/ops/sync/workflow.rhai",
        "call_mcp(\"contextos\", \"vault_info\", #{});",
    )?;
    write(
        vault_dir.path(),
        "registry/workflows/ops/sync/jobs/nightly.toml",
        "name = \"Nightly sync\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    write(
        vault_dir.path(),
        ".contextos/jobs.toml",
        "[ops.sync.nightly]\nlifecycle = \"registered\"\n",
    )?;
    let router = router_in_chatham(vault_dir.path(), config_dir.path()).await?;

    let (status, _) = send(
        &router,
        "POST",
        &format!("/{VAULT_NAME}/workflows/ops/sync/jobs/nightly/runs"),
    )
    .await?;
    assert_eq!(status, StatusCode::SEE_OTHER);

    let record = vault_dir.path().join("memory/jobs/ops/sync/nightly/2026/11/R-1.md");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let content = loop {
        if let Ok(content) = std::fs::read_to_string(&record)
            && content.contains("status: succeeded")
        {
            break content;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "no completed run record at {}; the jobs folder holds: {:?}",
                record.display(),
                walk(&vault_dir.path().join("memory/jobs"))
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(content.contains("2026-11-01T11:30:00"), "{content}");
    assert!(content.contains("+13:45"), "{content}");
    assert!(!vault_dir.path().join("memory/jobs/ops/sync/nightly/2026/10").exists());
    Ok(())
}

fn walk(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                found.extend(walk(&path));
            } else {
                found.push(path.display().to_string());
            }
        }
    }
    found
}
