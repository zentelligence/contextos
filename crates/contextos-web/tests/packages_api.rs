//! `/api/v1/{vault}/packages` and `/api/v1/server/packages` through the real router over a real vault behind a real
//! `contextos` session and a real host store: the JSON each answers with, the guarded `DELETE`, the forced
//! `PUT .../execute/remove`, and the status of each refusal.

mod support;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use contextos_web::package_store::AppDataRoots;
use contextos_web::package_stores::VaultPackagesOf;
use serde_json::Value;
use support::BoxError;
use support::jobs::{JobVault, Spec, VAULT_NAME};
use tower::ServiceExt as _;

const STATE: &str =
    "[ops.sync]\ncurrent = \"1.0.0\"\n\n[ops.sync.nightly]\nlifecycle = \"enabled\"\nrelease = \"1.0.0\"\n";

struct Site {
    vault: JobVault,
    router: Router,
}

/// A vault whose store holds `ops` 1.0.0, recorded in its ledger and as the current release of `sync`, with `nightly`
/// enabled on it; and a host store holding nothing.
async fn site() -> Result<Site, BoxError> {
    site_with(STATE).await
}

/// As [`site`], with the job state `state`.
async fn site_with(state: &str) -> Result<Site, BoxError> {
    let spec = Spec {
        state: Some(state),
        ..Spec::default()
    };
    let vault = JobVault::open(&spec).await?;
    vault.install_releases()?;
    let client = vault.clients.primary().ok_or("no primary session")?;
    VaultPackagesOf {
        client: &client,
        vault: VAULT_NAME,
    }
    .rebuild_ledger()
    .await?;
    let app_data = vault.config.path().to_path_buf();
    let roots = AppDataRoots::from(app_data.as_path());
    let router = contextos_web::build_router_in(
        std::sync::Arc::clone(&vault.clients),
        Some(&app_data),
        &app_data.join("web.toml"),
        &contextos_web::config::WorkflowsConfig::default(),
        Some(&roots),
    );
    Ok(Site { vault, router })
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
        let bytes = to_bytes(response.into_body(), usize::MAX).await?;
        let document = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)?
        };
        Ok((status, document))
    }

    fn release_folder(&self) -> std::path::PathBuf {
        self.vault.root().join(".contextos/packages/ops/1.0.0")
    }
}

fn packages() -> String {
    format!("/api/v1/{VAULT_NAME}/packages")
}

fn release() -> String {
    format!("/api/v1/{VAULT_NAME}/packages/ops/versions/1.0.0")
}

#[tokio::test]
async fn a_vault_store_is_listed_with_its_ledger_and_what_keeps_each_release() -> Result<(), BoxError> {
    let site = site().await?;

    let (status, body) = site.send("GET", &packages(), "").await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["store"], VAULT_NAME);
    assert_eq!(body["ledger"]["state"], "present");
    assert_eq!(body["releases"][0]["name"], "ops");
    assert_eq!(body["releases"][0]["workflows"][0]["kind"], "current");
    assert_eq!(body["releases"][0]["kept_by"][0]["reason"], "current");
    assert_eq!(body["releases"][0]["kept_by"][0]["guards_removal"], true);
    Ok(())
}

#[tokio::test]
async fn one_release_of_a_vault_store_is_inspected_and_one_that_is_not_there_is_404() -> Result<(), BoxError> {
    let site = site().await?;

    let (found, body) = site.send("GET", &release(), "").await?;
    let (missing, answer) = site
        .send("GET", &format!("/api/v1/{VAULT_NAME}/packages/ops/versions/9.9.9"), "")
        .await?;

    assert_eq!(found, StatusCode::OK);
    assert_eq!(
        (body["name"].as_str(), body["version"].as_str()),
        (Some("ops"), Some("1.0.0"))
    );
    assert_eq!(missing, StatusCode::NOT_FOUND);
    assert_eq!(answer["error"]["code"], "packages/not-found");
    Ok(())
}

#[tokio::test]
async fn deleting_the_current_release_is_refused_with_409_naming_why_and_removes_nothing() -> Result<(), BoxError> {
    let site = site().await?;

    let (status, body) = site.send("DELETE", &release(), "").await?;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "packages/in-use");
    assert_eq!(body["error"]["reasons"][0]["reason"], "current");
    assert!(site.release_folder().is_dir());
    Ok(())
}

