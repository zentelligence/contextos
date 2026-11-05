//! Notes screen: behaviour over a killed MCP session.
//! Real `contextos-mcp` sessions against real temporary vault fixtures, no mocking, per `testing.md`.
//!
//! Unix only: each test kills the session's process with `kill -KILL`.
#![cfg(unix)]

mod notes_common;
mod support;

use notes_common::*;

#[tokio::test]
async fn saving_the_body_over_a_killed_session_reports_the_transport_failure() -> Result<(), BoxError> {
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
    let pid = client.pid().ok_or("a stdio client reports its child PID")?;
    let config = contextos_web::notes_config::NotesConfig::default();

    std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status()?;
    wait_until_unreachable(&client).await?;

    let result =
        contextos_web::notes::save_note_body(&client, VAULT_NAME, &config, "2026/09/note.md", "Edited body.", None)
            .await;
    // A session already dead before the call starts fails on
    // `save_note_body`'s own fresh pre-write re-read first (`Load`); a
    // direct `Write` would only occur for a session that dies in the much
    // narrower window between that re-read succeeding and the write call
    // itself, not reproducible without flaky mid-request timing. Both
    // shapes carry the identical transport cause and both map to the same
    // `502` at the route level (`routes::notes::patch_body`).
    assert!(
        matches!(
            result,
            Err(
                contextos_web::notes::SaveNoteError::Load(contextos_web::notes::LoadNoteError::Write(_))
                    | contextos_web::notes::SaveNoteError::Write(_)
            )
        ),
        "{result:?}"
    );
    Ok(())
}

#[tokio::test]
async fn renaming_over_a_killed_session_reports_the_transport_failure() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "notes/2026/09/one.md",
        "---\ntitle: One\ndate: 2026-09-14\ntime: 09:30\nstatus: draft\ntags: []\n---\n\nOne body.\n",
    )?;
    let config_path = support::write_vault_config(config_dir.path(), vault_dir.path())?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let client = contextos_web::McpClient::connect(&entry).await?;
    let pid = client.pid().ok_or("a stdio client reports its child PID")?;
    let config = contextos_web::notes_config::NotesConfig::default();

    std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status()?;
    wait_until_unreachable(&client).await?;

    let result = contextos_web::notes::rename_note(&client, VAULT_NAME, &config, "2026/09/one.md", "renamed").await;
    assert!(
        matches!(result, Err(contextos_web::notes::RenameNoteError::Write(_))),
        "{result:?}"
    );
    Ok(())
}

#[tokio::test]
async fn deleting_over_a_killed_session_reports_the_transport_failure() -> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(
        vault_dir.path(),
        "notes/2026/09/one.md",
        "---\ntitle: One\ndate: 2026-09-14\ntime: 09:30\nstatus: draft\ntags: []\n---\n\nOne body.\n",
    )?;
    let config_path = support::write_vault_config(config_dir.path(), vault_dir.path())?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let client = contextos_web::McpClient::connect(&entry).await?;
    let pid = client.pid().ok_or("a stdio client reports its child PID")?;
    let config = contextos_web::notes_config::NotesConfig::default();

    std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status()?;
    wait_until_unreachable(&client).await?;

    let result = contextos_web::notes::delete_note(&client, VAULT_NAME, &config, "2026/09/one.md").await;
    assert!(
        matches!(result, Err(contextos_web::notes::DeleteNoteError::Write(_))),
        "{result:?}"
    );
    Ok(())
}
