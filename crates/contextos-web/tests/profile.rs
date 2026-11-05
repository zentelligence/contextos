//! Profile screen contract tests: real `contextos-mcp` sessions against real
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

/// The rendered edit form's own current `expected_hash` hidden field.
fn extract_expected_hash(body: &str) -> Option<String> {
    let marker = "name=\"expected_hash\" value=\"";
    let after = body.split(marker).nth(1)?;
    let (hash, _) = after.split_once('"')?;
    Some(hash.to_owned())
}

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

#[tokio::test]
async fn an_empty_vault_prompts_to_select_a_document() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/profile/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Select a document."), "{body}");
    Ok(())
}

#[tokio::test]
async fn the_bare_root_default_selects_the_first_identity_file() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/identity/who-am-i.md",
        "---\ntitle: Who Am I\n---\nI am the operator.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/profile/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("I am the operator."), "{body}");
    // The full-page header's own "Profile" control now targets this real
    // screen rather than the generic vault content route.
    assert!(
        body.contains(&format!("href=\"/{VAULT_NAME}/profile/\" aria-label=\"Profile\"")),
        "{body}"
    );
    Ok(())
}

#[tokio::test]
async fn get_on_a_real_identity_file_renders_it_with_an_edit_form() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/identity/who-am-i.md",
        "---\ntitle: Who Am I\n---\nI am the operator.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/profile/identity/who-am-i.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("I am the operator."), "{body}");
    // The edit form itself is loaded on demand from its own reserved
    // `/edit` sub-path (matching Wiki/Insights), never rendered inline in
    // the read view - only the top "Edit" control that opens it.
    assert!(!body.contains("wiki-edit-form"), "{body}");
    let edit_href = format!("/{VAULT_NAME}/profile/identity/who-am-i.md/edit");
    assert!(body.contains(&edit_href), "{body}");
    // The "Edit" control sits at the top, before the rendered body - never
    // after it, matching the Knowledge screens' identical corrected layout.
    let main_body_at = body
        .find("profile-main-body")
        .ok_or("expected a profile-main-body marker")?;
    let edit_control_at = body.find("Edit document").ok_or("expected an Edit control")?;
    assert!(edit_control_at < main_body_at, "{body}");

    let (edit_status, edit_body) = get(&router, &edit_href).await?;
    assert_eq!(edit_status, StatusCode::OK, "{edit_body}");
    assert!(edit_body.contains("I am the operator."), "{edit_body}");
    assert!(edit_body.contains("wiki-edit-form"), "{edit_body}");
    // The read view's own "Edit" control lives outside `#profile-main-body`:
    // a response that swapped only that div would leave it stale, showing
    // both the read-mode control and the edit-mode Save/Cancel pair at
    // once. The `/edit` response is the whole screen (its own sidebar
    // included) with `editing` populated instead, so the header's own
    // conditional actually takes effect.
    assert!(!edit_body.contains("Edit document"), "{edit_body}");
    assert!(edit_body.contains("class=\"profile-screen\""), "{edit_body}");

    // Cancel returns to the plain read view: the Edit control is back, and
    // the edit form is gone.
    let (cancel_status, cancel_body) = get(&router, &format!("/{VAULT_NAME}/profile/identity/who-am-i.md")).await?;
    assert_eq!(cancel_status, StatusCode::OK, "{cancel_body}");
    assert!(cancel_body.contains("Edit document"), "{cancel_body}");
    assert!(!cancel_body.contains("wiki-edit-form"), "{cancel_body}");
    Ok(())
}

#[tokio::test]
async fn each_groups_new_control_is_a_closed_disclosure_not_an_always_open_form() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/profile/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Both Identity's and Operating's own "New" controls are closed
    // disclosures - the title input exists in the markup, but neither
    // group's own `<details>` carries an `open` attribute, so the operator
    // sees a plain "New" button per group until they choose to expand it.
    let mut search_from = 0;
    let mut disclosures_found = 0;
    while let Some(offset) = body[search_from..].find("<details class=\"header-disclosure\">") {
        let start = search_from + offset;
        let marker_len = "<details class=\"header-disclosure\">".len();
        assert!(!body[start..start + marker_len + 8].contains(" open"), "{body}");
        disclosures_found += 1;
        search_from = start + marker_len;
    }
    assert_eq!(
        disclosures_found, 2,
        "expected one New disclosure each for Identity and Operating: {body}"
    );
    Ok(())
}

