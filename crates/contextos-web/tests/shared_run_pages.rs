//! Starting, following, and stopping a shared job's runs from its page, through the real router over a real package
//! store, real job files, the real host state file and claims directory, and a real `contextos-mcp` session.

mod support;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use contextos_web::routes::shared_workflows::{SharedRoots, SharedRoutesState};
use contextos_web::run_claim::{ClaimSlot, ClaimState};
use contextos_web::shared_claim::SharedClaim;
use contextos_web::shared_router;
use support::BoxError;
use support::shared::{SharedFixture, key};
use tower::ServiceExt as _;

/// A job file that says authoring is finished, so the automatic pass validates it.
const JOB: &str = "ready = true\nname = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";
const JOB_URL: &str = "/server/workflows/acme/sync/jobs/nightly";

struct Answer {
    status: StatusCode,
    body: String,
}

struct Site {
    fixture: SharedFixture,
    router: Router,
}

/// A script that makes the one call its scenario declares and recovers from the scenario's error, so a dry run of it
/// completes with a warning and needs no server to supply a schema.
const TRY_SCRIPT: &str =
    r#"try { call_mcp("contextos", "vault_info", #{}); } catch (e) { flag_degraded("the read failed"); }"#;
const EFFECT: &str = "\n[[workflow.permitted_effects]]\nmcp_server_name = \"contextos\"\ntool = \"vault_info\"\n";
const SCENARIO: &str =
    "[[call]]\nserver = \"contextos\"\ntool = \"vault_info\"\nkind = \"error\"\nerror = \"offline\"\n";

/// A registered shared job whose script is `script`, served through the real router.
async fn site(script: &str) -> Result<Site, BoxError> {
    site_with(script, "").await
}

/// As [`site`], with `effects` appended to the workflow's manifest entry.
async fn site_with(script: &str, effects: &str) -> Result<Site, BoxError> {
    let fixture = SharedFixture::new(JOB).await?;
    fixture.install_with("1.0.0", support::shared::DIGEST, script, effects)?;
    fixture.register().await?;
    let state = SharedRoutesState::new(
        SharedRoots {
            store_root: fixture.store_root(),
            jobs_root: fixture.jobs_root(),
            runs_root: fixture.runs_root(),
            state_root: fixture.root.path().join("state"),
            claims_root: fixture.claims_root(),
        },
        Arc::clone(&fixture.clients),
        Arc::new(fixture.root.path().join("web.toml")),
    );
    Ok(Site {
        router: shared_router(state),
        fixture,
    })
}

impl Site {
    async fn send(&self, method: &str, path: &str, body: Option<&str>) -> Result<Answer, BoxError> {
        let request = Request::builder().method(method).uri(path).header("hx-request", "true");
        let request = match body {
            Some(json) => request
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(json.to_owned()))?,
            None => request.body(Body::empty())?,
        };
        let response = self.router.clone().oneshot(request).await?;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await?;
        Ok(Answer {
            status,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }

    async fn start(&self, body: Option<&str>) -> Result<Answer, BoxError> {
        self.send("POST", &format!("{JOB_URL}/runs"), body).await
    }

    fn slot(&self) -> Result<ClaimSlot, BoxError> {
        Ok(key().claim_slot(&self.fixture.claims_root())?)
    }

    /// The text of every run record of the job.
    fn records(&self) -> Vec<String> {
        let mut found = Vec::new();
        let mut pending = vec![self.fixture.runs_root().join("acme/sync/nightly")];
        while let Some(directory) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|extension| extension == "md")
                    && let Ok(text) = std::fs::read_to_string(&path)
                {
                    found.push(text);
                }
            }
        }
        found
    }

    fn ship_scenario(&self) -> std::io::Result<()> {
        support::shared::write(
            &self.fixture.store_root().join("acme/1.0.0/sync/tests/offline.toml"),
            SCENARIO,
        )?;
        // The release stays whole: its manifest lists the scenario it now ships.
        support::release_fixture::seal_io(&self.fixture.store_root().join("acme/1.0.0"))
    }
}

/// Waits, against a real deadline and never a fixed pause, until `ready` holds.
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

fn has_status(records: &[String], status: &str) -> bool {
    records.iter().any(|text| text.contains(&format!("status: {status}")))
}

#[tokio::test]
async fn a_registered_shared_jobs_page_offers_run_now_and_a_dry_run() -> Result<(), BoxError> {
    let site = site("log_info(\"a\");").await?;

    let page = site.send("GET", JOB_URL, None).await?;

    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("id=\"job-runs\""), "{}", page.body);
    assert!(
        page.body.contains("Run now") && page.body.contains("Dry run"),
        "{}",
        page.body
    );
    Ok(())
}

