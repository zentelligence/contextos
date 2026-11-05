//! A vault job's page shows where the job sits among releases and its chain: the release it names and whether that is
//! current, previous, future, or retired, the job it succeeded and the job that succeeded it, a link that no longer holds,
//! why a job file cannot be used, and the runs of the whole chain together, each marked with its job and release.

mod support;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request};
use support::jobs::{JobVault, Spec, VAULT_NAME};
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const SUCCESSOR: &str =
    "name = \"Nightly v2\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"2.0.0\"\npredecessor = \"nightly\"\n";
const CHAIN_STATE: &str = "[ops.sync]\ncurrent = \"2.0.0\"\nprevious = \"1.0.0\"\n\n\
    [ops.sync.nightly]\nlifecycle = \"retired\"\nidentity = \"job-a\"\n\n\
    [ops.sync.nightly-v2]\nlifecycle = \"enabled\"\nidentity = \"job-b\"\n\n\
    [ops.sync.nightly-v2.link]\nidentity = \"job-a\"\nretired = false\n";

fn record(status: &str, started: &str, version: &str) -> String {
    format!("---\ntype: job-run\nstatus: {status}\ntrigger: scheduled\nstarted: {started}\nversion: {version}\n---\n")
}

/// A vault whose job `nightly` was succeeded by `nightly-v2`, each with a run record, with `state` as its job state.
async fn chain(state: &str) -> Result<JobVault, BoxError> {
    let vault = JobVault::open(&Spec {
        state: Some(state),
        ..Spec::default()
    })
    .await?;
    vault.write("registry/workflows/ops/sync/jobs/nightly-v2.toml", SUCCESSOR)?;
    vault.install_releases()?;
    vault.write(
        "memory/jobs/ops/sync/nightly/2026/11/R-1.md",
        &record("succeeded", "2026-11-01T03:00:00Z", "1.0.0"),
    )?;
    vault.write(
        "memory/jobs/ops/sync/nightly-v2/2026/11/R-1.md",
        &record("failed", "2026-11-03T03:00:00Z", "2.0.0"),
    )?;
    Ok(vault)
}

fn router_over(vault: &JobVault) -> Router {
    contextos_web::build_router(
        std::sync::Arc::clone(&vault.clients),
        Some(vault.config.path()),
        &vault.config.path().join("web.toml"),
    )
}

async fn job_page(vault: &JobVault, job: &str) -> Result<String, BoxError> {
    let request = Request::builder()
        .method(Method::GET)
        .uri(format!("/{VAULT_NAME}/workflows/ops/sync/jobs/{job}"))
        .header("hx-request", "true")
        .body(Body::empty())?;
    let response = router_over(vault).oneshot(request).await?;
    Ok(String::from_utf8_lossy(&to_bytes(response.into_body(), usize::MAX).await?).into_owned())
}

#[tokio::test]
async fn a_job_page_shows_its_release_kind_lock_and_the_jobs_either_side_of_it() -> Result<(), BoxError> {
    let vault = chain(CHAIN_STATE).await?;

    let page = job_page(&vault, "nightly-v2").await?;

    assert!(page.contains("id=\"job-release\""), "{page}");
    assert!(page.contains("Current, install date not recorded"), "{page}");
    assert!(page.contains("<dt>Version</dt><dd>2.0.0</dd>"), "{page}");
    assert!(page.contains("No lock yet"), "{page}");
    assert!(
        page.contains(&format!(
            "<dt>Succeeds</dt><dd><a href=\"/{VAULT_NAME}/workflows/ops/sync/jobs/nightly\">nightly</a>"
        )),
        "{page}"
    );
    assert!(page.contains("<dt>Succeeded by</dt><dd>No job</dd>"), "{page}");
    assert!(!page.contains("Link mismatch"), "{page}");
    Ok(())
}

#[tokio::test]
async fn the_predecessors_page_names_its_successor_and_the_release_it_is_left_on() -> Result<(), BoxError> {
    let vault = chain(CHAIN_STATE).await?;

    let page = job_page(&vault, "nightly").await?;

    assert!(page.contains("Previous, install date not recorded"), "{page}");
    assert!(
        page.contains(&format!(
            "<dt>Succeeded by</dt><dd><a href=\"/{VAULT_NAME}/workflows/ops/sync/jobs/nightly-v2\">nightly-v2</a>"
        )),
        "{page}"
    );
    Ok(())
}

#[tokio::test]
async fn the_runs_of_the_whole_chain_are_shown_together_each_marked_with_its_job_and_release() -> Result<(), BoxError> {
    let vault = chain(CHAIN_STATE).await?;

    for job in ["nightly", "nightly-v2"] {
        let page = job_page(&vault, job).await?;

        let newest = page
            .find("nightly-v2 · 2.0.0")
            .ok_or_else(|| format!("{job}: {page}"))?;
        let oldest = page.find("nightly · 1.0.0").ok_or_else(|| format!("{job}: {page}"))?;
        assert!(newest < oldest, "{job}: the newest run is first:\n{page}");
    }
    Ok(())
}

