//! Starting, following, and stopping a vault job's runs from its page, through the real router over a real `contextos-mcp`
//! session, a real vault, and a real claims directory: what each request answers with, what it leaves in the vault, and what
//! a run another process holds does to the page.

mod support;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode};
use contextos_web::McpClientSet;
use contextos_web::run_claim::{ClaimSlot, ClaimState, RunClaim};
use contextos_web::vault_runs::claim_slot;
use support::BoxError;
use tower::ServiceExt as _;

const VAULT_NAME: &str = "contract-fixture";
const JOB_URL: &str = "/contract-fixture/workflows/ops/sync/jobs/nightly";
const WORKFLOW: &str = "name = \"Sync\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = [\"contextos\"]\n";

struct Answer {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

struct Site {
    vault: tempfile::TempDir,
    config: tempfile::TempDir,
    router: Router,
}

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

/// A vault whose one job is in `lifecycle` and whose script is `script`, served through the real router.
async fn site(script: &str, lifecycle: &str) -> Result<Site, BoxError> {
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
    write(vault.path(), "registry/workflows/ops/sync/workflow.rhai", script)?;
    write(
        vault.path(),
        "registry/workflows/ops/sync/jobs/nightly.toml",
        "name = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    write(
        vault.path(),
        ".contextos/jobs.toml",
        &format!("[ops.sync.nightly]\nlifecycle = \"{lifecycle}\"\n"),
    )?;
    write(
        vault.path(),
        "registry/workflows/ops/sync/tests/happy.toml",
        "[[call]]\nserver = \"contextos\"\ntool = \"fs_read_text_file\"\nkind = \"valid\"\nresponse = { content = \"mocked\" }\n",
    )?;
    support::release_fixture::install_vault_releases(vault.path()).map_err(|error| error.to_string())?;
    let config_path = support::write_vault_config(config.path(), vault.path())?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    let router = contextos_web::build_router_in(
        clients,
        Some(config.path()),
        &config.path().join("web.toml"),
        &contextos_web::config::WorkflowsConfig::default(),
        Some(&contextos_web::package_store::AppDataRoots::from(config.path())),
    );
    Ok(Site { vault, config, router })
}

impl Site {
    async fn send(&self, method: &str, path: &str, htmx: bool, body: Option<&str>) -> Result<Answer, BoxError> {
        let mut request = Request::builder().method(method).uri(path);
        if htmx {
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

    async fn start(&self, body: Option<&str>) -> Result<Answer, BoxError> {
        self.send("POST", &format!("{JOB_URL}/runs"), true, body).await
    }

    fn slot(&self) -> Result<ClaimSlot, BoxError> {
        Ok(claim_slot(
            &support::host(self.config.path()),
            VAULT_NAME,
            "ops",
            "sync",
            "nightly",
        )?)
    }

    /// The text of every run record of the job.
    fn records(&self) -> Vec<String> {
        let mut found = Vec::new();
        let mut pending = vec![self.vault.path().join("memory/jobs/ops/sync/nightly")];
        while let Some(directory) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if let Ok(text) = std::fs::read_to_string(&path) {
                    found.push(text);
                }
            }
        }
        found
    }
}

async fn eventually(what: &str, mut ready: impl FnMut() -> bool) -> Result<(), BoxError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while !ready() {
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("timed out waiting for {what}").into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Ok(())
}

/// How a run is named in the page and in the address that asks it to stop.
fn label(number: u64) -> String {
    format!("R-{number}")
}

fn has_status(records: &[String], status: &str) -> bool {
    records.iter().any(|text| text.contains(&format!("status: {status}")))
}

#[tokio::test]
async fn a_registered_jobs_page_offers_run_now_and_a_dry_run_in_a_region_of_their_own() -> Result<(), BoxError> {
    let site = site("()", "registered").await?;

    let page = site.send("GET", JOB_URL, true, None).await?;

    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("id=\"job-runs\""), "{}", page.body);
    assert!(
        page.body.contains("Run now") && page.body.contains("Dry run"),
        "{}",
        page.body
    );
    assert!(!page.body.contains("hx-trigger"), "nothing is running: {}", page.body);
    Ok(())
}

#[tokio::test]
async fn run_now_starts_a_live_run_that_the_page_can_see_end() -> Result<(), BoxError> {
    let site = site("()", "registered").await?;

    let answer = site.start(None).await?;

    assert_eq!(answer.status, StatusCode::OK);
    assert!(
        answer.body.contains(&format!("The live run {} was started.", label(1)))
            && answer.body.contains("role=\"status\""),
        "{}",
        answer.body
    );
    eventually("the run to end", || has_status(&site.records(), "succeeded")).await?;
    // The run finishes its record while it still holds the claim, and only then lets go, so the page shows the run ended
    // once the claim is free.
    let slot = site.slot()?;
    eventually("the claim to be released", || {
        slot.state().is_ok_and(|state| state == ClaimState::Idle)
    })
    .await?;
    let region = site.send("GET", &format!("{JOB_URL}/runs"), true, None).await?;
    assert_eq!(region.status, StatusCode::OK);
    assert!(
        region.body.contains(&label(1)) && region.body.contains("succeeded"),
        "{}",
        region.body
    );
    assert!(
        !region.body.contains("Cancel run") && !region.body.contains("hx-trigger"),
        "an ended run is neither cancellable nor followed: {}",
        region.body
    );
    Ok(())
}

