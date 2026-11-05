//! `GET /shell/favourites` contract tests: real `contextos-mcp` sessions against real temporary vault fixtures,
//! exercising the header Favourites panel's "Favourite apps", "Favourite jobs", and "Recently accessed"
//! sections end to end. No mocking, per `testing.md`.

mod support;

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderValue, Request, StatusCode, header};
use contextos_web::McpClientSet;
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT_NAME: &str = "contract-fixture";

async fn router_over(vault_dir: &Path, config_dir: &Path) -> Result<Router, BoxError> {
    let config_path = support::write_vault_config(config_dir, vault_dir)?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    Ok(contextos_web::build_router(
        clients,
        Some(config_dir),
        &config_dir.join("web.toml"),
    ))
}

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

fn write_app(
    vault_dir: &Path,
    dir_name: &str,
    manifest_toml: &str,
    entry_name: &str,
    entry_content: &str,
) -> std::io::Result<()> {
    write(
        vault_dir,
        &format!("registry/apps/{dir_name}/manifest.toml"),
        manifest_toml,
    )?;
    write(
        vault_dir,
        &format!("registry/apps/{dir_name}/{entry_name}"),
        entry_content,
    )
}

fn write_workflow(
    vault_dir: &Path,
    workflow_slug: &str,
    workflow_toml: &str,
    entry_name: &str,
    entry_content: &str,
) -> std::io::Result<()> {
    write(
        vault_dir,
        &format!("registry/workflows/{workflow_slug}/workflows.toml"),
        &format!("workflows = [{workflow_slug:?}]\n"),
    )?;
    write(
        vault_dir,
        &format!("registry/workflows/{workflow_slug}/{workflow_slug}/workflow.toml"),
        workflow_toml,
    )?;
    write(
        vault_dir,
        &format!("registry/workflows/{workflow_slug}/{workflow_slug}/{entry_name}"),
        entry_content,
    )
}

fn write_job(vault_dir: &Path, workflow_slug: &str, job_slug: &str, job_toml: &str) -> std::io::Result<()> {
    write(
        vault_dir,
        &format!("registry/workflows/{workflow_slug}/{workflow_slug}/jobs/{job_slug}.toml"),
        job_toml,
    )
}

async fn get(
    router: &Router,
    path: &str,
    cookie: Option<&str>,
) -> Result<(StatusCode, String, Option<String>), BoxError> {
    let mut builder = Request::builder().uri(path);
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    let response = router.clone().oneshot(builder.body(Body::empty())?).await?;
    let status = response.status();
    let set_cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .map(std::borrow::ToOwned::to_owned);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned(), set_cookie))
}

/// `PATCH` with a `{"favourite": ...}` JSON body, matching the star toggle's own real request shape.
async fn patch_favourite(router: &Router, path: &str, favourite: bool) -> Result<StatusCode, BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(path)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!("{{\"favourite\":{favourite}}}")))?,
        )
        .await?;
    Ok(response.status())
}

/// The cookie's own `name=value` pair, stripped of `; Path=...; SameSite=...`, ready to send straight back as a
/// `Cookie` request header on the next call.
fn cookie_pair(set_cookie: &str) -> Result<String, BoxError> {
    Ok(set_cookie
        .split(';')
        .next()
        .ok_or("Set-Cookie header had no value")?
        .to_owned())
}

#[tokio::test]
async fn the_panel_lists_only_favourited_apps_for_the_requested_vault() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write_app(
        vault_dir.path(),
        "task-register",
        "name = \"Task Register Dashboard\"\nkind = \"spa\"\nentry = \"index.html\"\n",
        "index.html",
        "<html></html>",
    )?;
    write_app(
        vault_dir.path(),
        "unstarred",
        "name = \"Unstarred App\"\nkind = \"spa\"\nentry = \"index.html\"\n",
        "index.html",
        "<html></html>",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_status, body, _cookie) = get(&router, &format!("/shell/favourites?vault={VAULT_NAME}"), None).await?;
    assert!(!body.contains("Task Register Dashboard"));
    assert!(!body.contains("Unstarred App"));

    let status = patch_favourite(&router, &format!("/{VAULT_NAME}/apps/task-register"), true).await?;
    assert_eq!(status, StatusCode::OK);

    let (status, body, _cookie) = get(&router, &format!("/shell/favourites?vault={VAULT_NAME}"), None).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Task Register Dashboard"));
    assert!(!body.contains("Unstarred App"));
    Ok(())
}

