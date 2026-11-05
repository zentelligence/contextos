//! Wiki screen contract tests: real `contextos-mcp` sessions against real
//! temporary vault fixtures. No mocking, per `testing.md`.

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

/// A `GET` carrying `HX-Request: true`, returning just the `.wiki-screen`
/// fragment with no full-page shell (no top nav, no breadcrumb) around it -
/// the shape assertions about the fragment's own internal structure (the
/// accordion tree in particular) should read, since the shell's own
/// separate top breadcrumb can otherwise repeat a folder's own label
/// outside the tree entirely.
async fn get_fragment(router: &Router, path: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header("hx-request", "true")
                .body(Body::empty())?,
        )
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

async fn patch(router: &Router, path: &str, json_body: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(json_body.to_owned()))?,
        )
        .await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

async fn patch_with_headers(
    router: &Router,
    path: &str,
    json_body: &str,
) -> Result<(StatusCode, String, HeaderMap), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
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

async fn delete(router: &Router, path: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().method("DELETE").uri(path).body(Body::empty())?)
        .await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

/// A renamed page's or a newly created folder's own path from an
/// `HX-Redirect` response header (`/{{vault}}/wiki/{{path}}` -
/// the URL itself names the resource, no `?page=` to extract). Unlike
/// creating a page (see [`pushed_path`]), these two actions have no edit
/// form of their own to land in, so they keep the plain full-navigation
/// redirect.
fn redirected_path(headers: &HeaderMap) -> Option<String> {
    let target = headers.get("HX-Redirect")?.to_str().ok()?;
    let (_, path) = target.split_once("/wiki/")?;
    Some(path.to_owned())
}

/// The rendered edit form's own current `expected_hash` hidden field.
fn extract_expected_hash(body: &str) -> Option<String> {
    let marker = "name=\"expected_hash\" value=\"";
    let after = body.split(marker).nth(1)?;
    let (hash, _) = after.split_once('"')?;
    Some(hash.to_owned())
}

/// The rendered metadata panel's own current `expected_hash`, embedded in its own `hx-vals`
/// script rather than a hidden field (`metadata_panel.html`'s own `expected_hash: '...'`
/// literal) - the plain read view's own hash, present whether or not the page is being
/// edited, unlike [`extract_expected_hash`]'s edit-form-only hidden field.
fn extract_metadata_expected_hash(body: &str) -> Option<String> {
    let marker = "expected_hash: '";
    let after = body.split(marker).nth(1)?;
    let (hash, _) = after.split_once('\'')?;
    Some(hash.to_owned())
}

// ---------------------------------------------------------------------
// GET /{{vault_name}}/wiki/{{*path}}
// ---------------------------------------------------------------------

#[tokio::test]
async fn an_empty_wiki_renders_the_root_with_no_children_and_no_create_action() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Nothing here."), "{body}");
    assert!(body.contains("Open at least a domain and a topic folder"), "{body}");
    // "New folder" carries no `MIN_CREATE_DEPTH` gate: it is offered at the
    // root even though "New page" is not.
    assert!(body.contains("id=\"wiki-new-folder-field\""), "{body}");
    Ok(())
}

#[tokio::test]
async fn get_on_a_real_folder_path_lists_that_folders_own_children() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/wiki/business/finance/tax.md",
        "---\ntitle: Tax\n---\n\n# Tax\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/business/finance")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("tax"), "{body}");
    // Depth 2 (business/finance): the create action is available here.
    assert!(!body.contains("Open at least a domain and a topic folder"), "{body}");
    Ok(())
}

#[tokio::test]
async fn get_on_a_real_page_path_renders_its_content() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/wiki/business/finance/tax.md",
        "---\ntitle: Tax\n---\n\n# Tax rules\n\nPay quarterly.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/business/finance/tax.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Tax rules"), "{body}");
    assert!(body.contains("Pay quarterly."), "{body}");
    // Rename sits in the top header row, alongside Edit/Delete - never
    // after the rendered content, where it would read as detached from the
    // other per-page actions.
    let header_end = body
        .find("notes-main-body")
        .ok_or("expected a notes-main-body marker")?;
    let rename_at = body.find("Rename page").ok_or("expected a Rename control")?;
    assert!(rename_at < header_end, "{body}");
    Ok(())
}

