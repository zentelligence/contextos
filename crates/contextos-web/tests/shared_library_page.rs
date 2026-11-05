//! The host's packages page shows what each library release checks as, and lets the operator validate and resume the jobs on
//! the workflow releases that lock one, through the real router over a real package store, real job files, the host state
//! file, and a real `contextos-mcp` session.

mod support;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use contextos_web::routes::shared_workflows::{SharedRoots, SharedRoutesState};
use contextos_web::shared_router;
use support::BoxError;
use support::shared::SharedFixture;
use tower::ServiceExt as _;

const JOB: &str = "name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";
const LIBRARY: &str = "modules = [\"strings.rhai\"]\nexports = [\"strings.rhai\"]\n";

struct Answer {
    status: StatusCode,
    body: String,
}

struct Site {
    fixture: SharedFixture,
    router: Router,
}

async fn site() -> Result<Site, BoxError> {
    let fixture = SharedFixture::new(JOB).await?;
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

    fn install_library(&self, version: &str, library_toml: &str, module: &str) -> Result<(), BoxError> {
        support::release_fixture::write_whole_release(
            &self.fixture.store_root().join("acme-text").join(version),
            ("acme-text", version),
            "",
            &[
                ("library.toml", library_toml.as_bytes()),
                ("strings.rhai", module.as_bytes()),
            ],
        )
        .map_err(|error| error.to_string())?;
        Ok(())
    }
}

/// The part of `page` that belongs to the library release listed under `id`.
fn block<'a>(page: &'a str, id: &str) -> &'a str {
    let marker = format!("id=\"{id}\"");
    let Some(start) = page.find(&marker) else {
        return "";
    };
    let rest = &page[start..];
    let end = rest[marker.len()..]
        .find("class=\"library-release\"")
        .map_or(rest.len(), |at| at + marker.len());
    &rest[..end]
}

#[tokio::test]
async fn each_library_release_shows_whether_its_manifest_syntax_imports_and_declared_tools_check_out()
-> Result<(), BoxError> {
    let site = site().await?;
    site.install_library("1.0.0", LIBRARY, "fn shout() { 1 }")?;
    site.install_library("1.1.0", LIBRARY, "fn shout( { 1 }")?;
    site.install_library(
        "1.2.0",
        LIBRARY,
        "fn bill() { call_mcp(\"billing\", \"list_invoices\", #{}) }",
    )?;

    let page = site.send("GET", "/shared/packages", None).await?;

    assert_eq!(page.status, StatusCode::OK);
    let good = block(&page.body, "library-acme-text_1_0_0");
    for check in ["Manifest", "Syntax", "Imports", "Declared tools"] {
        assert!(
            good.contains(&format!("<dt>{check}</dt><dd class=\"check-passed\">Passed</dd>")),
            "{check}: {good}"
        );
    }
    let syntax = block(&page.body, "library-acme-text_1_1_0");
    assert!(
        syntax.contains("<dt>Syntax</dt><dd class=\"check-failed\">Failed"),
        "{syntax}"
    );
    assert!(
        syntax.contains("<dt>Manifest</dt><dd class=\"check-passed\">Passed</dd>"),
        "{syntax}"
    );
    let tools = block(&page.body, "library-acme-text_1_2_0");
    assert!(
        tools.contains("<dt>Declared tools</dt><dd class=\"check-failed\">Failed"),
        "{tools}"
    );
    assert!(tools.contains("billing") && tools.contains("list_invoices"), "{tools}");
    assert!(
        tools.contains("<dt>Syntax</dt><dd class=\"check-passed\">Passed</dd>"),
        "{tools}"
    );
    Ok(())
}

const READY: &str = "ready = true\nname = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";
const IMPORTS: &str = "import \"acme-text:strings\" as s;\ns::act()";
const LIBRARY_URL: &str = "/shared/packages/acme-text/versions/1.0.0";
const JOB_KEY: &str = "acme/sync/nightly";

