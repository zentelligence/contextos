//! A vault's workflows, jobs, and the actions an operator takes on a job, through the real router, a real `contextos-mcp`
//! session, and a real temporary vault: the list and its state filter, the workflow and job pages, and each action's answer
//! and its effect on `.contextos/jobs.toml`.

mod support;

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode};
use contextos_web::McpClientSet;
use contextos_web::job_lifecycle::{LifecycleChange, change_lifecycle};
use contextos_workflows::{Actor, JobFingerprint, JobLifecycle, JobState, JobsState, Transition};
use time::macros::datetime;
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT: &str = "contract-fixture";
const WORKFLOW: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"contextos\"]\n";
const SCRIPT: &str = "log_info(\"a\");";
const JOB: &str = "name = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";
const STATE_PATH: &str = ".contextos/jobs.toml";

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

struct Site {
    vault: tempfile::TempDir,
    _config: tempfile::TempDir,
    clients: Arc<McpClientSet>,
    router: Router,
}

async fn site() -> Result<Site, BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    write(vault.path(), "index.md", "# Root\n")?;
    write(vault.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write(
        vault.path(),
        "registry/workflows/ops/workflows.toml",
        "workflows = [\"sync\"]\n",
    )?;
    write(vault.path(), "registry/workflows/ops/sync/workflow.toml", WORKFLOW)?;
    write(vault.path(), "registry/workflows/ops/sync/sync.rhai", SCRIPT)?;
    write(vault.path(), "registry/workflows/ops/sync/jobs/nightly.toml", JOB)?;
    support::release_fixture::install_vault_releases(vault.path()).map_err(|error| error.to_string())?;
    let config_path = support::write_vault_config(config.path(), vault.path())?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    let router = contextos_web::build_router(
        Arc::clone(&clients),
        Some(config.path()),
        &config.path().join("web.toml"),
    );
    Ok(Site {
        vault,
        _config: config,
        clients,
        router,
    })
}