/// A real regression test for a real production defect: `fs_read_text_file`
/// caps out at 5 KiB, and this crate's own completeness read (now
/// `vault_content::read_full_text`, via `fs_attach_file`) once silently
/// failed to decode that tool's real response shape (an MCP embedded
/// resource content block, not the plain JSON text block its own
/// deserialization expected), so every wiki page over the cap was served
/// truncated with no error of any kind. A page comfortably past 5 KiB, with
/// a paragraph placed well after that boundary and another right at the
/// very end of the file, must render in full.
#[tokio::test]
async fn a_page_larger_than_the_5kb_read_text_file_cap_still_renders_in_full() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let filler = "Filler line to push this file well past the five kilobyte cap.\n".repeat(120);
    let content = format!(
        "---\ntitle: Large Page\n---\n\n# Large Page\n\n{filler}\nPast the cap paragraph.\n\nFinal line at the very end of the file.\n"
    );
    assert!(content.len() > 5 * 1024, "fixture must genuinely exceed the 5 KiB cap");
    write(vault_dir.path(), "memory/wiki/business/finance/large.md", &content)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/business/finance/large.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Past the cap paragraph."), "{body}");
    assert!(body.contains("Final line at the very end of the file."), "{body}");
    Ok(())
}

#[tokio::test]
async fn a_folders_new_folder_control_is_a_closed_disclosure_not_an_always_open_form() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The input still exists in the markup (a real `<details>` disclosure,
    // not a fragment fetched on demand), but its own `<details>` carries no
    // `open` attribute - the operator sees a plain "New folder" button
    // until they choose to expand it.
    let marker = "<details class=\"header-disclosure\">";
    let disclosure_at = body.find(marker).ok_or("expected a header-disclosure")?;
    assert!(
        !body[disclosure_at..disclosure_at + marker.len() + 8].contains(" open"),
        "{body}"
    );
    Ok(())
}

#[tokio::test]
async fn get_on_a_path_naming_neither_a_real_folder_nor_a_real_page_is_a_404() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body) = get(&router, &format!("/{VAULT_NAME}/wiki/business/finance/missing.md")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

// ---------------------------------------------------------------------
// GET /{{vault_name}}/wiki/{{*path}}/edit - the on-demand edit
// form, and PATCH /{{vault_name}}/wiki/{{*path}} - saving it
// ---------------------------------------------------------------------

#[tokio::test]
async fn get_on_a_page_paths_own_edit_sub_path_pre_fills_the_form_with_the_bodys_own_text() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/wiki/business/finance/tax.md",
        "---\ntitle: Tax\n---\n\n# Tax rules\n\nPay quarterly.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    // The plain read view no longer carries an inline edit form at all.
    let (read_status, read_body) = get(&router, &format!("/{VAULT_NAME}/wiki/business/finance/tax.md")).await?;
    assert_eq!(read_status, StatusCode::OK, "{read_body}");
    assert!(!read_body.contains("name=\"expected_hash\""), "{read_body}");

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/business/finance/tax.md/edit")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("# Tax rules"), "{body}");
    assert!(body.contains("Pay quarterly."), "{body}");
    // The frontmatter block itself never appears in the editable textarea.
    assert!(!body.contains("title: Tax"), "{body}");
    // The read view's own Rename/Edit/Delete cluster lives outside
    // `#wiki-main-body`: a response that swapped only that div would leave
    // it stale, showing both the read-mode actions and the edit-mode
    // Save/Cancel pair at once. The `/edit` response is the whole screen
    // (its own sidebar tree included) with `editing` populated instead, so
    // the header's own conditional actually takes effect.
    assert!(!body.contains("Rename page"), "{body}");
    assert!(!body.contains("title=\"Edit page\""), "{body}");
    assert!(!body.contains("title=\"Delete page\""), "{body}");
    assert!(body.contains("class=\"wiki-screen\""), "{body}");

    // Cancel returns to the plain read view.
    let (cancel_status, cancel_body) = get(&router, &format!("/{VAULT_NAME}/wiki/business/finance/tax.md")).await?;
    assert_eq!(cancel_status, StatusCode::OK, "{cancel_body}");
    assert!(cancel_body.contains("Tax rules"), "{cancel_body}");
    // The Rename/Edit/Delete cluster is back, and the edit form is gone - the metadata
    // panel's own always-present "Save" button (`patching_the_metadata_panel_...` tests cover
    // that one separately) is not this check's concern, so it asserts on the edit form's own
    // distinguishing class rather than the "Save" text a body-save button and the metadata
    // panel's save button now both carry.
    assert!(cancel_body.contains("title=\"Edit page\""), "{cancel_body}");
    assert!(!cancel_body.contains("class=\"wiki-edit-form\""), "{cancel_body}");
    Ok(())
}

