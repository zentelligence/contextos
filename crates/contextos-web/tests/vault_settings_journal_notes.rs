//! Vault settings: the journal and notes sections.
//! Real `contextos-mcp` sessions against real temporary vault fixtures, no mocking, per `testing.md`.

mod support;
mod vault_settings_common;

use vault_settings_common::*;

#[tokio::test]
async fn an_absent_journal_toml_pre_fills_the_form_with_the_scaffold_defaults() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/settings/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("value=\"journal\""), "{body}");
    assert!(body.contains("value=\"templates/daily-journal.md\""), "{body}");
    assert!(
        body.contains("id=\"journal-date-format-field\" type=\"text\" value=\"YYYY/MM/YYYY-MM-DD\""),
        "{body}"
    );
    let journal_panel = section_panel(&body, "journal")?;
    assert!(
        !journal_panel.contains("date-structure-active-field"),
        "{journal_panel}"
    );
    assert!(journal_panel.contains("Folder pane position"), "{journal_panel}");
    assert!(!journal_panel.contains("Split the sidebar"), "{journal_panel}");
    assert!(
        journal_panel.contains("<option value=\"right\" selected>Right</option>"),
        "{journal_panel}"
    );
    Ok(())
}

#[tokio::test]
async fn saving_the_journal_section_persists_both_fields() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &journal_body("diary", "templates/diary-entry.md"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");

    let saved = std::fs::read_to_string(vault_dir.path().join(".contextos/journal.toml"))?;
    assert!(saved.contains("diary"), "{saved}");
    assert!(saved.contains("templates/diary-entry.md"), "{saved}");
    assert!(saved.contains("YYYY/MM/YYYY-MM-DD"), "{saved}");
    assert!(!saved.contains("date_structure_active"), "{saved}");
    assert!(saved.contains("date_structure_side = \"right\""), "{saved}");
    Ok(())
}

#[tokio::test]
async fn saving_the_journal_section_persists_a_left_folder_pane_position() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        r#"{"target": "journal", "journal_dir": "journal", "template_path": "templates/daily-journal.md", "date_format": "YYYY-MM-DD", "date_structure_side": "left"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");

    let saved = std::fs::read_to_string(vault_dir.path().join(".contextos/journal.toml"))?;
    assert!(saved.contains("date_structure_side = \"left\""), "{saved}");
    Ok(())
}

#[tokio::test]
async fn saving_an_empty_journal_date_format_is_rejected_and_never_written() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        r#"{"target": "journal", "journal_dir": "journal", "template_path": "templates/daily-journal.md", "date_format": "", "date_structure_side": "right"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("date_format"), "{body}");
    assert!(!vault_dir.path().join(".contextos/journal.toml").exists());
    Ok(())
}

#[tokio::test]
async fn saving_an_empty_journal_directory_name_is_rejected_and_never_written() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &journal_body("", "templates/daily-journal.md"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("journal_dir"), "{body}");
    assert!(!vault_dir.path().join(".contextos/journal.toml").exists());
    Ok(())
}

#[tokio::test]
async fn saving_a_journal_template_path_carrying_a_traversal_segment_is_rejected() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &journal_body("journal", "templates/../secrets.md"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("template_path"), "{body}");
    assert!(!vault_dir.path().join(".contextos/journal.toml").exists());
    Ok(())
}

#[tokio::test]
async fn saving_the_journal_section_leaves_the_tasks_section_untouched() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        ".contextos/tasks.toml",
        "[[stage]]\nstate = \"backlog\"\nlabel = \"Someday\"\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &journal_body("journal", "templates/daily-journal.md"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Someday"), "{body}");
    let tasks_toml = std::fs::read_to_string(vault_dir.path().join(".contextos/tasks.toml"))?;
    assert!(tasks_toml.contains("Someday"));
    Ok(())
}

#[tokio::test]
async fn an_absent_notes_toml_pre_fills_the_form_with_the_scaffold_defaults() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/settings/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("value=\"notes\""), "{body}");
    assert!(body.contains("value=\"templates/unique-note.md\""), "{body}");
    assert!(
        body.contains("id=\"notes-date-format-field\" type=\"text\" value=\"YYYY/MM/YYYY-MM-DD-HHmm\""),
        "{body}"
    );
    assert!(
        body.contains("id=\"notes-date-structure-active-field\" type=\"checkbox\" checked"),
        "{body}"
    );
    assert!(
        body.contains("<option value=\"right\" selected>Right</option>"),
        "{body}"
    );
    Ok(())
}

