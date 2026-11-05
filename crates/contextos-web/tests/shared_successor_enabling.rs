//! Enabling a shared successor job retires its predecessor and moves the workflow's `current` and `previous`, in one write, against a real
//! package store, real job files, and the real host state file.

mod support;

use contextos_web::cli_shared::{CliSharedError, SharedRunOptions, require_runnable};
use contextos_web::job_operation::{OperationOutcome, OperatorRequest, perform};
use contextos_web::job_operation_shared::SharedJobStore;
use contextos_web::shared_claim::{SharedClaim, SharedJobKey};
use contextos_web::shared_validation::SharedValidationHost;
use contextos_workflows::{Actor, ChainRelation, JobLifecycle, JobSchedule, JobState, RunTrigger, Transition};
use semver::Version;
use support::BoxError;
use support::shared::{DIGEST, FIRST, OTHER_DIGEST, SharedFixture, key, write};

const PREDECESSOR: &str = "ready = true\nname = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";
const SUCCESSOR: &str = "ready = true\nname = \"Nightly again\"\ntimeout_secs = 30\nversion = \"2.0.0\"\n\
                         predecessor = \"nightly\"\n";

fn successor_key() -> SharedJobKey {
    SharedJobKey {
        job: "nightly-v2".to_owned(),
        ..key()
    }
}

async fn ask(
    fixture: &SharedFixture,
    job: &SharedJobKey,
    request: OperatorRequest,
) -> Result<OperationOutcome, BoxError> {
    let state = fixture.state();
    let jobs_root = fixture.jobs_root();
    let store_root = fixture.store_root();
    let known = ["contextos".to_owned()];
    let validation = SharedValidationHost {
        store_root: &store_root,
        clients: &fixture.clients,
        primary_server: "contextos",
        known_servers: &known,
    };
    let store = SharedJobStore {
        state: &state,
        jobs_root: &jobs_root,
        validation: &validation,
        key: job,
        claims_root: &fixture.claims_root(),
    };
    Ok(perform(&store, request, FIRST).await?)
}

/// Takes `job` through validation and a dry run, and enables it on `schedule` as an operator would.
async fn enable(fixture: &SharedFixture, job: &SharedJobKey, schedule: &str) -> Result<(), BoxError> {
    ask(fixture, job, OperatorRequest::Validate).await?;
    let version = fixture
        .read()?
        .job(&job.package, &job.workflow, &job.job)
        .and_then(|state| state.fingerprint().cloned())
        .ok_or("the job was not validated")?;
    contextos_web::shared_lifecycle::change_shared_lifecycle(
        &fixture.state(),
        contextos_web::shared_lifecycle::SharedJobChange {
            key: job,
            actor: Actor::DryRun,
            transition: Transition::DryRunCompleted(version),
            now: FIRST,
        },
    )
    .await?;
    ask(
        fixture,
        job,
        OperatorRequest::Schedule(JobSchedule::try_from(schedule)?),
    )
    .await?;
    Ok(())
}

async fn fixture_with_enabled_predecessor() -> Result<SharedFixture, BoxError> {
    let fixture = SharedFixture::new(PREDECESSOR).await?;
    fixture.install("1.0.0", DIGEST, "log_info(\"one\");")?;
    fixture.install("2.0.0", OTHER_DIGEST, "log_info(\"two\");")?;
    write(&fixture.jobs_root().join("acme/sync/jobs/nightly-v2.toml"), SUCCESSOR)?;
    enable(&fixture, &key(), "30 4 * * *").await?;
    Ok(fixture)
}

/// The fixture with `nightly-v2` validated and dry run, so it is registered and waits to be enabled.
async fn fixture_with_registered_successor() -> Result<SharedFixture, BoxError> {
    let fixture = fixture_with_enabled_predecessor().await?;
    ask(&fixture, &successor_key(), OperatorRequest::Validate).await?;
    let version = fixture
        .read()?
        .job("acme", "sync", "nightly-v2")
        .and_then(|state| state.fingerprint().cloned())
        .ok_or("the successor was not validated")?;
    contextos_web::shared_lifecycle::change_shared_lifecycle(
        &fixture.state(),
        contextos_web::shared_lifecycle::SharedJobChange {
            key: &successor_key(),
            actor: Actor::DryRun,
            transition: Transition::DryRunCompleted(version),
            now: FIRST,
        },
    )
    .await?;
    Ok(fixture)
}

