//! "Resume where I left off" contract tests: real `contextos-mcp` sessions against real temporary
//! vault fixtures, exercising the Wiki, Insights, and entity-folder sidebar roots' own
//! resume-last-visited redirect. No mocking, per `testing.md`.

mod support;

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode, header};

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

async fn get(router: &Router, path: &str) -> Result<(StatusCode, String, HeaderMap), BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty())?)
        .await?;
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned(), headers))
}

async fn get_with_cookie(router: &Router, path: &str, cookie: &str) -> Result<(StatusCode, HeaderMap), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header(header::COOKIE, HeaderValue::from_str(cookie)?)
                .body(Body::empty())?,
        )
        .await?;
    Ok((response.status(), response.headers().clone()))
}

async fn get_with_cookie_as_hx_request(
    router: &Router,
    path: &str,
    cookie: &str,
) -> Result<(StatusCode, HeaderMap), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header(header::COOKIE, HeaderValue::from_str(cookie)?)
                .header("hx-request", "true")
                .body(Body::empty())?,
        )
        .await?;
    Ok((response.status(), response.headers().clone()))
}

/// The `cc_last_visited` cookie's own `Set-Cookie` value, ready to send straight back as a
/// `Cookie` request header (the two headers share the same `name=value` prefix; everything after
/// the first `;` is `Set-Cookie`-only attribute syntax a `Cookie` header must not repeat).
fn last_visited_cookie(headers: &HeaderMap) -> Result<String, BoxError> {
    let raw = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with("cc_last_visited="))
        .ok_or("expected a cc_last_visited Set-Cookie header")?;
    let value_only = raw.split(';').next().ok_or("cookie header had no value")?;
    Ok(value_only.to_owned())
}

// ---------------------------------------------------------------------
// Wiki
// ---------------------------------------------------------------------

// Two layers record this one visit, each in its own cookie, so neither may replace the other's header.
#[tokio::test]
async fn visiting_a_wiki_page_records_it_as_last_visited_and_recently_accessed() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/wiki/business/finance/tax.md", "# Tax\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, _, headers) = get(&router, &format!("/{VAULT_NAME}/wiki/business/finance/tax.md")).await?;

    let names: Vec<&str> = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|value| value.split('=').next())
        .collect();
    assert!(names.contains(&"cc_last_visited"), "{names:?}");
    assert!(names.contains(&"cc_recent"), "{names:?}");
    Ok(())
}

#[tokio::test]
async fn visiting_a_wiki_page_then_returning_to_the_wiki_root_resumes_it() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/wiki/business/finance/tax.md", "# Tax\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, _, visit_headers) = get(&router, &format!("/{VAULT_NAME}/wiki/business/finance/tax.md")).await?;
    let cookie = last_visited_cookie(&visit_headers)?;

    let (status, headers) = get_with_cookie(&router, &format!("/{VAULT_NAME}/wiki/"), &cookie).await?;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let target = headers
        .get(header::LOCATION)
        .ok_or("expected a Location header")?
        .to_str()?;
    assert_eq!(target, format!("/{VAULT_NAME}/wiki/business/finance/tax.md"));
    Ok(())
}

#[tokio::test]
async fn the_wiki_root_with_no_last_visited_cookie_shows_the_ordinary_landing_view() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/wiki/business/finance/tax.md", "# Tax\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, _) = get(&router, &format!("/{VAULT_NAME}/wiki/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("business"), "{body}");
    Ok(())
}

#[tokio::test]
async fn a_last_visited_cookie_naming_a_deleted_page_falls_back_to_the_landing_view() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/wiki/business/finance/tax.md", "# Tax\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, _, visit_headers) = get(&router, &format!("/{VAULT_NAME}/wiki/business/finance/tax.md")).await?;
    let cookie = last_visited_cookie(&visit_headers)?;
    std::fs::remove_file(vault_dir.path().join("memory/wiki/business/finance/tax.md"))?;

    let (status, headers) = get_with_cookie(&router, &format!("/{VAULT_NAME}/wiki/"), &cookie).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(!headers.contains_key(header::LOCATION));
    Ok(())
}

#[tokio::test]
async fn resuming_via_an_hx_request_uses_hx_redirect_rather_than_a_real_redirect() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/wiki/business/finance/tax.md", "# Tax\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, _, visit_headers) = get(&router, &format!("/{VAULT_NAME}/wiki/business/finance/tax.md")).await?;
    let cookie = last_visited_cookie(&visit_headers)?;

    let (status, headers) = get_with_cookie_as_hx_request(&router, &format!("/{VAULT_NAME}/wiki/"), &cookie).await?;
    assert_eq!(status, StatusCode::OK);
    let target = headers
        .get("HX-Redirect")
        .ok_or("expected an HX-Redirect header")?
        .to_str()?;
    assert_eq!(target, format!("/{VAULT_NAME}/wiki/business/finance/tax.md"));
    Ok(())
}

