//! The favourite star of a job, through the real router over a real `contextos` session and a real temporary vault:
//! toggling it, what the card and the job's own page show, and that it never disturbs the job's stored schedule.

mod job_routes_common;
mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use job_routes_common::{BoxError, VAULT_NAME, get, patch_favourite, router_over, write, write_job, write_workflow};
use tower::ServiceExt as _;

#[tokio::test]
async fn setting_favourite_true_marks_the_list_cards_star_pressed() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write_workflow(
        vault_dir.path(),
        "sync-crm-contacts",
        "name = \"Sync CRM Contacts workflow\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = []\n",
        "workflow.rhai",
        "fn run() { true }",
    )?;
    write_job(
        vault_dir.path(),
        "sync-crm-contacts",
        "nightly",
        "name         = \"Sync CRM Contacts\"\nowner = \"vela\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_status, body) = get(&router, &format!("/{VAULT_NAME}/workflows")).await?;
    assert!(body.contains("aria-pressed=\"false\""));

    let (status, body) = patch_favourite(
        &router,
        &format!("/{VAULT_NAME}/workflows/sync-crm-contacts/sync-crm-contacts/jobs/nightly"),
        true,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("aria-pressed=\"true\""));

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/workflows")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("aria-pressed=\"true\""));
    Ok(())
}

#[tokio::test]
async fn setting_favourite_false_after_true_returns_to_unfavourited() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write_workflow(
        vault_dir.path(),
        "sync-crm-contacts",
        "name = \"Sync CRM Contacts workflow\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = []\n",
        "workflow.rhai",
        "fn run() { true }",
    )?;
    write_job(
        vault_dir.path(),
        "sync-crm-contacts",
        "nightly",
        "name         = \"Sync CRM Contacts\"\nowner = \"vela\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    patch_favourite(
        &router,
        &format!("/{VAULT_NAME}/workflows/sync-crm-contacts/sync-crm-contacts/jobs/nightly"),
        true,
    )
    .await?;
    patch_favourite(
        &router,
        &format!("/{VAULT_NAME}/workflows/sync-crm-contacts/sync-crm-contacts/jobs/nightly"),
        false,
    )
    .await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/workflows")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("aria-pressed=\"false\""));
    Ok(())
}

#[tokio::test]
async fn the_job_detail_pages_own_toggle_reflects_the_same_favourite_state() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write_workflow(
        vault_dir.path(),
        "sync-crm-contacts",
        "name = \"Sync CRM Contacts workflow\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = []\n",
        "workflow.rhai",
        "fn run() { true }",
    )?;
    write_job(
        vault_dir.path(),
        "sync-crm-contacts",
        "nightly",
        "name         = \"Sync CRM Contacts\"\nowner = \"vela\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    patch_favourite(
        &router,
        &format!("/{VAULT_NAME}/workflows/sync-crm-contacts/sync-crm-contacts/jobs/nightly"),
        true,
    )
    .await?;

    let (status, body) = get(
        &router,
        &format!("/{VAULT_NAME}/workflows/sync-crm-contacts/sync-crm-contacts/jobs/nightly"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("aria-pressed=\"true\""));
    Ok(())
}

#[tokio::test]
async fn setting_favourite_for_an_unregistered_slug_404s() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body) = patch_favourite(
        &router,
        &format!("/{VAULT_NAME}/workflows/no-such-workflow/no-such-workflow/jobs/no-such-job"),
        true,
    )
    .await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn favourite_updates_preserve_the_schedule_baseline_and_reject_malformed_state() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    write_workflow(
        vault.path(),
        "job",
        "name = \"Job workflow\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = []\n",
        "workflow.rhai",
        "()",
    )?;
    write_job(
        vault.path(),
        "job",
        "run",
        "name = \"Job\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    let original = "[job.job.run]\nlast_scheduled_fire = \"2026-09-20T09:00:00Z\"\n";
    write(vault.path(), ".contextos/jobs.toml", original)?;
    let router = router_over(vault.path(), config.path()).await?;
    for favourite in [true, false] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/{VAULT_NAME}/workflows/job/job/jobs/run"))
                    .header("content-type", "application/json")
                    .body(Body::from(format!("{{\"favourite\":{favourite}}}")))?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get("hx-trigger").and_then(|v| v.to_str().ok()),
            Some("favourites-changed")
        );
        let text = std::fs::read_to_string(vault.path().join(".contextos/jobs.toml"))?;
        assert!(
            !text.contains("current_run") && !text.contains("next_run_id"),
            "nothing a run writes is kept here: {text}"
        );
        let state: contextos_workflows::JobsState = toml::from_str(&text)?;
        let job_state = state.job("job", "job", "run").ok_or("job state lost")?;
        assert!(job_state.last_scheduled_fire.is_some());
        assert_eq!(job_state.favourite, favourite);
    }
    write(vault.path(), ".contextos/jobs.toml", "broken = [")?;
    let (status, _) = patch_favourite(&router, &format!("/{VAULT_NAME}/workflows/job/job/jobs/run"), true).await?;
    assert!(!status.is_success());
    assert_eq!(
        std::fs::read_to_string(vault.path().join(".contextos/jobs.toml"))?,
        "broken = ["
    );
    Ok(())
}
