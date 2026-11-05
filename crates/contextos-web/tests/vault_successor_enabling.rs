//! Enabling a successor job in a vault retires its predecessor and moves the workflow's `current` and `previous`, in one write, against a
//! real `contextos-mcp` session and a real temporary vault.

mod support;

use contextos_web::cli_run::{CliRunError, CliRunOptions};
use contextos_web::job_lifecycle::{LifecycleChange, change_lifecycle};
use contextos_web::job_operation::{JobStore, OperationOutcome, OperatorRequest, perform};
use contextos_web::job_operation_vault::VaultJobStore;
use contextos_web::run_claim::RunClaim;
use contextos_web::scheduler::job_is_enabled_now;
use contextos_web::vault_runs::claim_slot;
use contextos_workflows::{Actor, ChainRelation, JobLifecycle, JobSchedule, JobState, Transition};
use semver::Version;
use support::BoxError;
use support::jobs::{JobVault, READY_JOB, STATE_PATH, Spec, VAULT_NAME};
use time::macros::datetime;

const NOW: time::OffsetDateTime = datetime!(2026-10-01 03:00:00 UTC);
const SUCCESSOR: &str = "ready = true\nname = \"Nightly again\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"2.0.0\"\n\
                         predecessor = \"nightly\"\n";
const PREDECESSOR_ENABLED: &str = "[ops.sync.nightly]\nlifecycle = \"enabled\"\nschedule = \"30 4 * * *\"\n\
    identity = \"job-old\"\nrelease = \"1.0.0\"\nlifecycle_changed = \"2026-09-01T00:00:00Z\"\n\
    lifecycle_actor = \"operator\"\n\n[ops.sync]\ncurrent = \"1.0.0\"\n\
    current_at = \"2026-09-01T00:00:00Z\"\n";

fn schedule(expression: &str) -> Result<JobSchedule, BoxError> {
    Ok(JobSchedule::try_from(expression)?)
}

/// A vault whose `nightly` is enabled on release 1.0.0, with a registered successor `nightly-v2` on release 2.0.0.
async fn vault_with_registered_successor() -> Result<JobVault, BoxError> {
    let vault = JobVault::open(&Spec {
        job: READY_JOB,
        state: Some(PREDECESSOR_ENABLED),
        ..Spec::default()
    })
    .await?;
    vault.write("registry/workflows/ops/sync/jobs/nightly-v2.toml", SUCCESSOR)?;
    vault.install_releases()?;
    let client = vault.clients.get("contextos").ok_or("no primary server")?;
    let servers = vec!["contextos".to_owned()];
    let store = VaultJobStore {
        client: &client,
        vault_name: VAULT_NAME,
        known_mcp_servers: &servers,
        package: "ops",
        workflow_slug: "sync",
        job_slug: "nightly-v2",
        claims_root: vault.host.claims_root.as_deref(),
    };
    let validated = perform(&store, OperatorRequest::Validate, NOW).await?;
    assert!(
        matches!(validated, OperationOutcome::Validated { ref result, .. } if result.passed),
        "{validated:?}"
    );
    let version = store.version().await?.ok_or("the successor has no version")?;
    change_lifecycle(
        &client,
        VAULT_NAME,
        LifecycleChange {
            package: "ops",
            workflow_slug: "sync",
            job_slug: "nightly-v2",
            actor: Actor::DryRun,
            transition: Transition::DryRunCompleted(version),
            now: NOW,
        },
    )
    .await?;
    Ok(vault)
}

async fn operate(vault: &JobVault, job: &str, request: OperatorRequest) -> Result<OperationOutcome, BoxError> {
    let client = vault.clients.get("contextos").ok_or("no primary server")?;
    let servers = vec!["contextos".to_owned()];
    let store = VaultJobStore {
        client: &client,
        vault_name: VAULT_NAME,
        known_mcp_servers: &servers,
        package: "ops",
        workflow_slug: "sync",
        job_slug: job,
        claims_root: vault.host.claims_root.as_deref(),
    };
    Ok(perform(&store, request, NOW).await?)
}