#[tokio::test]
async fn saving_an_edited_body_persists_it_and_keeps_the_frontmatter_untouched() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/wiki/business/finance/tax.md",
        "---\ntitle: Tax\nstatus: draft\n---\n\n# Tax rules\n\nPay quarterly.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, body) = get(&router, &format!("/{VAULT_NAME}/wiki/business/finance/tax.md/edit")).await?;
    let expected_hash = extract_expected_hash(&body).ok_or("expected an expected_hash field")?;

    let (status, saved_body) = patch(
        &router,
        &format!("/{VAULT_NAME}/wiki/business/finance/tax.md"),
        &format!(r##"{{"content":"# Tax rules\n\nPay monthly now.\n","expected_hash":"{expected_hash}"}}"##),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{saved_body}");
    assert!(saved_body.contains("Pay monthly now."), "{saved_body}");

    let content = std::fs::read_to_string(vault_dir.path().join("memory/wiki/business/finance/tax.md"))?;
    assert!(content.contains("title: Tax"), "{content}");
    assert!(content.contains("status: draft"), "{content}");
    assert!(content.contains("Pay monthly now."), "{content}");
    assert!(!content.contains("Pay quarterly."), "{content}");
    Ok(())
}

#[tokio::test]
async fn saving_with_a_stale_hash_is_rejected_with_a_conflict_message_and_leaves_the_file_untouched()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/wiki/business/finance/tax.md",
        "---\ntitle: Tax\n---\n\nOriginal body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/wiki/business/finance/tax.md"),
        r#"{"content":"Conflicting body.\n","expected_hash":"not-the-real-hash"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains("This page changed since it was last loaded; reload and try again."),
        "{body}"
    );

    let content = std::fs::read_to_string(vault_dir.path().join("memory/wiki/business/finance/tax.md"))?;
    assert!(content.contains("Original body."), "{content}");
    Ok(())
}

#[tokio::test]
async fn saving_the_body_of_a_page_with_unparsable_frontmatter_reports_it_without_writing() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    // Opens a frontmatter block but never closes it - `frontmatter_read` reports this as its
    // own `Unclosed` error, real content the page still has (unlike a missing file, which
    // `read_page`'s own preceding fresh read already rules out), so `wiki::save_page_body`
    // reaches `SavePageError::Malformed` rather than misreporting it as `NotFound` - matching
    // `tests::notes`'s identical `saving_the_body_of_a_note_with_unparsable_frontmatter_reports_it_without_writing`
    // case for Wiki's own body-save path.
    let malformed = "---\ntitle: Tax\n\nBody with no closing frontmatter delimiter.\n";
    write(vault_dir.path(), "memory/wiki/business/finance/tax.md", malformed)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/wiki/business/finance/tax.md"),
        r#"{"content":"New body.\n","expected_hash":null}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("frontmatter could not be read"), "{body}");

    let unchanged = std::fs::read_to_string(vault_dir.path().join("memory/wiki/business/finance/tax.md"))?;
    assert_eq!(unchanged, malformed);
    Ok(())
}

