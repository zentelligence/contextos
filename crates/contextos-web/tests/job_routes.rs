//! Workflow/job registration, manual triggering, and run-lifecycle
//! contract tests: real `contextos-mcp` sessions against
//! real temporary vault fixtures, exercising `GET /{{vault_name}}/workflows`,
//! `GET /{{vault_name}}/workflows/{{workflow_slug}}/{{workflow_slug}}/jobs/{{slug}}`, and `POST
//! /{{vault_name}}/workflows/{{workflow_slug}}/{{workflow_slug}}/jobs/{{slug}}/runs` end to end. No
//! mocking, per `testing.md`.
//!
//! A job is a saved configuration of a workflow: a fixture registers a
//! `registry/workflows/{{package}}/{{workflow}}/workflow.toml` (name, entry,
//! `mcp_servers`) plus, nested inside it, one or more
//! `jobs/{{job-slug}}.toml` files (name, optional `schedule`/`timeout_secs`
//! override/owner). A job's own slug is unique only within its parent
//! workflow, never vault-wide.
//!
//! The scheduler's own cron-tick timing (`scheduler.rs`'s `due_tick`,
//! `evaluate_tick`) is proven deterministically at the unit level
//! (`contextos-workflows`' and `contextos-web`'s own `#[cfg(test)]` suites,
//! against fixed instants, never a real wall-clock wait); waiting out a
//! real 30-second poll interval here would only make this suite slow and
//! flaky for no additional coverage, so these tests exercise manual
//! triggering, which drives the identical run-lifecycle code path
//! immediately.

mod job_routes_common;
mod support;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use job_routes_common::{
    BoxError, VAULT_NAME, claim_of, get, patch_favourite, post, router_over, set_state,
    wait_for_detail_page_to_contain, write, write_job, write_workflow,
};
use std::path::Path;
use tower::ServiceExt as _;

// ---------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_valid_job_registers_and_appears_in_the_list() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write_workflow(
        vault_dir.path(),
        "sync-crm-contacts",
        r#"
            name        = "Sync CRM contacts workflow"
            entry       = "workflow.rhai"
            mcp_servers.allow = ["contextos"]
        "#,
        "workflow.rhai",
        "call_mcp(\"contextos\", \"vault_info\", #{});",
    )?;
    write_job(
        vault_dir.path(),
        "sync-crm-contacts",
        "nightly",
        "name = \"Sync CRM contacts\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/workflows")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Sync CRM contacts"));
    assert!(body.contains(&format!(
        "/{VAULT_NAME}/workflows/sync-crm-contacts/sync-crm-contacts/jobs/nightly"
    )));
    Ok(())
}

#[tokio::test]
async fn a_job_detail_page_shows_its_stored_state_and_schedule_and_starts_with_no_run_history() -> Result<(), BoxError>
{
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write_workflow(
        vault_dir.path(),
        "sync-crm-contacts",
        "name = \"Sync CRM contacts workflow\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = []\n",
        "workflow.rhai",
        "()",
    )?;
    write_job(
        vault_dir.path(),
        "sync-crm-contacts",
        "nightly",
        "name = \"Nightly sync\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    set_state(
        vault_dir.path(),
        "sync-crm-contacts",
        "nightly",
        "enabled",
        Some("0 3 * * *"),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(
        &router,
        &format!("/{VAULT_NAME}/workflows/sync-crm-contacts/sync-crm-contacts/jobs/nightly"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("0 3 * * *"));
    assert!(body.contains("enabled"));
    assert!(body.contains("No runs recorded yet"));
    Ok(())
}

#[tokio::test]
async fn an_mcp_servers_entry_naming_an_unconfigured_server_fails_workflow_registration_without_blocking_others()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write_workflow(
        vault_dir.path(),
        "broken",
        "name = \"Broken workflow\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = [\"not-configured\"]\n",
        "workflow.rhai",
        "()",
    )?;
    write_job(
        vault_dir.path(),
        "broken",
        "run",
        "name = \"Broken job\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    write_workflow(
        vault_dir.path(),
        "healthy",
        "name = \"Healthy workflow\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = []\n",
        "workflow.rhai",
        "()",
    )?;
    write_job(
        vault_dir.path(),
        "healthy",
        "run",
        "name = \"Healthy job\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/workflows")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("Broken job"));
    assert!(body.contains("Healthy job"));
    Ok(())
}