#[tokio::test]
async fn run_now_while_a_run_of_the_job_is_active_is_an_alert_and_starts_nothing() -> Result<(), BoxError> {
    let site = site("()", "registered").await?;
    let running = RunClaim::acquire(&site.slot()?)?;

    let answer = site.start(None).await?;

    assert_eq!(
        answer.status,
        StatusCode::OK,
        "an HTMX swap needs a 200 to happen at all"
    );
    assert!(
        answer.body.contains("role=\"alert\"") && answer.body.contains("workflow/job-busy"),
        "{}",
        answer.body
    );
    assert!(
        answer.body.contains("An active run in progress") && answer.body.contains("disabled"),
        "the button is disabled and says why: {}",
        answer.body
    );
    assert!(site.records().is_empty(), "nothing was started");
    drop(running);
    Ok(())
}

#[tokio::test]
async fn a_dry_run_starts_while_a_live_run_is_active_and_is_recorded_as_a_dry_run() -> Result<(), BoxError> {
    let site = site(
        "call_mcp(\"contextos\", \"fs_read_text_file\", #{ path: \"missing.md\" });",
        "registered",
    )
    .await?;
    let running = RunClaim::acquire(&site.slot()?)?;

    let answer = site.start(Some(r#"{"dry_run": true}"#)).await?;

    assert_eq!(answer.status, StatusCode::OK);
    assert!(
        answer.body.contains(&format!("The dry run {} was started.", label(1))),
        "{}",
        answer.body
    );
    eventually("the dry run to end", || {
        site.records()
            .iter()
            .any(|text| text.contains("dry_run: true") && text.contains("status: succeeded"))
    })
    .await?;
    drop(running);
    Ok(())
}

#[tokio::test]
async fn an_active_run_is_seen_and_can_be_cancelled_from_the_page() -> Result<(), BoxError> {
    let site = site("loop { }", "registered").await?;
    let started = site.start(None).await?;
    assert!(started.body.contains("was started"), "{}", started.body);
    let slot = site.slot()?;
    eventually("the run to say it is active", || {
        matches!(slot.state(), Ok(ClaimState::Held(Some(_))))
    })
    .await?;

    let region = site.send("GET", &format!("{JOB_URL}/runs"), true, None).await?;
    assert!(
        region.body.contains("Cancel run") && region.body.contains("hx-trigger=\"every 3s\""),
        "{}",
        region.body
    );
    let cancelled = site
        .send(
            "PUT",
            &format!("{JOB_URL}/runs/{}/execute/cancel", label(1)),
            true,
            None,
        )
        .await?;

    assert_eq!(cancelled.status, StatusCode::OK);
    assert!(
        cancelled.body.contains(&format!("{} was asked to stop", label(1))),
        "{}",
        cancelled.body
    );
    eventually("the run to end cancelled", || has_status(&site.records(), "cancelled")).await?;
    eventually("the claim to be released", || {
        slot.state().is_ok_and(|state| state == ClaimState::Idle)
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn asking_a_run_that_is_not_active_to_stop_is_an_alert_and_a_bad_run_number_is_not_found() -> Result<(), BoxError>
{
    let site = site("()", "registered").await?;

    let not_active = site
        .send(
            "PUT",
            &format!("{JOB_URL}/runs/{}/execute/cancel", label(9)),
            true,
            None,
        )
        .await?;
    let bad = site
        .send("PUT", &format!("{JOB_URL}/runs/nine/execute/cancel"), true, None)
        .await?;

    assert_eq!(not_active.status, StatusCode::OK);
    assert!(
        not_active.body.contains("role=\"alert\"") && not_active.body.contains(&format!("{} is not active", label(9))),
        "{}",
        not_active.body
    );
    assert_eq!(bad.status, StatusCode::NOT_FOUND);
    assert!(!site.slot()?.cancel_path().exists(), "nothing was written");
    Ok(())
}

#[tokio::test]
async fn a_form_post_that_is_not_htmx_starts_the_run_and_sends_the_browser_to_the_page() -> Result<(), BoxError> {
    let site = site("()", "registered").await?;

    let answer = site.send("POST", &format!("{JOB_URL}/runs"), false, None).await?;

    assert!(answer.status.is_redirection(), "{:?}", answer.status);
    assert_eq!(
        answer.headers.get("location").and_then(|value| value.to_str().ok()),
        Some(JOB_URL)
    );
    eventually("the run to end", || has_status(&site.records(), "succeeded")).await?;
    Ok(())
}

#[tokio::test]
async fn the_runs_fragment_is_for_htmx_and_a_browser_that_asks_for_it_is_sent_to_the_page() -> Result<(), BoxError> {
    let site = site("()", "registered").await?;

    let browser = site.send("GET", &format!("{JOB_URL}/runs"), false, None).await?;

    assert!(browser.status.is_redirection());
    assert_eq!(
        browser.headers.get("location").and_then(|value| value.to_str().ok()),
        Some(JOB_URL)
    );
    Ok(())
}

#[tokio::test]
async fn a_lifecycle_action_answers_with_the_runs_region_beside_the_status() -> Result<(), BoxError> {
    let site = site("()", "registered").await?;

    let answer = site
        .send("PUT", &format!("{JOB_URL}/execute/validate"), true, None)
        .await?;

    assert_eq!(answer.status, StatusCode::OK);
    assert!(
        answer.body.contains("id=\"job-status\"")
            && answer.body.contains("id=\"job-runs\"")
            && answer.body.contains("hx-swap-oob"),
        "{}",
        answer.body
    );
    Ok(())
}
