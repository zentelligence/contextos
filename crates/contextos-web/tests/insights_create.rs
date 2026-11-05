//! Insights screen contract tests for creating a note: `POST /{{vault_name}}/insights/`
//! (the "New" action) and `GET /{{vault_name}}/insights/new` (the dedicated,
//! bookmarkable create-note form) against a real `contextos-mcp` session and a real temporary
//! vault fixture, no mocking, per `testing.md`. Kept apart from `tests/insights.rs` (already close
//! to this workspace's own file-size guidance) rather than growing that file further, matching
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

/// A freshly created note's own path from its response's `HX-Push-Url`
/// header (`/{{vault}}/insights/{{path}}` - the URL itself names
/// the resource, no `?note=` to extract). Creation swaps straight into the
/// new note's own edit form in the same response, so the browser's own
/// address bar is updated in place via `HX-Push-Url` rather than a full
/// `HX-Redirect` navigation, matching `tests/wiki_create.rs`'s own identical
/// helper.
fn pushed_path(headers: &HeaderMap) -> Option<String> {
    let target = headers.get("HX-Push-Url")?.to_str().ok()?;
    let (_, path) = target.split_once("/insights/")?;
    Some(path.to_owned())
}

// ---------------------------------------------------------------------
// POST /{{vault_name}}/insights/ - the "New" action
// ---------------------------------------------------------------------

#[tokio::test]
async fn create_dispatches_note_create_with_a_default_proposed_status_and_redirects() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = post(
        &router,
        &format!("/{VAULT_NAME}/insights/"),
        r#"{"title":"Structure Beats Willpower","status":""}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let pushed = pushed_path(&headers).ok_or("expected an HX-Push-Url target")?;
    assert_eq!(pushed, "s/structure-beats-willpower.md");
    // Lands directly in the new, empty note's own edit form - never its
    // read view.
    assert!(body.contains("wiki-edit-form"), "{body}");

    let content = std::fs::read_to_string(vault_dir.path().join("memory/insights/s/structure-beats-willpower.md"))?;
    assert!(content.contains("type: insight"), "{content}");
    assert!(content.contains("status: proposed"), "{content}");
    assert!(content.contains("title: Structure Beats Willpower"), "{content}");
    Ok(())
}

#[tokio::test]
async fn create_with_an_explicit_status_persists_it() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, _headers) = post(
        &router,
        &format!("/{VAULT_NAME}/insights/"),
        r#"{"title":"Adopted Idea","status":"adopted"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let content = std::fs::read_to_string(vault_dir.path().join("memory/insights/a/adopted-idea.md"))?;
    assert!(content.contains("status: adopted"), "{content}");
    Ok(())
}

// ---------------------------------------------------------------------
// GET /{{vault_name}}/insights/new - the dedicated, bookmarkable
// "New insight" form
// ---------------------------------------------------------------------

#[tokio::test]
async fn get_new_pre_fills_a_default_title_and_the_proposed_status() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/insights/new")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("insights-create-form"), "{body}");
    // Seeded from `insights::default_title` - "Insight YYYY-MM-DD HHmm".
    assert!(body.contains("name=\"title\" value=\"Insight "), "{body}");
    // `insights::create_insight`'s own domain-level empty-status default,
    // pre-selected in the form's own status picker.
    assert!(body.contains("value=\"proposed\" selected"), "{body}");
    Ok(())
}

#[tokio::test]
async fn get_new_forms_own_submit_target_is_the_root_create_endpoint() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/insights/new")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The dedicated form posts straight to the root's own real create
    // endpoint (`routes::insights_create::create`) - the same one the
    // header's own "New" button now merely navigates to via `hx-get`
    // instead of posting to directly.
    assert!(body.contains(&format!("hx-post=\"/{VAULT_NAME}/insights/\"")), "{body}");
    Ok(())
}

#[tokio::test]
async fn the_header_new_button_now_navigates_to_the_dedicated_form_rather_than_posting_directly() -> Result<(), BoxError>
{
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/insights/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Matches `routes::wiki`'s own corrected header button (`hx-get .../new`,
    // never an immediate `hx-post` that creates with no form of its own to
    // review first).
    assert!(
        body.contains(&format!("hx-get=\"/{VAULT_NAME}/insights/new\"")),
        "{body}"
    );
    assert!(
        !body.contains(&format!("hx-post=\"/{VAULT_NAME}/insights/\"")),
        "{body}"
    );
    Ok(())
}

