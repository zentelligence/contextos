//! A release, or a library release in its lock, that is incomplete or cannot be verified says nothing about the job that
//! names it, so a validation pass leaves the job's stage and recorded version exactly as they were. Only a release that
//! is removed or damaged returns a job to draft.

mod support;

use support::BoxError;
use support::jobs::{JobVault, READY_JOB, Spec, current_enabled_state, current_state};
use time::macros::datetime;

const WORKFLOW: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"contextos\"]\n";
const PACKAGE: &str = "workflows = [\"sync\"]\n[[libraries]]\nname = \"crm\"\nrequirement = \"*\"\n";
const LIBRARY: &str = "modules = [\"client.rhai\"]\nexports = [\"client.rhai\"]\n";
const ENTRY: &str = "import \"crm:client\" as crm;\ncrm::a()\n";
const NOW: time::OffsetDateTime = datetime!(2026-10-02 03:00:00 UTC);

fn recorded_version(vault: &JobVault) -> Result<Option<String>, BoxError> {
    Ok(vault
        .state()?
        .job("ops", "sync", "nightly")
        .and_then(contextos_workflows::JobState::fingerprint)
        .map(|version| String::from(version.clone())))
}

fn make_incomplete(vault: &JobVault, release_file: &str) -> std::io::Result<()> {
    std::fs::remove_file(vault.root().join(release_file))
}

#[tokio::test]
async fn a_validated_job_whose_release_is_incomplete_stays_validated_with_its_version() -> Result<(), BoxError> {
    let spec = Spec::default();
    let state = current_state(&spec, "validated");
    let vault = JobVault::open(&Spec {
        state: Some(&state),
        ..spec
    })
    .await?;
    let before = recorded_version(&vault)?;
    make_incomplete(&vault, ".contextos/packages/ops/1.0.0/sync/sync.rhai")?;

    vault.pass(NOW).await;

    assert_eq!(vault.lifecycle()?, "validated");
    assert_eq!(recorded_version(&vault)?, before);
    Ok(())
}

#[tokio::test]
async fn a_registered_job_whose_release_is_incomplete_stays_registered_with_its_version() -> Result<(), BoxError> {
    let spec = Spec::default();
    let state = current_state(&spec, "registered");
    let vault = JobVault::open(&Spec {
        state: Some(&state),
        ..spec
    })
    .await?;
    let before = recorded_version(&vault)?;
    make_incomplete(&vault, ".contextos/packages/ops/1.0.0/sync/sync.rhai")?;

    vault.pass(NOW).await;

    assert_eq!(vault.lifecycle()?, "registered");
    assert_eq!(recorded_version(&vault)?, before);
    Ok(())
}

#[tokio::test]
async fn an_enabled_job_whose_release_is_incomplete_stays_enabled_with_its_version() -> Result<(), BoxError> {
    let spec = Spec::default();
    let state = current_enabled_state(&spec);
    let vault = JobVault::open(&Spec {
        state: Some(&state),
        ..spec
    })
    .await?;
    let before = recorded_version(&vault)?;
    make_incomplete(&vault, ".contextos/packages/ops/1.0.0/sync/sync.rhai")?;

    vault.pass(NOW).await;

    assert_eq!(vault.lifecycle()?, "enabled");
    assert_eq!(recorded_version(&vault)?, before);
    Ok(())
}

#[tokio::test]
async fn a_validated_job_whose_locked_library_release_is_incomplete_stays_validated_with_its_version()
-> Result<(), BoxError> {
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
    let before = recorded_version(&vault)?;
    make_incomplete(&vault, ".contextos/packages/crm/1.0.0/client.rhai")?;

    vault.pass(NOW).await;

    assert_eq!(vault.lifecycle()?, "validated");
    assert_eq!(recorded_version(&vault)?, before);
    Ok(())
}

#[tokio::test]
async fn a_validated_job_whose_release_is_removed_returns_to_draft() -> Result<(), BoxError> {
    let spec = Spec::default();
    let state = current_state(&spec, "validated");
    let vault = JobVault::open(&Spec {
        state: Some(&state),
        ..spec
    })
    .await?;
    std::fs::remove_dir_all(vault.root().join(".contextos/packages/ops"))?;

    vault.pass(NOW).await;

    assert_eq!(vault.lifecycle()?, "draft");
    Ok(())
}
