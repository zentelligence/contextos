//! The host's shared workflows, jobs, and the actions an operator takes on a shared job, through the real router over a
//! real package store, real job files, the real host state file, and a real `contextos-mcp` session: the list and its
//! filter, the workflow and job pages, and each action's answer and its effect on the host state.

mod support;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode};
use contextos_web::routes::shared_workflows::{SharedRoots, SharedRoutesState};
use contextos_web::shared_router;
use support::BoxError;
use support::shared::SharedFixture;
use tower::ServiceExt as _;

/// A job file that says authoring is finished, so the automatic pass validates it.
const READY: &str = "ready = true\nname = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";
const JOB: &str = "name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";

struct Answer {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

struct Site {
    fixture: SharedFixture,
    router: Router,
}

async fn site(job: &str) -> Result<Site, BoxError> {
    let fixture = SharedFixture::new(job).await?;
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

    fn lifecycle(&self) -> Result<&'static str, BoxError> {
        self.fixture.lifecycle()
    }
}

const JOB_URL: &str = "/shared/workflows/acme/sync/jobs/nightly";

fn job_url(tail: &str) -> String {
    format!("{JOB_URL}/{tail}")
}

#[tokio::test]
async fn a_shared_workflow_page_lists_its_releases_with_the_jobs_on_each() -> Result<(), BoxError> {
    let site = site(JOB).await?;

    let page = site.get("/shared/workflows/acme/sync").await?;

    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("id=\"workflow-releases\""), "{}", page.body);
    assert!(
        page.body.contains("<h3>Future, install date not recorded</h3>"),
        "{}",
        page.body
    );
    assert!(
        page.body
            .contains("<dt>draft</dt><dd><a href=\"/shared/workflows/acme/sync/jobs/nightly\">Nightly</a>"),
        "{}",
        page.body
    );
    assert!(page.body.contains("<dt>Version</dt><dd>1.0.0</dd>"), "{}", page.body);
    Ok(())
}

// ---------------------------------------------------------------------
// Pages
// ---------------------------------------------------------------------

#[tokio::test]
async fn the_list_shows_each_shared_workflow_with_its_jobs_and_a_filter_chip_per_state() -> Result<(), BoxError> {
    let site = site(JOB).await?;

    let page = site.get("/shared").await?;

    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.body.contains("acme / sync") && page.body.contains("Nightly"),
        "{}",
        page.body
    );
    assert!(page.body.contains(JOB_URL), "{}", page.body);
    for chip in ["Needs attention", "draft", "suspended", "retired"] {
        assert!(page.body.contains(chip), "{chip}");
    }
    assert!(
        !page.body.contains("Run now"),
        "a run from a page is not offered yet:\n{}",
        page.body
    );
    Ok(())
}

#[tokio::test]
async fn the_state_filter_shows_only_the_jobs_in_that_state() -> Result<(), BoxError> {
    let site = site(JOB).await?;

    let drafts = site.get("/shared?state=draft").await?;
    let enabled = site.get("/shared?state=enabled").await?;
    let unknown = site.get("/shared?state=sleeping").await?;

    assert!(drafts.body.contains("Nightly"));
    assert!(
        !enabled.body.contains("Nightly") && enabled.body.contains("No job matches this filter"),
        "{}",
        enabled.body
    );
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
    Ok(())
}

#[tokio::test]
async fn an_htmx_request_for_the_list_gets_the_fragment() -> Result<(), BoxError> {
    let site = site(JOB).await?;

    let fragment = site.send("GET", "/shared?state=draft", true, None).await?;

    assert!(
        fragment.body.contains("id=\"workflows-screen\"") && !fragment.body.contains("<html"),
        "{}",
        fragment.body
    );
    Ok(())
}

#[tokio::test]
async fn a_shared_workflow_page_shows_its_package_release_and_jobs() -> Result<(), BoxError> {
    let site = site(JOB).await?;

    let page = site.get("/shared/workflows/acme/sync").await?;

    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.body.contains("1.0.0") && page.body.contains("sync.rhai") && page.body.contains(JOB_URL),
        "{}",
        page.body
    );
    assert_eq!(
        site.get("/shared/workflows/acme/nothing").await?.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        site.get("/shared/../workflows/sync").await?.status,
        StatusCode::NOT_FOUND
    );
    Ok(())
}

