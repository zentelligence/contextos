//! Notes screen: the create form and posting a new note.
//! Real `contextos-mcp` sessions against real temporary vault fixtures, no mocking, per `testing.md`.

mod notes_common;
mod support;

use notes_common::*;

// ---------------------------------------------------------------------
// GET /{{vault_name}}/notes/new - the "New" form
// ---------------------------------------------------------------------

#[tokio::test]
async fn get_new_pre_fills_the_form_from_the_configured_template_and_a_free_default_path() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_template(vault_dir.path())?;
    write(
        vault_dir.path(),
        "notes/2026/09/existing.md",
        "---\ntitle: Existing note\ndate: 2026-09-01\ntime: 07:00\nstatus: draft\ntags: []\n---\n\nExisting body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/notes/new")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("id=\"notes-create-path-field\""), "{body}");
    let default_path = extract_create_form_path(&body).ok_or("a pre-filled default path")?;
    assert_ne!(default_path, "2026/09/existing.md");
    assert!(
        std::path::Path::new(&default_path)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("md")),
        "{default_path}"
    );
    // The dated note already on disk is still shown in the sidebar
    // alongside the create form, matching every other action's own
    // "whole screen, list included" response shape.
    assert!(body.contains("Existing note"), "{body}");
    Ok(())
}

// ---------------------------------------------------------------------
// POST /{{vault_name}}/notes/ - the "New" form's own submit action
// ---------------------------------------------------------------------

#[tokio::test]
async fn posting_new_creates_a_note_through_note_create_and_redirects_to_it() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_template(vault_dir.path())?;
    write(
        vault_dir.path(),
        "notes/2026/09/existing.md",
        "---\ntitle: Existing note\ndate: 2026-09-01\ntime: 07:00\nstatus: draft\ntags: []\n---\n\nExisting body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, form_body) = get(&router, &format!("/{VAULT_NAME}/notes/new")).await?;
    let default_path = extract_create_form_path(&form_body).ok_or("a pre-filled default path")?;

    let (status, body, headers) = post(
        &router,
        &format!("/{VAULT_NAME}/notes/"),
        &format!(r#"{{"path": "{default_path}", "content": "A fresh note body."}}"#),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let relative = redirected_note(&headers).ok_or("HX-Redirect header present with a note path")?;
    assert_eq!(relative, default_path);

    let content = std::fs::read_to_string(vault_dir.path().join("notes").join(&relative))?;
    assert!(content.contains("type: note"), "{content}");
    assert!(content.contains("status: draft"), "{content}");
    assert!(content.contains("A fresh note body."), "{content}");
    Ok(())
}

#[tokio::test]
async fn posting_new_when_note_create_itself_rejects_the_write_reports_the_failure() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), "templates/unique-note.md", TITLE_COLLIDING_TEMPLATE)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, form_body) = get(&router, &format!("/{VAULT_NAME}/notes/new")).await?;
    let default_path = extract_create_form_path(&form_body).ok_or("a pre-filled default path")?;

    let (status, body, headers) = post(
        &router,
        &format!("/{VAULT_NAME}/notes/"),
        &format!(r#"{{"path": "{default_path}", "content": "Body."}}"#),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(headers.get("HX-Redirect").is_none(), "{headers:?}");
    assert!(body.contains("could not be created"), "{body}");
    // The rejected form's own submitted values are preserved, not lost.
    assert!(body.contains(&format!("value=\"{default_path}\"")), "{body}");
    assert!(body.contains("Body."), "{body}");
    assert!(
        !vault_dir.path().join("notes").exists(),
        "a rejected create must never leave a partial write behind"
    );
    Ok(())
}

#[tokio::test]
async fn posting_new_with_an_empty_path_re_renders_the_form_with_an_actionable_error() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_template(vault_dir.path())?;
    write(
        vault_dir.path(),
        "notes/2026/09/existing.md",
        "---\ntitle: Existing\n---\n\nBody.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = post(
        &router,
        &format!("/{VAULT_NAME}/notes/"),
        r#"{"path": "", "content": "Some body."}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(headers.get("HX-Redirect").is_none(), "{headers:?}");
    assert!(body.contains("needs a file name"), "{body}");
    assert!(body.contains("id=\"notes-create-path-field\""), "{body}");
    Ok(())
}
