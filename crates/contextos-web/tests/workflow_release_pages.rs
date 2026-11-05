//! A vault workflow's page shows its releases as current, previous, future, or retired, with the jobs on each by
//! lifecycle stage and the flags a release raises, through the real router, a real `contextos` session, and a real
//! temporary vault.

mod support;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request};
use support::jobs::{JobVault, Spec, VAULT_NAME};
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const STATE_CURRENT_ENABLED: &str = "[ops.sync]\ncurrent = \"1.0.0\"\n\n[ops.sync.nightly]\nlifecycle = \"enabled\"\n";
const STATE_CURRENT_DRAFT: &str = "[ops.sync]\ncurrent = \"1.0.0\"\n\n[ops.sync.nightly]\nlifecycle = \"draft\"\n";
const STATE_PREVIOUS_ENABLED: &str =
    "[ops.sync]\ncurrent = \"2.0.0\"\nprevious = \"1.0.0\"\n\n[ops.sync.nightly]\nlifecycle = \"enabled\"\n";

async fn vault(state: &str) -> Result<JobVault, BoxError> {
    JobVault::open(&Spec {
        state: Some(state),
        ..Spec::default()
    })
    .await
}

fn router_over(vault: &JobVault) -> Router {
    contextos_web::build_router(
        std::sync::Arc::clone(&vault.clients),
        Some(vault.config.path()),
        &vault.config.path().join("web.toml"),
    )
}

async fn workflow_page(vault: &JobVault) -> Result<String, BoxError> {
    let request = Request::builder()
        .method(Method::GET)
        .uri(format!("/{VAULT_NAME}/workflows/ops/sync"))
        .header("hx-request", "true")
        .body(Body::empty())?;
    let response = router_over(vault).oneshot(request).await?;
    Ok(String::from_utf8_lossy(&to_bytes(response.into_body(), usize::MAX).await?).into_owned())
}

#[tokio::test]
async fn the_page_lists_the_current_release_and_the_jobs_on_it_by_stage() -> Result<(), BoxError> {
    let vault = vault(STATE_CURRENT_ENABLED).await?;

    let page = workflow_page(&vault).await?;

    assert!(page.contains("id=\"workflow-releases\""), "{page}");
    assert!(page.contains("<h3>Current, install date not recorded</h3>"), "{page}");
    assert!(page.contains("<dt>enabled</dt>"), "{page}");
    assert!(
        page.contains(&format!(
            "<a href=\"/{VAULT_NAME}/workflows/ops/sync/jobs/nightly\">Nightly</a></dd>"
        )),
        "{page}"
    );
    assert!(!page.contains("nothing running on it"), "{page}");
    Ok(())
}

#[tokio::test]
async fn a_current_release_with_no_job_enabled_or_suspended_reads_nothing_running_on_it() -> Result<(), BoxError> {
    let vault = vault(STATE_CURRENT_DRAFT).await?;

    let page = workflow_page(&vault).await?;

    assert!(page.contains("Current, nothing running on it"), "{page}");
    assert!(page.contains("<dt>draft</dt>"), "{page}");
    Ok(())
}

#[tokio::test]
async fn a_job_still_running_on_the_previous_release_is_flagged_as_needing_a_successor() -> Result<(), BoxError> {
    let vault = vault(STATE_PREVIOUS_ENABLED).await?;
    vault.publish_source_as("2.0.0")?;
    vault.name_version("1.0.0")?;

    let page = workflow_page(&vault).await?;

    assert!(page.contains("<h3>Previous, install date not recorded</h3>"), "{page}");
    assert!(page.contains("Needs a successor"), "{page}");
    assert!(page.contains("Current, nothing running on it"), "{page}");
    Ok(())
}

#[tokio::test]
async fn an_incomplete_release_is_flagged_on_the_page_with_what_it_lost() -> Result<(), BoxError> {
    let vault = vault(STATE_CURRENT_ENABLED).await?;
    std::fs::remove_file(vault.root().join(".contextos/packages/ops/1.0.0/sync/sync.rhai"))?;

    let page = workflow_page(&vault).await?;

    assert!(page.contains("incomplete"), "{page}");
    assert!(page.contains("sync/sync.rhai"), "{page}");
    assert!(page.contains("scheduled runs on it are refused"), "{page}");
    Ok(())
}

#[tokio::test]
async fn the_version_is_in_the_releases_details_and_not_in_its_heading() -> Result<(), BoxError> {
    let vault = vault(STATE_CURRENT_ENABLED).await?;

    let page = workflow_page(&vault).await?;

    let (before, details) = page.split_once("<summary>Details</summary>").ok_or("no details")?;
    assert!(!before.contains("1.0.0"), "{before}");
    assert!(details.contains("<dt>Version</dt><dd>1.0.0</dd>"), "{details}");
    Ok(())
}