#[tokio::test]
async fn a_shared_job_page_shows_its_status_facts_and_offers_only_what_its_state_allows() -> Result<(), BoxError> {
    let site = site(JOB).await?;

    let page = site.get(JOB_URL).await?;

    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.body.contains("id=\"job-status\"") && page.body.contains("Needs attention"),
        "{}",
        page.body
    );
    assert!(
        page.body.contains("1.0.0") && page.body.contains("acme"),
        "{}",
        page.body
    );
    assert!(
        page.body.contains("execute/validate") && !page.body.contains("execute/suspend"),
        "{}",
        page.body
    );
    assert!(!page.body.contains("Run now"), "{}", page.body);
    assert!(page.body.contains("No runs recorded yet"), "{}", page.body);
    Ok(())
}

#[tokio::test]
async fn a_shared_job_page_shows_its_release_and_the_runs_of_its_whole_chain_marked_by_job_and_release()
-> Result<(), BoxError> {
    let site = site(JOB).await?;
    site.fixture
        .install("2.0.0", support::shared::DIGEST, "log_info(\"b\");")?;
    support::shared::write(
        &site.fixture.jobs_root().join("acme/sync/jobs/nightly-v2.toml"),
        "name = \"Nightly v2\"\ntimeout_secs = 30\nversion = \"2.0.0\"\npredecessor = \"nightly\"\n",
    )?;
    let clock = Arc::new(contextos_workflows::SystemClock);
    let ledger =
        contextos_web::shared_ledger::SharedLedger::new(site.fixture.runs_root(), clock, time::Duration::days(30));
    for (job, version) in [("nightly", "1.0.0"), ("nightly-v2", "2.0.0")] {
        let identity = contextos_workflows::PackageIdentity {
            name: contextos_workflows::PackageName::try_from("acme")?,
            version: semver::Version::parse(version)?,
            digest: contextos_workflows::ContentDigest::try_new(support::shared::DIGEST)?,
        };
        ledger.record_skipped(
            &contextos_web::shared_claim::SharedJobKey {
                package: "acme".to_owned(),
                workflow: "sync".to_owned(),
                job: job.to_owned(),
            },
            contextos_web::shared_ledger::SkippedRun {
                identity: &identity,
                inputs: Vec::new(),
                reason: "the job's previous run still held its claim",
            },
        )?;
    }

    let page = site.get("/shared/workflows/acme/sync/jobs/nightly-v2").await?;

    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("id=\"job-release\""), "{}", page.body);
    assert!(
        page.body
            .contains("<dt>Succeeds</dt><dd><a href=\"/shared/workflows/acme/sync/jobs/nightly\">nightly</a>"),
        "{}",
        page.body
    );
    assert!(page.body.contains("nightly · 1.0.0"), "{}", page.body);
    assert!(page.body.contains("nightly-v2 · 2.0.0"), "{}", page.body);
    Ok(())
}

#[tokio::test]
async fn a_shared_job_page_lists_the_runs_the_host_recorded() -> Result<(), BoxError> {
    let site = site(JOB).await?;
    let ledger = contextos_web::shared_ledger::SharedLedger::new(
        site.fixture.runs_root(),
        Arc::new(contextos_workflows::SystemClock),
        time::Duration::days(30),
    );
    let identity = contextos_workflows::PackageIdentity {
        name: contextos_workflows::PackageName::try_from("acme")?,
        version: semver::Version::parse("1.0.0")?,
        digest: contextos_workflows::ContentDigest::try_new(support::shared::DIGEST)?,
    };
    ledger.record_skipped(
        &support::shared::key(),
        contextos_web::shared_ledger::SkippedRun {
            identity: &identity,
            inputs: Vec::new(),
            reason: "the job's previous run still held its claim",
        },
    )?;

    let page = site.get(JOB_URL).await?;

    assert!(
        page.body.contains("R-1") && page.body.contains("skipped"),
        "{}",
        page.body
    );
    assert!(page.body.contains("previous run still held its claim"), "{}", page.body);
    Ok(())
}

