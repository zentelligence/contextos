//! A vault's packages page shows what each library release checks as and lets the operator validate and resume the jobs on
//! the workflow releases that lock one, through the real router, a real `contextos` session, and a real temporary vault.

mod support;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use support::jobs::{JobVault, Spec, VAULT_NAME};
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const PACKAGE: &str = "workflows = [\"sync\"]\n[[libraries]]\nname = \"crm\"\nrequirement = \"*\"\n";
const LIBRARY: &str = "modules = [\"client.rhai\"]\nexports = [\"client.rhai\"]\n";
const ENTRY: &str = "import \"crm:client\" as crm;\ncrm::a()\n";
const WORKFLOW: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"contextos\"]\n";
const JOB_PAGE: &str = "/contract-fixture/workflows/ops/sync/jobs/nightly";
const LIBRARY_URL: &str = "/contract-fixture/packages/crm/versions/1.0.0";
const JOB_KEY: &str = "ops/sync/nightly";

/// A vault with the library `crm` 1.0.0 and a draft job on `ops` that imports it, validated once so the release is locked.
async fn vault() -> Result<JobVault, BoxError> {
    let vault = JobVault::open(&Spec {
        package: PACKAGE,
        workflow: WORKFLOW,
        script: ENTRY,
        tasks: false,
        ..Spec::default()
    })
    .await?;
    vault.write("registry/libraries/crm/library.toml", LIBRARY)?;
    vault.write("registry/libraries/crm/client.rhai", "fn a() { 1 }\n")?;
    vault.install_releases()?;
    let validated = send(&vault, Method::PUT, &format!("{JOB_PAGE}/execute/validate"), None).await?;
    assert!(validated.1.contains("Validation passed"), "{}", validated.1);
    Ok(vault)
}

/// The job stored as suspended on the current release, as a failing run would leave it.
fn suspend_in_state(vault: &JobVault) -> Result<(), BoxError> {
    let path = vault.root().join(".contextos/jobs.toml");
    let text = std::fs::read_to_string(&path)?;
    assert!(text.contains("lifecycle = \"validated\""), "{text}");
    std::fs::write(
        path,
        format!("[ops.sync]\ncurrent = \"1.0.0\"\n\n{text}")
            .replace(
                "lifecycle = \"validated\"",
                "lifecycle = \"suspended\"\nschedule = \"* * * * *\"",
            )
            .replace("lifecycle_actor = \"validator\"", "lifecycle_actor = \"operator\""),
    )?;
    Ok(())
}

fn router_over(vault: &JobVault) -> Router {
    contextos_web::build_router(
        std::sync::Arc::clone(&vault.clients),
        Some(vault.config.path()),
        &vault.config.path().join("web.toml"),
    )
}

async fn send(
    vault: &JobVault,
    method: Method,
    path: &str,
    body: Option<&str>,
) -> Result<(StatusCode, String), BoxError> {
    let request = Request::builder().method(method).uri(path).header("hx-request", "true");
    let request = match body {
        Some(json) => request
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Body::from(json.to_owned()))?,
        None => request.body(Body::empty())?,
    };
    let response = router_over(vault).oneshot(request).await?;
    let status = response.status();
    Ok((
        status,
        String::from_utf8_lossy(&to_bytes(response.into_body(), usize::MAX).await?).into_owned(),
    ))
}

#[tokio::test]
async fn the_library_release_shows_its_checks_and_the_jobs_on_the_workflow_release_locking_it() -> Result<(), BoxError>
{
    let vault = vault().await?;

    let (status, page) = send(&vault, Method::GET, &format!("/{VAULT_NAME}/packages"), None).await?;

    assert_eq!(status, StatusCode::OK);
    for check in ["Manifest", "Syntax", "Imports", "Declared tools"] {
        assert!(
            page.contains(&format!("<dt>{check}</dt><dd class=\"check-passed\">Passed</dd>")),
            "{check}: {page}"
        );
    }
    assert!(page.contains(&format!("<a href=\"{JOB_PAGE}\">Nightly</a>")), "{page}");
    assert!(page.contains("<span class=\"badge\">validated</span>"), "{page}");
    assert!(
        page.contains(&format!("hx-put=\"{LIBRARY_URL}/execute/validate-dependents\"")),
        "{page}"
    );
    assert!(!page.contains("<script"), "the page adds no client-side script");
    Ok(())
}

#[tokio::test]
async fn validating_the_dependents_validates_each_job_and_reports_it() -> Result<(), BoxError> {
    let vault = vault().await?;

    let (status, answer) = send(
        &vault,
        Method::PUT,
        &format!("{LIBRARY_URL}/execute/validate-dependents"),
        None,
    )
    .await?;

    assert_eq!(status, StatusCode::OK);
    assert!(!answer.contains("<html"), "{answer}");
    assert!(
        answer.contains(&format!(
            "<li class=\"check-passed\"><a href=\"{JOB_PAGE}\">Nightly</a>: Validation passed."
        )),
        "{answer}"
    );
    Ok(())
}

#[tokio::test]
async fn resuming_the_picked_dependents_resumes_each_through_the_job_operation() -> Result<(), BoxError> {
    let vault = vault().await?;
    suspend_in_state(&vault)?;
    let (_, page) = send(&vault, Method::GET, &format!("/{VAULT_NAME}/packages"), None).await?;
    assert!(
        page.contains(&format!("<input type=\"checkbox\" name=\"{JOB_KEY}\" value=\"on\">")),
        "{page}"
    );

    let (status, answer) = send(
        &vault,
        Method::PUT,
        &format!("{LIBRARY_URL}/execute/resume-dependents"),
        Some(&format!("{{\"{JOB_KEY}\": \"on\"}}")),
    )
    .await?;

    assert_eq!(status, StatusCode::OK);
    assert!(
        answer.contains(&format!(
            "<li class=\"check-passed\"><a href=\"{JOB_PAGE}\">Nightly</a>: The job is now enabled."
        )),
        "{answer}"
    );
    let state = std::fs::read_to_string(vault.root().join(".contextos/jobs.toml"))?;
    assert!(state.contains("lifecycle = \"enabled\""), "{state}");
    Ok(())
}

#[tokio::test]
async fn a_job_that_cannot_be_resumed_is_reported_for_itself_and_changes_nothing() -> Result<(), BoxError> {
    let vault = vault().await?;

    let (_, answer) = send(
        &vault,
        Method::PUT,
        &format!("{LIBRARY_URL}/execute/resume-dependents"),
        Some(&format!("{{\"{JOB_KEY}\": \"on\"}}")),
    )
    .await?;

    assert!(
        answer.contains("<li class=\"check-failed\">") && answer.contains("validated"),
        "a validated job is refused by the table with the state named: {answer}"
    );
    let state = std::fs::read_to_string(vault.root().join(".contextos/jobs.toml"))?;
    assert!(state.contains("lifecycle = \"validated\""), "{state}");
    Ok(())
}

#[tokio::test]
async fn an_action_on_a_vault_or_library_release_that_does_not_exist_is_not_found() -> Result<(), BoxError> {
    let vault = vault().await?;

    for path in [
        "/no-such-vault/packages/crm/versions/1.0.0/execute/validate-dependents",
        "/contract-fixture/packages/crm/versions/9.9.9/execute/validate-dependents",
        "/contract-fixture/packages/crm/versions/1.0.0/execute/resume",
    ] {
        let (status, _) = send(&vault, Method::PUT, path, None).await?;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
    }
    Ok(())
}