#[tokio::test]
async fn a_jobs_page_shows_its_own_timeout_and_never_its_workflows_default() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write_workflow(
        vault_dir.path(),
        "sync-crm-contacts",
        "name = \"Sync CRM contacts workflow\"\nentry = \"workflow.rhai\"\ntimeout_secs = 120\nmcp_servers.allow = []\n",
        "workflow.rhai",
        "()",
    )?;
    write_job(
        vault_dir.path(),
        "sync-crm-contacts",
        "nightly",
        "name = \"Nightly sync\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(
        &router,
        &format!("/{VAULT_NAME}/workflows/sync-crm-contacts/sync-crm-contacts/jobs/nightly"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("30s"), "the job states its own timeout");
    assert!(!body.contains("120s"), "a job never takes its workflow's timeout");
    Ok(())
}

#[tokio::test]
async fn a_jobs_own_timeout_override_takes_precedence_over_its_workflows_default() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write_workflow(
        vault_dir.path(),
        "sync-crm-contacts",
        "name = \"Sync CRM contacts workflow\"\nentry = \"workflow.rhai\"\ntimeout_secs = 120\nmcp_servers.allow = []\n",
        "workflow.rhai",
        "()",
    )?;
    write_job(
        vault_dir.path(),
        "sync-crm-contacts",
        "nightly",
        "name         = \"Nightly sync\"\nowner = \"vela\"\ntimeout_secs = 45\nversion = \"1.0.0\"\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(
        &router,
        &format!("/{VAULT_NAME}/workflows/sync-crm-contacts/sync-crm-contacts/jobs/nightly"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("45s"));
    Ok(())
}

#[tokio::test]
async fn two_workflows_with_identically_slugged_jobs_are_both_reachable_and_run_independently() -> Result<(), BoxError>
{
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    for workflow_slug in ["workflow-a", "workflow-b"] {
        write_workflow(
            vault_dir.path(),
            workflow_slug,
            &format!("name = {workflow_slug:?}\nentry = \"workflow.rhai\"\nmcp_servers.allow = []\n"),
            "workflow.rhai",
            "()",
        )?;
        write_job(
            vault_dir.path(),
            workflow_slug,
            "nightly",
            &format!(
                "name = \"Nightly ({workflow_slug})\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n"
            ),
        )?;
    }
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/workflows")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(&format!("/{VAULT_NAME}/workflows/workflow-a/workflow-a/jobs/nightly")));
    assert!(body.contains(&format!("/{VAULT_NAME}/workflows/workflow-b/workflow-b/jobs/nightly")));

    for workflow_slug in ["workflow-a", "workflow-b"] {
        let (status, body) = get(
            &router,
            &format!("/{VAULT_NAME}/workflows/{workflow_slug}/{workflow_slug}/jobs/nightly"),
        )
        .await?;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains(&format!("Nightly ({workflow_slug})")));
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Manual triggering and run lifecycle
// ---------------------------------------------------------------------

#[tokio::test]
async fn manual_run_now_creates_a_run_record_that_completes_as_succeeded() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write_workflow(
        vault_dir.path(),
        "sync-crm-contacts",
        "name = \"Sync CRM contacts workflow\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = [\"contextos\"]\n",
        "workflow.rhai",
        "call_mcp(\"contextos\", \"vault_info\", #{});",
    )?;
    write_job(
        vault_dir.path(),
        "sync-crm-contacts",
        "nightly",
        "name = \"Nightly sync\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    set_state(vault_dir.path(), "sync-crm-contacts", "nightly", "registered", None)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let status = post(
        &router,
        &format!("/{VAULT_NAME}/workflows/sync-crm-contacts/sync-crm-contacts/jobs/nightly/runs"),
    )
    .await?;
    assert_eq!(status, StatusCode::SEE_OTHER);

    let body = wait_for_detail_page_to_contain(
        &router,
        &format!("/{VAULT_NAME}/workflows/sync-crm-contacts/sync-crm-contacts/jobs/nightly"),
        "succeeded",
    )
    .await?;
    assert!(body.contains("R-1"));
    assert!(body.contains("manual"));

    // The run record itself carries the full contract: read it back
    // directly rather than relying on the detail page's own rendering to
    // prove the underlying frontmatter. Its path is dated by the run's
    // own UTC start instant, not a fixed fixture date, since the
    // scheduler reads the real wall clock, and nested by workflow then
    // job slug.
    let run_file = find_run_record(
        &vault_dir
            .path()
            .join("memory/jobs/sync-crm-contacts/sync-crm-contacts/nightly"),
        "R-1.md",
    )?;
    let content = std::fs::read_to_string(run_file)?;
    assert!(content.contains("type: job-run"));
    assert!(content.contains("status: succeeded"));
    assert!(content.contains("trigger: manual"));
    assert!(content.contains("workflow: sync-crm-contacts"));
    assert!(content.contains("job: nightly"));
    Ok(())
}