#[tokio::test]
async fn saving_the_notes_section_persists_both_fields() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &notes_settings_body("scratch", "templates/scratch-note.md"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");

    let saved = std::fs::read_to_string(vault_dir.path().join(".contextos/notes.toml"))?;
    assert!(saved.contains("scratch"), "{saved}");
    assert!(saved.contains("templates/scratch-note.md"), "{saved}");
    assert!(saved.contains("YYYY/MM/YYYY-MM-DD-HHmm"), "{saved}");
    assert!(saved.contains("date_structure_active = true"), "{saved}");
    assert!(saved.contains("date_structure_side = \"right\""), "{saved}");
    Ok(())
}

#[tokio::test]
async fn saving_the_notes_section_persists_a_left_side_and_inactive_toggle() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        r#"{"target": "notes", "notes_dir": "notes", "template_path": "templates/unique-note.md", "date_format": "DD-MM-YYYY", "date_structure_active": false, "date_structure_side": "left"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");

    let saved = std::fs::read_to_string(vault_dir.path().join(".contextos/notes.toml"))?;
    assert!(saved.contains("date_structure_active = false"), "{saved}");
    assert!(saved.contains("date_structure_side = \"left\""), "{saved}");
    Ok(())
}

#[tokio::test]
async fn an_absent_notes_toml_pre_fills_the_metadata_panel_side_to_the_right() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/settings/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("id=\"notes-metadata-panel-side-field\""), "{body}");
    Ok(())
}

#[tokio::test]
async fn saving_the_notes_section_persists_the_metadata_panel_side_independently_of_the_dated_pane_side()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        r#"{"target": "notes", "notes_dir": "notes", "template_path": "templates/unique-note.md", "date_format": "YYYY-MM-DD", "date_structure_active": true, "date_structure_side": "right", "metadata_panel_side": "left"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");

    let saved = std::fs::read_to_string(vault_dir.path().join(".contextos/notes.toml"))?;
    assert!(saved.contains("date_structure_side = \"right\""), "{saved}");
    assert!(saved.contains("metadata_panel_side = \"left\""), "{saved}");
    Ok(())
}

#[tokio::test]
async fn saving_an_invalid_notes_metadata_panel_side_is_rejected_and_never_written() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        r#"{"target": "notes", "notes_dir": "notes", "template_path": "templates/unique-note.md", "date_format": "YYYY-MM-DD", "date_structure_active": true, "date_structure_side": "right", "metadata_panel_side": "centre"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("metadata_panel_side"), "{body}");
    assert!(
        !vault_dir.path().join(".contextos/notes.toml").exists(),
        "an invalid metadata_panel_side must never be written"
    );
    Ok(())
}

#[tokio::test]
async fn saving_an_empty_notes_date_format_is_rejected_and_never_written() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        r#"{"target": "notes", "notes_dir": "notes", "template_path": "templates/unique-note.md", "date_format": "", "date_structure_active": true, "date_structure_side": "right"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("date_format"), "{body}");
    assert!(!vault_dir.path().join(".contextos/notes.toml").exists());
    Ok(())
}

#[tokio::test]
async fn saving_an_invalid_notes_date_structure_side_is_rejected_and_never_written() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        r#"{"target": "notes", "notes_dir": "notes", "template_path": "templates/unique-note.md", "date_format": "YYYY-MM-DD", "date_structure_active": true, "date_structure_side": "centre"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("date_structure_side"), "{body}");
    assert!(!vault_dir.path().join(".contextos/notes.toml").exists());
    Ok(())
}

#[tokio::test]
async fn saving_an_empty_notes_directory_name_is_rejected_and_never_written() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &notes_settings_body("", "templates/unique-note.md"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("notes_dir"), "{body}");
    assert!(!vault_dir.path().join(".contextos/notes.toml").exists());
    Ok(())
}

#[tokio::test]
async fn saving_a_notes_template_path_carrying_a_traversal_segment_is_rejected() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &notes_settings_body("notes", "templates/../secrets.md"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("template_path"), "{body}");
    assert!(!vault_dir.path().join(".contextos/notes.toml").exists());
    Ok(())
}