#[tokio::test]
async fn a_job_file_that_does_not_load_is_listed_as_needing_attention_and_can_be_validated() -> Result<(), BoxError> {
    let site = site(JOB).await?;
    support::shared::write(
        &site.fixture.jobs_root().join("acme/sync/jobs/broken.toml"),
        "name = \"Broken\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nowner = \"nobody\"\n",
    )?;

    let attention = site.get("/shared?state=attention").await?;
    let validated = site
        .put("/shared/workflows/acme/sync/jobs/broken/execute/validate", None)
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
    Ok(())
}

// ---------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------

#[tokio::test]
async fn validating_from_the_page_records_the_result_and_swaps_in_the_new_status() -> Result<(), BoxError> {
    let site = site(JOB).await?;

    let answer = site.put(&job_url("execute/validate"), None).await?;

    assert_eq!(answer.status, StatusCode::OK);
    assert!(
        answer.body.contains("Validation passed") && answer.body.contains("id=\"job-status\""),
        "{}",
        answer.body
    );
    assert_eq!(site.lifecycle()?, "validated");
    Ok(())
}

#[tokio::test]
async fn a_shared_job_goes_through_schedule_suspend_resume_retire_reactivate_and_delete_from_its_page()
-> Result<(), BoxError> {
    let site = site(READY).await?;
    site.fixture.register().await?;

    let enabled = site.patch(JOB_URL, "{\"schedule\": \"0 3 * * *\"}").await?;
    assert!(
        enabled.body.contains("The job is now enabled") && enabled.body.contains("0 3 * * *"),
        "{}",
        enabled.body
    );
    assert!(site.fixture.on_trial()?);

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
    assert_eq!(site.lifecycle()?, "suspended");

    let resumed = site.put(&job_url("execute/resume"), None).await?;
    assert!(resumed.body.contains("The job is now enabled"), "{}", resumed.body);

    site.put(&job_url("execute/retire"), None).await?;
    assert_eq!(site.lifecycle()?, "retired");
    let confirm = site.get(&job_url("confirm-delete")).await?;
    assert!(confirm.body.contains("hx-delete"), "{}", confirm.body);

    let deleted = site.send("DELETE", JOB_URL, true, None).await?;
    assert_eq!(deleted.status, StatusCode::OK);
    assert_eq!(
        deleted.headers.get("hx-redirect").and_then(|value| value.to_str().ok()),
        Some("/shared/workflows/acme/sync")
    );
    assert!(site.fixture.read()?.job("acme", "sync", "nightly").is_none());
    Ok(())
}

#[tokio::test]
async fn deleting_a_retired_shared_job_from_its_page_removes_its_file_too() -> Result<(), BoxError> {
    let site = site("ready = true\nname = \"Nightly\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n").await?;
    site.fixture.enable().await?;
    site.put(&job_url("execute/retire"), None).await?;
    assert_eq!(site.lifecycle()?, "retired");
    assert!(site.fixture.job_path().exists());

    let deleted = site.send("DELETE", JOB_URL, true, None).await?;

    assert_eq!(deleted.status, StatusCode::OK);
    assert!(!site.fixture.job_path().exists(), "the job file goes with its state");
    assert!(site.fixture.read()?.job("acme", "sync", "nightly").is_none());
    let relisted = site.get("/shared").await?;
    assert!(
        !relisted.body.contains("nightly"),
        "the deleted job is not listed again: {}",
        relisted.body
    );
    Ok(())
}

#[tokio::test]
async fn the_delete_confirmation_of_a_shared_job_says_its_file_is_removed_with_its_state() -> Result<(), BoxError> {
    let site = site("ready = true\nname = \"Nightly\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n").await?;

    let confirm = site.get(&job_url("confirm-delete")).await?;

    assert!(confirm.body.contains("job file is removed"), "{}", confirm.body);
    assert!(!confirm.body.contains("comes back as a draft"), "{}", confirm.body);
    Ok(())
}

