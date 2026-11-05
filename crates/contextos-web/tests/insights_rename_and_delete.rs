//! Insights: renaming, the edit suffix, and deleting.
//! Real `contextos-mcp` sessions against real temporary vault fixtures, no mocking, per `testing.md`.

mod insights_common;
mod support;

use insights_common::*;

// ---------------------------------------------------------------------
// PATCH /{{vault_name}}/insights/{{*path}}/rename - the combined
// title/status action
// ---------------------------------------------------------------------

#[tokio::test]
async fn renaming_updates_the_title_and_status_and_moves_the_file_to_the_retitled_slug_and_shard()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/insights/a/agent-backbone.md",
        "---\ntitle: Agent backbone\nstatus: adopted\n---\n\nThree components.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = patch_with_headers(
        &router,
        &format!("/{VAULT_NAME}/insights/a/agent-backbone.md/rename"),
        r#"{"title":"Deterministic Beats Probabilistic","status":"dismissed"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let redirected = redirected_path(&headers).ok_or("expected an HX-Redirect target")?;
    assert_eq!(redirected, "d/deterministic-beats-probabilistic.md");

    assert!(!vault_dir.path().join("memory/insights/a/agent-backbone.md").exists());
    let content = std::fs::read_to_string(
        vault_dir
            .path()
            .join("memory/insights/d/deterministic-beats-probabilistic.md"),
    )?;
    assert!(
        content.contains("title: Deterministic Beats Probabilistic"),
        "{content}"
    );
    assert!(content.contains("status: dismissed"), "{content}");
    assert!(content.contains("Three components."), "{content}");
    Ok(())
}

#[tokio::test]
async fn renaming_with_an_empty_title_is_refused_and_leaves_the_file_untouched() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/insights/a/agent-backbone.md",
        "---\ntitle: Agent backbone\nstatus: adopted\n---\n\nThree components.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = patch_with_headers(
        &router,
        &format!("/{VAULT_NAME}/insights/a/agent-backbone.md/rename"),
        r#"{"title":"   ","status":"adopted"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(headers.get("HX-Redirect").is_none(), "{body}");
    assert!(body.contains("A note needs a title."), "{body}");
    assert!(vault_dir.path().join("memory/insights/a/agent-backbone.md").exists());
    Ok(())
}

#[tokio::test]
async fn renaming_with_an_invalid_status_is_refused_and_leaves_the_file_untouched() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/insights/a/agent-backbone.md",
        "---\ntitle: Agent backbone\nstatus: adopted\n---\n\nThree components.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, headers) = patch_with_headers(
        &router,
        &format!("/{VAULT_NAME}/insights/a/agent-backbone.md/rename"),
        r#"{"title":"Agent backbone","status":"maybe"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(headers.get("HX-Redirect").is_none(), "{body}");
    assert!(
        body.contains("Status must be one of proposed, adopted, or dismissed."),
        "{body}"
    );
    let content = std::fs::read_to_string(vault_dir.path().join("memory/insights/a/agent-backbone.md"))?;
    assert!(content.contains("status: adopted"), "{content}");
    Ok(())
}

// ---------------------------------------------------------------------
// GET /{{vault_name}}/insights/{{*path}}/edit
// ---------------------------------------------------------------------

#[tokio::test]
async fn get_on_a_notes_edit_suffix_returns_a_pre_populated_edit_form_fragment_not_the_read_view()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/insights/a/agent-backbone.md",
        "---\ntitle: Agent backbone\nstatus: adopted\n---\n\nThree components.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/insights/a/agent-backbone.md/edit")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Three components."), "{body}");
    assert!(body.contains("notes-body-textarea"), "{body}");
    assert!(body.contains("id=\"insights-main-body\""), "{body}");
    // The pre-populated edit form, never the normal rendered-markdown read
    // view alongside it.
    assert!(!body.contains("wiki-rendered"), "{body}");
    // The read view's own Rename/Edit/Delete cluster lives outside
    // `#insights-main-body`: a response that swapped only that div would
    // leave it stale, showing both the read-mode actions and the edit-mode
    // Save/Cancel pair at once. The `/edit` response is the whole screen
    // (its own shard picker included) with `editing` populated instead, so
    // the header's own conditional actually takes effect.
    assert!(!body.contains("title=\"Edit body\""), "{body}");
    assert!(!body.contains("title=\"Delete note\""), "{body}");
    assert!(body.contains("class=\"insights-screen\""), "{body}");
    // Never the whole screen chrome - a reserved sub-path always answers
    // with just its own fragment.
    assert!(!body.contains("<html"), "{body}");
    Ok(())
}

