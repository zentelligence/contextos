//! Wiki screen contract tests for creating a page: `POST /{{vault_name}}/wiki/{{*path}}`
//! (the "New" action) and `GET /{{vault_name}}/wiki/{{*path}}/new` (the dedicated,
//! bookmarkable create-page form) against a real `contextos-mcp` session and a real temporary
//! vault fixture, no mocking, per `testing.md`. Kept apart from `tests/wiki.rs` (already close to
//! this workspace's own file-size guidance) rather than growing that file further, matching
//! `tests/tasks_create.rs`'s identical precedent.

mod support;

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode};

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

async fn get(router: &Router, path: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty())?)
        .await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

async fn post(router: &Router, path: &str, json_body: &str) -> Result<(StatusCode, String, HeaderMap), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(json_body.to_owned()))?,
        )
        .await?;
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned(), headers))
}

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

/// A freshly created page's own path from its response's `HX-Push-Url`
/// header (`/{{vault}}/wiki/{{path}}` - the URL itself names the
/// resource, no `?page=` to extract). Creation swaps straight into the new
/// page's own edit form in the same response, so the browser's own address
/// bar is updated in place via `HX-Push-Url` rather than a full
/// `HX-Redirect` navigation.
fn pushed_path(headers: &HeaderMap) -> Option<String> {
    let target = headers.get("HX-Push-Url")?.to_str().ok()?;
    let (_, path) = target.split_once("/wiki/")?;
    Some(path.to_owned())
}

// ---------------------------------------------------------------------
// POST /{{vault_name}}/wiki/{{*path}} - the "New" action
// ---------------------------------------------------------------------

#[tokio::test]
async fn create_on_a_domain_and_topic_depth_folder_dispatches_note_create_and_redirects() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/wiki/business/finance/.gitkeep", "")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = post(
        &router,
        &format!("/{VAULT_NAME}/wiki/business/finance"),
        r#"{"title":"Payment Terms"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let pushed = pushed_path(&headers).ok_or("expected an HX-Push-Url target")?;
    assert_eq!(pushed, "business/finance/payment-terms.md");
    // Lands directly in the new, empty page's own edit form - never its
    // read view - so there is somewhere to immediately start typing rather
    // than a separate "Edit" click on a page with nothing to render yet.
    assert!(body.contains("wiki-edit-form"), "{body}");
    assert!(body.contains("Payment Terms"), "{body}");

    let content = std::fs::read_to_string(vault_dir.path().join("memory/wiki/business/finance/payment-terms.md"))?;
    // `note_create`'s own guaranteed default frontmatter schema, proving
    // this went through `note_create` and not a direct `fs_write_file`.
    assert!(content.contains("type: wiki"), "{content}");
    assert!(content.contains("title: Payment Terms"), "{content}");
    assert!(content.contains("created:"), "{content}");
    Ok(())
}

#[tokio::test]
async fn create_at_the_wiki_root_is_refused_as_too_shallow() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = post(
        &router,
        &format!("/{VAULT_NAME}/wiki/"),
        r#"{"title":"Should not exist"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(headers.get("HX-Redirect").is_none(), "{body}");
    assert!(
        body.contains("A new page needs at least a domain and a topic folder to sit in."),
        "{body}"
    );
    assert!(!vault_dir.path().join("memory/wiki").exists());
    Ok(())
}

#[tokio::test]
async fn create_at_a_single_domain_level_is_also_refused_as_too_shallow() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/wiki/business/.gitkeep", "")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = post(
        &router,
        &format!("/{VAULT_NAME}/wiki/business"),
        r#"{"title":"Should not exist"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(headers.get("HX-Redirect").is_none(), "{body}");
    assert!(
        body.contains("A new page needs at least a domain and a topic folder to sit in."),
        "{body}"
    );
    Ok(())
}

#[tokio::test]
async fn create_with_an_empty_title_is_refused() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/wiki/business/finance/.gitkeep", "")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = post(
        &router,
        &format!("/{VAULT_NAME}/wiki/business/finance"),
        r#"{"title":"   "}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(headers.get("HX-Redirect").is_none(), "{body}");
    assert!(body.contains("A page needs a title."), "{body}");
    Ok(())
}

// ---------------------------------------------------------------------
// GET /{{vault_name}}/wiki/{{*path}}/new - the dedicated,
// bookmarkable "New page" form
// ---------------------------------------------------------------------

#[tokio::test]
async fn get_new_on_a_domain_and_topic_depth_folder_pre_fills_a_default_title() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/wiki/business/finance/.gitkeep", "")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/business/finance/new")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("wiki-create-form"), "{body}");
    // Seeded from `wiki::default_title` - "Page YYYY-MM-DD HHmm".
    assert!(body.contains("name=\"title\" value=\"Page "), "{body}");
    Ok(())
}

#[tokio::test]
async fn get_new_forms_own_submit_target_is_the_folders_own_create_endpoint() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/wiki/business/finance/.gitkeep", "")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/business/finance/new")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The dedicated form posts straight to `business/finance`'s own real
    // create endpoint (`create_dispatch`/`create_path`) - the same one the
    // header's own "New" button now merely navigates to via `hx-get`
    // instead of posting to directly.
    assert!(
        body.contains(&format!("hx-post=\"/{VAULT_NAME}/wiki/business/finance\"")),
        "{body}"
    );
    Ok(())
}