impl Site {
    /// The host with the library `acme-text` 1.0.0, and `acme` 1.0.0 importing it, whose job `nightly` is enabled, locked to
    /// the library, and then suspended.
    async fn with_a_suspended_job() -> Result<Self, BoxError> {
        let site = site_with(READY).await?;
        site.fixture
            .install_library("acme-text", "1.0.0", "strings", "fn act() { 1 }")?;
        site.fixture
            .install_depending_on("1.0.0", IMPORTS, &[("acme-text", "^1")])?;
        site.fixture.enable().await?;
        site.send(
            "PUT",
            "/shared/workflows/acme/sync/jobs/nightly/execute/suspend",
            Some("{\"reason\": \"held\"}"),
        )
        .await?;
        Ok(site)
    }
}

async fn site_with(job: &str) -> Result<Site, BoxError> {
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

#[tokio::test]
async fn the_library_release_lists_the_jobs_on_the_workflow_releases_locking_it_and_offers_a_pick_for_a_suspended_one()
-> Result<(), BoxError> {
    let site = Site::with_a_suspended_job().await?;

    let page = site.send("GET", "/shared/packages", None).await?;

    let release = block(&page.body, "library-acme-text_1_0_0");
    assert!(
        release.contains("<a href=\"/shared/workflows/acme/sync/jobs/nightly\">Nightly</a>"),
        "{release}"
    );
    assert!(release.contains("<span class=\"badge\">suspended</span>"), "{release}");
    assert!(
        release.contains(&format!("<input type=\"checkbox\" name=\"{JOB_KEY}\" value=\"on\">")),
        "{release}"
    );
    assert!(
        release.contains(&format!("hx-put=\"{LIBRARY_URL}/execute/validate-dependents\"")),
        "{release}"
    );
    assert!(
        release.contains(&format!("hx-put=\"{LIBRARY_URL}/execute/resume-dependents\"")),
        "{release}"
    );
    assert!(!page.body.contains("<script"), "the page adds no client-side script");
    Ok(())
}

#[tokio::test]
async fn validating_the_dependents_validates_each_job_through_the_job_operation_and_reports_it() -> Result<(), BoxError>
{
    let site = Site::with_a_suspended_job().await?;

    let answer = site
        .send("PUT", &format!("{LIBRARY_URL}/execute/validate-dependents"), None)
        .await?;

    assert_eq!(answer.status, StatusCode::OK);
    assert!(!answer.body.contains("<html"), "{}", answer.body);
    assert!(
        answer.body.contains("id=\"library-acme-text_1_0_0-jobs\""),
        "{}",
        answer.body
    );
    assert!(
        answer.body.contains("<li class=\"check-passed\"><a href=\"/shared/workflows/acme/sync/jobs/nightly\">Nightly</a>: Validation passed."),
        "{}",
        answer.body
    );
    assert_eq!(site.fixture.lifecycle()?, "suspended", "validation does not resume");
    Ok(())
}

#[tokio::test]
async fn resuming_the_picked_dependents_resumes_each_through_the_job_operation_and_reports_every_pick()
-> Result<(), BoxError> {
    let site = Site::with_a_suspended_job().await?;

    let answer = site
        .send(
            "PUT",
            &format!("{LIBRARY_URL}/execute/resume-dependents"),
            Some(&format!("{{\"{JOB_KEY}\": \"on\", \"acme/sync/ghost\": \"on\"}}")),
        )
        .await?;

    assert_eq!(answer.status, StatusCode::OK);
    assert!(
        answer.body.contains("<li class=\"check-passed\"><a href=\"/shared/workflows/acme/sync/jobs/nightly\">Nightly</a>: The job is now enabled."),
        "{}",
        answer.body
    );
    assert!(
        answer.body.contains("<li class=\"check-failed\">acme/sync/ghost: It is not a job on a workflow release locked to this library release"),
        "{}",
        answer.body
    );
    assert_eq!(site.fixture.lifecycle()?, "enabled");
    assert!(
        !answer.body.contains("type=\"checkbox\""),
        "an enabled job is no longer offered for a pick: {}",
        answer.body
    );
    Ok(())
}

#[tokio::test]
async fn a_resume_whose_job_no_longer_validates_is_reported_with_its_failures_and_the_job_stays_suspended()
-> Result<(), BoxError> {
    let site = Site::with_a_suspended_job().await?;
    std::fs::write(
        site.fixture.job_path(),
        "ready = true\nname = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n[inputs]\nundeclared = \"x\"\n",
    )?;

    let answer = site
        .send(
            "PUT",
            &format!("{LIBRARY_URL}/execute/resume-dependents"),
            Some(&format!("{{\"{JOB_KEY}\": \"on\"}}")),
        )
        .await?;

    assert!(
        answer.body.contains("<li class=\"check-failed\">") && answer.body.contains("workflow/validation-failed"),
        "{}",
        answer.body
    );
    assert_eq!(site.fixture.lifecycle()?, "suspended");
    Ok(())
}

#[tokio::test]
async fn resuming_with_nothing_picked_resumes_nothing_and_says_so() -> Result<(), BoxError> {
    let site = Site::with_a_suspended_job().await?;

    let answer = site
        .send("PUT", &format!("{LIBRARY_URL}/execute/resume-dependents"), Some("{}"))
        .await?;

    assert!(
        answer
            .body
            .contains("role=\"alert\">No job was picked. Pick the jobs to resume."),
        "{}",
        answer.body
    );
    assert_eq!(site.fixture.lifecycle()?, "suspended");
    Ok(())
}

#[tokio::test]
async fn an_action_on_a_library_release_the_store_does_not_hold_or_a_command_that_is_not_one_is_not_found()
-> Result<(), BoxError> {
    let site = Site::with_a_suspended_job().await?;

    for path in [
        "/shared/packages/acme-text/versions/9.9.9/execute/validate-dependents",
        "/shared/packages/acme/versions/1.0.0/execute/validate-dependents",
        "/shared/packages/acme-text/versions/1.0.0/execute/retire",
    ] {
        let answer = site.send("PUT", path, None).await?;
        assert_eq!(answer.status, StatusCode::NOT_FOUND, "{path}");
    }
    Ok(())
}

#[tokio::test]
async fn a_pick_body_that_is_not_the_pick_form_is_answered_in_the_block_and_changes_nothing() -> Result<(), BoxError> {
    let site = Site::with_a_suspended_job().await?;

    let answer = site
        .send(
            "PUT",
            &format!("{LIBRARY_URL}/execute/resume-dependents"),
            Some("[\"x\"]"),
        )
        .await?;

    assert_eq!(answer.status, StatusCode::OK);
    assert!(
        answer.body.contains("The picked jobs could not be read"),
        "{}",
        answer.body
    );
    assert_eq!(site.fixture.lifecycle()?, "suspended");
    Ok(())
}

/// Changes a file of the installed library release after it was installed, which leaves the release damaged.
fn tamper_with_the_library(site: &Site) -> Result<(), BoxError> {
    std::fs::write(
        site.fixture.store_root().join("acme-text/1.0.0/strings.rhai"),
        b"fn act() { 2 }",
    )?;
    Ok(())
}

#[tokio::test]
async fn a_library_release_that_does_not_read_whole_is_listed_with_its_manifest_failed_and_the_other_checks_not_made()
-> Result<(), BoxError> {
    let site = Site::with_a_suspended_job().await?;
    tamper_with_the_library(&site)?;

    let page = site.send("GET", "/shared/packages", None).await?;

    assert_eq!(page.status, StatusCode::OK);
    let listed = block(&page.body, "library-acme-text_1_0_0");
    assert!(
        !listed.is_empty(),
        "the damaged library release is not listed: {}",
        page.body
    );
    assert!(
        listed.contains("<dt>Manifest</dt><dd class=\"check-failed\">Failed"),
        "{listed}"
    );
    assert!(listed.contains("strings.rhai"), "{listed}");
    for check in ["Syntax", "Imports", "Declared tools"] {
        assert!(
            listed.contains(&format!("<dt>{check}</dt><dd class=\"check-skipped\">Not checked")),
            "{check}: {listed}"
        );
    }
    assert!(
        listed.contains("acme sync"),
        "the workflow release locked to it: {listed}"
    );
    Ok(())
}

#[tokio::test]
async fn the_actions_on_a_library_release_that_does_not_read_whole_still_reach_the_jobs_locked_to_it()
-> Result<(), BoxError> {
    let site = Site::with_a_suspended_job().await?;
    tamper_with_the_library(&site)?;

    let answer = site
        .send("PUT", &format!("{LIBRARY_URL}/execute/validate-dependents"), None)
        .await?;

    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    assert!(answer.body.contains("Nightly"), "{}", answer.body);
    Ok(())
}
