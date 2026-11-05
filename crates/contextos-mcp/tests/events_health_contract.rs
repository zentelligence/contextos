//! A completed write that returns a secondary-service failure fires `vault.degraded` once, the write that clears it
//! fires `vault.recovered`, and a note that cannot be written is a warning that never fails the write.
mod support;

use serde_json::json;
use support::events_vault::{event_notes, events_harness, events_of_kind, field, warnings};
use support::git_harness::{BoxError, Harness};

/// Breaks version control for the next write by making the repository's index unreadable.
fn break_git(harness: &Harness) -> std::io::Result<std::path::PathBuf> {
    let index = harness.vault.path().join(".git/index");
    let held = harness.vault.path().join(".git/index.held");
    std::fs::rename(&index, &held)?;
    std::fs::create_dir(&index)?;
    Ok(held)
}

fn repair_git(harness: &Harness, held: &std::path::Path) -> std::io::Result<()> {
    let index = harness.vault.path().join(".git/index");
    std::fs::remove_dir(&index)?;
    std::fs::rename(held, index)
}

#[tokio::test]
async fn a_git_failure_on_a_completed_write_fires_vault_degraded_once_and_recovery_fires_vault_recovered()
-> Result<(), BoxError> {
    let harness = events_harness().await?;
    let held = break_git(&harness)?;

    let first = harness
        .call("fs_write_file", json!({"path": "alpha/one.md", "content": "# One\n"}))
        .await?;
    assert_eq!(
        first.is_error,
        Some(false),
        "the write itself still completes: {first:?}"
    );
    let seen = warnings(first.structured_content.as_ref().ok_or("no result")?);
    assert!(seen.iter().any(|warning| warning.starts_with("git/")), "{seen:?}");
    let degraded = events_of_kind(harness.vault.path(), "vault.degraded");
    assert_eq!(degraded.len(), 1, "{:?}", event_notes(harness.vault.path()));
    assert!(degraded[0].1.contains("\nseverity: warning\n"));
    assert_eq!(field(&degraded[0].1, "service").as_deref(), Some("git"));
    assert!(field(&degraded[0].1, "code").is_some_and(|code| code.starts_with("git/")));

    harness
        .structured("fs_write_file", json!({"path": "alpha/two.md", "content": "# Two\n"}))
        .await
        .ok();
    assert_eq!(
        events_of_kind(harness.vault.path(), "vault.degraded").len(),
        1,
        "a code already current fires nothing"
    );

    repair_git(&harness, &held)?;
    harness
        .structured(
            "fs_write_file",
            json!({"path": "alpha/three.md", "content": "# Three\n"}),
        )
        .await?;
    let recovered = events_of_kind(harness.vault.path(), "vault.recovered");
    assert_eq!(recovered.len(), 1, "{:?}", event_notes(harness.vault.path()));
    assert!(recovered[0].1.contains("\nseverity: info\n"));
    assert_eq!(
        field(&recovered[0].1, "code"),
        field(&degraded[0].1, "code"),
        "recovery names the code that was current"
    );
    Ok(())
}

#[tokio::test]
async fn a_notice_fires_nothing_and_a_clean_write_fires_nothing() -> Result<(), BoxError> {
    let harness = events_harness().await?;
    harness
        .structured(
            "fs_write_file",
            json!({"path": "alpha/crlf.md", "content": "# A\r\nline\r\n"}),
        )
        .await?;
    harness
        .structured("fs_write_file", json!({"path": "alpha/clean.md", "content": "# B\n"}))
        .await?;
    assert_eq!(event_notes(harness.vault.path()), Vec::new());
    Ok(())
}

#[tokio::test]
async fn an_event_note_that_cannot_be_written_is_a_warning_and_the_write_stands() -> Result<(), BoxError> {
    let harness = events_harness().await?;
    // A file where the year folder should be makes every event note unwritable, whatever year the clock says.
    std::fs::create_dir_all(harness.vault.path().join("memory/events"))?;
    for year in 2020..2040 {
        std::fs::write(harness.vault.path().join(format!("memory/events/{year}")), "in the way")?;
    }

    let result = harness
        .call(
            "frontmatter_update",
            json!({"path": "memory/tasks/a-001.md", "patch": {"state": "doing"}}),
        )
        .await?;

    assert_eq!(result.is_error, Some(false), "{result:?}");
    let seen = warnings(result.structured_content.as_ref().ok_or("no result")?);
    assert!(
        seen.iter().any(|warning| warning.starts_with("events/write-failed:")),
        "{seen:?}"
    );
    let task = std::fs::read_to_string(harness.vault.path().join("memory/tasks/a-001.md"))?;
    assert!(task.contains("state: doing"), "the task write stands");
    Ok(())
}

#[tokio::test]
async fn a_move_between_vaults_marks_degraded_only_the_vault_whose_git_failed() -> Result<(), BoxError> {
    let first = tempfile::Builder::new().prefix("first").tempdir()?;
    let second = tempfile::Builder::new().prefix("second").tempdir()?;
    let server = contextos_mcp::ContextOsServer::try_from(support::scoped_config(vec![
        first.path().to_path_buf(),
        second.path().to_path_buf(),
    ])?)?;
    for vault in [first.path(), second.path()] {
        support::git_harness::call(&server, "git_init", json!({"vault": vault})).await?;
    }
    let source = first.path().join("move.md");
    support::git_harness::call(&server, "fs_write_file", json!({"path": source, "content": "move\n"})).await?;
    server.flush_git()?;
    // The second vault's repository can no longer stage; the first vault's is untouched.
    let index = second.path().join(".git/index");
    std::fs::remove_file(&index)?;
    std::fs::create_dir(&index)?;

    let moved = support::git_harness::call(
        &server,
        "fs_move_file",
        json!({"source": source, "destination": second.path().join("moved.md")}),
    )
    .await?;
    assert_eq!(moved.is_error, Some(false), "{moved:?}");

    assert_eq!(
        events_of_kind(second.path(), "vault.degraded").len(),
        1,
        "{:?}",
        event_notes(second.path())
    );
    assert_eq!(
        events_of_kind(first.path(), "vault.degraded"),
        Vec::new(),
        "the first vault's Git staged the move cleanly"
    );
    Ok(())
}
