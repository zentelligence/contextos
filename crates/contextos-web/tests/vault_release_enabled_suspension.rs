//! An `enabled` vault job whose release, or locked library release, is removed is suspended by the next validation pass
//! with `workflow/package-unavailable`; one whose release is damaged is suspended with `workflow/validation-failed`.

mod support;

use contextos_workflows::JobState;
use support::BoxError;
use support::jobs::{JobVault, READY_JOB, STATE_PATH, Spec, current_enabled_state};
use time::macros::datetime;

const WORKFLOW: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"contextos\"]\n";
const PACKAGE: &str = "workflows = [\"sync\"]\n[[libraries]]\nname = \"crm\"\nrequirement = \"*\"\n";
const LIBRARY: &str = "modules = [\"client.rhai\"]\nexports = [\"client.rhai\"]\n";
const ENTRY: &str = "import \"crm:client\" as crm;\ncrm::a()\n";
const NOW: time::OffsetDateTime = datetime!(2026-10-02 03:00:00 UTC);

async fn enabled_job() -> Result<JobVault, BoxError> {
    let spec = Spec::default();
    let state = current_enabled_state(&spec);
    JobVault::open(&Spec {
        state: Some(&state),
        ..spec
    })
    .await
}

/// A job that imports from the library `crm`, validated by a first pass, then made `enabled` keeping its version.
async fn enabled_job_with_library() -> Result<JobVault, BoxError> {
    let vault = JobVault::open(&Spec {
        package: PACKAGE,
        workflow: WORKFLOW,
        script: ENTRY,
        job: READY_JOB,
        tasks: false,
        ..Spec::default()
    })
    .await?;
    vault.write("registry/libraries/crm/library.toml", LIBRARY)?;
    vault.write("registry/libraries/crm/client.rhai", "fn a() { 1 }")?;
    vault.install_releases()?;
    vault.pass(NOW).await;
    assert_eq!(vault.lifecycle()?, "validated");
    let stored = vault.read(STATE_PATH)?;
    let enabled: String = stored
        .lines()
        .map(|line| {
            if line == "lifecycle = \"validated\"" {
                "lifecycle = \"enabled\"\nschedule = \"* * * * *\"".to_owned()
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    vault.write(STATE_PATH, &enabled)?;
    assert_eq!(vault.lifecycle()?, "enabled");
    Ok(vault)
}

fn suspension_code(vault: &JobVault) -> Result<String, BoxError> {
    let state = vault.state()?;
    let record = state
        .job("ops", "sync", "nightly")
        .and_then(JobState::suspension)
        .ok_or("the job has no suspension record")?;
    Ok(record.code.clone())
}

#[tokio::test]
async fn an_enabled_job_whose_release_is_removed_is_suspended_as_package_unavailable() -> Result<(), BoxError> {
    let vault = enabled_job().await?;
    std::fs::remove_dir_all(vault.root().join(".contextos/packages/ops"))?;

    vault.pass(NOW).await;

    assert_eq!(vault.lifecycle()?, "suspended");
    assert_eq!(suspension_code(&vault)?, "workflow/package-unavailable");
    Ok(())
}

#[tokio::test]
async fn an_enabled_job_whose_release_is_damaged_is_suspended_as_validation_failed() -> Result<(), BoxError> {
    let vault = enabled_job().await?;
    vault.write(
        ".contextos/packages/ops/1.0.0/sync/sync.rhai",
        "log_info(\"tampered\");",
    )?;

    vault.pass(NOW).await;

    assert_eq!(vault.lifecycle()?, "suspended");
    assert_eq!(suspension_code(&vault)?, "workflow/validation-failed");
    Ok(())
}

#[tokio::test]
async fn an_enabled_job_whose_locked_library_release_is_removed_is_suspended_as_package_unavailable()
-> Result<(), BoxError> {
    let vault = enabled_job_with_library().await?;
    std::fs::remove_dir_all(vault.root().join(".contextos/packages/crm"))?;

    vault.pass(NOW).await;

    assert_eq!(vault.lifecycle()?, "suspended");
    assert_eq!(suspension_code(&vault)?, "workflow/package-unavailable");
    Ok(())
}

#[tokio::test]
async fn an_enabled_job_whose_locked_library_release_is_damaged_is_suspended_as_validation_failed()
-> Result<(), BoxError> {
    let vault = enabled_job_with_library().await?;
    vault.write(".contextos/packages/crm/1.0.0/client.rhai", "fn a() { 2 }")?;

    vault.pass(NOW).await;

    assert_eq!(vault.lifecycle()?, "suspended");
    assert_eq!(suspension_code(&vault)?, "workflow/validation-failed");
    Ok(())
}