struct Answer {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Site {
    async fn send(&self, method: &str, path: &str, hx: bool, body: Option<&str>) -> Result<Answer, BoxError> {
        let mut request = Request::builder().method(method).uri(path);
        if hx {
            request = request.header("hx-request", "true");
        }
        let request = match body {
            Some(json) => request
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(json.to_owned()))?,
            None => request.body(Body::empty())?,
        };
        let response = self.router.clone().oneshot(request).await?;
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = to_bytes(response.into_body(), usize::MAX).await?;
        Ok(Answer {
            status,
            headers,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }

    async fn get(&self, path: &str) -> Result<Answer, BoxError> {
        self.send("GET", path, false, None).await
    }

    async fn put(&self, path: &str, body: Option<&str>) -> Result<Answer, BoxError> {
        self.send("PUT", path, true, body).await
    }

    async fn patch(&self, path: &str, body: &str) -> Result<Answer, BoxError> {
        self.send("PATCH", path, true, Some(body)).await
    }

    fn state(&self) -> Result<JobsState, BoxError> {
        let path = self.vault.path().join(STATE_PATH);
        if !path.exists() {
            return Ok(JobsState::default());
        }
        Ok(toml::from_str(&std::fs::read_to_string(path)?)?)
    }

    fn lifecycle(&self) -> Result<JobLifecycle, BoxError> {
        Ok(self.state()?.lifecycle_of("ops", "sync", "nightly"))
    }

    /// A completed dry run for the version the job has now, which registers a validated job.
    async fn dry_run_completed(&self) -> Result<(), BoxError> {
        let version: JobFingerprint = support::release_fixture::vault_version(self.vault.path(), "ops", JOB)
            .map_err(|error| error.to_string())?;
        let client = self.clients.get("contextos").ok_or("no primary server")?;
        change_lifecycle(
            &client,
            VAULT,
            LifecycleChange {
                package: "ops",
                workflow_slug: "sync",
                job_slug: "nightly",
                actor: Actor::DryRun,
                transition: Transition::DryRunCompleted(version),
                now: datetime!(2026-10-01 03:00:00 UTC),
            },
        )
        .await?;
        Ok(())
    }

    /// Validates through the page, completes a dry run, and sets a schedule through the page: an enabled job.
    async fn enable(&self) -> Result<(), BoxError> {
        self.put(&job_url("execute/validate"), None).await?;
        self.dry_run_completed().await?;
        self.patch(&job_url(""), "{\"schedule\": \"0 3 * * *\"}").await?;
        assert_eq!(self.lifecycle()?, JobLifecycle::Enabled);
        Ok(())
    }
}

fn job_url(tail: &str) -> String {
    if tail.is_empty() {
        format!("/{VAULT}/workflows/ops/sync/jobs/nightly")
    } else {
        format!("/{VAULT}/workflows/ops/sync/jobs/nightly/{tail}")
    }
}

// ---------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------

#[tokio::test]
async fn the_list_shows_each_workflow_with_its_jobs_and_a_filter_chip_per_state() -> Result<(), BoxError> {
    let site = site().await?;

    let page = site.get(&format!("/{VAULT}/workflows")).await?;

    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.body.contains("Sync") && page.body.contains("Nightly"),
        "{}",
        page.body
    );
    assert!(page.body.contains(&job_url("")), "{}", page.body);
    for chip in ["Needs attention", "draft", "suspended", "retired"] {
        assert!(page.body.contains(chip), "{chip}");
    }
    assert!(
        page.body.contains("href=\"/shared\""),
        "the shared workflows are one link away:\n{}",
        page.body
    );
    Ok(())
}

#[tokio::test]
async fn the_state_filter_shows_only_the_jobs_in_that_state_and_attention_means_drafts_and_suspended()
-> Result<(), BoxError> {
    let site = site().await?;

    let drafts = site.get(&format!("/{VAULT}/workflows?state=draft")).await?;
    let enabled = site.get(&format!("/{VAULT}/workflows?state=enabled")).await?;
    let attention = site.get(&format!("/{VAULT}/workflows?state=attention")).await?;

    assert!(drafts.body.contains("Nightly"));
    assert!(!enabled.body.contains("Nightly"), "{}", enabled.body);
    assert!(enabled.body.contains("No job matches this filter"), "{}", enabled.body);
    assert!(attention.body.contains("Nightly") && attention.body.contains("Needs attention"));
    Ok(())
}

#[tokio::test]
async fn an_unknown_state_filter_is_refused_and_not_treated_as_all() -> Result<(), BoxError> {
    let site = site().await?;

    let answer = site.get(&format!("/{VAULT}/workflows?state=sleeping")).await?;

    assert_eq!(answer.status, StatusCode::BAD_REQUEST);
    assert!(
        answer.body.contains("workflows/unknown-state-filter"),
        "{}",
        answer.body
    );
    Ok(())
}

#[tokio::test]
async fn an_htmx_request_for_the_list_gets_the_fragment_a_chip_swaps_in() -> Result<(), BoxError> {
    let site = site().await?;

    let fragment = site
        .send("GET", &format!("/{VAULT}/workflows?state=draft"), true, None)
        .await?;

    assert!(fragment.body.contains("id=\"workflows-screen\""), "{}", fragment.body);
    assert!(!fragment.body.contains("<html"), "{}", fragment.body);
    Ok(())
}

#[tokio::test]
async fn a_workflow_page_shows_its_script_timeout_servers_and_jobs() -> Result<(), BoxError> {
    let site = site().await?;

    let page = site.get(&format!("/{VAULT}/workflows/ops/sync")).await?;

    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.body.contains("sync.rhai") && page.body.contains("contextos"),
        "{}",
        page.body
    );
    assert!(page.body.contains(&job_url("")), "{}", page.body);
    let missing = site.get(&format!("/{VAULT}/workflows/no-such-workflow")).await?;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn a_job_page_shows_its_status_and_offers_only_what_its_state_allows() -> Result<(), BoxError> {
    let site = site().await?;

    let page = site.get(&job_url("")).await?;

    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.body.contains("id=\"job-status\"") && page.body.contains("Needs attention"),
        "{}",
        page.body
    );
    assert!(page.body.contains("execute/validate"), "{}", page.body);
    assert!(
        !page.body.contains("execute/suspend") && !page.body.contains("confirm-delete"),
        "{}",
        page.body
    );
    Ok(())
}