#[tokio::test]
async fn a_predecessor_key_that_now_belongs_to_another_job_is_flagged_on_the_successors_page() -> Result<(), BoxError> {
    let vault = chain(&CHAIN_STATE.replace("identity = \"job-a\"\nretired", "identity = \"job-old\"\nretired")).await?;

    let page = job_page(&vault, "nightly-v2").await?;

    assert!(page.contains("Link mismatch"), "{page}");
    assert!(page.contains("another identity"), "{page}");
    Ok(())
}

#[tokio::test]
async fn a_job_file_that_cannot_be_read_says_why_on_the_jobs_page() -> Result<(), BoxError> {
    let vault = chain(CHAIN_STATE).await?;
    vault.write(
        "registry/workflows/ops/sync/jobs/nightly.toml",
        "name = [unterminated\n",
    )?;

    let page = job_page(&vault, "nightly").await?;

    assert!(page.contains("Job file cannot be used"), "{page}");
    assert!(page.contains("validation/artefact-invalid"), "{page}");
    Ok(())
}

const THIRD: &str =
    "name = \"Nightly v3\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"3.0.0\"\npredecessor = \"nightly-v2\"\n";
/// A chain of three in which the first two were each retired by their successor's enabling.
const RETIRED_TWICE: &str = "[ops.sync]\ncurrent = \"3.0.0\"\nprevious = \"2.0.0\"\n\n\
    [ops.sync.nightly]\nlifecycle = \"retired\"\nidentity = \"job-a\"\n\n\
    [ops.sync.nightly.retirement]\nby = \"system\"\ncause = \"successor-enabled\"\nat = \"2026-11-02T10:00:00Z\"\n\
    replaced_by = { job = \"nightly-v2\", identity = \"job-b\" }\n\n\
    [ops.sync.nightly-v2]\nlifecycle = \"retired\"\nidentity = \"job-b\"\n\n\
    [ops.sync.nightly-v2.link]\nidentity = \"job-a\"\nretired = true\n\n\
    [ops.sync.nightly-v2.retirement]\nby = \"system\"\ncause = \"successor-enabled\"\nat = \"2026-11-03T10:00:00Z\"\n\
    replaced_by = { job = \"nightly-v3\", identity = \"job-c\" }\n\n\
    [ops.sync.nightly-v3]\nlifecycle = \"enabled\"\nidentity = \"job-c\"\n\n\
    [ops.sync.nightly-v3.link]\nidentity = \"job-b\"\nretired = false\n";

#[tokio::test]
async fn every_job_of_a_chain_whose_earlier_jobs_were_retired_shows_the_whole_chains_runs_and_its_successor()
-> Result<(), BoxError> {
    let vault = chain(RETIRED_TWICE).await?;
    vault.write("registry/workflows/ops/sync/jobs/nightly-v3.toml", THIRD)?;
    vault.install_releases()?;
    vault.write(
        "memory/jobs/ops/sync/nightly-v3/2026/11/R-1.md",
        &record("succeeded", "2026-11-04T03:00:00Z", "3.0.0"),
    )?;

    for (job, successor) in [
        ("nightly", Some("nightly-v2")),
        ("nightly-v2", Some("nightly-v3")),
        ("nightly-v3", None),
    ] {
        let page = job_page(&vault, job).await?;

        let newest = page
            .find("nightly-v3 · 3.0.0")
            .ok_or_else(|| format!("{job}: {page}"))?;
        let middle = page
            .find("nightly-v2 · 2.0.0")
            .ok_or_else(|| format!("{job}: {page}"))?;
        let oldest = page.find("nightly · 1.0.0").ok_or_else(|| format!("{job}: {page}"))?;
        assert!(newest < middle && middle < oldest, "{job}: newest first:\n{page}");
        let expected = match successor {
            Some(next) => {
                format!("<dt>Succeeded by</dt><dd><a href=\"/{VAULT_NAME}/workflows/ops/sync/jobs/{next}\">{next}</a>")
            }
            None => "<dt>Succeeded by</dt><dd>No job</dd>".to_owned(),
        };
        assert!(page.contains(&expected), "{job}: {page}");
    }
    Ok(())
}

#[tokio::test]
async fn a_jobs_page_shows_its_own_last_run_though_a_successor_has_twenty_newer_runs() -> Result<(), BoxError> {
    let vault = chain(CHAIN_STATE).await?;
    for number in 2..=21 {
        vault.write(
            &format!("memory/jobs/ops/sync/nightly-v2/2026/11/R-{number}.md"),
            &record("succeeded", &format!("2026-11-{:02}T03:00:00Z", number + 2), "2.0.0"),
        )?;
    }

    let page = job_page(&vault, "nightly").await?;

    assert!(page.contains("<dt>Last run</dt><dd>2026-11-01T03:00:00"), "{page}");
    assert!(!page.contains("Never run"), "{page}");
    Ok(())
}

#[tokio::test]
async fn the_chains_runs_are_still_shown_beside_the_jobs_own_last_run() -> Result<(), BoxError> {
    let vault = chain(CHAIN_STATE).await?;

    let page = job_page(&vault, "nightly").await?;

    assert!(page.contains("nightly-v2 · 2.0.0"), "{page}");
    assert!(!page.contains("so only this job's runs are shown"), "{page}");
    Ok(())
}