#[tokio::test]
async fn opening_a_wiki_pages_own_edit_form_also_counts_as_visiting_it() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/wiki/business/finance/tax.md", "# Tax\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, _, visit_headers) = get(&router, &format!("/{VAULT_NAME}/wiki/business/finance/tax.md/edit")).await?;
    let cookie = last_visited_cookie(&visit_headers)?;

    let (status, headers) = get_with_cookie(&router, &format!("/{VAULT_NAME}/wiki/"), &cookie).await?;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let target = headers
        .get(header::LOCATION)
        .ok_or("expected a Location header")?
        .to_str()?;
    assert_eq!(target, format!("/{VAULT_NAME}/wiki/business/finance/tax.md"));
    Ok(())
}

// ---------------------------------------------------------------------
// Insights
// ---------------------------------------------------------------------

#[tokio::test]
async fn visiting_an_insight_note_then_returning_to_the_insights_root_resumes_it() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/insights/a/agent-backbone.md",
        "---\nstatus: proposed\n---\n\n# Agent backbone\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, _, visit_headers) = get(&router, &format!("/{VAULT_NAME}/insights/a/agent-backbone.md")).await?;
    let cookie = last_visited_cookie(&visit_headers)?;

    let (status, headers) = get_with_cookie(&router, &format!("/{VAULT_NAME}/insights/"), &cookie).await?;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let target = headers
        .get(header::LOCATION)
        .ok_or("expected a Location header")?
        .to_str()?;
    assert_eq!(target, format!("/{VAULT_NAME}/insights/a/agent-backbone.md"));
    Ok(())
}

#[tokio::test]
async fn the_insights_root_with_no_last_visited_cookie_shows_the_ordinary_landing_view() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _, headers) = get(&router, &format!("/{VAULT_NAME}/insights/")).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(!headers.contains_key(header::LOCATION));
    Ok(())
}

#[tokio::test]
async fn a_last_visited_insights_cookie_naming_a_deleted_note_falls_back_to_the_landing_view() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/insights/a/agent-backbone.md",
        "---\nstatus: proposed\n---\n\n# Agent backbone\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, _, visit_headers) = get(&router, &format!("/{VAULT_NAME}/insights/a/agent-backbone.md")).await?;
    let cookie = last_visited_cookie(&visit_headers)?;
    std::fs::remove_file(vault_dir.path().join("memory/insights/a/agent-backbone.md"))?;

    let (status, headers) = get_with_cookie(&router, &format!("/{VAULT_NAME}/insights/"), &cookie).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(!headers.contains_key(header::LOCATION));
    Ok(())
}

// ---------------------------------------------------------------------
// Entity folders
// ---------------------------------------------------------------------

fn seed_one_entity(vault_dir: &Path) -> std::io::Result<()> {
    write(
        vault_dir,
        "memory/entities/index.md",
        "# Entities\n\n| File | Entity |\n| --- | --- |\n| zenith.md | Zenith |\n",
    )
}

#[tokio::test]
async fn visiting_an_entity_file_then_returning_to_that_entitys_own_root_resumes_it() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    write(vault_dir.path(), "zenith/finance/overview.md", "# Overview\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, _, visit_headers) = get(&router, &format!("/{VAULT_NAME}/entity/zenith/finance/overview.md")).await?;
    let cookie = last_visited_cookie(&visit_headers)?;

    let (status, headers) = get_with_cookie(&router, &format!("/{VAULT_NAME}/entity/zenith"), &cookie).await?;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let target = headers
        .get(header::LOCATION)
        .ok_or("expected a Location header")?
        .to_str()?;
    assert_eq!(target, format!("/{VAULT_NAME}/entity/zenith/finance/overview.md"));
    Ok(())
}

#[tokio::test]
async fn a_last_visited_entry_for_a_different_entity_does_not_redirect_this_ones_root() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/entities/index.md",
        "# Entities\n\n| File | Entity |\n| --- | --- |\n\
         | zenith.md | Zenith |\n| acme.md | Acme |\n",
    )?;
    write(vault_dir.path(), "zenith/finance/overview.md", "# Overview\n")?;
    write(vault_dir.path(), "acme/onboarding.md", "# Onboarding\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, _, visit_headers) = get(&router, &format!("/{VAULT_NAME}/entity/zenith/finance/overview.md")).await?;
    let cookie = last_visited_cookie(&visit_headers)?;

    let (status, headers) = get_with_cookie(&router, &format!("/{VAULT_NAME}/entity/acme"), &cookie).await?;
    assert_eq!(status, StatusCode::OK);
    assert!(!headers.contains_key(header::LOCATION));
    Ok(())
}