#[tokio::test]
async fn get_on_a_missing_identity_file_is_a_404() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body) = get(&router, &format!("/{VAULT_NAME}/profile/identity/missing.md")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn saving_an_edited_identity_body_persists_it_and_keeps_the_frontmatter_untouched() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/identity/who-am-i.md",
        "---\ntitle: Who Am I\n---\nOriginal body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_status, edit_fragment) = get(&router, &format!("/{VAULT_NAME}/profile/identity/who-am-i.md/edit")).await?;
    let expected_hash = extract_expected_hash(&edit_fragment).ok_or("expected an expected_hash field")?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/profile/identity/who-am-i.md"),
        &format!(r#"{{"content": "Updated body.\n", "expected_hash": "{expected_hash}"}}"#),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Updated body."), "{body}");

    let on_disk = std::fs::read_to_string(vault_dir.path().join("memory/identity/who-am-i.md"))?;
    assert!(on_disk.starts_with("---\ntitle: Who Am I\n---\n"), "{on_disk}");
    assert!(on_disk.contains("Updated body."), "{on_disk}");
    Ok(())
}

#[tokio::test]
async fn creating_a_new_identity_document_redirects_to_it() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "memory/identity/index.md", "# Identity\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = post(
        &router,
        &format!("/{VAULT_NAME}/profile/identity"),
        r#"{"title": "Voice Profiles"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Lands directly in the new, empty document's own edit form via
    // `HX-Push-Url` (address bar updated in place) rather than a full
    // `HX-Redirect` navigation to its read view - matching
    // `routes::wiki`'s identical "new" contract.
    let target = headers
        .get("hx-push-url")
        .ok_or("expected an HX-Push-Url header")?
        .to_str()?;
    assert_eq!(target, format!("/{VAULT_NAME}/profile/identity/voice-profiles.md"));
    assert!(body.contains("wiki-edit-form"), "{body}");
    assert!(
        vault_dir.path().join("memory/identity/voice-profiles.md").is_file(),
        "the new identity file should exist on disk"
    );
    Ok(())
}

#[tokio::test]
async fn creating_a_document_with_an_empty_title_is_refused() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, _headers) =
        post(&router, &format!("/{VAULT_NAME}/profile/operating"), r#"{"title": ""}"#).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("A file needs a title."), "{body}");
    Ok(())
}

fn seed_one_entity(vault_dir: &Path) -> std::io::Result<()> {
    write(
        vault_dir,
        "memory/entities/index.md",
        "# Entities\n\n| File | Entity |\n| --- | --- |\n| zenith.md | Zenith |\n",
    )
}

#[tokio::test]
async fn a_configured_entity_missing_its_own_file_and_folder_shows_a_provisioning_prompt() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/profile/entities/zenith")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("has not been set up yet"), "{body}");
    assert!(
        body.contains(&format!("/{VAULT_NAME}/profile/entities/zenith")),
        "{body}"
    );
    Ok(())
}

#[tokio::test]
async fn provisioning_a_configured_entity_creates_both_its_profile_page_and_its_own_top_level_folder()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, _headers) = post(&router, &format!("/{VAULT_NAME}/profile/entities/zenith"), "{}").await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("has not been set up yet"), "{body}");

    assert!(
        vault_dir.path().join("memory/entities/zenith.md").is_file(),
        "the entity's own profile page should now exist"
    );
    assert!(
        vault_dir.path().join("zenith").is_dir(),
        "the entity's own top-level folder should now exist"
    );
    Ok(())
}

#[tokio::test]
async fn provisioning_an_unconfigured_slug_is_a_404() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body, _headers) = post(&router, &format!("/{VAULT_NAME}/profile/entities/nonexistent"), "{}").await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn a_real_file_under_entities_matching_no_configured_slug_still_shows_as_an_orphan() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/entities/stray-note.md",
        "---\ntitle: Stray Note\n---\nNot a configured entity.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/profile/entities/stray-note")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Not a configured entity."), "{body}");
    Ok(())
}

#[tokio::test]
async fn get_on_neither_a_configured_slug_nor_a_real_orphan_file_is_a_404() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body) = get(&router, &format!("/{VAULT_NAME}/profile/entities/nonexistent")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}
