//! Vault settings: the entity documents and profile sections.
//! Real `contextos-mcp` sessions against real temporary vault fixtures, no mocking, per `testing.md`.

mod support;
mod vault_settings_common;

use vault_settings_common::*;

#[tokio::test]
async fn an_absent_entity_docs_toml_pre_fills_the_form_with_the_right_side_default() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/settings/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let panel = section_panel(&body, "entity_docs")?;
    assert!(panel.contains("id=\"entity-folder-side-field\""), "{panel}");
    assert!(panel.contains("id=\"entity-metadata-panel-side-field\""), "{panel}");
    assert!(
        panel.matches("<option value=\"right\" selected>Right</option>").count() == 2,
        "{panel}"
    );
    Ok(())
}

#[tokio::test]
async fn saving_the_entity_docs_section_persists_the_side() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &entity_docs_settings_body("left"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");

    let saved = std::fs::read_to_string(vault_dir.path().join(".contextos/entity-docs.toml"))?;
    assert!(saved.contains("folder_side = \"left\""), "{saved}");
    Ok(())
}

#[tokio::test]
async fn saving_the_entity_docs_section_persists_the_metadata_panel_side_independently_of_the_folder_side()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &entity_docs_settings_body_with_metadata_side("right", "left"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");

    let saved = std::fs::read_to_string(vault_dir.path().join(".contextos/entity-docs.toml"))?;
    assert!(saved.contains("folder_side = \"right\""), "{saved}");
    assert!(saved.contains("metadata_panel_side = \"left\""), "{saved}");
    Ok(())
}

#[tokio::test]
async fn saving_an_invalid_entity_docs_metadata_panel_side_is_rejected_and_never_written() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &entity_docs_settings_body_with_metadata_side("right", "centre"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("metadata_panel_side"), "{body}");
    assert!(!vault_dir.path().join(".contextos/entity-docs.toml").exists());
    Ok(())
}

#[tokio::test]
async fn saving_an_invalid_entity_docs_folder_side_is_rejected_and_never_written() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &entity_docs_settings_body("centre"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("folder_side"), "{body}");
    assert!(!vault_dir.path().join(".contextos/entity-docs.toml").exists());
    Ok(())
}

#[tokio::test]
async fn saving_the_entity_docs_section_leaves_the_insights_section_untouched() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        ".contextos/insights.toml",
        "insights_dir = \"insights\"\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &entity_docs_settings_body("right"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("insights"), "{body}");
    let insights_toml = std::fs::read_to_string(vault_dir.path().join(".contextos/insights.toml"))?;
    assert!(insights_toml.contains("insights"));
    Ok(())
}

#[tokio::test]
async fn an_absent_profile_toml_pre_fills_the_form_with_the_right_side_default() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/settings/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let panel = section_panel(&body, "profile")?;
    assert!(
        panel.contains("<option value=\"right\" selected>Right</option>"),
        "{panel}"
    );
    Ok(())
}

#[tokio::test]
async fn saving_the_profile_section_persists_both_fields() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &profile_settings_body("left", "templates/entity-profile.md"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");

    let saved = std::fs::read_to_string(vault_dir.path().join(".contextos/profile.toml"))?;
    assert!(saved.contains("side = \"left\""), "{saved}");
    assert!(
        saved.contains("entity_template_path = \"templates/entity-profile.md\""),
        "{saved}"
    );
    Ok(())
}

#[tokio::test]
async fn saving_an_empty_entity_template_path_is_accepted_not_rejected() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &profile_settings_body("left", ""),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");

    let saved = std::fs::read_to_string(vault_dir.path().join(".contextos/profile.toml"))?;
    assert!(saved.contains("entity_template_path = \"\""), "{saved}");
    Ok(())
}

#[tokio::test]
async fn saving_an_invalid_profile_side_is_rejected_and_never_written() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &profile_settings_body("centre", ""),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("side must be"), "{body}");
    assert!(!vault_dir.path().join(".contextos/profile.toml").exists());
    Ok(())
}

#[tokio::test]
async fn saving_a_profile_template_path_carrying_a_traversal_segment_is_rejected() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &profile_settings_body("left", "../outside.md"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!vault_dir.path().join(".contextos/profile.toml").exists());
    Ok(())
}

#[tokio::test]
async fn saving_the_profile_section_leaves_the_entity_docs_section_untouched() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        ".contextos/entity-docs.toml",
        "folder_side = \"left\"\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &profile_settings_body("right", ""),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let panel = section_panel(&body, "entity_docs")?;
    assert!(
        panel.contains("<option value=\"left\" selected>Left</option>"),
        "{panel}"
    );
    let entity_docs_toml = std::fs::read_to_string(vault_dir.path().join(".contextos/entity-docs.toml"))?;
    assert!(entity_docs_toml.contains("folder_side = \"left\""));
    Ok(())
}
