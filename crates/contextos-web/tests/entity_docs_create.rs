//! Entity-folder screen contract tests for creating a file: `POST /{{vault_name}}/entity/{{*path}}`
//! (the "New" action) and `GET /{{vault_name}}/entity/{{*path}}/new` (the dedicated,
//! bookmarkable create-file form) against a real `contextos-mcp` session and a real temporary vault
//! fixture, no mocking, per `testing.md`. Kept apart from `tests/entity_docs.rs` (already close to
//! this workspace's own file-size guidance) rather than growing that file further, matching
//! `tests/wiki_create.rs`'s identical precedent.

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

/// A vault with one configured entity, `zenith`, matching
/// `tests/entity_docs.rs::seed_one_entity`'s identical fixture.
fn seed_one_entity(vault_dir: &Path) -> std::io::Result<()> {
    write(
        vault_dir,
        "memory/entities/index.md",
        "# Entities\n\n| File | Entity |\n| --- | --- |\n| zenith.md | Zenith |\n",
    )
}

/// A freshly created markdown file's own path from its response's
/// `HX-Push-Url` header (`/{{vault}}/entity/{{entity_slug}}/{{path}}`,
/// the URL itself names the resource, no `?file=` to extract). Creation
/// swaps straight into the new file's own edit form in the same response,
/// so the browser's own address bar is updated in place via `HX-Push-Url`
/// rather than a full `HX-Redirect` navigation, matching `routes::wiki`'s
/// identical test helper.
fn pushed_path(headers: &HeaderMap) -> Option<String> {
    let target = headers.get("HX-Push-Url")?.to_str().ok()?;
    let (_, path) = target.split_once("/entity/zenith/")?;
    Some(path.to_owned())
}

// ---------------------------------------------------------------------
// POST /{{vault_name}}/entity/{{*path}} - the "New" file action
// ---------------------------------------------------------------------

#[tokio::test]
async fn create_at_a_nested_folder_dispatches_note_create_and_redirects() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    write(vault_dir.path(), "zenith/finance/.gitkeep", "")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = post(
        &router,
        &format!("/{VAULT_NAME}/entity/zenith/finance"),
        r#"{"title":"Payment Terms"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let pushed = pushed_path(&headers).ok_or("expected an HX-Push-Url target")?;
    assert_eq!(pushed, "finance/payment-terms.md");
    assert!(body.contains("entity-docs-edit-form"), "{body}");

    let content = std::fs::read_to_string(vault_dir.path().join("zenith/finance/payment-terms.md"))?;
    assert!(content.contains("title: Payment Terms"), "{content}");
    // `note_create`'s own guaranteed default frontmatter schema, proving
    // this went through `note_create` and not a direct `fs_write_file`.
    assert!(content.contains("created:"), "{content}");
    Ok(())
}

#[tokio::test]
async fn create_with_an_empty_title_is_refused() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = post(&router, &format!("/{VAULT_NAME}/entity/zenith"), r#"{"title":"   "}"#).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(headers.get("HX-Redirect").is_none(), "{body}");
    assert!(body.contains("A file needs a title."), "{body}");
    Ok(())
}

// ---------------------------------------------------------------------
// GET /{{vault_name}}/entity/{{*path}}/new - the dedicated,
// bookmarkable "New file" form
// ---------------------------------------------------------------------

#[tokio::test]
async fn get_new_at_the_entitys_own_root_pre_fills_a_default_title() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/entity/zenith/new")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("entity-docs-create-form"), "{body}");
    // Seeded from `entity_docs::default_title` - "File YYYY-MM-DD HHmm".
    assert!(body.contains("name=\"title\" value=\"File "), "{body}");
    // Unlike Wiki, an entity's own root carries no minimum-depth floor: it
    // always renders a real, usable form, never a too-shallow explanation.
    assert!(body.contains("New file in Zenith"), "{body}");
    Ok(())
}

#[tokio::test]
async fn get_new_at_a_nested_folder_pre_fills_a_default_title() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    write(vault_dir.path(), "zenith/finance/.gitkeep", "")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/entity/zenith/finance/new")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("entity-docs-create-form"), "{body}");
    assert!(body.contains("name=\"title\" value=\"File "), "{body}");
    assert!(body.contains("New file in finance"), "{body}");
    Ok(())
}

