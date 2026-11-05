//! A one-off run of a vault workflow takes a release from the vault's package store, or the source in `registry/`, and
//! never a job, a claim, or a run record, against a real `contextos-mcp` session and a real temporary vault.

mod support;

use std::sync::Arc;

use contextos_web::cli_run::{OneOffCode, OneOffVaultRun, run_workflow_once_reported};
use contextos_web::run_report::RunReport;
use contextos_workflows::{ReleaseChoice, RunOutcome, WorkflowFailureKind};
use semver::Version;
use support::BoxError;
use support::jobs::{JobVault, STATE_PATH, Spec, VAULT_NAME, summary_of};
use tokio_util::sync::CancellationToken;

const CURRENT_IS_ONE: &str = "[ops.sync]\ncurrent = \"1.0.0\"\n";
const CURRENT_IS_TWO: &str = "[ops.sync]\ncurrent = \"2.0.0\"\n";
const THROWS: &str = "throw \"the source was run\";";

async fn vault_with_state(state: &'static str) -> Result<JobVault, BoxError> {
    JobVault::open(&Spec {
        state: Some(state),
        ..Spec::default()
    })
    .await
}

async fn once(vault: &JobVault, code: OneOffCode) -> Result<RunReport, BoxError> {
    Ok(run_workflow_once_reported(
        Arc::clone(&vault.clients),
        "contextos",
        OneOffVaultRun {
            code,
            vault: VAULT_NAME.to_owned(),
            package: "ops".to_owned(),
            workflow_slug: "sync".to_owned(),
            arguments: Vec::new(),
            dry_run_scenario: None,
            log_level: None,
        },
        CancellationToken::new(),
    )
    .await?)
}

fn release(choice: ReleaseChoice) -> OneOffCode {
    OneOffCode::Release(choice)
}

fn refused_as(report: &RunReport, kind: WorkflowFailureKind) -> bool {
    !report.started && matches!(&report.outcome, RunOutcome::Failed(failure) if failure.kind == Some(kind))
}

#[tokio::test]
async fn a_one_off_run_of_current_runs_the_installed_release_not_the_source_edited_since() -> Result<(), BoxError> {
    let vault = vault_with_state(CURRENT_IS_ONE).await?;
    vault.write("registry/workflows/ops/sync/sync.rhai", THROWS)?;

    let report = once(&vault, release(ReleaseChoice::Current)).await?;

    assert!(matches!(report.outcome, RunOutcome::Succeeded), "{:?}", report.outcome);
    Ok(())
}

#[tokio::test]
async fn a_one_off_run_of_source_runs_the_source_and_installs_nothing() -> Result<(), BoxError> {
    let vault = vault_with_state(CURRENT_IS_ONE).await?;
    vault.write("registry/workflows/ops/sync/sync.rhai", THROWS)?;
    let installed = std::fs::read_dir(vault.root().join(".contextos/packages/ops"))?.count();

    let report = once(&vault, OneOffCode::Source).await?;

    assert!(matches!(report.outcome, RunOutcome::Failed(_)), "{:?}", report.outcome);
    assert!(report.started, "the source script ran");
    assert_eq!(
        std::fs::read_dir(vault.root().join(".contextos/packages/ops"))?.count(),
        installed
    );
    Ok(())
}

#[tokio::test]
async fn a_one_off_run_takes_no_claim_and_writes_no_run_record_whatever_the_code() -> Result<(), BoxError> {
    let vault = vault_with_state(CURRENT_IS_ONE).await?;

    for code in [release(ReleaseChoice::Current), OneOffCode::Source] {
        let report = once(&vault, code).await?;
        assert!(matches!(report.outcome, RunOutcome::Succeeded), "{:?}", report.outcome);
    }

    assert!(vault.run_records().is_empty(), "{:?}", vault.run_records());
    assert!(!vault.root().join(".contextos/claims").exists());
    Ok(())
}