#[tokio::test]
async fn run_now_starts_a_live_run_the_page_sees_end() -> Result<(), BoxError> {
    let site = site("log_info(\"a\");").await?;

    let answer = site.start(None).await?;

    assert_eq!(answer.status, StatusCode::OK);
    assert!(
        answer.body.contains("The live run was started.") && answer.body.contains("role=\"status\""),
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
    let region = site.send("GET", &format!("{JOB_URL}/runs"), None).await?;
    assert!(region.body.contains("succeeded"), "{}", region.body);
    assert!(
        !region.body.contains("Cancel run") && !region.body.contains("hx-trigger"),
        "{}",
        region.body
    );
    Ok(())
}

#[tokio::test]
async fn run_now_while_a_run_of_the_job_is_active_is_an_alert_and_starts_nothing() -> Result<(), BoxError> {
    let site = site("log_info(\"a\");").await?;
    let running = SharedClaim::acquire(&site.fixture.claims_root(), &key())?;

    let answer = site.start(None).await?;

    assert_eq!(answer.status, StatusCode::OK);
    assert!(
        answer.body.contains("role=\"alert\"") && answer.body.contains("workflow/job-busy"),
        "{}",
        answer.body
    );
    assert!(site.records().is_empty(), "nothing was started");
    drop(running);
    Ok(())
}

#[tokio::test]
async fn a_dry_run_is_recorded_as_one_and_does_not_wait_for_a_live_run() -> Result<(), BoxError> {
    let site = site_with(TRY_SCRIPT, EFFECT).await?;
    site.ship_scenario()?;
    let running = SharedClaim::acquire(&site.fixture.claims_root(), &key())?;

    let answer = site.start(Some(r#"{"dry_run": true}"#)).await?;

    assert!(answer.body.contains("The dry run was started."), "{}", answer.body);
    eventually("the dry run to end", || {
        site.records()
            .iter()
            .any(|text| text.contains("dry_run: true") && text.contains("status: degraded"))
    })
    .await?;
    drop(running);
    Ok(())
}

#[tokio::test]
async fn a_dry_run_that_could_not_start_is_told_to_the_page_when_it_ends() -> Result<(), BoxError> {
    let site = site("log_info(\"a\");").await?;

    let answer = site.start(Some(r#"{"dry_run": true}"#)).await?;
    assert!(answer.body.contains("The dry run was started."), "{}", answer.body);

    // The package ships no scenario, so the run fails before it has a record, and only the page's own table remembers it.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let told = loop {
        let region = site.send("GET", &format!("{JOB_URL}/runs"), None).await?;
        if region.body.contains("Last run started here") {
            break region.body;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("the page was never told how the dry run ended: {}", region.body).into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    assert!(told.contains("role=\"alert\"") && told.contains("scenario"), "{told}");
    assert!(
        site.records().is_empty(),
        "a run that failed before it began has no record"
    );
    Ok(())
}

#[tokio::test]
async fn an_active_shared_run_is_seen_and_cancelled_from_the_page() -> Result<(), BoxError> {
    let site = site("loop { }").await?;
    site.start(None).await?;
    let slot = site.slot()?;
    let mut active = None;
    eventually("the run to say it is active", || {
        if let Ok(ClaimState::Held(Some(run))) = slot.state() {
            active = Some(run);
            true
        } else {
            false
        }
    })
    .await?;
    let run_id = active.ok_or("the run never said it was active")?.run_id;

    let region = site.send("GET", &format!("{JOB_URL}/runs"), None).await?;
    assert!(region.body.contains("Cancel run"), "{}", region.body);
    let cancelled = site
        .send("PUT", &format!("{JOB_URL}/runs/R-{run_id}/execute/cancel"), None)
        .await?;

    assert_eq!(cancelled.status, StatusCode::OK);
    assert!(cancelled.body.contains("asked to stop"), "{}", cancelled.body);
    eventually("the run to end cancelled", || has_status(&site.records(), "cancelled")).await?;
    eventually("the claim to be released", || {
        slot.state().is_ok_and(|state| state == ClaimState::Idle)
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn the_list_offers_run_now_for_a_registered_shared_job_and_its_form_post_runs_it() -> Result<(), BoxError> {
    let site = site("log_info(\"a\");").await?;

    let list = site.send("GET", "/server/workflows", None).await?;
    let form = site
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("{JOB_URL}/runs"))
                .body(Body::empty())?,
        )
        .await?;

    assert!(
        list.body.contains(&format!("action=\"{JOB_URL}/runs\"")) && list.body.contains("Run now"),
        "{}",
        list.body
    );
    assert!(form.status().is_redirection(), "{:?}", form.status());
    assert_eq!(
        form.headers().get("location").and_then(|value| value.to_str().ok()),
        Some(JOB_URL)
    );
    eventually("the run to end", || has_status(&site.records(), "succeeded")).await?;
    Ok(())
}