#[tokio::test]
async fn resuming_a_job_whose_release_no_longer_validates_is_an_alert_with_the_failures_and_it_stays_suspended()
-> Result<(), BoxError> {
    let site = site(READY).await?;
    site.fixture.enable().await?;
    site.put(&job_url("execute/suspend"), Some("{\"reason\": \"held\"}"))
        .await?;
    site.fixture
        .install("1.1.0", support::shared::OTHER_DIGEST, "let y = (2 +;")?;
    site.fixture.name_release("1.1.0")?;

    let answer = site.put(&job_url("execute/resume"), None).await?;

    assert_eq!(answer.status, StatusCode::OK);
    assert!(
        answer.body.contains("workflow/validation-failed") && answer.body.contains("validation/script-syntax"),
        "{}",
        answer.body
    );
    assert_eq!(site.lifecycle()?, "suspended");
    Ok(())
}

#[tokio::test]
async fn a_refused_action_is_an_alert_with_its_code_and_writes_nothing() -> Result<(), BoxError> {
    let site = site(JOB).await?;

    let answer = site
        .put(&job_url("execute/suspend"), Some("{\"reason\": \"held\"}"))
        .await?;

    assert_eq!(answer.status, StatusCode::OK);
    assert!(
        answer.body.contains("role=\"alert\"") && answer.body.contains("workflow/transition-refused"),
        "{}",
        answer.body
    );
    assert!(site.fixture.state_text().is_err(), "no state file was made");
    Ok(())
}

#[tokio::test]
async fn a_shared_job_has_no_favourites_and_a_malformed_update_is_an_alert() -> Result<(), BoxError> {
    let site = site(JOB).await?;

    let favourite = site.patch(JOB_URL, "{\"favourite\": true}").await?;
    let neither = site.patch(JOB_URL, "{}").await?;

    assert!(
        favourite.body.contains("no favourites") && favourite.body.contains("role=\"alert\""),
        "{}",
        favourite.body
    );
    assert!(neither.body.contains("exactly one"), "{}", neither.body);
    assert!(site.fixture.state_text().is_err());
    Ok(())
}