#[tokio::test]
async fn two_packages_holding_a_workflow_and_a_job_of_the_same_names_have_pages_and_state_of_their_own()
-> Result<(), BoxError> {
    let site = site().await?;
    write(
        site.vault.path(),
        "registry/workflows/crm/workflows.toml",
        "workflows = [\"sync\"]\n",
    )?;
    write(site.vault.path(), "registry/workflows/crm/sync/workflow.toml", WORKFLOW)?;
    write(site.vault.path(), "registry/workflows/crm/sync/sync.rhai", SCRIPT)?;
    write(site.vault.path(), "registry/workflows/crm/sync/jobs/nightly.toml", JOB)?;
    let crm_job = format!("/{VAULT}/workflows/crm/sync/jobs/nightly");

    let ops_page = site.get(&job_url("")).await?;
    let crm_page = site.get(&crm_job).await?;
    let list = site.get(&format!("/{VAULT}/workflows")).await?;
    site.put(&job_url("execute/validate"), None).await?;

    assert_eq!(ops_page.status, StatusCode::OK);
    assert_eq!(crm_page.status, StatusCode::OK);
    assert!(ops_page.body.contains(&job_url("runs")), "{}", ops_page.body);
    assert!(crm_page.body.contains(&format!("{crm_job}/runs")), "{}", crm_page.body);
    assert!(
        list.body.contains(&job_url("")) && list.body.contains(&crm_job),
        "both are listed, each at its own address: {}",
        list.body
    );
    let state = site.state()?;
    assert_ne!(state.lifecycle_of("ops", "sync", "nightly"), JobLifecycle::Draft);
    assert!(
        state.job("crm", "sync", "nightly").is_none(),
        "validating one leaves the other with nothing recorded"
    );
    Ok(())
}

#[tokio::test]
async fn the_old_jobs_urls_are_gone() -> Result<(), BoxError> {
    let site = site().await?;

    for path in [
        format!("/{VAULT}/jobs"),
        format!("/{VAULT}/jobs/"),
        format!("/{VAULT}/jobs/sync/nightly/"),
    ] {
        assert_eq!(site.get(&path).await?.status, StatusCode::NOT_FOUND, "{path}");
    }
    Ok(())
}

#[tokio::test]
async fn a_job_that_does_not_register_is_listed_as_needing_attention_and_can_be_validated() -> Result<(), BoxError> {
    let site = site().await?;
    write(
        site.vault.path(),
        "registry/workflows/ops/sync/jobs/broken.toml",
        "name = \"Broken\"\nowner = \"nobody\"\n",
    )?;

    let attention = site.get(&format!("/{VAULT}/workflows?state=attention")).await?;
    let validated = site
        .put(
            &format!("/{VAULT}/workflows/ops/sync/jobs/broken/execute/validate"),
            None,
        )
        .await?;

    assert!(
        attention.body.contains("broken") && attention.body.contains("does not register"),
        "{}",
        attention.body
    );
    assert_eq!(validated.status, StatusCode::OK);
    assert!(
        validated.body.contains("workflow/validation-failed"),
        "{}",
        validated.body
    );
    assert!(
        validated.body.contains("validation/owner-unresolved"),
        "every failure is listed:\n{}",
        validated.body
    );
    Ok(())
}

// ---------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------

#[tokio::test]
async fn validating_from_the_page_records_the_result_and_swaps_in_the_new_status() -> Result<(), BoxError> {
    let site = site().await?;

    let answer = site.put(&job_url("execute/validate"), None).await?;

    assert_eq!(answer.status, StatusCode::OK);
    assert!(
        answer.body.contains("Validation passed") && answer.body.contains("id=\"job-status\""),
        "{}",
        answer.body
    );
    assert!(!answer.body.contains("<html"), "{}", answer.body);
    assert_eq!(site.lifecycle()?, JobLifecycle::Validated);
    Ok(())
}

