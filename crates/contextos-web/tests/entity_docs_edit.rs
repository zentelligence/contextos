//! Entity documents: editing, metadata, and deleting.
//! Real `contextos-mcp` sessions against real temporary vault fixtures, no mocking, per `testing.md`.

mod entity_docs_common;
mod support;

use entity_docs_common::*;

// ---------------------------------------------------------------------
// GET .../edit and PATCH - editing an existing markdown file
// ---------------------------------------------------------------------

#[tokio::test]
async fn get_on_a_file_paths_own_edit_sub_path_pre_fills_the_form_then_cancel_returns_to_the_read_view()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    write(
        vault_dir.path(),
        "zenith/finance/tax.md",
        "---\ntitle: Tax\n---\n\n# Tax rules\n\nPay quarterly.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, _headers) = get(&router, &format!("/{VAULT_NAME}/entity/zenith/finance/tax.md/edit")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("# Tax rules"), "{body}");
    assert!(body.contains("Pay quarterly."), "{body}");
    assert!(!body.contains("title: Tax"), "{body}");
    // The read view's own Edit/Delete cluster lives outside
    // `#entity-docs-main-body`: a response that swapped only that div
    // would leave it stale, showing both the read-mode actions and the
    // edit-mode Save/Cancel pair at once. The `/edit` response is the
    // whole screen (its own sidebar tree included) with `editing`
    // populated instead, so the header's own conditional actually takes
    // effect.
    assert!(!body.contains("title=\"Edit file\""), "{body}");
    assert!(!body.contains("title=\"Delete file\""), "{body}");
    assert!(body.contains("class=\"entity-docs-screen\""), "{body}");

    // Cancel returns to the plain read view.
    let (cancel_status, cancel_body, _headers) =
        get(&router, &format!("/{VAULT_NAME}/entity/zenith/finance/tax.md")).await?;
    assert_eq!(cancel_status, StatusCode::OK, "{cancel_body}");
    assert!(cancel_body.contains("Tax rules"), "{cancel_body}");
    // The Edit/Delete cluster is back, and the edit form is gone - the metadata panel's own
    // always-present "Save" button (`patching_the_metadata_panel_...` tests cover it) needs a
    // distinguishing class rather than the "Save" text a body-save button and the metadata
    // panel's Save button share, matching `tests::wiki`'s identical fix for the same shared
    // wording.
    assert!(cancel_body.contains("title=\"Edit file\""), "{cancel_body}");
    assert!(
        !cancel_body.contains("class=\"entity-docs-edit-form\""),
        "{cancel_body}"
    );
    Ok(())
}

#[tokio::test]
async fn saving_an_edited_body_persists_it_and_keeps_the_frontmatter_untouched() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    write(
        vault_dir.path(),
        "zenith/finance/tax.md",
        "---\ntitle: Tax\nstatus: draft\n---\n\n# Tax rules\n\nPay quarterly.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, body, _headers) = get(&router, &format!("/{VAULT_NAME}/entity/zenith/finance/tax.md/edit")).await?;
    let expected_hash = extract_expected_hash(&body).ok_or("expected an expected_hash field")?;

    let (status, saved_body) = patch(
        &router,
        &format!("/{VAULT_NAME}/entity/zenith/finance/tax.md"),
        &format!(r##"{{"content":"# Tax rules\n\nPay monthly now.\n","expected_hash":"{expected_hash}"}}"##),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{saved_body}");
    assert!(saved_body.contains("Pay monthly now."), "{saved_body}");

    let content = std::fs::read_to_string(vault_dir.path().join("zenith/finance/tax.md"))?;
    assert!(content.contains("title: Tax"), "{content}");
    assert!(content.contains("status: draft"), "{content}");
    assert!(content.contains("Pay monthly now."), "{content}");
    assert!(!content.contains("Pay quarterly."), "{content}");
    Ok(())
}

#[tokio::test]
async fn saving_a_missing_file_is_a_404() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body) = patch(
        &router,
        &format!("/{VAULT_NAME}/entity/zenith/finance/missing.md"),
        r#"{"content":"Anything.\n"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn saving_the_body_of_a_file_with_unparsable_frontmatter_reports_it_without_writing() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    // Opens a frontmatter block but never closes it - `frontmatter_read` reports this as its own
    // `Unclosed` error, real content the file still has (unlike a missing file, which `read_file`'s
    // own preceding fresh read already rules out), so `entity_docs::save_file_body` reaches
    // `SaveEntityFileError::Malformed` rather than misreporting it as `Load(NotFound)` - the
    // latent defect Wiki's own reviewer found in `wiki::save_page_body`
    // (`saving_the_body_of_a_page_with_unparsable_frontmatter_reports_it_without_writing`), shared
    // via `vault_content::save_body`'s identical contract and confirmed present here too.
    let malformed = "---\ntitle: Tax\n\nBody with no closing frontmatter delimiter.\n";
    write(vault_dir.path(), "zenith/finance/tax.md", malformed)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/entity/zenith/finance/tax.md"),
        r#"{"content":"New body.\n","expected_hash":null}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("frontmatter could not be read"), "{body}");

    let unchanged = std::fs::read_to_string(vault_dir.path().join("zenith/finance/tax.md"))?;
    assert_eq!(unchanged, malformed);
    Ok(())
}

#[tokio::test]
async fn the_metadata_panel_renders_every_real_frontmatter_field_on_a_selected_file() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    write(
        vault_dir.path(),
        "zenith/finance/tax.md",
        "---\ntitle: Tax\nstatus: draft\ntags:\n  - finance\n  - quarterly\n---\n\n# Tax rules\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, _headers) = get(&router, &format!("/{VAULT_NAME}/entity/zenith/finance/tax.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("data-field=\"title\""), "{body}");
    assert!(body.contains("data-field=\"status\""), "{body}");
    assert!(body.contains("value=\"draft\""), "{body}");
    assert!(body.contains("data-field=\"tags\""), "{body}");
    assert!(body.contains("finance, quarterly"), "{body}");
    // The metadata panel's own save target is scoped to this entity's own slug, not a fixed
    // literal like Wiki's `"wiki"`.
    assert!(
        body.contains("hx-patch=\"/contract-fixture/entity/zenith/finance/tax.md\""),
        "{body}"
    );
    Ok(())
}

