//! What settling a live run's outcome does to the host's examined mark: the run is marked examined only when the
//! settlement suspended the job, because only then does a suspension handle the failure. A settlement that was not
//! recorded, or that settles nothing, leaves the failed run to be looked at. Real `contextos` sessions over real
//! temporary vaults.

mod support;

use contextos_web::discover_jobs;
use contextos_web::job_probation::{LiveRunEnded, Settlement, settle_and_examine};
use contextos_workflows::{RunFailure, RunOutcome};
use support::BoxError;
use support::jobs::{FAILING_SCRIPT, JobVault, Spec, VAULT_NAME, enabled_state, version_of};
use time::OffsetDateTime;
use time::macros::datetime;

const NOW: OffsetDateTime = datetime!(2026-10-01 03:00:30 UTC);

fn failed() -> RunOutcome {
    RunOutcome::Failed(RunFailure::from("it broke".to_owned()))
}

fn mark(vault: &JobVault) -> Option<String> {
    let folder = vault
        .config
        .path()
        .join("claims")
        .join("vaults")
        .join(VAULT_NAME.to_ascii_lowercase())
        .join("ops")
        .join("sync");
    std::fs::read_to_string(folder.join("nightly.run-failed-examined"))
        .ok()
        .map(|text| text.trim().to_owned())
}

fn spec() -> Spec<'static> {
    Spec {
        script: FAILING_SCRIPT,
        ..Spec::default()
    }
}

async fn vault(on_trial: bool) -> Result<JobVault, BoxError> {
    let mut state = enabled_state(&spec());
    if on_trial {
        state.push_str("first_run = true\n");
    }
    JobVault::open(&Spec {
        state: Some(&state),
        ..spec()
    })
    .await
}

async fn settle(vault: &JobVault, version_known: bool, outcome: &RunOutcome) -> Result<Settlement, BoxError> {
    let client = vault.clients.get("contextos").ok_or("no primary server")?;
    let job = discover_jobs(&client, VAULT_NAME, &["contextos".to_owned()])
        .await?
        .into_iter()
        .find(|job| job.slug == "nightly")
        .ok_or("the job was not discovered")?;
    let version = version_known.then(|| version_of(&spec()));
    Ok(settle_and_examine(LiveRunEnded {
        client: &client,
        host: &vault.host,
        vault_name: VAULT_NAME,
        job: &job,
        version,
        outcome,
        run: 3,
        now: NOW,
    })
    .await)
}

#[tokio::test]
async fn a_settlement_that_suspends_the_job_marks_the_run_it_names_examined() -> Result<(), BoxError> {
    let vault = vault(true).await?;

    let settled = settle(&vault, true, &failed()).await?;

    assert_eq!(settled, Settlement::Suspended);
    assert_eq!(mark(&vault).as_deref(), Some("3"), "the suspension handles run 3");
    Ok(())
}

#[tokio::test]
async fn a_settlement_that_could_not_be_recorded_leaves_the_failed_run_unexamined() -> Result<(), BoxError> {
    let vault = vault(true).await?;

    // Without the version that ran, the outcome cannot be applied to the trial.
    let settled = settle(&vault, false, &failed()).await?;

    assert_eq!(settled, Settlement::NotRecorded);
    assert_eq!(vault.lifecycle()?, "enabled");
    assert_eq!(
        mark(&vault),
        None,
        "nothing handles the failure, so it is still to be looked at"
    );
    Ok(())
}

#[tokio::test]
async fn a_settlement_that_settles_nothing_leaves_the_failed_run_unexamined() -> Result<(), BoxError> {
    // A job past its trial: a failed run is a failure of an enabled job, for a run-failed task, not for a suspension.
    let vault = vault(false).await?;

    let settled = settle(&vault, true, &failed()).await?;

    assert_eq!(settled, Settlement::NothingToSettle);
    assert_eq!(mark(&vault), None);
    Ok(())
}
