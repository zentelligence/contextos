//! A vault's packages page lists each library release with the workflow releases locked to it, through the real router, a
//! real `contextos` session, and a real temporary vault.

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

/// A vault with the library `crm` at 1.0.0 and 1.1.0, and `ops` current on 1.0.0 locked to `crm` 1.0.0.
async fn vault() -> Result<JobVault, BoxError> {
    let lock = format!(
        "[ops.sync]\ncurrent = \"1.0.0\"\n\n[ops.sync.\"1.0.0\".lock.crm]\nversion = \"1.0.0\"\ndigest = \"sha256:{}\"\n",
        "0".repeat(64)
    );
    let vault = JobVault::open(&Spec {
        package: PACKAGE,
        workflow: WORKFLOW,
        script: ENTRY,
        state: Some(&lock),
        tasks: false,
        ..Spec::default()
    })
    .await?;
    vault.write("registry/libraries/crm/library.toml", LIBRARY)?;
    vault.write("registry/libraries/crm/client.rhai", "fn a() { 1 }\n")?;
    vault.install_releases()?;
    support::release_fixture::write_whole_release(
        &vault.root().join(".contextos/packages/crm/1.1.0"),
        ("crm", "1.1.0"),
        "",
        &[("library.toml", LIBRARY.as_bytes()), ("client.rhai", b"fn a() { 2 }\n")],
    )
    .map_err(|error| error.to_string())?;
    Ok(vault)
}

fn router_over(vault: &JobVault) -> Router {
    contextos_web::build_router(
        std::sync::Arc::clone(&vault.clients),
        Some(vault.config.path()),
        &vault.config.path().join("web.toml"),
    )
}

async fn get(vault: &JobVault, path: &str, htmx: bool) -> Result<(StatusCode, String), BoxError> {
    let mut request = Request::builder().method(Method::GET).uri(path);
    if htmx {
        request = request.header("hx-request", "true");
    }
    let response = router_over(vault).oneshot(request.body(Body::empty())?).await?;
    let status = response.status();
    Ok((
        status,
        String::from_utf8_lossy(&to_bytes(response.into_body(), usize::MAX).await?).into_owned(),
    ))
}

#[tokio::test]
async fn the_packages_page_lists_each_library_release_with_the_workflow_release_locked_to_it() -> Result<(), BoxError> {
    let vault = vault().await?;

    let (status, page) = get(&vault, &format!("/{VAULT_NAME}/packages"), true).await?;

    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("<strong>crm 1.0.0</strong>"), "{page}");
    assert!(page.contains("<strong>crm 1.1.0</strong>"), "{page}");
    assert!(
        page.contains(&format!(
            "<a href=\"/{VAULT_NAME}/workflows/ops/sync\">ops sync, current</a>"
        )),
        "{page}"
    );
    assert!(
        page.contains("Older than the newest release, and still locked"),
        "{page}"
    );
    assert!(page.contains("No workflow release is locked to it."), "{page}");
    Ok(())
}

#[tokio::test]
async fn the_packages_page_is_a_full_page_for_a_full_navigation_and_the_workflows_list_links_to_it()
-> Result<(), BoxError> {
    let vault = vault().await?;

    let (status, page) = get(&vault, &format!("/{VAULT_NAME}/packages"), false).await?;
    let (_, list) = get(&vault, &format!("/{VAULT_NAME}/workflows"), true).await?;

    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("<html"), "{page}");
    assert!(list.contains(&format!("href=\"/{VAULT_NAME}/packages\"")), "{list}");
    assert!(list.contains(">Library Releases</a>"), "{list}");
    assert!(!list.contains("Installed packages"), "{list}");
    assert!(list.contains("href=\"/shared/workflows\""), "{list}");
    Ok(())
}

#[tokio::test]
async fn the_packages_page_of_a_vault_that_does_not_exist_is_not_found() -> Result<(), BoxError> {
    let vault = vault().await?;

    let (status, _) = get(&vault, "/no-such-vault/packages", true).await?;

    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn a_library_release_that_does_not_read_whole_is_listed_with_its_manifest_failed() -> Result<(), BoxError> {
    let vault = vault().await?;
    std::fs::write(
        vault.root().join(".contextos/packages/crm/1.0.0/client.rhai"),
        "fn a() { 3 }\n",
    )?;

    let (status, page) = get(&vault, &format!("/{VAULT_NAME}/packages"), true).await?;

    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("<strong>crm 1.0.0</strong>"), "{page}");
    assert!(
        page.contains("<dt>Manifest</dt><dd class=\"check-failed\">Failed"),
        "{page}"
    );
    assert!(
        page.contains("<dt>Syntax</dt><dd class=\"check-skipped\">Not checked"),
        "{page}"
    );
    Ok(())
}