#[tokio::test]
async fn the_metadata_panel_renders_every_real_frontmatter_field_on_a_selected_page() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/wiki/business/finance/tax.md",
        "---\ntitle: Tax\nstatus: draft\ntags:\n  - finance\n  - quarterly\n---\n\n# Tax rules\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/business/finance/tax.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("data-field=\"title\""), "{body}");
    assert!(body.contains("data-field=\"status\""), "{body}");
    assert!(body.contains("value=\"draft\""), "{body}");
    assert!(body.contains("data-field=\"tags\""), "{body}");
    assert!(body.contains("finance, quarterly"), "{body}");
    Ok(())
}

#[tokio::test]
async fn a_page_with_no_frontmatter_fields_shows_the_generic_empty_state_message() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/wiki/business/finance/tax.md",
        "Plain body with no frontmatter block at all.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/business/finance/tax.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("This page has no frontmatter fields."), "{body}");
    Ok(())
}

#[tokio::test]
async fn patching_the_metadata_panel_saves_the_submitted_fields_and_leaves_the_body_untouched() -> Result<(), BoxError>
{
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/wiki/business/finance/tax.md",
        "---\ntitle: Tax\nstatus: draft\n---\n\n# Tax rules\n\nPay quarterly.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, body) = get(&router, &format!("/{VAULT_NAME}/wiki/business/finance/tax.md")).await?;
    let expected_hash = extract_metadata_expected_hash(&body).ok_or("expected an expected_hash field")?;

    let (status, saved_body) = patch(
        &router,
        &format!("/{VAULT_NAME}/wiki/business/finance/tax.md"),
        &format!(r#"{{"patch":{{"status":"final"}},"expected_hash":"{expected_hash}"}}"#),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{saved_body}");

    let content = std::fs::read_to_string(vault_dir.path().join("memory/wiki/business/finance/tax.md"))?;
    assert!(content.contains("status: final"), "{content}");
    assert!(content.contains("title: Tax"), "{content}");
    assert!(content.contains("Pay quarterly."), "{content}");
    Ok(())
}

#[tokio::test]
async fn patching_the_metadata_panel_with_a_stale_hash_is_rejected_and_leaves_the_file_untouched()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/wiki/business/finance/tax.md",
        "---\ntitle: Tax\nstatus: draft\n---\n\nOriginal body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/wiki/business/finance/tax.md"),
        r#"{"patch":{"status":"final"},"expected_hash":"not-the-real-hash"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains("This page changed since it was last loaded; reload and try again."),
        "{body}"
    );

    let content = std::fs::read_to_string(vault_dir.path().join("memory/wiki/business/finance/tax.md"))?;
    assert!(content.contains("status: draft"), "{content}");
    Ok(())
}

#[tokio::test]
async fn patching_the_metadata_panel_on_a_page_with_unparsable_frontmatter_reports_it_without_writing()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    // Opens a frontmatter block but never closes it - `frontmatter_read` reports this as its
    // own `Unclosed` error, real content the page still has (unlike a missing file), so
    // `wiki::load_page_frontmatter` reaches `LoadWikiPageError::Malformed` on its own fresh
    // re-read rather than `NotFound`, matching `tests::notes`'s identical
    // `saving_the_body_of_a_note_with_unparsable_frontmatter_reports_it_without_writing` case
    // for the metadata-patch path instead of the body-save one.
    let malformed = "---\ntitle: Tax\n\nBody with no closing frontmatter delimiter.\n";
    write(vault_dir.path(), "memory/wiki/business/finance/tax.md", malformed)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/wiki/business/finance/tax.md"),
        r#"{"patch":{"status":"final"},"expected_hash":null}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("frontmatter could not be read"), "{body}");

    let unchanged = std::fs::read_to_string(vault_dir.path().join("memory/wiki/business/finance/tax.md"))?;
    assert_eq!(unchanged, malformed);
    Ok(())
}

#[tokio::test]
async fn the_metadata_panel_side_swaps_the_layout_class_independently_of_the_tree_side() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/wiki/business/finance/tax.md",
        "---\ntitle: Tax\n---\n\n# Tax rules\n",
    )?;
    // The tree pane stays on its own shipped default (right); only the metadata panel moves
    // to the left, so the layout needs its own dedicated left column for it
    // (`notes-layout-metadata-left-panel-right`) rather than either single-side modifier.
    write(
        vault_dir.path(),
        ".contextos/wiki.toml",
        "metadata_panel_side = \"left\"\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/business/finance/tax.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("notes-layout-metadata-left-panel-right"), "{body}");
    Ok(())
}

