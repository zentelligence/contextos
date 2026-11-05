//! A scheduled vault tick whose run record cannot be created, as a scheduler pass against a real `contextos-mcp` session
//! and a real temporary vault sees it: the run is not started, because a vault run's record is its visibility in the
//! vault; the tick is still recorded as fired, so it is not retried every pass; and the one warning it logs names no
//! folder of the host or the vault.

mod support;

use std::path::Path;
use std::sync::Arc;

use contextos_web::McpClientSet;
use contextos_web::scheduler::poll_once;
use contextos_web::vault_runs::claim_slot;
use contextos_workflows::Clock;
use support::logs::capture;
use time::OffsetDateTime;
use time::macros::datetime;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT_NAME: &str = "contract-fixture";

/// 03:00:30 on a day when a job scheduled every minute fell due at 03:00:00, inside the scheduler's look-back window.
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        datetime!(2026-10-01 03:00:30 UTC)
    }
}

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

#[tokio::test]
async fn a_scheduled_vault_tick_whose_record_cannot_be_created_does_not_run_and_is_still_recorded_as_fired()
-> Result<(), BoxError> {
    let vault_dir = tempfile::tempdir()?;
    let config_dir = tempfile::tempdir()?;
    write(vault_dir.path(), "index.md", "# Root\n")?;
    write(vault_dir.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write(
        vault_dir.path(),
        "registry/workflows/ops/workflows.toml",
        "workflows = [\"sync\"]\n",
    )?;
    write(
        vault_dir.path(),
        "registry/workflows/ops/sync/workflow.toml",
        "name = \"Sync\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = [\"contextos\"]\n",
    )?;
    // The script leaves a mark in the vault, so a run that was started can be told from one that was not.
    write(
        vault_dir.path(),
        "registry/workflows/ops/sync/workflow.rhai",
        &format!("call_mcp(\"contextos\", \"fs_create_directory\", #{{ path: \"{VAULT_NAME}://started\" }});"),
    )?;
    write(
        vault_dir.path(),
        "registry/workflows/ops/sync/jobs/nightly.toml",
        "name = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    write(
        vault_dir.path(),
        ".contextos/jobs.toml",
        "[ops.sync.nightly]\nlifecycle = \"enabled\"\nschedule = \"* * * * *\"\n",
    )?;
    // A file where the run records' folder belongs, so the tool answers that the record cannot be created.
    write(vault_dir.path(), "memory/jobs", "not a folder")?;
    let config_path = support::write_vault_config(config_dir.path(), vault_dir.path())?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    let host = support::host(config_dir.path());
    let (writer, _guard) = capture();

    poll_once(&clients, &host, &FixedClock).await;

    assert!(
        !vault_dir.path().join("started").exists(),
        "a run whose record cannot be created is not run"
    );
    let slot = claim_slot(&host, VAULT_NAME, "ops", "sync", "nightly")?;
    assert!(slot.last_fire()?.is_some(), "the tick is recorded as fired");
    let log = writer.lines().join("\n");
    let warnings: Vec<&str> = log.lines().filter(|line| line.contains(" WARN ")).collect();
    assert_eq!(warnings.len(), 1, "one warning: {log}");
    for folder in [vault_dir.path(), config_dir.path()] {
        assert!(
            !log.contains(&folder.to_string_lossy().into_owned()),
            "the log names a folder of the host or the vault: {log}"
        );
    }
    Ok(())
}
