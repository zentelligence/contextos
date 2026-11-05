//! `PUT .../jobs/{job}/execute/successor` and `POST .../jobs/execute/successor`, for a vault and for the host, through
//! the real router over a real vault behind a real `contextos` session and a real host store: the JSON each answers with,
//! its status for each kind of refusal, and the job files left.

mod support;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use contextos_web::package_store::AppDataRoots;
use serde_json::Value;
use support::BoxError;
use support::jobs::{JobVault, Spec, VAULT_NAME};
use tower::ServiceExt as _;

const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SHARED_JOB: &str = "name = \"Nightly\"\ntimeout_secs = 120\nversion = \"1.0.0\"\nready = true\n";

fn state(package: &str) -> String {
    format!(
        "[{package}.sync]\ncurrent = \"1.0.0\"\nfuture = \"1.1.0\"\n\n[{package}.sync.nightly]\nlifecycle = \"enabled\"\n"
    )
}

struct Site {
    vault: JobVault,
    router: Router,
    app_data: std::path::PathBuf,
}

async fn site() -> Result<Site, BoxError> {
    let state_text = state("ops");
    let spec = Spec {
        state: Some(&state_text),
        ..Spec::default()
    };
    let vault = JobVault::open(&spec).await?;
    vault.write(
        ".contextos/packages/ops/1.1.0/manifest.toml",
        "name = \"ops\"\nversion = \"1.1.0\"\n",
    )?;
    vault.write(
        ".contextos/packages/ops/1.1.0/workflows.toml",
        "workflows = [\"sync\"]\n",
    )?;
    let app_data = vault.config.path().to_path_buf();
    let roots = AppDataRoots::from(app_data.as_path());
    for version in ["1.0.0", "1.1.0"] {
        let package = roots.store.join(format!("acme/{version}"));
        support::shared::write(
            &package.join("manifest.toml"),
            &format!(
                "name = \"acme\"\nversion = \"{version}\"\ndigest = \"{DIGEST}\"\n\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n"
            ),
        )?;
        support::shared::write(&package.join("sync.rhai"), "log_info(\"a\");")?;
        support::release_fixture::seal_io(&package)?;
    }
    support::shared::write(&roots.jobs.join("acme/sync/jobs/nightly.toml"), SHARED_JOB)?;
    support::shared::write(&roots.state.join("shared-jobs.toml"), &state("acme"))?;
    let router = contextos_web::build_router_in(
        std::sync::Arc::clone(&vault.clients),
        Some(&app_data),
        &app_data.join("web.toml"),
        &contextos_web::config::WorkflowsConfig::default(),
        Some(&roots),
    );
    Ok(Site {
        vault,
        router,
        app_data,
    })
}

impl Site {
    async fn send(&self, method: &str, path: &str, body: &str) -> Result<(StatusCode, Value), BoxError> {
        let response = self
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_owned()))?,
            )
            .await?;
        let status = response.status();
        let content_type = response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let bytes = to_bytes(response.into_body(), usize::MAX).await?;
        assert_eq!(
            content_type.as_deref(),
            Some("application/json"),
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        Ok((status, serde_json::from_slice(&bytes)?))
    }

    fn vault_job(&self, job: &str) -> std::path::PathBuf {
        self.vault
            .root()
            .join(format!("registry/workflows/ops/sync/jobs/{job}.toml"))
    }

    fn shared_job(&self, job: &str) -> std::path::PathBuf {
        self.app_data.join(format!("workflows/acme/sync/jobs/{job}.toml"))
    }
}

fn vault_one(job: &str) -> String {
    format!("/api/v1/{VAULT_NAME}/workflows/ops/sync/jobs/{job}/execute/successor")
}

fn vault_all() -> String {
    format!("/api/v1/{VAULT_NAME}/workflows/ops/sync/jobs/execute/successor")
}

#[tokio::test]
async fn a_vault_successor_is_made_by_a_put_on_the_job_and_answered_with_the_report() -> Result<(), BoxError> {
    let site = site().await?;

    let (status, body) = site
        .send("PUT", &vault_one("nightly"), r#"{"new_job":"nightly-next"}"#)
        .await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["store"], VAULT_NAME);
    assert_eq!(body["release"]["kind"], "future");
    assert_eq!(body["successors"][0]["job"], "nightly-next");
    assert_eq!(
        body["successors"][0]["carries_over"], false,
        "no ledger holds the interfaces to compare"
    );
    assert!(body["successors"][0]["interface_changes"].is_null(), "{body}");
    assert!(site.vault_job("nightly-next").is_file());
    Ok(())
}