#[tokio::test]
async fn a_one_off_run_of_a_release_the_workflow_does_not_have_is_refused_as_not_runnable() -> Result<(), BoxError> {
    let vault = vault_with_state("").await?;

    for choice in [ReleaseChoice::Current, ReleaseChoice::Previous] {
        let report = once(&vault, release(choice.clone())).await?;
        assert!(
            refused_as(&report, WorkflowFailureKind::JobNotRunnable),
            "{choice:?}: {:?}",
            report.outcome
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_one_off_run_of_a_retired_release_is_refused_as_not_runnable() -> Result<(), BoxError> {
    let vault = vault_with_state(CURRENT_IS_TWO).await?;

    let report = once(&vault, release(ReleaseChoice::Version(Version::new(1, 0, 0)))).await?;

    assert!(
        refused_as(&report, WorkflowFailureKind::JobNotRunnable),
        "{:?}",
        report.outcome
    );
    Ok(())
}

#[tokio::test]
async fn a_one_off_run_of_a_release_that_was_removed_or_changed_is_refused_before_its_script_starts()
-> Result<(), BoxError> {
    let vault = vault_with_state(CURRENT_IS_ONE).await?;
    vault.write(
        ".contextos/packages/ops/1.0.0/sync/sync.rhai",
        "log_info(\"tampered\");",
    )?;

    let changed = once(&vault, release(ReleaseChoice::Current)).await?;
    std::fs::remove_dir_all(vault.root().join(".contextos/packages/ops"))?;
    let removed = once(&vault, release(ReleaseChoice::Current)).await?;

    for report in [changed, removed] {
        assert!(
            refused_as(&report, WorkflowFailureKind::PackageUnavailable),
            "{:?}",
            report.outcome
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_one_off_run_of_a_version_the_store_never_recorded_is_refused_as_not_runnable() -> Result<(), BoxError> {
    let vault = vault_with_state(CURRENT_IS_ONE).await?;
    vault.rebuild_ledger().await?;

    let report = once(&vault, release(ReleaseChoice::Version(Version::new(9, 0, 0)))).await?;

    assert!(
        refused_as(&report, WorkflowFailureKind::JobNotRunnable),
        "{:?}",
        report.outcome
    );
    Ok(())
}

#[tokio::test]
async fn a_one_off_run_of_a_version_recorded_but_removed_is_refused_as_package_unavailable() -> Result<(), BoxError> {
    let vault = vault_with_state(CURRENT_IS_ONE).await?;
    vault.rebuild_ledger().await?;
    vault.remove_release("ops", "1.0.0").await?;

    let report = once(&vault, release(ReleaseChoice::Version(Version::new(1, 0, 0)))).await?;

    assert!(
        refused_as(&report, WorkflowFailureKind::PackageUnavailable),
        "{:?}",
        report.outcome
    );
    Ok(())
}

/// A vault with releases 1.0.0, 2.0.0, and 3.0.0 of the workflow installed, in which the state records 2.0.0 as
/// current, 1.0.0 as previous, and 3.0.0 as future.
async fn vault_with_three_releases() -> Result<JobVault, BoxError> {
    let vault = vault_with_state("").await?;
    for version in ["2.0.0", "3.0.0"] {
        vault.publish_source_as(version)?;
    }
    vault.write(
        STATE_PATH,
        "[ops.sync]\ncurrent = \"2.0.0\"\nprevious = \"1.0.0\"\nfuture = \"3.0.0\"\n",
    )?;
    Ok(vault)
}

#[tokio::test]
async fn a_one_off_run_of_future_or_previous_runs_that_release_and_the_summary_names_its_version()
-> Result<(), BoxError> {
    let vault = vault_with_three_releases().await?;

    for (choice, version) in [
        (ReleaseChoice::Future, "3.0.0"),
        (ReleaseChoice::Previous, "1.0.0"),
        (ReleaseChoice::Current, "2.0.0"),
    ] {
        let report = once(&vault, release(choice.clone())).await?;

        assert!(
            matches!(report.outcome, RunOutcome::Succeeded),
            "{choice:?}: {:?}",
            report.outcome
        );
        let document = serde_json::to_value(summary_of(report))?;
        assert_eq!(document["scope"]["package_version"], version, "{choice:?}");
    }
    Ok(())
}
