//! Vault settings: the wiki and insights sections.
//! Real `contextos-mcp` sessions against real temporary vault fixtures, no mocking, per `testing.md`.

mod support;
mod vault_settings_common;

use vault_settings_common::*;

#[tokio::test]
async fn an_absent_wiki_toml_pre_fills_the_form_with_the_scaffold_default() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/settings/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains("id=\"wiki-dir-field\" type=\"text\" value=\"memory/wiki\""),
        "{body}"
    );
    let panel = section_panel(&body, "wiki")?;
    assert!(panel.contains("id=\"wiki-tree-side-field\""), "{panel}");
    assert!(panel.contains("id=\"wiki-metadata-panel-side-field\""), "{panel}");
    assert!(
        panel.matches("<option value=\"right\" selected>Right</option>").count() == 2,
        "{panel}"
    );
    Ok(())
}

#[tokio::test]
async fn saving_the_wiki_section_persists_both_fields() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &wiki_settings_body("wiki", "left"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");

    let saved = std::fs::read_to_string(vault_dir.path().join(".contextos/wiki.toml"))?;
    assert!(saved.contains("wiki"), "{saved}");
    assert!(saved.contains("tree_side = \"left\""), "{saved}");
    Ok(())
}

#[tokio::test]
async fn saving_the_wiki_section_persists_the_metadata_panel_side_independently_of_the_tree_side()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &wiki_settings_body_with_metadata_side("wiki", "right", "left"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");

    let saved = std::fs::read_to_string(vault_dir.path().join(".contextos/wiki.toml"))?;
    assert!(saved.contains("tree_side = \"right\""), "{saved}");
    assert!(saved.contains("metadata_panel_side = \"left\""), "{saved}");
    Ok(())
}

#[tokio::test]
async fn saving_an_empty_wiki_directory_is_rejected_and_never_written() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &wiki_settings_body("", "right"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("wiki_dir"), "{body}");
    assert!(!vault_dir.path().join(".contextos/wiki.toml").exists());
    Ok(())
}

#[tokio::test]
async fn saving_a_wiki_directory_carrying_a_traversal_segment_is_rejected() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &wiki_settings_body("memory/../secrets", "right"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("wiki_dir"), "{body}");
    assert!(!vault_dir.path().join(".contextos/wiki.toml").exists());
    Ok(())
}

#[tokio::test]
async fn saving_an_invalid_wiki_tree_side_is_rejected_and_never_written() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &wiki_settings_body("memory/wiki", "centre"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("tree_side"), "{body}");
    assert!(!vault_dir.path().join(".contextos/wiki.toml").exists());
    Ok(())
}

#[tokio::test]
async fn saving_an_invalid_wiki_metadata_panel_side_is_rejected_and_never_written() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &wiki_settings_body_with_metadata_side("memory/wiki", "right", "centre"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("metadata_panel_side"), "{body}");
    assert!(!vault_dir.path().join(".contextos/wiki.toml").exists());
    Ok(())
}

#[tokio::test]
async fn saving_the_wiki_section_leaves_the_notes_section_untouched() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/notes.toml", "notes_dir = \"scratch\"\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &wiki_settings_body("memory/wiki", "right"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("value=\"scratch\""), "{body}");
    let notes_toml = std::fs::read_to_string(vault_dir.path().join(".contextos/notes.toml"))?;
    assert!(notes_toml.contains("scratch"));
    Ok(())
}

#[tokio::test]
async fn an_absent_insights_toml_pre_fills_the_form_with_the_scaffold_default() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/settings/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains("id=\"insights-dir-field\" type=\"text\" value=\"memory/insights\""),
        "{body}"
    );
    let panel = section_panel(&body, "insights")?;
    assert!(panel.contains("id=\"insights-shard-side-field\""), "{panel}");
    assert!(panel.contains("id=\"insights-metadata-panel-side-field\""), "{panel}");
    assert!(
        panel.matches("<option value=\"right\" selected>Right</option>").count() == 2,
        "{panel}"
    );
    Ok(())
}

#[tokio::test]
async fn saving_the_insights_section_persists_both_fields() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &insights_settings_body("insights", "left"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");

    let saved = std::fs::read_to_string(vault_dir.path().join(".contextos/insights.toml"))?;
    assert!(saved.contains("insights"), "{saved}");
    assert!(saved.contains("shard_side = \"left\""), "{saved}");
    Ok(())
}

#[tokio::test]
async fn saving_the_insights_section_persists_the_metadata_panel_side_independently_of_the_shard_side()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &insights_settings_body_with_metadata_side("insights", "right", "left"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");

    let saved = std::fs::read_to_string(vault_dir.path().join(".contextos/insights.toml"))?;
    assert!(saved.contains("shard_side = \"right\""), "{saved}");
    assert!(saved.contains("metadata_panel_side = \"left\""), "{saved}");
    Ok(())
}

#[tokio::test]
async fn saving_an_empty_insights_directory_is_rejected_and_never_written() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &insights_settings_body("", "right"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("insights_dir"), "{body}");
    assert!(!vault_dir.path().join(".contextos/insights.toml").exists());
    Ok(())
}

#[tokio::test]
async fn saving_an_invalid_insights_shard_side_is_rejected_and_never_written() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &insights_settings_body("memory/insights", "centre"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("shard_side"), "{body}");
    assert!(!vault_dir.path().join(".contextos/insights.toml").exists());
    Ok(())
}

#[tokio::test]
async fn saving_an_invalid_insights_metadata_panel_side_is_rejected_and_never_written() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &insights_settings_body_with_metadata_side("memory/insights", "right", "centre"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("metadata_panel_side"), "{body}");
    assert!(!vault_dir.path().join(".contextos/insights.toml").exists());
    Ok(())
}

#[tokio::test]
async fn saving_the_insights_section_leaves_the_wiki_section_untouched() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/wiki.toml", "wiki_dir = \"wiki\"\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &insights_settings_body("memory/insights", "right"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("wiki"), "{body}");
    let wiki_toml = std::fs::read_to_string(vault_dir.path().join(".contextos/wiki.toml"))?;
    assert!(wiki_toml.contains("wiki"));
    Ok(())
}