#[tokio::test]
async fn the_panel_with_no_vault_query_renders_no_favourited_apps() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write_app(
        vault_dir.path(),
        "task-register",
        "name = \"Task Register Dashboard\"\nkind = \"spa\"\nentry = \"index.html\"\n",
        "index.html",
        "<html></html>",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;
    patch_favourite(&router, &format!("/{VAULT_NAME}/apps/task-register"), true).await?;

    let (status, body, _cookie) = get(&router, "/shell/favourites", None).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("Task Register Dashboard"));
    assert!(body.contains("No favourited apps"));
    Ok(())
}

#[tokio::test]
async fn the_panel_lists_favourited_jobs_for_the_requested_vault() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write_workflow(
        vault_dir.path(),
        "sync-crm-contacts",
        "name = \"Sync CRM Contacts workflow\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = []\n",
        "workflow.rhai",
        "()",
    )?;
    write_job(
        vault_dir.path(),
        "sync-crm-contacts",
        "nightly",
        "name         = \"Sync CRM Contacts\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_status, body, _cookie) = get(&router, &format!("/shell/favourites?vault={VAULT_NAME}"), None).await?;
    assert!(!body.contains("Sync CRM Contacts"));

    patch_favourite(
        &router,
        &format!("/{VAULT_NAME}/workflows/sync-crm-contacts/sync-crm-contacts/jobs/nightly"),
        true,
    )
    .await?;

    let (status, body, _cookie) = get(&router, &format!("/shell/favourites?vault={VAULT_NAME}"), None).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Sync CRM Contacts"));
    assert!(body.contains(&format!(
        "/{VAULT_NAME}/workflows/sync-crm-contacts/sync-crm-contacts/jobs/nightly"
    )));
    Ok(())
}

#[tokio::test]
async fn the_panel_renders_all_four_sections_with_empty_views() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, _cookie) = get(&router, &format!("/shell/favourites?vault={VAULT_NAME}"), None).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Favourite views"));
    assert!(body.contains("Favourite jobs"));
    assert!(body.contains("No favourited views"));
    assert!(body.contains("No favourited jobs"));
    Ok(())
}

#[tokio::test]
async fn visiting_pages_populates_recently_accessed_and_the_panel_reads_it_back() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body, set_cookie) = get(&router, &format!("/{VAULT_NAME}/tasks/"), None).await?;
    assert_eq!(status, StatusCode::OK);
    let cookie = cookie_pair(&set_cookie.ok_or("expected a Set-Cookie header on a tracked navigation")?)?;

    let (_status, body, _cookie) = get(&router, "/shell/favourites", Some(&cookie)).await?;
    assert!(body.contains("Recently accessed"));
    assert!(body.contains(&format!("href=\"/{VAULT_NAME}/tasks/\"")));
    assert!(body.contains("Tasks"));
    Ok(())
}

#[tokio::test]
async fn an_htmx_fragment_request_does_not_update_recently_accessed() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/{VAULT_NAME}/tasks/"))
                .header("hx-request", HeaderValue::from_static("true"))
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get(header::SET_COOKIE).is_none());
    Ok(())
}

#[tokio::test]
async fn favourite_views_round_trip_and_reject_unknown_destinations() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    write(vault.path(), "index.md", "# Root\n")?;
    let router = router_over(vault.path(), config.path()).await?;
    let endpoint = format!("/{VAULT_NAME}/views/tasks");
    assert_eq!(patch_favourite(&router, &endpoint, true).await?, StatusCode::OK);
    let stored = std::fs::read_to_string(vault.path().join(".contextos/views.toml"))?;
    assert!(stored.contains("[views.tasks]"));
    assert!(stored.contains("favourite = true"));
    let (_, body, _) = get(&router, &format!("/shell/favourites?vault={VAULT_NAME}"), None).await?;
    assert!(body.contains(&format!("href=\"/{VAULT_NAME}/tasks/\"")));
    assert_eq!(patch_favourite(&router, &endpoint, false).await?, StatusCode::OK);
    let (_, body, _) = get(&router, &format!("/shell/favourites?vault={VAULT_NAME}"), None).await?;
    assert!(body.contains("No favourited views"));
    assert_eq!(
        patch_favourite(&router, &format!("/{VAULT_NAME}/views/made-up"), true).await?,
        StatusCode::NOT_FOUND
    );
    write(vault.path(), ".contextos/views.toml", "malformed = [")?;
    assert!(!patch_favourite(&router, &endpoint, true).await?.is_success());
    assert_eq!(
        std::fs::read_to_string(vault.path().join(".contextos/views.toml"))?,
        "malformed = ["
    );
    Ok(())
}