#[tokio::test]
async fn a_job_goes_from_validated_to_enabled_suspended_resumed_retired_reactivated_and_deleted_through_the_page()
-> Result<(), BoxError> {
    let site = site().await?;
    site.enable().await?;
    assert!(
        site.state()?
            .job("ops", "sync", "nightly")
            .is_some_and(JobState::first_run_pending)
    );

    let suspended = site
        .put(
            &job_url("execute/suspend"),
            Some("{\"reason\": \"the upstream API is down\"}"),
        )
        .await?;
    assert!(
        suspended.body.contains("the upstream API is down"),
        "{}",
        suspended.body
    );
    assert_eq!(site.lifecycle()?, JobLifecycle::Suspended);

    let resumed = site.put(&job_url("execute/resume"), None).await?;
    assert!(resumed.body.contains("The job is now enabled"), "{}", resumed.body);
    assert_eq!(site.lifecycle()?, JobLifecycle::Enabled);

    site.put(&job_url("execute/retire"), None).await?;
    assert_eq!(site.lifecycle()?, JobLifecycle::Retired);

    let confirm = site.get(&job_url("confirm-delete")).await?;
    assert!(confirm.body.contains("hx-delete"), "{}", confirm.body);

    site.put(&job_url("execute/reactivate"), None).await?;
    assert_eq!(site.lifecycle()?, JobLifecycle::Draft);

    site.enable().await?;
    site.put(&job_url("execute/retire"), None).await?;
    let deleted = site.send("DELETE", &job_url(""), true, None).await?;
    assert_eq!(deleted.status, StatusCode::OK);
    assert_eq!(
        deleted.headers.get("hx-redirect").and_then(|value| value.to_str().ok()),
        Some(format!("/{VAULT}/workflows/ops/sync").as_str())
    );
    assert!(site.state()?.job("ops", "sync", "nightly").is_none());
    Ok(())
}

#[tokio::test]
async fn a_refused_action_is_an_alert_with_its_code_and_what_would_allow_it_and_writes_nothing() -> Result<(), BoxError>
{
    let site = site().await?;

    let answer = site
        .put(&job_url("execute/suspend"), Some("{\"reason\": \"held\"}"))
        .await?;

    assert_eq!(answer.status, StatusCode::OK, "so HTMX swaps it in");
    assert!(
        answer.body.contains("role=\"alert\"") && answer.body.contains("workflow/transition-refused"),
        "{}",
        answer.body
    );
    assert!(!site.vault.path().join(STATE_PATH).exists());
    Ok(())
}

#[tokio::test]
async fn a_schedule_sent_to_the_job_enables_it_and_a_bad_one_is_an_alert() -> Result<(), BoxError> {
    let site = site().await?;
    site.put(&job_url("execute/validate"), None).await?;
    site.dry_run_completed().await?;

    let bad = site.patch(&job_url(""), "{\"schedule\": \"every tuesday\"}").await?;
    assert!(
        bad.body.contains("role=\"alert\"") && bad.body.contains("every tuesday"),
        "{}",
        bad.body
    );
    assert_eq!(site.lifecycle()?, JobLifecycle::Registered);

    let good = site.patch(&job_url(""), "{\"schedule\": \"0 3 * * *\"}").await?;
    assert!(
        good.body.contains("The job is now enabled") && good.body.contains("0 3 * * *"),
        "{}",
        good.body
    );

    let cleared = site.patch(&job_url(""), "{\"schedule\": \"\"}").await?;
    assert!(cleared.body.contains("The job is now registered"), "{}", cleared.body);
    Ok(())
}

#[tokio::test]
async fn an_update_that_names_both_things_or_neither_is_an_alert_and_changes_nothing() -> Result<(), BoxError> {
    let site = site().await?;

    for body in ["{}", "{\"favourite\": true, \"schedule\": \"0 3 * * *\"}", "not json"] {
        let answer = site.patch(&job_url(""), body).await?;
        assert_eq!(answer.status, StatusCode::OK, "{body}");
        assert!(
            answer.body.contains("role=\"alert\"") && answer.body.contains("exactly one"),
            "{body}:\n{}",
            answer.body
        );
    }
    assert!(!site.vault.path().join(STATE_PATH).exists());
    Ok(())
}

