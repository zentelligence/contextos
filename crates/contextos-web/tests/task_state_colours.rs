//! A task's state chip is shown in its stage's colour: the one `.contextos/tasks.toml` configures for the stage, else
//! the shipped one for that state, so every state, `review` included, is coloured. Real `contextos-mcp` sessions over
//! real temporary vaults.

mod support;

#[path = "tasks_common/mod.rs"]
mod tasks_common;

use tasks_common::{StatusCode, VAULT_NAME, get, router_over, task_note, write};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[tokio::test]
async fn a_review_task_is_shown_in_the_shipped_review_colour() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        &task_note("T-001", "review", "zenith", None),
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-002.md",
        &task_note("T-002", "next", "zenith", None),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/?view=table")).await?;

    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("<span class=\"status-chip colour-purple\">review</span>"),
        "{body}"
    );
    assert!(
        body.contains("<span class=\"status-chip colour-blue\">next</span>"),
        "{body}"
    );
    Ok(())
}

#[tokio::test]
async fn a_stage_colour_set_in_vault_settings_is_the_one_shown() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        ".contextos/tasks.toml",
        "[[stage]]\nstate = \"review\"\nlabel = \"Review\"\nbucket = \"needs_call\"\ncolour = \"red\"\n\n\
         [[stage]]\nstate = \"triage\"\nlabel = \"Triage\"\nbucket = \"backlog\"\n",
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-001.md",
        &task_note("T-001", "review", "zenith", None),
    )?;
    write(
        vault_dir.path(),
        "memory/tasks/t-002.md",
        &task_note("T-002", "triage", "zenith", None),
    )?;
    let router = router_over(vault_dir.path(), config_dir.path()).await?;

    let (status, body) = get(&router, &format!("/{VAULT_NAME}/tasks/?view=table")).await?;

    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("<span class=\"status-chip colour-red\">review</span>"),
        "{body}"
    );
    assert!(
        body.contains("<span class=\"status-chip colour-grey\">triage</span>"),
        "{body}"
    );
    Ok(())
}
