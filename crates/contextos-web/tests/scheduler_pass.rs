//! One scheduler pass (`contextos_web::scheduler::poll_once`) against a real `contextos-mcp` session and a real temporary
//! vault, with the clock fixed so the due tick is known: only a job that is `enabled` is started, whatever schedule is
//! stored for the others.

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use contextos_web::McpClientSet;
use contextos_web::scheduler::poll_once;
use contextos_workflows::Clock;
use time::OffsetDateTime;
use time::macros::datetime;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

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

fn run_records(vault_dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![vault_dir.join("memory/jobs")];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("R-"))
            {
                found.push(path);
            }
        }
    }
    found
}

async fn pass_over(lifecycle: &str) -> Result<(tempfile::TempDir, String), BoxError> {
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
        "name = \"Sync\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = []\n",
    )?;
    write(vault_dir.path(), "registry/workflows/ops/sync/workflow.rhai", "()")?;
    write(
        vault_dir.path(),
        "registry/workflows/ops/sync/jobs/nightly.toml",
        "name = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    // A validated or registered job always has the version it was validated for, so one is stored for it here.
    support::release_fixture::install_vault_releases(vault_dir.path()).map_err(|error| error.to_string())?;
    let version = String::from(
        support::release_fixture::vault_version(
            vault_dir.path(),
            "ops",
            "name = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
        )
        .map_err(|error| error.to_string())?,
    );
    let recorded_version = if matches!(lifecycle, "validated" | "registered") {
        format!("fingerprint = \"{version}\"\n")
    } else {
        String::new()
    };
    let state =
        format!("[ops.sync.nightly]\n{recorded_version}lifecycle = \"{lifecycle}\"\nschedule = \"* * * * *\"\n");
    write(vault_dir.path(), ".contextos/jobs.toml", &state)?;
    let config_path = support::write_vault_config(config_dir.path(), vault_dir.path())?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    let host = support::host(config_dir.path());

    poll_once(&clients, &host, &FixedClock).await;

    Ok((vault_dir, state))
}

#[tokio::test]
async fn an_enabled_job_that_is_due_is_started_by_the_scheduler() -> Result<(), BoxError> {
    let (vault_dir, _state) = pass_over("enabled").await?;

    let records = run_records(vault_dir.path());

    assert_eq!(records.len(), 1, "the due tick started one run");
    let text = std::fs::read_to_string(&records[0])?;
    assert!(text.contains("trigger: scheduled"), "{text}");
    Ok(())
}

#[tokio::test]
async fn a_job_in_any_other_state_is_not_started_whatever_schedule_is_stored_for_it() -> Result<(), BoxError> {
    for lifecycle in ["draft", "validated", "registered", "suspended", "retired"] {
        let (vault_dir, state) = pass_over(lifecycle).await?;

        assert!(
            run_records(vault_dir.path()).is_empty(),
            "{lifecycle}: nothing is started and no tick is recorded as skipped"
        );
        assert_eq!(
            std::fs::read_to_string(vault_dir.path().join(".contextos/jobs.toml"))?,
            state,
            "{lifecycle}: the scheduler left the state as it found it"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_scheduler_pass_validates_a_ready_draft_job_and_leaves_it_unscheduled() -> Result<(), BoxError> {
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
        "name = \"Sync\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = []\n",
    )?;
    write(vault_dir.path(), "registry/workflows/ops/sync/workflow.rhai", "()")?;
    write(
        vault_dir.path(),
        "registry/workflows/ops/sync/jobs/nightly.toml",
        "ready = true\nname = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    support::release_fixture::install_vault_releases(vault_dir.path()).map_err(|error| error.to_string())?;
    let config_path = support::write_vault_config(config_dir.path(), vault_dir.path())?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    let host = support::host(config_dir.path());

    poll_once(&clients, &host, &FixedClock).await;

    let state = std::fs::read_to_string(vault_dir.path().join(".contextos/jobs.toml"))?;
    assert!(state.contains("lifecycle = \"validated\""), "{state}");
    assert!(
        run_records(vault_dir.path()).is_empty(),
        "validated is not scheduled or run"
    );
    Ok(())
}