#[tokio::test]
async fn saving_a_missing_page_is_a_404() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body) = patch(
        &router,
        &format!("/{VAULT_NAME}/wiki/business/finance/missing.md"),
        r#"{"content":"Anything.\n"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

// ---------------------------------------------------------------------
// The accordion folder tree
// ---------------------------------------------------------------------

#[tokio::test]
async fn browsing_a_deep_folder_renders_a_nested_accordion_with_only_the_anchor_path_open() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/wiki/business/finance/tax.md",
        "---\ntitle: Tax\n---\n\n# Tax\n",
    )?;
    write(
        vault_dir.path(),
        "memory/wiki/business/marketing/campaigns.md",
        "---\ntitle: Campaigns\n---\n\n# Campaigns\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get_fragment(&router, &format!("/{VAULT_NAME}/wiki/business/finance")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Only the sidebar's own accordion tree, not the main pane's own
    // "finance" heading, which also contains that same word.
    let tree = body
        .split("class=\"notes-sidebar\"")
        .nth(1)
        .ok_or("expected a sidebar section")?;
    // Nested `.nav-group` elements, matching Notes' own accordion shape.
    assert!(tree.matches("class=\"nav-group\"").count() >= 2, "{tree}");
    // `business` and `finance` are both on the anchor path and open; the
    // sibling `marketing` folder is closed.
    let business_index = tree.find(">business<").ok_or("missing business folder heading")?;
    let finance_index = tree.find(">finance<").ok_or("missing finance folder heading")?;
    let marketing_index = tree.find(">marketing<").ok_or("missing marketing folder heading")?;
    let business_open = tree[..business_index]
        .rfind("<details class=\"nav-group\"")
        .is_some_and(|start| tree[start..business_index].contains("open"));
    assert!(business_open, "{tree}");
    let finance_open = tree[..finance_index]
        .rfind("<details class=\"nav-group\"")
        .is_some_and(|start| tree[start..finance_index].contains("open"));
    assert!(finance_open, "{tree}");
    let marketing_open = tree[..marketing_index]
        .rfind("<details class=\"nav-group\"")
        .is_some_and(|start| tree[start..marketing_index].contains("open"));
    assert!(!marketing_open, "{tree}");
    Ok(())
}

// ---------------------------------------------------------------------
// PATCH /{{vault_name}}/wiki/{{*path}}/rename - retitling a page
// ---------------------------------------------------------------------

#[tokio::test]
async fn rename_via_patch_moves_the_file_and_updates_its_frontmatter_title() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/wiki/business/finance/tax.md",
        "---\ntitle: Tax\n---\n\n# Tax\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = patch_with_headers(
        &router,
        &format!("/{VAULT_NAME}/wiki/business/finance/tax.md/rename"),
        r#"{"title":"Payroll Tax"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let redirected = redirected_path(&headers).ok_or("expected an HX-Redirect target")?;
    assert_eq!(redirected, "business/finance/payroll-tax.md");

    assert!(
        !vault_dir.path().join("memory/wiki/business/finance/tax.md").exists(),
        "the old filename should no longer exist"
    );
    let content = std::fs::read_to_string(vault_dir.path().join("memory/wiki/business/finance/payroll-tax.md"))?;
    assert!(content.contains("title: Payroll Tax"), "{content}");
    Ok(())
}

// ---------------------------------------------------------------------
// POST /{{vault_name}}/wiki/{{*path}}/execute/new-folder - "New
// folder", never gated by MIN_CREATE_DEPTH
// ---------------------------------------------------------------------