#[tokio::test]
async fn get_new_scoped_to_a_real_shard_is_a_404_not_the_root_scoped_form() -> Result<(), BoxError> {
    // Unlike Wiki's own `/new`, which is scoped to whichever real folder it
    // trails, an insight note's own shard is always derived from its
    // title's slug, never from where the operator was browsing - there is
    // no per-shard `/new` at all, only the bare root's own. `a/new` is
    // therefore not a reserved form target; `a` is a real shard
    // (`insights::list_shards`'s own fixed single-character keyspace) but
    // `a/new` names neither a real shard nor a real note, so this must
    // `404` exactly as any other unresolvable path already does, never
    // silently fall through to the root-scoped form.
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body) = get(&router, &format!("/{VAULT_NAME}/insights/a/new")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

// ---------------------------------------------------------------------
// POST error paths - every rejected create re-renders the dedicated `/new`
// form, submitted values preserved, never the plain shard-browse fragment
// ---------------------------------------------------------------------

#[tokio::test]
async fn posting_with_no_title_field_at_all_is_refused_rather_than_defaulted() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = post(&router, &format!("/{VAULT_NAME}/insights/"), "{}").await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(headers.get("HX-Push-Url").is_none(), "{body}");
    // No server-synthesised fallback title any more, matching
    // `routes::wiki_create::CreateBody`'s own corrected "no auto-naming"
    // contract - the header "New" button always loads the dedicated `/new`
    // form first now, pre-filled and editable, so a request that carries no
    // title at all is a stale or tampered one, not a legitimate flow this
    // handler still needs to auto-name for.
    assert!(body.contains("A note needs a title."), "{body}");
    assert!(body.contains("insights-create-form"), "{body}");
    Ok(())
}

#[tokio::test]
async fn posting_an_empty_title_re_renders_the_new_form_with_the_submitted_status_preserved() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = post(
        &router,
        &format!("/{VAULT_NAME}/insights/"),
        r#"{"title":"   ","status":"adopted"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(headers.get("HX-Push-Url").is_none(), "{body}");
    assert!(body.contains("A note needs a title."), "{body}");
    // Re-renders the dedicated create form itself - never the plain
    // shard-browse fragment - so the operator's own (blank) title and their
    // own chosen status both stay right there ready to be corrected, rather
    // than losing their place.
    assert!(body.contains("insights-create-form"), "{body}");
    assert!(body.contains("name=\"title\" value=\"   \""), "{body}");
    assert!(body.contains("value=\"adopted\" selected"), "{body}");
    Ok(())
}

#[tokio::test]
async fn posting_an_invalid_status_re_renders_the_new_form_with_the_submitted_title_preserved() -> Result<(), BoxError>
{
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = post(
        &router,
        &format!("/{VAULT_NAME}/insights/"),
        r#"{"title":"Bad Status","status":"maybe"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(headers.get("HX-Push-Url").is_none(), "{body}");
    assert!(
        body.contains("Status must be one of proposed, adopted, or dismissed."),
        "{body}"
    );
    assert!(body.contains("insights-create-form"), "{body}");
    assert!(body.contains("name=\"title\" value=\"Bad Status\""), "{body}");
    assert!(!vault_dir.path().join("memory/insights").exists());
    Ok(())
}

#[tokio::test]
async fn a_note_created_through_the_dedicated_new_form_creates_it_and_lands_on_its_own_edit_form()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    // Load the dedicated form first, exactly as an operator following a
    // bookmarked or shared `/new` link would.
    let (get_status, form_body) = get(&router, &format!("/{VAULT_NAME}/insights/new")).await?;
    assert_eq!(get_status, StatusCode::OK, "{form_body}");
    assert!(form_body.contains("insights-create-form"), "{form_body}");

    let (status, body, headers) = post(
        &router,
        &format!("/{VAULT_NAME}/insights/"),
        r#"{"title":"Payment Terms","status":""}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let pushed = pushed_path(&headers).ok_or("expected an HX-Push-Url target")?;
    assert_eq!(pushed, "p/payment-terms.md");
    assert!(body.contains("wiki-edit-form"), "{body}");

    let content = std::fs::read_to_string(vault_dir.path().join("memory/insights/p/payment-terms.md"))?;
    assert!(content.contains("type: insight"), "{content}");
    assert!(content.contains("title: Payment Terms"), "{content}");
    assert!(content.contains("status: proposed"), "{content}");
    Ok(())
}
