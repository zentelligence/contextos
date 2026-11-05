//! A suspended job's page and its schedule action, through the real router, a real `contextos` session, and a real
//! temporary vault: the page shows the structured suspension (code, who, when, the run that failed, and why), and
//! replacing the schedule of a suspended job changes nothing about when or by whom it was suspended.

mod support;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use contextos_workflows::Actor;
use support::jobs::{JobVault, Spec};
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const JOB_URL: &str = "/contract-fixture/workflows/ops/sync/jobs/nightly";
const RUN_RECORD: &str = "memory/jobs/ops/sync/nightly/2026/10/01/R-4.md";

/// A job suspended by the system at 03:00 UTC on 1 October because its first live run, R-4, failed.
fn suspended_by_a_failed_first_run(spec: &Spec<'_>) -> String {
    format!(
        "{}\n[ops.sync.nightly.suspension]\ncode = \"workflow/probation-failed\"\nby = \"system\"\n\
         at = \"2026-10-01T03:00:00Z\"\nreason = \"the first live run failed\"\nrun = 4\n\
         failure = \"workflow/script-error\"\n",
        support::jobs::suspended_state(spec)
            .replace(
                "lifecycle_changed = \"2026-09-01T00:00:00Z\"",
                "lifecycle_changed = \"2026-10-01T03:00:00Z\""
            )
            .replace("lifecycle_actor = \"operator\"", "lifecycle_actor = \"system\"")
    )
}

fn site(vault: &JobVault) -> Router {
    contextos_web::build_router(
        vault.clients.clone(),
        Some(vault.config.path()),
        &vault.config.path().join("web.toml"),
    )
}

async fn send(router: &Router, request: Request<Body>) -> Result<(StatusCode, String), BoxError> {
    let response = router.clone().oneshot(request).await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

async fn fixture() -> Result<JobVault, BoxError> {
    let mut spec = Spec::default();
    let state = suspended_by_a_failed_first_run(&spec);
    spec.state = Some(&state);
    let vault = JobVault::open(&spec).await?;
    vault.write(RUN_RECORD, "---\ntype: job-run\nid: R-4\nstatus: failed\n---\n")?;
    Ok(vault)
}

/// The page's suspension block: the row that is headed `Suspended`, up to the end of that row.
fn suspension_block(page: &str) -> Option<&str> {
    let start = page.find("<dt>Suspended</dt>")?;
    let rest = &page[start..];
    Some(&rest[..rest.find("</div>").unwrap_or(rest.len())])
}

#[tokio::test]
async fn a_suspended_jobs_page_shows_its_code_actor_time_run_link_and_reason() -> Result<(), BoxError> {
    let vault = fixture().await?;
    let router = site(&vault);

    let (status, page) = send(&router, Request::builder().uri(JOB_URL).body(Body::empty())?).await?;

    assert_eq!(status, StatusCode::OK, "{page}");
    let block = suspension_block(&page).ok_or_else(|| format!("the page has no suspension block: {page}"))?;
    assert!(
        block.contains("workflow/probation-failed"),
        "the code, in the block: {block}"
    );
    assert!(block.contains("by system"), "who suspended it, in the block: {block}");
    assert!(
        block.contains("<time>2026-10-01T03:00:00Z</time>"),
        "when, as the page shows every time so the operator's zone applies, in the block: {block}"
    );
    assert!(
        block.contains("the first live run failed"),
        "the reason, in the block: {block}"
    );
    let link = block
        .match_indices("href=\"")
        .map(|(at, _)| block[at + 6..].split('"').next().unwrap_or_default())
        .find(|href| href.contains("memory/jobs") && href.contains("R-4"));
    assert!(
        link.is_some(),
        "a link to the record of the run that failed, in the block: {block}"
    );
    Ok(())
}

#[tokio::test]
async fn replacing_a_suspended_jobs_schedule_from_its_page_keeps_when_and_by_whom_it_was_suspended()
-> Result<(), BoxError> {
    let vault = fixture().await?;
    let router = site(&vault);
    let before = vault.state()?;
    let suspension = before
        .job("ops", "sync", "nightly")
        .and_then(|job| job.suspension())
        .cloned()
        .ok_or("the fixture stores a suspension")?;

    let (status, body) = send(
        &router,
        Request::builder()
            .method("PATCH")
            .uri(JOB_URL)
            .header("hx-request", "true")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from("{\"schedule\": \"0 4 * * *\"}"))?,
    )
    .await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    let after = vault.state()?;
    let job = after.job("ops", "sync", "nightly").ok_or("the job is stored")?;
    assert_eq!(job.schedule(), Some("0 4 * * *"), "the new schedule is stored");
    assert_eq!(vault.lifecycle()?, "suspended");
    assert_eq!(job.lifecycle_actor(), Some(Actor::System), "who suspended it");
    assert_eq!(
        job.lifecycle_changed(),
        before
            .job("ops", "sync", "nightly")
            .and_then(contextos_workflows::JobState::lifecycle_changed),
        "when it was suspended"
    );
    assert_eq!(
        job.suspension(),
        Some(&suspension),
        "the suspension record is unchanged"
    );
    Ok(())
}

#[tokio::test]
async fn a_hand_edited_suspension_with_a_huge_reason_never_makes_an_unbounded_page() -> Result<(), BoxError> {
    let mut spec = Spec::default();
    let huge = "y".repeat(256 * 1024);
    let state = suspended_by_a_failed_first_run(&spec).replace("the first live run failed", &huge);
    spec.state = Some(&state);
    let vault = JobVault::open(&spec).await?;
    let router = site(&vault);

    let (status, page) = send(&router, Request::builder().uri(JOB_URL).body(Body::empty())?).await?;

    assert_eq!(status, StatusCode::OK);
    let block = suspension_block(&page).ok_or("the page has no suspension block")?;
    assert!(
        block.len() < 2048,
        "the reason was cut when it was read: {} bytes",
        block.len()
    );
    assert!(
        page.len() < 128 * 1024,
        "the whole page stays bounded: {} bytes",
        page.len()
    );
    Ok(())
}
