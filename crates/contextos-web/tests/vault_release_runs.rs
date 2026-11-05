//! A run of a saved vault job reads its code from the release its job file names, verified before the script starts,
//! against a real `contextos-mcp` session and a real temporary vault.

mod support;

use contextos_web::cli_run::CliRunOptions;
use contextos_workflows::{RunOutcome, WorkflowFailureKind};
use support::BoxError;
use support::jobs::{JobVault, Spec, VAULT_NAME, enabled_state, lifecycle_state};

/// A vault whose job is `registered` for the version its source has, so a run by hand is allowed.
async fn registered_vault() -> Result<JobVault, BoxError> {
    let spec = Spec::default();
    let state = lifecycle_state(&spec, "registered");
    JobVault::open(&Spec {
        state: Some(&state),
        ..spec
    })
    .await
}

async fn run(vault: &JobVault) -> Result<RunOutcome, BoxError> {
    Ok(support::run_job(
        std::sync::Arc::clone(&vault.clients),
        &vault.host,
        VAULT_NAME,
        "ops",
        "sync",
        "nightly",
        CliRunOptions {
            dry_run_scenario: None,
            record: true,
        },
    )
    .await?)
}

fn refused_as_unavailable(outcome: &RunOutcome) -> bool {
    matches!(outcome, RunOutcome::Failed(failure) if failure.kind == Some(WorkflowFailureKind::PackageUnavailable))
}

#[tokio::test]
async fn a_run_executes_the_release_not_the_source_that_was_edited_since() -> Result<(), BoxError> {
    let vault = registered_vault().await?;
    vault.write("registry/workflows/ops/sync/sync.rhai", "throw \"the source was run\";")?;

    let outcome = run(&vault).await?;

    assert!(matches!(outcome, RunOutcome::Succeeded), "{outcome:?}");
    Ok(())
}

#[tokio::test]
async fn a_run_of_a_release_that_was_removed_is_refused_before_its_script_starts() -> Result<(), BoxError> {
    let vault = registered_vault().await?;
    std::fs::remove_dir_all(vault.root().join(".contextos/packages/ops"))?;

    let outcome = run(&vault).await?;

    assert!(refused_as_unavailable(&outcome), "{outcome:?}");
    Ok(())
}

#[tokio::test]
async fn a_run_of_a_release_whose_file_was_changed_is_refused() -> Result<(), BoxError> {
    let vault = registered_vault().await?;
    vault.write(
        ".contextos/packages/ops/1.0.0/sync/sync.rhai",
        "log_info(\"tampered\");",
    )?;

    let outcome = run(&vault).await?;

    assert!(refused_as_unavailable(&outcome), "{outcome:?}");
    Ok(())
}

#[tokio::test]
async fn a_run_of_an_incomplete_release_is_refused() -> Result<(), BoxError> {
    let vault = registered_vault().await?;
    std::fs::remove_file(vault.root().join(".contextos/packages/ops/1.0.0/sync/sync.rhai"))?;

    let outcome = run(&vault).await?;

    assert!(refused_as_unavailable(&outcome), "{outcome:?}");
    Ok(())
}

const CALLS_A_SERVER: &str = "call_mcp(\"contextos\", \"vault_info\", #{});";

#[tokio::test]
async fn a_run_is_scoped_to_the_servers_the_release_declares_not_those_the_source_now_declares() -> Result<(), BoxError>
{
    let spec = Spec {
        script: CALLS_A_SERVER,
        ..Spec::default()
    };
    let state = lifecycle_state(&spec, "registered");
    let vault = JobVault::open(&Spec {
        state: Some(&state),
        ..spec
    })
    .await?;
    vault.write("registry/workflows/ops/workflows.toml", "workflows = [\"sync\"]\n")?;
    vault.write(
        "registry/workflows/ops/sync/workflow.toml",
        "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = []\n",
    )?;

    let outcome = run(&vault).await?;

    assert!(matches!(outcome, RunOutcome::Succeeded), "{outcome:?}");
    Ok(())
}

#[tokio::test]
async fn a_server_only_the_source_now_declares_is_not_available_to_a_run_of_the_release() -> Result<(), BoxError> {
    let spec = Spec {
        workflow: "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = []\n",
        script: "try { call_mcp(\"contextos\", \"vault_info\", #{}); } catch (e) { throw \"refused\"; }",
        ..Spec::default()
    };
    let state = lifecycle_state(&spec, "registered");
    let vault = JobVault::open(&Spec {
        state: Some(&state),
        ..spec
    })
    .await?;
    vault.write("registry/workflows/ops/workflows.toml", "workflows = [\"sync\"]\n")?;
    vault.write(
        "registry/workflows/ops/sync/workflow.toml",
        "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"contextos\"]\n",
    )?;

    let outcome = run(&vault).await?;

    assert!(matches!(outcome, RunOutcome::Failed(_)), "{outcome:?}");
    Ok(())
}

#[tokio::test]
async fn a_run_refused_for_a_removed_release_leaves_an_enabled_jobs_trial_pending() -> Result<(), BoxError> {
    let spec = Spec::default();
    let state = format!("{}first_run = true\n", enabled_state(&spec));
    let vault = JobVault::open(&Spec {
        state: Some(&state),
        ..spec
    })
    .await?;
    std::fs::remove_dir_all(vault.root().join(".contextos/packages/ops"))?;

    let outcome = run(&vault).await?;

    assert!(refused_as_unavailable(&outcome), "{outcome:?}");
    let state = vault.state()?;
    let job = state.job("ops", "sync", "nightly");
    assert!(
        job.is_some_and(contextos_workflows::JobState::first_run_pending),
        "the trial is still pending"
    );
    assert_eq!(
        vault.lifecycle()?,
        "enabled",
        "the next pass applies the stage table, not the run"
    );
    Ok(())
}