/// Recursively finds `file_name` under `root`
/// (`memory/jobs/{{workflow-slug}}/{{job-slug}}/`'s own `{{YYYY}}/{{MM}}/`
/// date partitioning): the test does not hardcode a date, since the
/// scheduler reads the real wall clock.
fn find_run_record(root: &Path, file_name: &str) -> Result<std::path::PathBuf, BoxError> {
    for entry in walk(root)? {
        if entry.file_name().is_some_and(|name| name == file_name) {
            return Ok(entry);
        }
    }
    Err(format!("{file_name:?} not found anywhere under {}", root.display()).into())
}

fn walk(dir: &Path) -> std::io::Result<Vec<std::path::PathBuf>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk(&path)?);
        } else {
            found.push(path);
        }
    }
    Ok(found)
}

#[tokio::test]
async fn a_failing_script_records_status_failed_with_the_error_message_and_is_never_retried() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write_workflow(
        vault_dir.path(),
        "always-fails",
        "name = \"Always fails workflow\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = [\"contextos\"]\n",
        "workflow.rhai",
        "call_mcp(\"contextos\", \"fs_read_text_file\", #{ path: \"does-not-exist.md\" });",
    )?;
    write_job(
        vault_dir.path(),
        "always-fails",
        "attempt",
        "name = \"Always fails\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    set_state(vault_dir.path(), "always-fails", "attempt", "registered", None)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let status = post(
        &router,
        &format!("/{VAULT_NAME}/workflows/always-fails/always-fails/jobs/attempt/runs"),
    )
    .await?;
    assert_eq!(status, StatusCode::SEE_OTHER);

    let body = wait_for_detail_page_to_contain(
        &router,
        &format!("/{VAULT_NAME}/workflows/always-fails/always-fails/jobs/attempt"),
        "failed",
    )
    .await?;
    assert!(body.contains("does-not-exist"));

    // No automatic retry: only R-1 exists even after the failed run has
    // fully completed. `contextos-mcp` also maintains its own managed
    // `index.md` per folder on every mutation (`config.rs`'s `index_md`;
    // `memory/jobs` is excluded from that by default, but the assertion
    // below counts by the run records' own `R-{{n}}.md` naming
    // regardless), so the run files are counted by name rather than by
    // raw directory-entry count.
    let run_dir = vault_dir.path().join("memory/jobs/always-fails/always-fails/attempt");
    let run_files: Vec<_> = walk(&run_dir)?
        .into_iter()
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("R-"))
        })
        .collect();
    assert_eq!(
        run_files.len(),
        1,
        "expected exactly one run record, found {run_files:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_manual_trigger_while_a_run_is_active_is_rejected_as_busy() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write_workflow(
        vault_dir.path(),
        "slow-job",
        "name = \"Slow workflow\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = []\n",
        "workflow.rhai",
        "()",
    )?;
    write_job(
        vault_dir.path(),
        "slow-job",
        "execute",
        "name = \"Slow job\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    set_state(vault_dir.path(), "slow-job", "execute", "registered", None)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;
    // A previous run of this job is still executing, so it holds the job's claim.
    let _running = contextos_web::run_claim::RunClaim::acquire(&claim_of(config_dir.path(), "slow-job", "execute")?)?;

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/{VAULT_NAME}/workflows/slow-job/slow-job/jobs/execute/runs"))
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    Ok(())
}

// ---------------------------------------------------------------------
// Favouriting
// ---------------------------------------------------------------------