#[tokio::test]
async fn a_normal_get_after_visiting_edit_still_renders_the_plain_read_view() -> Result<(), BoxError> {
    // The Cancel control in the edit fragment is a plain `hx-get` back to
    // this exact route - it adds no server-side state of its own, so a
    // normal `GET` on the note's own path must keep answering with the
    // read view regardless of whether an edit form was ever opened.
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/insights/a/agent-backbone.md",
        "---\ntitle: Agent backbone\nstatus: adopted\n---\n\n# Agent backbone\n\nThree components.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_status, _edit_body) = get(&router, &format!("/{VAULT_NAME}/insights/a/agent-backbone.md/edit")).await?;
    let (status, body) = get(&router, &format!("/{VAULT_NAME}/insights/a/agent-backbone.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("wiki-rendered"), "{body}");
    assert!(body.contains("id=\"insights-main-body\""), "{body}");
    assert!(!body.contains("notes-body-textarea"), "{body}");
    // The Rename/Edit/Delete cluster is back, and the edit form is gone - the metadata panel's
    // own always-present "Save" button (`patching_the_metadata_panel_...` tests cover that one
    // separately) is not this check's concern, so it asserts on the edit form's own
    // distinguishing class rather than the "Save" text a body-save button and the metadata
    // panel's save button now both carry, matching `tests/wiki.rs`'s identical fix.
    assert!(body.contains("title=\"Edit body\""), "{body}");
    assert!(!body.contains("class=\"wiki-edit-form\""), "{body}");
    Ok(())
}

#[tokio::test]
async fn get_on_a_missing_notes_edit_suffix_is_a_404() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body) = get(&router, &format!("/{VAULT_NAME}/insights/a/missing.md/edit")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn get_on_a_bare_shards_edit_suffix_is_a_404() -> Result<(), BoxError> {
    // `a/edit` names no real note (`as_note_path`'s own two-segment shape),
    // so it must 404 the same way any other malformed path already does -
    // never be mistaken for the shard `a`'s own edit action.
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body) = get(&router, &format!("/{VAULT_NAME}/insights/a/edit")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

// ---------------------------------------------------------------------
// GET /{{vault_name}}/insights/{{*path}}/confirm-delete
// ---------------------------------------------------------------------

#[tokio::test]
async fn confirm_delete_returns_a_confirmation_fragment_naming_the_note() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/insights/a/agent-backbone.md",
        "---\ntitle: Agent backbone\nstatus: adopted\n---\n\nThree components.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(
        &router,
        &format!("/{VAULT_NAME}/insights/a/agent-backbone.md/confirm-delete"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Agent backbone"), "{body}");
    assert!(body.contains("agent-backbone.md"), "{body}");
    assert!(
        body.contains(&format!("hx-delete=\"/{VAULT_NAME}/insights/a/agent-backbone.md\"")),
        "{body}"
    );
    assert!(body.contains("Cancel"), "{body}");
    assert!(!body.contains("<html"), "{body}");
    Ok(())
}

#[tokio::test]
async fn confirm_delete_for_a_note_that_does_not_exist_renders_the_shard_fragment_error_not_a_raw_404()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/insights/a/missing.md/confirm-delete")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("could not be found"), "{body}");
    Ok(())
}

// ---------------------------------------------------------------------
// DELETE /{{vault_name}}/insights/{{*path}}
// ---------------------------------------------------------------------

#[tokio::test]
async fn deleting_a_note_removes_it_and_it_no_longer_appears_in_a_subsequent_shard_listing() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/insights/a/agent-backbone.md",
        "---\ntitle: Agent backbone\nstatus: adopted\n---\n\nThree components.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = delete(&router, &format!("/{VAULT_NAME}/insights/a/agent-backbone.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!vault_dir.path().join("memory/insights/a/agent-backbone.md").exists());

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/insights/a")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains("Agent backbone"), "{body}");
    assert!(body.contains("Nothing in this shard."), "{body}");
    Ok(())
}

#[tokio::test]
async fn deleting_a_note_that_does_not_exist_returns_the_error_fragment_not_a_raw_404() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = delete(&router, &format!("/{VAULT_NAME}/insights/a/missing.md")).await?;
    // Never a raw 404 for this case - the HTMX surface's own error-fragment
    // rule.
    assert_ne!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("could not be found"), "{body}");
    Ok(())
}

#[tokio::test]
async fn deleting_a_bare_shard_path_is_a_404() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body) = delete(&router, &format!("/{VAULT_NAME}/insights/a")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn deleting_with_a_traversal_path_is_a_404() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body) = delete(&router, &format!("/{VAULT_NAME}/insights/../../etc/passwd")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}