#[tokio::test]
async fn a_forced_removal_removes_the_release_and_names_the_job_the_next_pass_suspends() -> Result<(), BoxError> {
    let site = site().await?;

    let (status, body) = site
        .send("PUT", &format!("{}/execute/remove", release()), r#"{"force":true}"#)
        .await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["store"], VAULT_NAME);
    assert_eq!(body["cause"], "forced");
    assert_eq!(body["affected_jobs"][0]["job"], "nightly");
    assert_eq!(body["affected_jobs"][0]["lifecycle"], "enabled");
    assert!(!site.release_folder().exists());
    Ok(())
}

#[tokio::test]
async fn a_store_whose_ledger_is_missing_refuses_the_removal_with_409() -> Result<(), BoxError> {
    let site = site().await?;
    std::fs::remove_file(site.vault.root().join(".contextos/packages/ledger.toml"))?;

    let (status, body) = site
        .send("PUT", &format!("{}/execute/remove", release()), r#"{"force":true}"#)
        .await?;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "packages/ledger-unreadable");
    assert!(site.release_folder().is_dir());
    Ok(())
}

#[tokio::test]
async fn a_vault_the_server_does_not_serve_is_404_and_a_query_on_a_delete_is_400() -> Result<(), BoxError> {
    let site = site().await?;

    let (unknown, body) = site.send("GET", "/api/v1/no-such-vault/packages", "").await?;
    let (queried, answer) = site.send("DELETE", &format!("{}?force=true", release()), "").await?;

    assert_eq!(unknown, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "vault/not-found");
    assert_eq!(queried, StatusCode::BAD_REQUEST);
    assert_eq!(answer["error"]["code"], "packages/invalid-query");
    assert!(site.release_folder().is_dir());
    Ok(())
}

#[tokio::test]
async fn the_host_store_is_reached_under_shared_by_the_same_router() -> Result<(), BoxError> {
    let site = site().await?;

    let (status, body) = site.send("GET", "/api/v1/server/packages", "").await?;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["store"], "shared");
    assert_eq!(body["releases"], serde_json::json!([]));
    Ok(())
}

#[tokio::test]
async fn deleting_a_release_no_rule_holds_removes_it_and_answers_204_with_no_body() -> Result<(), BoxError> {
    let site = site_with("[ops.sync]\nfuture = \"1.0.0\"\n").await?;
    std::fs::remove_file(site.vault.root().join("registry/workflows/ops/sync/jobs/nightly.toml"))?;

    let (status, body) = site.send("DELETE", &release(), "").await?;

    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(body, Value::Null);
    assert!(!site.release_folder().exists());
    Ok(())
}

/// Damages the release's entry script, so the release no longer reads whole.
fn damage(site: &Site) -> Result<(), BoxError> {
    fn first_script(folder: &std::path::Path) -> Option<std::path::PathBuf> {
        for entry in std::fs::read_dir(folder).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(found) = first_script(&path) {
                    return Some(found);
                }
            } else if path.extension().is_some_and(|extension| extension == "rhai") {
                return Some(path);
            }
        }
        None
    }
    let script = first_script(&site.release_folder()).ok_or("the release holds no script")?;
    std::fs::write(script, b"tampered")?;
    Ok(())
}

#[tokio::test]
async fn a_damaged_release_a_job_runs_on_is_refused_with_409_and_forcing_it_names_the_job() -> Result<(), BoxError> {
    let site = site().await?;
    damage(&site)?;

    let (refused, body) = site.send("DELETE", &release(), "").await?;
    let still_there = site.release_folder().is_dir();
    let (forced, answer) = site
        .send("PUT", &format!("{}/execute/remove", release()), r#"{"force":true}"#)
        .await?;

    assert_eq!(refused, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "packages/in-use");
    assert_eq!(body["error"]["reasons"][0]["reason"], "current");
    assert!(still_there);
    assert_eq!(forced, StatusCode::OK, "{answer}");
    assert_eq!(answer["cause"], "forced");
    assert_eq!(answer["affected_jobs"][0]["job"], "nightly");
    assert!(!site.release_folder().exists());
    Ok(())
}