#[tokio::test]
async fn owner_badges_and_idle_status_render_for_both_member_kinds() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    write(
        vault.path(),
        ".contextos/team.toml",
        "[[member]]\nslug = \"operator\"\nname = \"Human Owner\"\ntype = \"human\"\nchief_of_staff = true\n[[member]]\nslug = \"assistant\"\nname = \"Agent Owner\"\ntype = \"agent\"\n",
    )?;
    // The invalid job states no timeout, which is what keeps a job from registering.
    for (slug, owner) in [
        ("human-job", "operator"),
        ("agent-job", "assistant"),
        ("invalid-job", "assistant"),
    ] {
        write_workflow(
            vault.path(),
            slug,
            &format!("name = {slug:?}\nentry = \"workflow.rhai\"\nmcp_servers.allow = []\n"),
            "workflow.rhai",
            "()",
        )?;
        write_job(
            vault.path(),
            slug,
            "run",
            &format!(
                "name = {slug:?}\nowner = {owner:?}\n{}",
                if slug == "invalid-job" {
                    ""
                } else {
                    "timeout_secs = 30\nversion = \"1.0.0\"\n"
                }
            ),
        )?;
    }
    let router = router_over(vault.path(), config.path()).await?;
    let (_, list) = get(&router, &format!("/{VAULT_NAME}/workflows")).await?;
    assert!(list.contains("Human Owner"));
    assert!(list.contains("Agent Owner"));
    // A job that does not register is still listed, flagged, so an operator can open it and validate it.
    assert!(
        list.contains("invalid-job") && list.contains("does not register"),
        "{list}"
    );
    for (slug, icon) in [("human-job", "user-round"), ("agent-job", "bot")] {
        let (status, body) = get(&router, &format!("/{VAULT_NAME}/workflows/{slug}/{slug}/jobs/run")).await?;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains(&format!("ic-{icon}")));
        assert!(body.contains("status-dot idle"));
        assert!(body.contains("Run log"));
        assert!(body.contains("Never run"));
    }
    Ok(())
}

#[tokio::test]
async fn workflows_sidebar_uses_latest_run_attention_independently_of_favourites() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    for (slug, status) in [
        ("failed-job", "failed"),
        ("skipped-job", "skipped"),
        ("running-job", "running"),
        ("succeeded-job", "succeeded"),
        ("new-job", ""),
    ] {
        write_workflow(
            vault.path(),
            slug,
            &format!("name = {slug:?}\nentry = \"workflow.rhai\"\nmcp_servers.allow = []\n"),
            "workflow.rhai",
            "()",
        )?;
        write_job(
            vault.path(),
            slug,
            "run",
            &format!("name = {slug:?}\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n"),
        )?;
        if !status.is_empty() {
            write(
                vault.path(),
                &format!("memory/jobs/{slug}/{slug}/run/2026/09/R-2.md"),
                &format!(
                    "---\ntype: job-run\nworkflow: {slug}\njob: run\nstatus: {status}\ntrigger: manual\nstarted: 2026-09-20T10:00:00Z\n---\n"
                ),
            )?;
            write(
                vault.path(),
                &format!("memory/jobs/{slug}/{slug}/run/2026/08/R-1.md"),
                "---\nstatus: failed\n---\n",
            )?;
        }
    }
    let router = router_over(vault.path(), config.path()).await?;
    patch_favourite(
        &router,
        &format!("/{VAULT_NAME}/workflows/succeeded-job/succeeded-job/jobs/run"),
        true,
    )
    .await?;
    let (_, body) = get(&router, &format!("/{VAULT_NAME}/workflows")).await?;
    let sidebar = body
        .split("<aside")
        .nth(1)
        .ok_or("sidebar missing")?
        .split("</aside>")
        .next()
        .ok_or("sidebar end missing")?;
    assert!(sidebar.contains("failed-job"));
    assert!(sidebar.contains("skipped-job"));
    assert!(sidebar.contains("status-dot error"));
    assert!(sidebar.contains("status-dot warning"));
    for slug in ["running-job", "succeeded-job", "new-job"] {
        assert!(!sidebar.contains(slug));
    }
    for (slug, state) in [
        ("failed-job", "blocked"),
        ("skipped-job", "idle"),
        ("running-job", "working"),
        ("succeeded-job", "done"),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/{VAULT_NAME}/workflows/{slug}/{slug}/jobs/run"))
                    .header("hx-request", "true")
                    .body(Body::empty())?,
            )
            .await?;
        let body = String::from_utf8(to_bytes(response.into_body(), usize::MAX).await?.to_vec())?;
        assert!(!body.contains("<html"));
        assert!(body.contains(&format!("status-dot {state}")), "missing {state}: {body}");
    }
    Ok(())
}

