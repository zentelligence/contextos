//! Notes screen: saving a note's body and metadata.
//! Real `contextos-mcp` sessions against real temporary vault fixtures, no mocking, per `testing.md`.

mod notes_common;
mod support;

use notes_common::*;

// ---------------------------------------------------------------------
// PATCH /{{vault_name}}/notes/body
// ---------------------------------------------------------------------

#[tokio::test]
async fn saving_the_body_preserves_frontmatter_and_updates_only_the_body() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "notes/2026/09/note.md",
        "---\ntitle: A note\ndate: 2026-09-14\ntime: 09:30\nstatus: draft\ntags: []\n---\n\nOriginal body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (_, loaded) = get(&router, &format!("/{VAULT_NAME}/notes/2026/09/note.md")).await?;
    let expected_hash = extract_expected_hash(&loaded).ok_or("expected_hash present in the rendered form")?;

    let (status, body, _) = patch(
        &router,
        &format!("/{VAULT_NAME}/notes/2026/09/note.md"),
        &format!(r#"{{"content": "Edited body.", "expected_hash": "{expected_hash}"}}"#),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Edited body."), "{body}");

    let saved = std::fs::read_to_string(vault_dir.path().join("notes/2026/09/note.md"))?;
    assert!(saved.contains("title: A note"), "{saved}");
    assert!(saved.contains("status: draft"), "{saved}");
    assert!(saved.contains("Edited body."), "{saved}");
    assert!(!saved.contains("Original body."), "{saved}");
    Ok(())
}

#[tokio::test]
async fn saving_the_body_with_a_stale_hash_is_rejected_and_leaves_the_file_unchanged() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "notes/2026/09/note.md",
        "---\ntitle: A note\ndate: 2026-09-14\ntime: 09:30\nstatus: draft\ntags: []\n---\n\nOriginal body.\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, _) = patch(
        &router,
        &format!("/{VAULT_NAME}/notes/2026/09/note.md"),
        r#"{"content": "Edited body.", "expected_hash": "0000000000000000000000000000000000000000000000000000000000000000"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("changed since it was last loaded"), "{body}");

    let saved = std::fs::read_to_string(vault_dir.path().join("notes/2026/09/note.md"))?;
    assert!(saved.contains("Original body."), "{saved}");
    Ok(())
}

#[tokio::test]
async fn saving_the_body_of_a_note_with_unparsable_frontmatter_reports_it_without_writing() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    // Opens a frontmatter block but never closes it - `frontmatter_read`
    // reports this as its `Unclosed` error, real content the note still
    // has (unlike a missing file), so `save_note_body` reaches
    // `LoadNoteError::Malformed` on its own fresh re-read rather than
    // `NotFound`. Addressed directly by its own path, bypassing the sidebar
    // list (which excludes an unparsable note from selection entirely,
    // `notes::list_notes`), matching how a stale or hand-edited link would
    // reach this same file.
    let malformed = "---\ntitle: A note\n\nBody with no closing frontmatter delimiter.\n";
    write(vault_dir.path(), "notes/2026/09/malformed.md", malformed)?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body, _) = patch(
        &router,
        &format!("/{VAULT_NAME}/notes/2026/09/malformed.md"),
        r#"{"content": "New body.", "expected_hash": null}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("frontmatter could not be read"), "{body}");

    let unchanged = std::fs::read_to_string(vault_dir.path().join("notes/2026/09/malformed.md"))?;
    assert_eq!(unchanged, malformed);
    Ok(())
}

// ---------------------------------------------------------------------
// notes::save_note_metadata - the metadata panel's own write path
// ---------------------------------------------------------------------

#[tokio::test]
async fn saving_metadata_merges_the_patch_and_leaves_the_body_and_other_fields_untouched() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "notes/2026/09/note.md",
        "---\ntitle: A note\ndate: 2026-09-14\ntime: 09:30\nstatus: draft\ntags: []\n---\n\nOriginal body.\n",
    )?;
    let config_path = support::write_vault_config(config_dir.path(), vault_dir.path())?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let client = contextos_web::McpClient::connect(&entry).await?;
    let config = contextos_web::notes_config::NotesConfig::default();

    let mut patch = serde_json::Map::new();
    patch.insert("status".to_owned(), serde_json::Value::String("final".to_owned()));
    let saved =
        contextos_web::notes::save_note_metadata(&client, VAULT_NAME, &config, "2026/09/note.md", patch, None).await?;
    assert!(saved);

    let content = std::fs::read_to_string(vault_dir.path().join("notes/2026/09/note.md"))?;
    assert!(content.contains("status: final"), "{content}");
    assert!(content.contains("title: A note"), "{content}");
    assert!(content.contains("Original body."), "{content}");
    Ok(())
}

#[tokio::test]
async fn saving_metadata_with_a_stale_hash_is_rejected_and_leaves_the_file_unchanged() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let original = "---\ntitle: A note\nstatus: draft\n---\n\nBody.\n";
    write(vault_dir.path(), "notes/2026/09/note.md", original)?;
    let config_path = support::write_vault_config(config_dir.path(), vault_dir.path())?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let client = contextos_web::McpClient::connect(&entry).await?;
    let config = contextos_web::notes_config::NotesConfig::default();

    let mut patch = serde_json::Map::new();
    patch.insert("status".to_owned(), serde_json::Value::String("final".to_owned()));
    let saved = contextos_web::notes::save_note_metadata(
        &client,
        VAULT_NAME,
        &config,
        "2026/09/note.md",
        patch,
        Some("0".repeat(64)),
    )
    .await?;
    assert!(!saved);

    let unchanged = std::fs::read_to_string(vault_dir.path().join("notes/2026/09/note.md"))?;
    assert_eq!(unchanged, original);
    Ok(())
}

#[tokio::test]
async fn saving_metadata_with_a_null_patch_value_removes_that_frontmatter_key() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "notes/2026/09/note.md",
        "---\ntitle: A note\nstatus: draft\n---\n\nBody.\n",
    )?;
    let config_path = support::write_vault_config(config_dir.path(), vault_dir.path())?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let client = contextos_web::McpClient::connect(&entry).await?;
    let config = contextos_web::notes_config::NotesConfig::default();

    let mut patch = serde_json::Map::new();
    patch.insert("status".to_owned(), serde_json::Value::Null);
    let saved =
        contextos_web::notes::save_note_metadata(&client, VAULT_NAME, &config, "2026/09/note.md", patch, None).await?;
    assert!(saved);

    let content = std::fs::read_to_string(vault_dir.path().join("notes/2026/09/note.md"))?;
    assert!(!content.contains("status"), "{content}");
    assert!(content.contains("title: A note"), "{content}");
    Ok(())
}