#[tokio::test]
async fn a_file_with_no_frontmatter_fields_shows_the_generic_empty_state_message() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    write(
        vault_dir.path(),
        "zenith/finance/tax.md",
        "Plain body with no frontmatter block at all.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, _headers) = get(&router, &format!("/{VAULT_NAME}/entity/zenith/finance/tax.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("This file has no frontmatter fields."), "{body}");
    Ok(())
}

#[tokio::test]
async fn patching_the_metadata_panel_saves_the_submitted_fields_and_leaves_the_body_untouched() -> Result<(), BoxError>
{
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    write(
        vault_dir.path(),
        "zenith/finance/tax.md",
        "---\ntitle: Tax\nstatus: draft\n---\n\n# Tax rules\n\nPay quarterly.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, body, _headers) = get(&router, &format!("/{VAULT_NAME}/entity/zenith/finance/tax.md")).await?;
    let expected_hash = extract_metadata_expected_hash(&body).ok_or("expected an expected_hash field")?;

    let (status, saved_body) = patch(
        &router,
        &format!("/{VAULT_NAME}/entity/zenith/finance/tax.md"),
        &format!(r#"{{"patch":{{"status":"final"}},"expected_hash":"{expected_hash}"}}"#),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{saved_body}");

    let content = std::fs::read_to_string(vault_dir.path().join("zenith/finance/tax.md"))?;
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
    seed_one_entity(vault_dir.path())?;
    write(
        vault_dir.path(),
        "zenith/finance/tax.md",
        "---\ntitle: Tax\nstatus: draft\n---\n\nOriginal body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/entity/zenith/finance/tax.md"),
        r#"{"patch":{"status":"final"},"expected_hash":"not-the-real-hash"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains("This file changed since it was last loaded; reload and try again."),
        "{body}"
    );

    let content = std::fs::read_to_string(vault_dir.path().join("zenith/finance/tax.md"))?;
    assert!(content.contains("status: draft"), "{content}");
    Ok(())
}

#[tokio::test]
async fn patching_the_metadata_panel_on_a_file_with_unparsable_frontmatter_reports_it_without_writing()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    // Opens a frontmatter block but never closes it - `frontmatter_read` reports this as its own
    // `Unclosed` error, real content the file still has (unlike a missing file), so
    // `entity_docs::load_file_frontmatter` reaches `LoadEntityFileError::Malformed` on its own
    // fresh re-read rather than `NotFound`, matching `tests::wiki`'s identical
    // `patching_the_metadata_panel_on_a_page_with_unparsable_frontmatter_reports_it_without_writing`
    // case for the metadata-patch path instead of the body-save one.
    let malformed = "---\ntitle: Tax\n\nBody with no closing frontmatter delimiter.\n";
    write(vault_dir.path(), "zenith/finance/tax.md", malformed)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/entity/zenith/finance/tax.md"),
        r#"{"patch":{"status":"final"},"expected_hash":null}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("frontmatter could not be read"), "{body}");

    let unchanged = std::fs::read_to_string(vault_dir.path().join("zenith/finance/tax.md"))?;
    assert_eq!(unchanged, malformed);
    Ok(())
}

#[tokio::test]
async fn the_metadata_panel_side_swaps_the_layout_class_independently_of_the_folder_side() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    write(
        vault_dir.path(),
        "zenith/finance/tax.md",
        "---\ntitle: Tax\n---\n\n# Tax rules\n",
    )?;
    // The folder pane stays on its own shipped default (right); only the metadata panel moves to
    // the left, so the layout needs its own dedicated left column for it
    // (`notes-layout-metadata-left-panel-right`) rather than either single-side modifier.
    write(
        vault_dir.path(),
        ".contextos/entity-docs.toml",
        "metadata_panel_side = \"left\"\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, _headers) = get(&router, &format!("/{VAULT_NAME}/entity/zenith/finance/tax.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("notes-layout-metadata-left-panel-right"), "{body}");
    Ok(())
}