#[tokio::test]
async fn a_command_or_a_job_that_does_not_exist_is_not_found() -> Result<(), BoxError> {
    let site = site().await?;

    let command = site.put(&job_url("execute/explode"), None).await?;
    let job = site
        .put(
            &format!("/{VAULT}/workflows/ops/sync/jobs/no-such-job/execute/validate"),
            None,
        )
        .await?;
    let delete = site
        .send(
            "DELETE",
            &format!("/{VAULT}/workflows/ops/sync/jobs/no-such-job"),
            true,
            None,
        )
        .await?;

    assert_eq!(command.status, StatusCode::NOT_FOUND);
    assert_eq!(job.status, StatusCode::NOT_FOUND);
    assert_eq!(delete.status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn the_delete_confirmation_is_for_a_job_that_exists_in_a_vault_that_exists() -> Result<(), BoxError> {
    let site = site().await?;

    let real = site.get(&job_url("confirm-delete")).await?;
    let no_job = site
        .get(&format!("/{VAULT}/workflows/ops/sync/jobs/no-such-job/confirm-delete"))
        .await?;
    let no_vault = site
        .get("/%2Fevil.com/workflows/ops/sync/jobs/nightly/confirm-delete")
        .await?;
    let odd_job = site
        .get(&format!("/{VAULT}/workflows/ops/sync/jobs/Night%3Fly/confirm-delete"))
        .await?;

    assert_eq!(real.status, StatusCode::OK);
    assert_eq!(no_job.status, StatusCode::NOT_FOUND);
    assert_eq!(no_vault.status, StatusCode::NOT_FOUND);
    assert_eq!(odd_job.status, StatusCode::NOT_FOUND);
    assert!(!no_vault.body.contains("evil.com"), "{}", no_vault.body);
    Ok(())
}

#[tokio::test]
async fn a_state_file_that_cannot_be_read_is_an_error_and_not_a_page_of_drafts() -> Result<(), BoxError> {
    let site = site().await?;
    site.put(&job_url("execute/validate"), None).await?;
    write(site.vault.path(), STATE_PATH, "workflows = 7\n")?;

    let list = site.get(&format!("/{VAULT}/workflows")).await?;
    let page = site.get(&job_url("")).await?;

    assert_eq!(list.status, StatusCode::BAD_GATEWAY, "{}", list.body);
    assert_eq!(page.status, StatusCode::BAD_GATEWAY, "{}", page.body);
    assert!(!page.body.contains("Needs attention"), "{}", page.body);
    Ok(())
}

#[tokio::test]
async fn run_now_is_offered_only_where_a_run_by_hand_is_allowed() -> Result<(), BoxError> {
    let site = site().await?;

    let draft = site.get(&job_url("")).await?;
    let draft_list = site.get(&format!("/{VAULT}/workflows")).await?;
    site.put(&job_url("execute/validate"), None).await?;
    site.dry_run_completed().await?;
    let registered = site.get(&job_url("")).await?;
    let registered_list = site.get(&format!("/{VAULT}/workflows")).await?;

    assert!(
        !draft.body.contains("Run now") && !draft_list.body.contains("Run now"),
        "a draft cannot be run"
    );
    assert!(
        registered.body.contains("Run now") && registered_list.body.contains("Run now"),
        "a registered job can"
    );
    Ok(())
}

#[tokio::test]
async fn validating_a_retired_job_from_its_page_is_refused_and_not_offered() -> Result<(), BoxError> {
    let site = site().await?;
    site.enable().await?;
    site.put(&job_url("execute/retire"), None).await?;

    let page = site.get(&job_url("")).await?;
    let answer = site.put(&job_url("execute/validate"), None).await?;

    assert!(!page.body.contains("execute/validate"), "{}", page.body);
    assert!(
        answer.body.contains("role=\"alert\"") && answer.body.contains("retired"),
        "{}",
        answer.body
    );
    Ok(())
}
