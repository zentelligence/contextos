//! Mutations are exclusive across every contextos process writing to the same vault, not only within one process.
//! Each test plays a second process by holding the vault's host write lock itself, the way another `contextos`
//! instance would, and shows a mutating tool waits for it.
mod support;

use std::time::Duration;

use contextos_mcp::ContextOsServer;
use serde_json::json;
use support::git_harness::call;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Where `support::scoped_config` puts a vault's state directory, and so its host write lock.
fn host_write_lock(vault: &std::path::Path) -> std::path::PathBuf {
    vault.join(".contextos").join("vault-write.lock")
}

fn task_vault() -> Result<(tempfile::TempDir, ContextOsServer), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir_all(vault.path().join("memory/tasks"))?;
    std::fs::write(
        vault.path().join("memory/tasks/a-001.md"),
        "---\nid: A-001\nstate: next\n---\n# First\n",
    )?;
    let server = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    Ok((vault, server))
}

#[tokio::test]
async fn a_write_waits_while_another_process_holds_the_vaults_host_write_lock() -> Result<(), BoxError> {
    let (vault, server) = task_vault()?;
    let lock_path = host_write_lock(vault.path());
    std::fs::create_dir_all(lock_path.parent().ok_or("lock has no parent")?)?;
    let other_process = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;
    other_process.lock()?;

    let writer = tokio::spawn({
        let server = server.clone();
        async move {
            call(
                &server,
                "frontmatter_update",
                json!({"path": "memory/tasks/a-001.md", "patch": {"state": "doing"}}),
            )
            .await
        }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let untouched = std::fs::read_to_string(vault.path().join("memory/tasks/a-001.md"))?;
    assert!(
        untouched.contains("state: next"),
        "the write went ahead while another process held the lock"
    );
    assert!(
        !writer.is_finished(),
        "the write finished while another process held the lock"
    );

    other_process.unlock()?;
    let result = writer.await??;
    assert_ne!(result.is_error, Some(true), "{result:?}");
    let written = std::fs::read_to_string(vault.path().join("memory/tasks/a-001.md"))?;
    assert!(written.contains("state: doing"), "{written}");
    Ok(())
}

#[tokio::test]
async fn two_server_instances_claiming_the_same_version_of_a_task_produce_exactly_one_claim() -> Result<(), BoxError> {
    let (vault, first) = task_vault()?;
    let second = ContextOsServer::try_from(support::scoped_config(vec![vault.path().to_path_buf()])?)?;
    let info = call(&first, "fs_get_file_info", json!({"path": "memory/tasks/a-001.md"})).await?;
    let hash = info
        .structured_content
        .as_ref()
        .and_then(|body| body.get("content_hash"))
        .and_then(serde_json::Value::as_str)
        .ok_or("content_hash missing")?
        .to_owned();

    let claim = |server: ContextOsServer, claimant: &'static str, hash: String| async move {
        call(
            &server,
            "frontmatter_update",
            json!({
                "path": "memory/tasks/a-001.md",
                "patch": {"state": "doing", "claimed_by": claimant},
                "expected_hash": hash,
            }),
        )
        .await
    };
    let (kai, sol) = tokio::join!(
        claim(first, "kai/run-1", hash.clone()),
        claim(second, "sol/run-1", hash)
    );
    let outcomes = [kai?, sol?];
    let claimed = outcomes.iter().filter(|result| result.is_error != Some(true)).count();
    assert_eq!(claimed, 1, "{outcomes:?}");
    let written = std::fs::read_to_string(vault.path().join("memory/tasks/a-001.md"))?;
    assert_eq!(written.matches("claimed_by").count(), 1, "{written}");
    Ok(())
}

#[tokio::test]
async fn a_cli_doctor_resolution_waits_while_another_process_holds_the_vaults_host_write_lock() -> Result<(), BoxError>
{
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::create_dir(vault.path().join("alpha"))?;
    std::fs::write(vault.path().join("alpha/note.md"), "# Note\n")?;
    let mut config = support::scoped_config(vec![vault.path().to_path_buf()])?;
    config.vaults[0].git.enabled = false;
    let lock_path = host_write_lock(vault.path());
    std::fs::create_dir_all(lock_path.parent().ok_or("lock has no parent")?)?;
    let other_process = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;
    other_process.lock()?;

    let resolution = tokio::spawn(contextos_mcp::resolve_for_cli(config));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !vault.path().join("alpha/index.md").exists(),
        "the resolution wrote while another process held the lock"
    );
    assert!(
        !resolution.is_finished(),
        "the resolution finished while another process held the lock"
    );

    other_process.unlock()?;
    let outcomes = resolution.await??.outcomes;
    assert!(
        outcomes
            .iter()
            .any(|outcome| outcome.subject == "Managed indexes" && outcome.resolved),
        "{outcomes:?}"
    );
    assert!(vault.path().join("alpha/index.md").exists());
    Ok(())
}