#[tokio::test]
async fn enabling_a_successor_retires_its_predecessor_and_moves_current_and_previous() -> Result<(), BoxError> {
    let vault = vault_with_registered_successor().await?;

    operate(&vault, "nightly-v2", OperatorRequest::Schedule(schedule("0 5 * * *")?)).await?;

    let state = vault.state()?;
    assert_eq!(state.lifecycle_of("ops", "sync", "nightly-v2"), JobLifecycle::Enabled);
    assert_eq!(state.lifecycle_of("ops", "sync", "nightly"), JobLifecycle::Retired);
    let versions = state
        .package_releases("ops")
        .and_then(|held| held.workflows.get("sync"))
        .ok_or("no versions recorded")?;
    assert_eq!(versions.current, Some(Version::new(2, 0, 0)));
    assert_eq!(versions.previous, Some(Version::new(1, 0, 0)));
    assert!(vault.read(STATE_PATH)?.contains("successor-enabled"));
    Ok(())
}

#[tokio::test]
async fn clearing_the_schedule_of_a_retired_job_succeeds_and_leaves_it_retired() -> Result<(), BoxError> {
    let vault = vault_with_registered_successor().await?;
    operate(&vault, "nightly-v2", OperatorRequest::Schedule(schedule("0 5 * * *")?)).await?;

    operate(&vault, "nightly", OperatorRequest::ClearSchedule).await?;

    assert_eq!(
        vault.state()?.lifecycle_of("ops", "sync", "nightly"),
        JobLifecycle::Retired
    );
    Ok(())
}

#[tokio::test]
async fn a_successor_enabled_with_no_schedule_given_takes_its_predecessors_schedule() -> Result<(), BoxError> {
    let vault = vault_with_registered_successor().await?;

    operate(&vault, "nightly-v2", OperatorRequest::Enable).await?;

    let state = vault.state()?;
    assert_eq!(state.lifecycle_of("ops", "sync", "nightly-v2"), JobLifecycle::Enabled);
    assert_eq!(state.lifecycle_of("ops", "sync", "nightly"), JobLifecycle::Retired);
    assert_eq!(
        state.job("ops", "sync", "nightly-v2").and_then(JobState::schedule),
        Some("30 4 * * *")
    );
    Ok(())
}

async fn run_by_hand(vault: &JobVault, job: &str, dry: bool) -> Result<contextos_workflows::RunOutcome, CliRunError> {
    let options = CliRunOptions {
        dry_run_scenario: dry.then_some(None),
        ..CliRunOptions::default()
    };
    support::run_job(
        vault.clients.clone(),
        &vault.host,
        VAULT_NAME,
        "ops",
        "sync",
        job,
        options,
    )
    .await
}

#[tokio::test]
async fn a_live_run_by_hand_of_a_job_with_a_standing_successor_is_refused_naming_the_successor() -> Result<(), BoxError>
{
    let vault = vault_with_registered_successor().await?;

    let refused = run_by_hand(&vault, "nightly", false).await;

    let Err(CliRunError::ChainHeld(hold)) = &refused else {
        return Err(format!("expected the chain to refuse the run: {refused:?}").into());
    };
    assert_eq!(hold.job, "nightly-v2");
    assert_eq!(hold.relation, ChainRelation::Successor);
    let message = refused.err().map(|error| error.to_string()).unwrap_or_default();
    assert!(message.starts_with("workflow/job-not-runnable"), "{message}");
    assert!(message.contains("nightly-v2"), "{message}");
    assert!(vault.run_records().is_empty(), "a refused run leaves no record");
    Ok(())
}