#[tokio::test]
async fn a_command_or_a_job_that_does_not_exist_is_not_found() -> Result<(), BoxError> {
    let site = site(JOB).await?;

    let command = site.put(&job_url("execute/explode"), None).await?;
    let job = site
        .put("/shared/workflows/acme/sync/jobs/nothing/execute/validate", None)
        .await?;
    let escaping = site
        .put("/shared/workflows/acme/sync/jobs/..%2F/execute/validate", None)
        .await?;
    let delete = site
        .send("DELETE", "/shared/workflows/acme/sync/jobs/nothing", true, None)
        .await?;

    assert_eq!(command.status, StatusCode::NOT_FOUND);
    assert_eq!(job.status, StatusCode::NOT_FOUND);
    assert_eq!(escaping.status, StatusCode::NOT_FOUND);
    assert_eq!(delete.status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn the_shared_delete_confirmation_is_for_a_job_that_exists_and_a_name_that_is_a_slug() -> Result<(), BoxError> {
    let site = site(JOB).await?;

    let real = site.get(&job_url("confirm-delete")).await?;
    let none = site
        .get("/shared/workflows/acme/sync/jobs/nothing/confirm-delete")
        .await?;
    let odd = site
        .get("/shared/workflows/acme/sync/jobs/Night%3Fly/confirm-delete")
        .await?;

    assert_eq!(real.status, StatusCode::OK);
    assert_eq!(none.status, StatusCode::NOT_FOUND);
    assert_eq!(odd.status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn validating_a_retired_shared_job_is_refused() -> Result<(), BoxError> {
    let site = site(READY).await?;
    site.fixture.enable().await?;
    site.put(&job_url("execute/retire"), None).await?;

    let answer = site.put(&job_url("execute/validate"), None).await?;

    assert!(
        answer.body.contains("role=\"alert\"") && answer.body.contains("retired"),
        "{}",
        answer.body
    );
    Ok(())
}

#[tokio::test]
async fn the_packages_page_shows_each_releases_exports_and_the_installed_packages_that_need_it() -> Result<(), BoxError>
{
    let site = site(JOB).await?;
    site.fixture
        .install_library("acme-text", "1.2.0", "strings", "fn shout() { 1 }")?;
    site.fixture
        .install_depending_on("1.0.0", "()", &[("acme-text", "^1")])?;
    let broken = site.fixture.store_root().join("broken").join("1.0.0");
    std::fs::create_dir_all(&broken)?;
    std::fs::write(broken.join("manifest.toml"), "this is not a manifest")?;

    let page = site.get("/shared/packages").await?;

    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("<html"), "a full page for a full navigation");
    for expected in [
        "acme-text",
        "1.2.0",
        "strings.rhai",
        "acme 1.0.0 (^1)",
        "broken 1.0.0",
        "unreadable",
    ] {
        assert!(page.body.contains(expected), "{expected}: {}", page.body);
    }
    let fragment = site.send("GET", "/shared/packages", true, None).await?;
    assert!(
        !fragment.body.contains("<script"),
        "the page adds no client-side script"
    );
    let list = site.get("/shared").await?;
    assert!(
        list.body.contains("/shared/packages"),
        "the workflows list links to the packages page"
    );
    Ok(())
}

#[tokio::test]
async fn the_packages_page_lists_each_library_release_with_the_workflow_releases_locked_to_it() -> Result<(), BoxError>
{
    let site = site(JOB).await?;
    let store = site.fixture.store_root();
    let library = "modules = [\"strings.rhai\"]\nexports = [\"strings.rhai\"]\n";
    for version in ["1.0.0", "1.1.0"] {
        support::release_fixture::write_whole_release(
            &store.join("acme-text").join(version),
            ("acme-text", version),
            "",
            &[
                ("library.toml", library.as_bytes()),
                ("strings.rhai", b"fn shout() { 1 }"),
            ],
        )
        .map_err(|error| error.to_string())?;
    }
    support::release_fixture::write_whole_release(
        &store.join("ops").join("1.0.0"),
        ("ops", "1.0.0"),
        "",
        &[
            ("workflows.toml", b"workflows = [\"sync\"]\n"),
            ("sync/workflow.toml", b"name = \"Sync\"\nentry = \"sync.rhai\"\n"),
            ("sync/sync.rhai", b"log_info(\"a\");"),
        ],
    )
    .map_err(|error| error.to_string())?;
    support::shared::write(
        &site.fixture.root.path().join("state/shared-jobs.toml"),
        &format!(
            "[ops.sync]\ncurrent = \"1.0.0\"\n\n[ops.sync.\"1.0.0\".lock.acme-text]\nversion = \"1.0.0\"\ndigest = \"sha256:{}\"\n",
            "0".repeat(64)
        ),
    )?;

    let page = site.get("/shared/packages").await?;

    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("id=\"library-releases\""), "{}", page.body);
    assert!(page.body.contains("<strong>acme-text 1.0.0</strong>"), "{}", page.body);
    assert!(
        page.body
            .contains("<a href=\"/shared/workflows/ops/sync\">ops sync, current</a>"),
        "{}",
        page.body
    );
    assert!(
        page.body.contains("Older than the newest release, and still locked"),
        "{}",
        page.body
    );
    assert!(
        page.body.contains("No workflow release is locked to it."),
        "{}",
        page.body
    );
    Ok(())
}

#[tokio::test]
async fn an_htmx_request_for_the_packages_page_gets_the_fragment_and_an_empty_store_says_so() -> Result<(), BoxError> {
    let site = site(JOB).await?;
    std::fs::remove_dir_all(site.fixture.store_root())?;

    let fragment = site.send("GET", "/shared/packages", true, None).await?;

    assert_eq!(fragment.status, StatusCode::OK);
    assert!(!fragment.body.contains("<html"), "{}", fragment.body);
    assert!(
        fragment.body.contains("No packages are installed."),
        "{}",
        fragment.body
    );
    Ok(())
}