#[tokio::test]
async fn latest_run_is_selected_before_history_results_are_bounded() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    write_workflow(
        vault.path(),
        "many-runs",
        "name = \"Many runs workflow\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = []\n",
        "workflow.rhai",
        "()",
    )?;
    write_job(
        vault.path(),
        "many-runs",
        "run",
        "name = \"Many runs\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    for id in 1..=5000 {
        write(
            vault.path(),
            &format!("memory/jobs/many-runs/many-runs/run/2025/01/R-{id}.md"),
            "---\ntype: job-run\nworkflow: many-runs\njob: run\nstatus: succeeded\nstarted: 2025-01-01T00:00:00Z\n---\n",
        )?;
    }
    write(
        vault.path(),
        "memory/jobs/many-runs/many-runs/run/2026/09/R-5001.md",
        "---\ntype: job-run\nworkflow: many-runs\njob: run\nstatus: failed\nstarted: 2026-09-20T00:00:00Z\n---\n",
    )?;
    let router = router_over(vault.path(), config.path()).await?;
    let (_, body) = get(
        &router,
        &format!("/{VAULT_NAME}/workflows/many-runs/many-runs/jobs/run"),
    )
    .await?;
    assert!(body.contains("R-5001"));
    assert!(body.contains("status-dot blocked"));
    Ok(())
}

#[tokio::test]
async fn run_now_on_a_job_that_is_not_registered_is_refused_and_starts_nothing() -> Result<(), BoxError> {
    for lifecycle in ["draft", "validated", "retired"] {
        let vault_dir = tempfile::tempdir()?;
        let config_dir = tempfile::tempdir()?;
        write(vault_dir.path(), "index.md", "# Root\n")?;
        write(vault_dir.path(), ".contextos/team.toml", support::jobs::TEAM)?;
        write_workflow(
            vault_dir.path(),
            "sync-crm-contacts",
            "name = \"Sync CRM contacts workflow\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = [\"contextos\"]\n",
            "workflow.rhai",
            "call_mcp(\"contextos\", \"vault_info\", #{});",
        )?;
        write_job(
            vault_dir.path(),
            "sync-crm-contacts",
            "nightly",
            "name = \"Nightly sync\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
        )?;
        set_state(vault_dir.path(), "sync-crm-contacts", "nightly", lifecycle, None)?;
        let router = router_over(vault_dir.path(), config_dir.path()).await?;

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/{VAULT_NAME}/workflows/sync-crm-contacts/sync-crm-contacts/jobs/nightly/runs"
                    ))
                    .body(Body::empty())?,
            )
            .await?;

        assert_eq!(response.status(), StatusCode::CONFLICT, "{lifecycle}");
        let body = to_bytes(response.into_body(), usize::MAX).await?;
        let body = String::from_utf8_lossy(&body);
        assert!(body.contains("workflow/job-not-runnable"), "{lifecycle}: {body}");
        assert!(
            body.contains(&format!("the job is {lifecycle}")),
            "the refusal names the state: {body}"
        );
        assert_eq!(
            claim_of(config_dir.path(), "sync-crm-contacts", "nightly")?.state()?,
            contextos_web::run_claim::ClaimState::Idle,
            "{lifecycle}: nothing was claimed"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_job_manifest_with_a_schedule_key_does_not_register_but_is_listed_and_cannot_run() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write_workflow(
        vault_dir.path(),
        "sync-crm-contacts",
        "name = \"Sync CRM contacts workflow\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = []\n",
        "workflow.rhai",
        "()",
    )?;
    write_job(
        vault_dir.path(),
        "sync-crm-contacts",
        "nightly",
        "name = \"Nightly sync\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nschedule = \"0 3 * * *\"\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(
        &router,
        &format!("/{VAULT_NAME}/workflows/sync-crm-contacts/sync-crm-contacts/jobs/nightly"),
    )
    .await?;

    assert_eq!(status, StatusCode::OK, "the job has a page, so it can be validated");
    assert!(body.contains("does not register"), "{body}");
    assert!(
        !body.contains("Run now"),
        "a job that does not register offers no run:\n{body}"
    );
    let run = post(
        &router,
        &format!("/{VAULT_NAME}/workflows/sync-crm-contacts/sync-crm-contacts/jobs/nightly/runs"),
    )
    .await?;
    assert_eq!(
        run,
        StatusCode::CONFLICT,
        "a job that authors its own schedule cannot be run"
    );
    Ok(())
}
