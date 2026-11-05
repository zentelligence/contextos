//! Notes screen: renaming and deleting notes.
//! Real `contextos-mcp` sessions against real temporary vault fixtures, no mocking, per `testing.md`.

mod notes_common;
mod support;

use notes_common::*;

// ---------------------------------------------------------------------
// PATCH /{{vault_name}}/notes/rename
// ---------------------------------------------------------------------

#[tokio::test]
async fn renaming_a_note_moves_it_within_the_same_directory_and_redirects_to_its_new_address() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "notes/2026/09/2026-09-14-0930.md",
        "---\ntitle: A note\ndate: 2026-09-14\ntime: 09:30\nstatus: draft\ntags: []\n---\n\nBody.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = patch(
        &router,
        &format!("/{VAULT_NAME}/notes/2026/09/2026-09-14-0930.md/rename"),
        r#"{"new_name": "renamed"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let redirect = headers
        .get("HX-Redirect")
        .and_then(|value| value.to_str().ok())
        .ok_or("HX-Redirect header present")?;
    assert_eq!(redirect, format!("/{VAULT_NAME}/notes/2026/09/renamed.md"));

    assert!(!vault_dir.path().join("notes/2026/09/2026-09-14-0930.md").exists());
    let moved = std::fs::read_to_string(vault_dir.path().join("notes/2026/09/renamed.md"))?;
    assert!(moved.contains("title: A note"), "{moved}");
    assert!(moved.contains("Body."), "{moved}");
    Ok(())
}

#[tokio::test]
async fn renaming_to_a_name_already_taken_in_the_same_directory_is_rejected() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "notes/2026/09/one.md",
        "---\ntitle: One\ndate: 2026-09-14\ntime: 09:30\nstatus: draft\ntags: []\n---\n\nOne body.\n",
    )?;
    write(
        vault_dir.path(),
        "notes/2026/09/two.md",
        "---\ntitle: Two\ndate: 2026-09-14\ntime: 09:31\nstatus: draft\ntags: []\n---\n\nTwo body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, _) = patch(
        &router,
        &format!("/{VAULT_NAME}/notes/2026/09/one.md/rename"),
        r#"{"new_name": "two"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("could not be renamed"), "{body}");
    assert!(vault_dir.path().join("notes/2026/09/one.md").exists());
    assert!(vault_dir.path().join("notes/2026/09/two.md").exists());
    Ok(())
}

#[tokio::test]
async fn renaming_to_a_name_carrying_a_path_segment_is_rejected() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "notes/2026/09/one.md",
        "---\ntitle: One\ndate: 2026-09-14\ntime: 09:30\nstatus: draft\ntags: []\n---\n\nOne body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, _) = patch(
        &router,
        &format!("/{VAULT_NAME}/notes/2026/09/one.md/rename"),
        r#"{"new_name": "../escape"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("File name"), "{body}");
    assert!(vault_dir.path().join("notes/2026/09/one.md").exists());
    Ok(())
}

// ---------------------------------------------------------------------
// GET /{{vault_name}}/notes/confirm-delete
// ---------------------------------------------------------------------

#[tokio::test]
async fn confirm_delete_returns_a_confirmation_fragment_naming_the_note() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "notes/2026/09/one.md",
        "---\ntitle: One to delete\ndate: 2026-09-14\ntime: 09:30\nstatus: draft\ntags: []\n---\n\nOne body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/notes/2026/09/one.md/confirm-delete")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Names the note being deleted, so the operator can recognise it before
    // confirming.
    assert!(body.contains("One to delete"), "{body}");
    assert!(body.contains("one.md"), "{body}");
    // The contract's own `GET /{entities}/{id}/confirm-delete` pattern: a
    // real `hx-delete` trigger inside the confirmation fragment itself,
    // addressing this exact note by its own real path.
    assert!(
        body.contains(&format!("hx-delete=\"/{VAULT_NAME}/notes/2026/09/one.md\"")),
        "{body}"
    );
    assert!(body.contains("Cancel"), "{body}");
    // Never the whole screen chrome - this is an inline fragment swapped
    // into the main pane, not a page.
    assert!(!body.contains("<html"), "{body}");
    Ok(())
}

#[tokio::test]
async fn confirm_delete_for_a_note_that_does_not_exist_renders_the_full_screen_error_not_a_raw_404()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "notes/2026/09/existing.md",
        "---\ntitle: Existing\ndate: 2026-09-14\ntime: 09:30\nstatus: draft\ntags: []\n---\n\nBody.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    // A validly-shaped path (real `.md` file name, no traversal), but
    // nothing actually written at it.
    let (status, body) = get(
        &router,
        &format!("/{VAULT_NAME}/notes/2026/09/missing.md/confirm-delete"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("could not be found"), "{body}");
    Ok(())
}