#[tokio::test]
async fn new_folder_is_reachable_at_the_wiki_root_even_though_new_page_is_not() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = post(
        &router,
        &format!("/{VAULT_NAME}/wiki/execute/new-folder"),
        r#"{"name":"Business"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let redirected = redirected_path(&headers).ok_or("expected an HX-Redirect target")?;
    assert_eq!(redirected, "business");
    assert!(vault_dir.path().join("memory/wiki/business").is_dir());

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/business")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The accordion tree renders the whole real tree, not just this
    // folder's own children, so "business" itself now appears as a
    // (childless) `.nav-group` rather than the sidebar's own
    // whole-tree-empty "Nothing here." state.
    assert!(body.contains(">business<"), "{body}");
    assert!(
        body.contains("Choose a topic to drill in, or a page to read it."),
        "{body}"
    );
    Ok(())
}

#[tokio::test]
async fn new_folder_is_reachable_one_level_deep_at_a_single_top_level_folder() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/wiki/business/.gitkeep", "")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = post(
        &router,
        &format!("/{VAULT_NAME}/wiki/business/execute/new-folder"),
        r#"{"name":"Finance"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let redirected = redirected_path(&headers).ok_or("expected an HX-Redirect target")?;
    assert_eq!(redirected, "business/finance");
    assert!(vault_dir.path().join("memory/wiki/business/finance").is_dir());
    Ok(())
}

#[tokio::test]
async fn new_folder_with_an_empty_name_is_refused() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = post(
        &router,
        &format!("/{VAULT_NAME}/wiki/execute/new-folder"),
        r#"{"name":"   "}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(headers.get("HX-Redirect").is_none(), "{body}");
    assert!(body.contains("A folder needs a name."), "{body}");
    Ok(())
}

// ---------------------------------------------------------------------
// GET .../confirm-delete and DELETE - a page
// ---------------------------------------------------------------------

#[tokio::test]
async fn confirm_delete_names_the_page_and_delete_moves_it_to_trash() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/wiki/business/finance/tax.md",
        "---\ntitle: Tax\n---\n\n# Tax\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(
        &router,
        &format!("/{VAULT_NAME}/wiki/business/finance/tax.md/confirm-delete"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Delete page"), "{body}");
    assert!(body.contains("tax"), "{body}");

    let (status, body) = delete(&router, &format!("/{VAULT_NAME}/wiki/business/finance/tax.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !vault_dir.path().join("memory/wiki/business/finance/tax.md").exists(),
        "the page should have been moved to trash"
    );
    Ok(())
}

// ---------------------------------------------------------------------
// GET .../confirm-delete and DELETE - a folder
// ---------------------------------------------------------------------

#[tokio::test]
async fn confirm_delete_names_the_folder_and_delete_removes_it_when_empty() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/wiki/business/.gitkeep", "")?;
    // Genuinely empty on disk - unlike every other fixture folder in this
    // file, this one carries no placeholder file of its own, so the real
    // delete below exercises the actual empty-directory path rather than
    // one still holding a leftover `.gitkeep`.
    std::fs::create_dir_all(vault_dir.path().join("memory/wiki/business/finance"))?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/wiki/business/finance/confirm-delete")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Delete folder"), "{body}");
    assert!(body.contains("finance"), "{body}");

    let (status, body) = delete(&router, &format!("/{VAULT_NAME}/wiki/business/finance")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !vault_dir.path().join("memory/wiki/business/finance").exists(),
        "the empty folder should have been deleted"
    );
    Ok(())
}

#[tokio::test]
async fn delete_on_a_non_empty_folder_is_refused_with_an_actionable_message_and_the_folder_survives()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/wiki/business/finance/tax.md",
        "---\ntitle: Tax\n---\n\n# Tax\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = delete(&router, &format!("/{VAULT_NAME}/wiki/business/finance")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains("This folder still has pages or subfolders and must be emptied first."),
        "{body}"
    );
    assert!(
        vault_dir.path().join("memory/wiki/business/finance/tax.md").exists(),
        "the non-empty folder must survive a refused delete"
    );
    Ok(())
}

#[tokio::test]
async fn delete_on_the_wiki_root_is_a_404() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body) = delete(&router, &format!("/{VAULT_NAME}/wiki/")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn delete_on_a_path_naming_neither_a_real_page_nor_a_real_folder_is_a_404() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body) = delete(&router, &format!("/{VAULT_NAME}/wiki/business/missing.md")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}
