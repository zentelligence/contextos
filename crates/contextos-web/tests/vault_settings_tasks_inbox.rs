//! Vault settings: the inbox/outbox section, and its independence from the tasks section (whose own editor is
//! covered by `vault_settings_tasks_editor`).
//! Real `contextos-mcp` sessions against real temporary vault fixtures, no mocking, per `testing.md`.

mod support;
mod vault_settings_common;

use vault_settings_common::*;

#[tokio::test]
async fn an_absent_inbox_outbox_toml_pre_fills_the_form_with_built_in_defaults() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/settings/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("karla"), "{body}");
    assert!(body.contains("value=\"inbox\""), "{body}");
    assert!(body.contains("value=\"outbox\""), "{body}");
    assert!(body.contains("value=\"processed\""), "{body}");
    Ok(())
}

#[tokio::test]
async fn the_curator_agent_select_offers_every_registered_agent() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        ".contextos/team.toml",
        "[[member]]\nslug = \"karla\"\nname = \"Karla\"\n[[member]]\nslug = \"aria\"\nname = \"Aria\"\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/settings/")).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Karla"), "{body}");
    assert!(body.contains("Aria"), "{body}");
    Ok(())
}

#[tokio::test]
async fn saving_a_curator_agent_registered_in_agents_toml_persists_it() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        ".contextos/team.toml",
        "[[member]]\nslug = \"karla\"\nname = \"Karla\"\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &inbox_outbox_body("karla", "inbox", "outbox", "processed"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");

    let saved = std::fs::read_to_string(vault_dir.path().join(".contextos/inbox-outbox.toml"))?;
    assert!(saved.contains("karla"));
    Ok(())
}

#[tokio::test]
async fn saving_a_curator_agent_absent_from_agents_toml_is_rejected_and_never_written() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &inbox_outbox_body("karla", "inbox", "outbox", "processed"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("not registered"), "{body}");
    assert!(!vault_dir.path().join(".contextos/inbox-outbox.toml").exists());
    Ok(())
}

#[tokio::test]
async fn saving_an_empty_directory_name_is_rejected_and_never_written() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        ".contextos/team.toml",
        "[[member]]\nslug = \"karla\"\nname = \"Karla\"\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &inbox_outbox_body("karla", "", "outbox", "processed"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("inbox_dir"), "{body}");
    assert!(!vault_dir.path().join(".contextos/inbox-outbox.toml").exists());
    Ok(())
}

#[tokio::test]
async fn saving_a_directory_name_carrying_a_path_segment_is_rejected() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        ".contextos/team.toml",
        "[[member]]\nslug = \"karla\"\nname = \"Karla\"\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &inbox_outbox_body("karla", "../escape", "outbox", "processed"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("inbox_dir"), "{body}");
    assert!(!vault_dir.path().join(".contextos/inbox-outbox.toml").exists());
    Ok(())
}

#[tokio::test]
async fn saving_the_inbox_outbox_section_leaves_the_tasks_section_untouched() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        ".contextos/tasks.toml",
        "[[stage]]\nstate = \"backlog\"\nlabel = \"Someday\"\n",
    )?;
    write(
        vault_dir.path(),
        ".contextos/team.toml",
        "[[member]]\nslug = \"karla\"\nname = \"Karla\"\n",
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = patch(
        &router,
        &format!("/{VAULT_NAME}/settings/"),
        &inbox_outbox_body("karla", "inbox", "outbox", "processed"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("Someday"), "{body}");
    let tasks_toml = std::fs::read_to_string(vault_dir.path().join(".contextos/tasks.toml"))?;
    assert!(tasks_toml.contains("Someday"));
    Ok(())
}