#[tokio::test]
async fn the_header_new_button_now_navigates_to_the_dedicated_form_rather_than_posting_directly() -> Result<(), BoxError>
{
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/wiki/business/finance/.gitkeep", "")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/business/finance")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Matches `routes::notes`'s own corrected header button (`hx-get .../new`,
    // never an immediate `hx-post` that creates with no form of its own to
    // review first).
    assert!(
        body.contains(&format!("hx-get=\"/{VAULT_NAME}/wiki/business/finance/new\"")),
        "{body}"
    );
    assert!(
        !body.contains(&format!("hx-post=\"/{VAULT_NAME}/wiki/business/finance\"")),
        "{body}"
    );
    Ok(())
}

#[tokio::test]
async fn posting_with_no_title_field_at_all_is_refused_rather_than_defaulted() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/wiki/business/finance/.gitkeep", "")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = post(&router, &format!("/{VAULT_NAME}/wiki/business/finance"), "{}").await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(headers.get("HX-Redirect").is_none(), "{body}");
    assert!(headers.get("HX-Push-Url").is_none(), "{body}");
    // No server-synthesised fallback title any more, matching
    // `routes::notes::CreateBody`'s own corrected "no auto-naming" contract
    // - the header "New" button always loads the dedicated `/new` form
    // first now, pre-filled and editable, so a request that carries no
    // title at all is a stale or tampered one, not a legitimate flow this
    // handler still needs to auto-name for.
    assert!(body.contains("A page needs a title."), "{body}");
    assert!(body.contains("wiki-create-form"), "{body}");
    Ok(())
}

#[tokio::test]
async fn get_new_at_the_wiki_root_shows_the_too_shallow_explanation_not_a_form() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/new")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The Wiki root can never satisfy `MIN_CREATE_DEPTH`, so `/new` here
    // shows the same explanation the POST path already gives
    // `CreateWikiPageError::TooShallow`, never a form that could not
    // possibly succeed.
    assert!(
        body.contains("A new page needs at least a domain and a topic folder to sit in."),
        "{body}"
    );
    assert!(!body.contains("wiki-create-form"), "{body}");
    Ok(())
}

#[tokio::test]
async fn get_new_at_a_single_domain_level_also_shows_the_too_shallow_explanation_not_a_form() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/wiki/business/.gitkeep", "")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/business/new")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains("A new page needs at least a domain and a topic folder to sit in."),
        "{body}"
    );
    assert!(!body.contains("wiki-create-form"), "{body}");
    Ok(())
}

#[tokio::test]
async fn get_new_on_a_folder_that_does_not_exist_is_a_404() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body) = get(&router, &format!("/{VAULT_NAME}/wiki/business/finance/new")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn posting_an_empty_title_re_renders_the_new_form_with_the_submitted_title_preserved() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/wiki/business/finance/.gitkeep", "")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = post(
        &router,
        &format!("/{VAULT_NAME}/wiki/business/finance"),
        r#"{"title":"   "}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(headers.get("HX-Redirect").is_none(), "{body}");
    assert!(body.contains("A page needs a title."), "{body}");
    // Re-renders the dedicated create form itself - never the plain folder
    // browse fragment - so the operator's own (blank) title stays right
    // there ready to be corrected, rather than losing their place.
    assert!(body.contains("wiki-create-form"), "{body}");
    assert!(body.contains("name=\"title\" value=\"   \""), "{body}");
    Ok(())
}

#[tokio::test]
async fn posting_too_shallow_still_shows_the_same_explanation_not_a_form() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = post(
        &router,
        &format!("/{VAULT_NAME}/wiki/"),
        r#"{"title":"Should not exist"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(headers.get("HX-Redirect").is_none(), "{body}");
    assert!(
        body.contains("A new page needs at least a domain and a topic folder to sit in."),
        "{body}"
    );
    assert!(!body.contains("wiki-create-form"), "{body}");
    Ok(())
}

#[tokio::test]
async fn a_page_created_through_the_dedicated_new_form_creates_it_and_lands_on_its_own_edit_form()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/wiki/business/finance/.gitkeep", "")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    // Load the dedicated form first, exactly as an operator following a
    // bookmarked or shared `/new` link would.
    let (get_status, form_body) = get(&router, &format!("/{VAULT_NAME}/wiki/business/finance/new")).await?;
    assert_eq!(get_status, StatusCode::OK, "{form_body}");
    assert!(form_body.contains("wiki-create-form"), "{form_body}");

    let (status, body, headers) = post(
        &router,
        &format!("/{VAULT_NAME}/wiki/business/finance"),
        r#"{"title":"Payment Terms"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let pushed = pushed_path(&headers).ok_or("expected an HX-Push-Url target")?;
    assert_eq!(pushed, "business/finance/payment-terms.md");
    assert!(body.contains("wiki-edit-form"), "{body}");

    let content = std::fs::read_to_string(vault_dir.path().join("memory/wiki/business/finance/payment-terms.md"))?;
    assert!(content.contains("type: wiki"), "{content}");
    assert!(content.contains("title: Payment Terms"), "{content}");
    Ok(())
}