#[tokio::test]
async fn confirm_delete_with_a_malformed_note_path_is_rejected_as_a_plain_not_found() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    // Traversal is never a valid note path - `validate_note_path`'s own
    // contract, the same one `patch_body`/`patch_rename_dispatch` already
    // rely on: this is a stable rejection, not a rendered fragment of any
    // kind.
    let (status, _) = get(&router, &format!("/{VAULT_NAME}/notes/../../etc/passwd/confirm-delete")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

// ---------------------------------------------------------------------
// DELETE /{{vault_name}}/notes/
// ---------------------------------------------------------------------

#[tokio::test]
async fn deleting_a_note_removes_it_and_it_no_longer_appears_in_a_subsequent_list() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "notes/2026/09/older.md",
        "---\ntitle: Older note\ndate: 2026-09-10\ntime: 07:00\nstatus: draft\ntags: []\n---\n\nOlder body.\n",
    )?;
    write(
        vault_dir.path(),
        "notes/2026/09/newer.md",
        "---\ntitle: Newer note\ndate: 2026-09-14\ntime: 09:30\nstatus: draft\ntags: []\n---\n\nNewer body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = delete(&router, &format!("/{VAULT_NAME}/notes/2026/09/older.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The whole `.notes-screen` fragment comes back (same convention as
    // every other mutation on this screen), and the deleted note's own
    // title is gone from it - both its sidebar row and, since the only
    // other note is now selected, its own detail pane.
    assert!(!body.contains("Older note"), "{body}");
    assert!(!body.contains("Older body."), "{body}");
    assert!(body.contains("Newer note"), "{body}");

    assert!(!vault_dir.path().join("notes/2026/09/older.md").exists());

    // A fresh `GET` confirms the same thing from the list's own
    // freshly-loaded state, not just this one response.
    let (status, body) = get(&router, &format!("/{VAULT_NAME}/notes/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("Older note"), "{body}");
    assert!(body.contains("Newer note"), "{body}");
    Ok(())
}

#[tokio::test]
async fn deleting_a_note_that_does_not_exist_returns_the_error_fragment_not_a_raw_404() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "notes/2026/09/existing.md",
        "---\ntitle: Existing\ndate: 2026-09-14\ntime: 09:30\nstatus: draft\ntags: []\n---\n\nBody.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = delete(&router, &format!("/{VAULT_NAME}/notes/2026/09/missing.md")).await?;
    // Never a raw 404 for this case - the HTMX surface's own error-fragment
    // rule: a fragment the caller can swap straight in, not a bare status
    // code with no body HTMX itself can render anywhere.
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("could not be"), "{body}");
    assert!(vault_dir.path().join("notes/2026/09/existing.md").exists());
    Ok(())
}

#[tokio::test]
async fn deleting_with_a_malformed_note_path_returns_a_raw_not_found() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "notes/2026/09/one.md",
        "---\ntitle: One\ndate: 2026-09-14\ntime: 09:30\nstatus: draft\ntags: []\n---\n\nOne body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    // The bare root names no note at all - `delete_root`'s own "never a
    // deletable target of its own" contract.
    let (status, _) = delete(&router, &format!("/{VAULT_NAME}/notes/")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A non-`.md` path is equally malformed - `validate_note_path`'s own
    // contract, the same one `patch_body`/`patch_rename_dispatch` already
    // rely on.
    let (status, _) = delete(&router, &format!("/{VAULT_NAME}/notes/2026/09")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);

    assert!(vault_dir.path().join("notes/2026/09/one.md").exists());
    Ok(())
}

// ---------------------------------------------------------------------
// A renamed note's own relative path, once it contains an apostrophe, is
// still safe wherever this screen renders it back.
//
// (The "+ New" button's own former `hx-vals` DOM-read-then-splice pattern
// this suite used to guard here no longer exists at all - "+ New" is now a
// plain `hx-get` navigation to the "New" form with no note-specific value
// embedded in any JS expression, so that specific injection surface was
// removed along with the immediate-create flow, not merely made safe.)
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_note_renamed_to_contain_an_apostrophe_still_renders_safely_everywhere_it_appears() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "notes/2026/09/one.md",
        "---\ntitle: One\ndate: 2026-09-14\ntime: 09:30\nstatus: draft\ntags: []\n---\n\nOne body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _, _) = patch(
        &router,
        &format!("/{VAULT_NAME}/notes/2026/09/one.md/rename"),
        r#"{"new_name": "client's report"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK);

    let encoded = contextos_web::inbox_outbox::encode_query_value("2026/09/client's report.md");
    let (status, body) = get(&router, &format!("/{VAULT_NAME}/notes/{encoded}")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Askama's own default HTML-attribute escaping, never a raw splice: the
    // rename field's own starting value renders the apostrophe escaped.
    assert!(body.contains("value=\"client&#39;s report\""), "{body}");
    Ok(())
}