#[tokio::test]
async fn a_post_on_the_workflows_jobs_makes_a_successor_for_every_job() -> Result<(), BoxError> {
    let site = site().await?;

    let (status, body) = site.send("POST", &vault_all(), r#"{"to":"future"}"#).await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["successors"][0]["job"], "nightly-v1-1-0");
    assert!(site.vault_job("nightly-v1-1-0").is_file());
    let (empty, again) = site.send("POST", &vault_all(), "").await?;
    assert_eq!(
        empty,
        StatusCode::CONFLICT,
        "an empty body means the defaults, and the key is now taken: {again}"
    );
    Ok(())
}

#[tokio::test]
async fn a_shared_successor_uses_shared_in_place_of_the_vault() -> Result<(), BoxError> {
    let site = site().await?;

    let (status, body) = site
        .send(
            "PUT",
            "/api/v1/shared/workflows/acme/sync/jobs/nightly/execute/successor",
            r#"{"new_job":"nightly-next","to":"1.1.0"}"#,
        )
        .await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["store"], "shared");
    assert!(site.shared_job("nightly-next").is_file());
    let (again, refused) = site
        .send(
            "POST",
            "/api/v1/shared/workflows/acme/sync/jobs/execute/successor",
            "{}",
        )
        .await?;
    assert_eq!(again, StatusCode::CONFLICT, "the job now has a successor: {refused}");
    Ok(())
}

#[tokio::test]
async fn a_post_on_the_hosts_workflow_makes_a_successor_for_every_shared_job() -> Result<(), BoxError> {
    let site = site().await?;

    let (status, body) = site
        .send(
            "POST",
            "/api/v1/shared/workflows/acme/sync/jobs/execute/successor",
            "{}",
        )
        .await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["successors"][0]["job"], "nightly-v1-1-0");
    assert!(site.shared_job("nightly-v1-1-0").is_file());
    Ok(())
}

#[tokio::test]
async fn each_kind_of_refusal_has_its_status_and_the_error_document() -> Result<(), BoxError> {
    let site = site().await?;
    support::shared::write(&site.vault_job("taken"), support::jobs::JOB)?;

    let taken = site
        .send("PUT", &vault_one("nightly"), r#"{"new_job":"taken"}"#)
        .await?;
    let invalid_key = site
        .send("PUT", &vault_one("nightly"), r#"{"new_job":"future"}"#)
        .await?;
    let missing_job = site
        .send("PUT", &vault_one("absent"), r#"{"new_job":"absent-next"}"#)
        .await?;
    let not_installed = site
        .send("PUT", &vault_one("nightly"), r#"{"new_job":"n","to":"9.0.0"}"#)
        .await?;
    let unknown_vault = site
        .send(
            "PUT",
            "/api/v1/nowhere/workflows/ops/sync/jobs/nightly/execute/successor",
            r#"{"new_job":"n"}"#,
        )
        .await?;

    assert_eq!(
        (taken.0, taken.1["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("workflow/job-exists"))
    );
    assert_eq!(
        (invalid_key.0, invalid_key.1["error"]["code"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("workflow/job-key-invalid"))
    );
    assert_eq!(
        (missing_job.0, missing_job.1["error"]["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("workflow/job-not-found"))
    );
    assert_eq!(
        (not_installed.0, not_installed.1["error"]["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("workflow/package-unavailable"))
    );
    assert_eq!(unknown_vault.0, StatusCode::NOT_FOUND);
    assert!(!site.vault_job("n").exists());
    Ok(())
}

#[tokio::test]
async fn a_malformed_body_is_a_bad_request_and_changes_nothing() -> Result<(), BoxError> {
    let site = site().await?;

    for (method, path, body) in [
        ("PUT", vault_one("nightly"), "{}"),
        ("PUT", vault_one("nightly"), r#"{"new_job":"n","extra":1}"#),
        ("PUT", vault_one("nightly"), r#"{"new_job":"n","to":"^1"}"#),
        ("PUT", vault_one("nightly"), "not json"),
        ("POST", vault_all(), r#"{"to":"later"}"#),
        ("POST", vault_all(), r#"{"new_job":"n"}"#),
    ] {
        let (status, answer) = site.send(method, &path, body).await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {path} {body}: {answer}");
        assert_eq!(answer["error"]["code"], "workflow/request-invalid", "{body}");
    }
    assert!(!site.vault_job("n").exists());
    assert!(!site.vault_job("nightly-v1-1-0").exists());
    Ok(())
}

#[tokio::test]
async fn the_page_routes_for_the_same_job_are_unchanged() -> Result<(), BoxError> {
    let site = site().await?;
    let response = site
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/{VAULT_NAME}/workflows/ops/sync/jobs/nightly"))
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}