#[tokio::test]
async fn a_live_run_by_hand_of_a_successor_is_refused_while_its_predecessor_is_enabled() -> Result<(), BoxError> {
    let vault = vault_with_registered_successor().await?;

    let refused = run_by_hand(&vault, "nightly-v2", false).await;

    let Err(CliRunError::ChainHeld(hold)) = &refused else {
        return Err(format!("expected the chain to refuse the run: {refused:?}").into());
    };
    assert_eq!(hold.job, "nightly");
    assert_eq!(hold.relation, ChainRelation::Predecessor(JobLifecycle::Enabled));
    assert!(vault.run_records().is_empty(), "a refused run leaves no record");
    Ok(())
}

#[tokio::test]
async fn a_dry_run_of_a_successor_is_not_refused_by_the_chain() -> Result<(), BoxError> {
    let vault = vault_with_registered_successor().await?;

    let outcome = run_by_hand(&vault, "nightly-v2", true).await;

    assert!(!matches!(outcome, Err(CliRunError::ChainHeld(_))), "{outcome:?}");
    Ok(())
}

#[tokio::test]
async fn enabling_a_successor_is_refused_as_busy_while_its_predecessor_holds_its_run_claim() -> Result<(), BoxError> {
    let vault = vault_with_registered_successor().await?;
    let slot = claim_slot(&vault.host, VAULT_NAME, "ops", "sync", "nightly")?;
    let claim = RunClaim::acquire(&slot)?;

    let refused = operate(&vault, "nightly-v2", OperatorRequest::Schedule(schedule("0 5 * * *")?)).await;

    let Err(error) = refused else {
        return Err("the enabling went ahead under the predecessor's claim".into());
    };
    assert!(error.to_string().contains("workflow/job-busy"), "{error}");
    let state = vault.state()?;
    assert_eq!(state.lifecycle_of("ops", "sync", "nightly"), JobLifecycle::Enabled);
    assert_eq!(
        state.lifecycle_of("ops", "sync", "nightly-v2"),
        JobLifecycle::Registered
    );

    claim.release().await?;
    operate(&vault, "nightly-v2", OperatorRequest::Schedule(schedule("0 5 * * *")?)).await?;
    assert_eq!(
        vault.state()?.lifecycle_of("ops", "sync", "nightly"),
        JobLifecycle::Retired
    );
    Ok(())
}

#[tokio::test]
async fn a_job_retired_by_an_enabling_is_no_longer_enabled_when_the_scheduler_checks_after_taking_its_claim()
-> Result<(), BoxError> {
    let vault = vault_with_registered_successor().await?;
    let client = vault.clients.get("contextos").ok_or("no primary server")?;
    assert!(job_is_enabled_now(&client, VAULT_NAME, "ops", "sync", "nightly").await);

    operate(&vault, "nightly-v2", OperatorRequest::Schedule(schedule("0 5 * * *")?)).await?;

    assert!(!job_is_enabled_now(&client, VAULT_NAME, "ops", "sync", "nightly").await);
    assert!(job_is_enabled_now(&client, VAULT_NAME, "ops", "sync", "nightly-v2").await);
    Ok(())
}

#[tokio::test]
async fn enabling_a_successor_reports_the_predecessor_retired_and_how_current_and_previous_moved()
-> Result<(), BoxError> {
    let vault = vault_with_registered_successor().await?;

    let outcome = operate(&vault, "nightly-v2", OperatorRequest::Schedule(schedule("0 5 * * *")?)).await?;

    let OperationOutcome::Enabled(report) = outcome else {
        return Err(format!("expected an enabling report: {outcome:?}").into());
    };
    assert_eq!(report.schedule, "0 5 * * *");
    assert_eq!(report.retired.as_deref(), Some("nightly"));
    assert_eq!(
        report.current.map(|moved| (moved.from, moved.to)),
        Some((Some("1.0.0".to_owned()), Some("2.0.0".to_owned())))
    );
    assert_eq!(
        report.previous.map(|moved| (moved.from, moved.to)),
        Some((None, Some("1.0.0".to_owned())))
    );
    Ok(())
}