#[tokio::test]
async fn enabling_a_successor_retires_its_predecessor_and_moves_current_and_previous() -> Result<(), BoxError> {
    let fixture = fixture_with_registered_successor().await?;

    ask(&fixture, &successor_key(), OperatorRequest::Enable).await?;

    let state = fixture.read()?;
    assert_eq!(state.lifecycle_of("acme", "sync", "nightly-v2"), JobLifecycle::Enabled);
    assert_eq!(state.lifecycle_of("acme", "sync", "nightly"), JobLifecycle::Retired);
    assert_eq!(
        state.job("acme", "sync", "nightly-v2").and_then(JobState::schedule),
        Some("30 4 * * *")
    );
    let versions = state
        .package_releases("acme")
        .and_then(|held| held.workflows.get("sync"))
        .ok_or("no versions recorded")?;
    assert_eq!(versions.current, Some(Version::new(2, 0, 0)));
    assert_eq!(versions.previous, Some(Version::new(1, 0, 0)));
    Ok(())
}

#[tokio::test]
async fn clearing_the_schedule_of_a_retired_shared_job_succeeds() -> Result<(), BoxError> {
    let fixture = fixture_with_enabled_predecessor().await?;
    ask(&fixture, &key(), OperatorRequest::Retire).await?;

    ask(&fixture, &key(), OperatorRequest::ClearSchedule).await?;

    assert_eq!(fixture.lifecycle()?, "retired");
    Ok(())
}

fn live() -> SharedRunOptions {
    SharedRunOptions {
        trigger: RunTrigger::Manual,
        dry_run_scenario: None,
    }
}

#[tokio::test]
async fn a_live_run_by_hand_of_a_shared_job_with_a_standing_successor_is_refused_naming_the_successor()
-> Result<(), BoxError> {
    let fixture = fixture_with_registered_successor().await?;

    let refused = require_runnable(&fixture.state(), &key(), &live()).await;

    let Err(CliSharedError::ChainHeld(hold)) = &refused else {
        return Err(format!("expected the chain to refuse the run: {refused:?}").into());
    };
    assert_eq!(hold.job, "nightly-v2");
    assert_eq!(hold.relation, ChainRelation::Successor);
    let message = refused.err().map(|error| error.to_string()).unwrap_or_default();
    assert!(message.starts_with("workflow/job-not-runnable"), "{message}");
    assert!(message.contains("nightly-v2"), "{message}");
    Ok(())
}

#[tokio::test]
async fn a_live_run_by_hand_of_a_shared_successor_is_refused_while_its_predecessor_is_enabled() -> Result<(), BoxError>
{
    let fixture = fixture_with_registered_successor().await?;

    let refused = require_runnable(&fixture.state(), &successor_key(), &live()).await;

    let Err(CliSharedError::ChainHeld(hold)) = &refused else {
        return Err(format!("expected the chain to refuse the run: {refused:?}").into());
    };
    assert_eq!(hold.job, "nightly");
    assert_eq!(hold.relation, ChainRelation::Predecessor(JobLifecycle::Enabled));
    Ok(())
}

#[tokio::test]
async fn a_dry_run_of_a_shared_successor_is_not_refused_by_the_chain() -> Result<(), BoxError> {
    let fixture = fixture_with_registered_successor().await?;
    let dry = SharedRunOptions {
        trigger: RunTrigger::Manual,
        dry_run_scenario: Some(None),
    };

    require_runnable(&fixture.state(), &successor_key(), &dry).await?;
    Ok(())
}

#[tokio::test]
async fn enabling_a_shared_successor_is_refused_as_busy_while_its_predecessor_holds_its_run_claim()
-> Result<(), BoxError> {
    let fixture = fixture_with_registered_successor().await?;
    let claim = SharedClaim::acquire(&fixture.claims_root(), &key())?;

    let refused = ask(&fixture, &successor_key(), OperatorRequest::Enable).await;

    let Err(error) = refused else {
        return Err("the enabling went ahead under the predecessor's claim".into());
    };
    assert!(error.to_string().contains("workflow/job-busy"), "{error}");
    let state = fixture.read()?;
    assert_eq!(state.lifecycle_of("acme", "sync", "nightly"), JobLifecycle::Enabled);
    assert_eq!(
        state.lifecycle_of("acme", "sync", "nightly-v2"),
        JobLifecycle::Registered
    );

    claim.release().await?;
    ask(&fixture, &successor_key(), OperatorRequest::Enable).await?;
    assert_eq!(
        fixture.read()?.lifecycle_of("acme", "sync", "nightly"),
        JobLifecycle::Retired
    );
    Ok(())
}

#[tokio::test]
async fn enabling_a_shared_successor_reports_the_predecessor_retired_and_how_current_and_previous_moved()
-> Result<(), BoxError> {
    let fixture = fixture_with_registered_successor().await?;

    let outcome = ask(&fixture, &successor_key(), OperatorRequest::Enable).await?;

    let OperationOutcome::Enabled(report) = outcome else {
        return Err(format!("expected an enabling report: {outcome:?}").into());
    };
    assert_eq!(report.schedule, "30 4 * * *");
    assert_eq!(report.retired.as_deref(), Some("nightly"));
    assert_eq!(
        report.current.map(|moved| (moved.from, moved.to)),
        Some((Some("1.0.0".to_owned()), Some("2.0.0".to_owned())))
    );
    Ok(())
}
