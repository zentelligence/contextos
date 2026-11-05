//! What the Vault Health pane says about a vault's workflow releases, through the real router, a real `contextos`
//! session, and a real temporary vault: releases that are incomplete or damaged, templates that are gone, folders an
//! unfinished operation left, a ledger that disagrees with the store, and jobs that name releases the store lacks.

mod support;

use std::path::Path;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request};
use support::jobs::{JobVault, Spec, VAULT_NAME};
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const WORKFLOW: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\n";
const PACKAGE: &str = "workflows = [\"sync\"]\ntemplates = [\"letter.md\"]\n";

/// A vault holding the source of the package `ops` and its job, installed with a ledger, and the router over it.
async fn open() -> Result<JobVault, BoxError> {
    let vault = JobVault::open(&Spec {
        package: PACKAGE,
        workflow: WORKFLOW,
        ..Spec::default()
    })
    .await?;
    vault.write("registry/workflows/ops/letter.md", "Dear you\n")?;
    vault.install_releases()?;
    vault.rebuild_ledger().await?;
    Ok(vault)
}

fn router_over(vault: &JobVault) -> Router {
    contextos_web::build_router(
        std::sync::Arc::clone(&vault.clients),
        Some(vault.config.path()),
        &vault.config.path().join("web.toml"),
    )
}

async fn pane(router: &Router) -> Result<String, BoxError> {
    let request = Request::builder()
        .method(Method::GET)
        .uri(format!("/{VAULT_NAME}/health"))
        .header("hx-request", "true")
        .body(Body::empty())?;
    let response = router.clone().oneshot(request).await?;
    Ok(String::from_utf8_lossy(&to_bytes(response.into_body(), usize::MAX).await?).into_owned())
}

/// The pane over a vault with `ops` installed, after `damage` has been done to the vault.
async fn pane_after(damage: impl FnOnce(&Path) -> std::io::Result<()>) -> Result<String, BoxError> {
    let vault = open().await?;
    damage(vault.root())?;
    pane(&router_over(&vault)).await
}

#[tokio::test]
async fn a_store_of_whole_releases_is_reported_whole() -> Result<(), BoxError> {
    let body = pane_after(|_| Ok(())).await?;

    assert!(body.contains("Workflow releases"), "{body}");
    assert!(body.contains("are whole"), "{body}");
    Ok(())
}

#[tokio::test]
async fn an_incomplete_release_is_flagged_with_the_entry_it_lost() -> Result<(), BoxError> {
    let body =
        pane_after(|vault| std::fs::remove_file(vault.join(".contextos/packages/ops/1.0.0/sync/sync.rhai"))).await?;

    assert!(body.contains("ops 1.0.0 is incomplete"), "{body}");
    assert!(body.contains("sync/sync.rhai"), "{body}");
    assert!(body.contains("Needs you"), "{body}");
    Ok(())
}

#[tokio::test]
async fn a_damaged_release_is_flagged_as_damaged() -> Result<(), BoxError> {
    let body = pane_after(|vault| {
        std::fs::write(
            vault.join(".contextos/packages/ops/1.0.0/sync/sync.rhai"),
            "log_info(\"tampered\");",
        )
    })
    .await?;

    assert!(body.contains("ops 1.0.0 is damaged"), "{body}");
    Ok(())
}

#[tokio::test]
async fn a_template_the_release_declares_and_source_lost_is_flagged() -> Result<(), BoxError> {
    let body = pane_after(|vault| std::fs::remove_file(vault.join("registry/workflows/ops/letter.md"))).await?;

    assert!(
        body.contains("ops/sync release 1.0.0 is missing the templates letter.md"),
        "{body}"
    );
    Ok(())
}

#[tokio::test]
async fn folders_an_unfinished_operation_left_are_flagged_by_name() -> Result<(), BoxError> {
    let body = pane_after(|vault| {
        std::fs::create_dir_all(vault.join(".contextos/packages/_staging-abc123"))?;
        std::fs::create_dir_all(vault.join(".contextos/packages/_removing-def456"))
    })
    .await?;

    assert!(body.contains("_staging-abc123"), "{body}");
    assert!(body.contains("_removing-def456"), "{body}");
    Ok(())
}

#[tokio::test]
async fn a_ledger_that_is_gone_from_a_store_that_holds_releases_is_flagged() -> Result<(), BoxError> {
    let body = pane_after(|vault| std::fs::remove_file(vault.join(".contextos/packages/ledger.toml"))).await?;

    assert!(body.contains("has no ledger"), "{body}");
    Ok(())
}

#[tokio::test]
async fn jobs_that_name_releases_in_a_store_that_does_not_exist_are_flagged() -> Result<(), BoxError> {
    let body = pane_after(|vault| std::fs::remove_dir_all(vault.join(".contextos/packages"))).await?;

    assert!(body.contains("There is no package store in this vault"), "{body}");
    assert!(body.contains("ops/sync/nightly"), "{body}");
    Ok(())
}