#[tokio::test]
async fn get_new_forms_own_submit_target_is_the_folders_own_create_endpoint() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    write(vault_dir.path(), "zenith/finance/.gitkeep", "")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/entity/zenith/finance/new")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The dedicated form posts straight to `finance`'s own real create
    // endpoint (`create_dispatch`/`create_path`) - the same one the
    // header's own "New" button now merely navigates to via `hx-get`
    // instead of posting to directly.
    assert!(
        body.contains(&format!("hx-post=\"/{VAULT_NAME}/entity/zenith/finance\"")),
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
    seed_one_entity(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/entity/zenith")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Matches `routes::wiki`'s own corrected header button (`hx-get .../new`,
    // never an immediate `hx-post` that creates with no form of its own to
    // review first).
    assert!(
        body.contains(&format!("hx-get=\"/{VAULT_NAME}/entity/zenith/new\"")),
        "{body}"
    );
    assert!(
        !body.contains(&format!("hx-post=\"/{VAULT_NAME}/entity/zenith\"")),
        "{body}"
    );
    Ok(())
}

#[tokio::test]
async fn posting_with_no_title_field_at_all_is_refused_rather_than_defaulted() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = post(&router, &format!("/{VAULT_NAME}/entity/zenith"), "{}").await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(headers.get("HX-Redirect").is_none(), "{body}");
    assert!(headers.get("HX-Push-Url").is_none(), "{body}");
    // No server-synthesised fallback title any more, matching
    // `routes::wiki_create::CreateBody`'s own corrected "no auto-naming"
    // contract - the header "New" button always loads the dedicated `/new`
    // form first now, pre-filled and editable, so a request that carries no
    // title at all is a stale or tampered one, not a legitimate flow this
    // handler still needs to auto-name for.
    assert!(body.contains("A file needs a title."), "{body}");
    assert!(body.contains("entity-docs-create-form"), "{body}");
    Ok(())
}

#[tokio::test]
async fn get_new_on_a_folder_that_does_not_exist_is_a_404() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body) = get(&router, &format!("/{VAULT_NAME}/entity/zenith/finance/new")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn get_new_on_an_unconfigured_entity_slug_is_a_404() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body) = get(&router, &format!("/{VAULT_NAME}/entity/nonexistent/new")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn posting_an_empty_title_re_renders_the_new_form_with_the_submitted_title_preserved() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    write(vault_dir.path(), "zenith/finance/.gitkeep", "")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = post(
        &router,
        &format!("/{VAULT_NAME}/entity/zenith/finance"),
        r#"{"title":"   "}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(headers.get("HX-Redirect").is_none(), "{body}");
    assert!(body.contains("A file needs a title."), "{body}");
    // Re-renders the dedicated create form itself - never the plain folder
    // browse fragment - so the operator's own (blank) title stays right
    // there ready to be corrected, rather than losing their place.
    assert!(body.contains("entity-docs-create-form"), "{body}");
    assert!(body.contains("name=\"title\" value=\"   \""), "{body}");
    Ok(())
}

#[tokio::test]
async fn posting_a_duplicate_title_re_renders_the_new_form_with_the_submitted_title_preserved() -> Result<(), BoxError>
{
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    write(vault_dir.path(), "zenith/finance/.gitkeep", "")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (first_status, first_body, _headers) = post(
        &router,
        &format!("/{VAULT_NAME}/entity/zenith/finance"),
        r#"{"title":"Payment Terms"}"#,
    )
    .await?;
    assert_eq!(first_status, StatusCode::OK, "{first_body}");

    let (status, body, headers) = post(
        &router,
        &format!("/{VAULT_NAME}/entity/zenith/finance"),
        r#"{"title":"Payment Terms"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(headers.get("HX-Push-Url").is_none(), "{body}");
    // Re-renders the dedicated create form itself, the operator's own
    // (rejected) title preserved, rather than the plain folder-browse
    // fragment - matching the identical contract for `EmptyTitle`.
    assert!(body.contains("entity-docs-create-form"), "{body}");
    assert!(body.contains("name=\"title\" value=\"Payment Terms\""), "{body}");
    Ok(())
}

#[tokio::test]
async fn a_file_created_through_the_dedicated_new_form_creates_it_and_lands_on_its_own_edit_form()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    write(vault_dir.path(), "zenith/finance/.gitkeep", "")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    // Load the dedicated form first, exactly as an operator following a
    // bookmarked or shared `/new` link would.
    let (get_status, form_body) = get(&router, &format!("/{VAULT_NAME}/entity/zenith/finance/new")).await?;
    assert_eq!(get_status, StatusCode::OK, "{form_body}");
    assert!(form_body.contains("entity-docs-create-form"), "{form_body}");

    let (status, body, headers) = post(
        &router,
        &format!("/{VAULT_NAME}/entity/zenith/finance"),
        r#"{"title":"Payment Terms"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let pushed = pushed_path(&headers).ok_or("expected an HX-Push-Url target")?;
    assert_eq!(pushed, "finance/payment-terms.md");
    assert!(body.contains("entity-docs-edit-form"), "{body}");

    let content = std::fs::read_to_string(vault_dir.path().join("zenith/finance/payment-terms.md"))?;
    assert!(content.contains("title: Payment Terms"), "{content}");
    Ok(())
}