// ---------------------------------------------------------------------
// GET .../confirm-delete and DELETE - a markdown file
// ---------------------------------------------------------------------

#[tokio::test]
async fn confirm_delete_names_the_file_and_delete_moves_it_to_trash() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    write(
        vault_dir.path(),
        "zenith/finance/tax.md",
        "---\ntitle: Tax\n---\n\n# Tax\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, _headers) = get(
        &router,
        &format!("/{VAULT_NAME}/entity/zenith/finance/tax.md/confirm-delete"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Delete file"), "{body}");
    assert!(body.contains("tax"), "{body}");

    let (status, body) = delete(&router, &format!("/{VAULT_NAME}/entity/zenith/finance/tax.md")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !vault_dir.path().join("zenith/finance/tax.md").exists(),
        "the file should have been moved to trash"
    );
    Ok(())
}

// ---------------------------------------------------------------------
// GET .../confirm-delete and DELETE - a non-markdown file: deletable even
// though it is never rendered inline on this screen
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_non_markdown_file_is_deletable_even_though_it_has_no_inline_read_view() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    write(vault_dir.path(), "zenith/finance/export.csv", "a,b\n1,2\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, _headers) = get(
        &router,
        &format!("/{VAULT_NAME}/entity/zenith/finance/export.csv/confirm-delete"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Delete file"), "{body}");
    assert!(body.contains("export.csv"), "{body}");

    let (status, body) = delete(&router, &format!("/{VAULT_NAME}/entity/zenith/finance/export.csv")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !vault_dir.path().join("zenith/finance/export.csv").exists(),
        "a non-markdown file should be deletable even though it is never rendered inline here"
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
    seed_one_entity(vault_dir.path())?;
    write(vault_dir.path(), "zenith/finance/.gitkeep", "")?;
    // Genuinely empty on disk, matching `tests/wiki.rs`'s identical fixture
    // reasoning - no leftover placeholder file inside it.
    std::fs::create_dir_all(vault_dir.path().join("zenith/finance/tax"))?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, _headers) = get(
        &router,
        &format!("/{VAULT_NAME}/entity/zenith/finance/tax/confirm-delete"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Delete folder"), "{body}");
    assert!(body.contains("tax"), "{body}");

    let (status, body) = delete(&router, &format!("/{VAULT_NAME}/entity/zenith/finance/tax")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        !vault_dir.path().join("zenith/finance/tax").exists(),
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
    seed_one_entity(vault_dir.path())?;
    write(
        vault_dir.path(),
        "zenith/finance/tax.md",
        "---\ntitle: Tax\n---\n\n# Tax\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = delete(&router, &format!("/{VAULT_NAME}/entity/zenith/finance")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains("This folder still has files or subfolders and must be emptied first."),
        "{body}"
    );
    assert!(
        vault_dir.path().join("zenith/finance/tax.md").exists(),
        "the non-empty folder must survive a refused delete"
    );
    Ok(())
}

#[tokio::test]
async fn delete_on_an_entitys_own_root_is_a_404() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body) = delete(&router, &format!("/{VAULT_NAME}/entity/zenith")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn delete_on_a_path_naming_neither_a_real_file_nor_a_real_folder_is_a_404() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    seed_one_entity(vault_dir.path())?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, _body) = delete(&router, &format!("/{VAULT_NAME}/entity/zenith/missing.md")).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}