#[tokio::test]
async fn saving_the_notes_section_leaves_the_journal_section_untouched() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        ".contextos/journal.toml",
        "journal_dir = \"diary\"\ntemplate_path = \"templates/daily-journal.md\"\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &notes_settings_body("notes", "templates/unique-note.md"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("value=\"diary\""), "{body}");
    let journal_toml = std::fs::read_to_string(vault_dir.path().join(".contextos/journal.toml"))?;
    assert!(journal_toml.contains("diary"));
    Ok(())
}

/// A Journal save carrying the section settings, as the form's JSON encoding sends them.
fn journal_sections_body(opened: &str, closed: &str, read_only: &str, expected_hash: &str) -> Result<String, BoxError> {
    Ok(serde_json::to_string(&serde_json::json!({
        "target": "journal",
        "journal_dir": "journal",
        "template_path": "templates/daily-journal.md",
        "date_format": "YYYY/MM/YYYY-MM-DD",
        "date_structure_side": "right",
        "task_opened_section": opened,
        "task_closed_section": closed,
        "read_only_sections": read_only,
        "expected_hash": expected_hash,
    }))?)
}

/// The version of `journal.toml` the Journal pane's form was rendered from.
fn journal_form_hash(panel: &str) -> Option<String> {
    let start = panel.find("expected_hash: '")? + "expected_hash: '".len();
    let end = panel[start..].find('\'')? + start;
    Some(panel[start..end].to_owned())
}

#[tokio::test]
async fn the_journal_form_offers_the_section_settings_empty_by_default() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/settings/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let panel = section_panel(&body, "journal")?;
    assert!(
        panel.contains("id=\"journal-task-opened-section-field\" type=\"text\" value=\"\""),
        "{panel}"
    );
    assert!(
        panel.contains("id=\"journal-task-closed-section-field\" type=\"text\" value=\"\""),
        "{panel}"
    );
    assert!(panel.contains("id=\"journal-read-only-sections-field\""), "{panel}");
    for absent in ["Loops", "Activity Log"] {
        assert!(!panel.contains(absent), "no section is named by default: {panel}");
    }
    Ok(())
}

#[tokio::test]
async fn saving_the_journal_section_settings_persists_them_and_shows_them_again() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &journal_sections_body(
            "  Loops opened today ",
            "Loops closed today",
            "Activity Log\n\n  Agent notes  \n",
            "",
        )?,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let saved = std::fs::read_to_string(vault_dir.path().join(".contextos/journal.toml"))?;
    let config: contextos_web::JournalConfig = toml::from_str(&saved)?;
    assert_eq!(config.task_opened_section.as_deref(), Some("Loops opened today"));
    assert_eq!(config.task_closed_section.as_deref(), Some("Loops closed today"));
    assert_eq!(config.read_only_sections, ["Activity Log", "Agent notes"]);

    let (_, body) = get(&router, &format!("/{VAULT_NAME}/settings/")).await?;
    let panel = section_panel(&body, "journal")?;
    assert!(panel.contains("value=\"Loops opened today\""), "{panel}");
    assert!(panel.contains("Activity Log\nAgent notes</textarea>"), "{panel}");

    // Blank settings are left out of the file altogether.
    let hash = journal_form_hash(panel).ok_or("the form carries the file's version")?;
    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &journal_sections_body("", " ", "\n", &hash)?,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let saved = std::fs::read_to_string(vault_dir.path().join(".contextos/journal.toml"))?;
    for absent in ["task_opened_section", "task_closed_section", "read_only_sections"] {
        assert!(!saved.contains(absent), "{saved}");
    }
    Ok(())
}

#[tokio::test]
async fn a_section_setting_that_is_not_a_plain_heading_is_rejected_and_never_written() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    for (opened, read_only, field) in [
        ("## New actions", "", "task_opened_section"),
        ("New\nactions", "", "task_opened_section"),
        ("New actions", "# Agent log", "read_only_sections"),
        ("New actions", "Agent\u{7}log", "read_only_sections"),
        ("[](x)", "", "task_opened_section"),
    ] {
        let (status, body) = patch(
            &router,
            &format!("/{VAULT_NAME}/settings/"),
            &journal_sections_body(opened, "", read_only, "")?,
        )
        .await?;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains(field), "{opened:?} {read_only:?}: {body}");
        assert!(!vault_dir.path().join(".contextos/journal.toml").exists());
    }
    Ok(())
}
