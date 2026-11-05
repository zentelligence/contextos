//! What events on a shared (host) release do to a shared job: an incomplete release under an enabled job, a release that
//! stops being current, and a direct edit of `version` or `predecessor`, against a real package store, real job files,
//! the real host state file, and a real `contextos-mcp` session.

mod support;

use contextos_web::job_operation::{OperationError, OperationOutcome, OperatorRequest, perform};
use contextos_web::job_operation_shared::SharedJobStore;
use contextos_web::shared_claim::SharedJobKey;
use contextos_web::shared_validation::SharedValidationHost;
use contextos_workflows::{Actor, JobSchedule, JobState, Suspension, SuspensionReason, Transition};
use support::BoxError;
use support::shared::{DIGEST, FIRST, OTHER_DIGEST, SharedFixture as Fixture, key, write};
use time::macros::datetime;

use contextos_web::run_settings::HostContext;
use contextos_web::shared_scheduler::{SharedSchedulerHost, poll_shared_once, start_due_shared_jobs};
use contextos_workflows::Clock;
use time::OffsetDateTime;

const READY: &str = "name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nready = true\n";
const ENABLED_AT: OffsetDateTime = datetime!(2026-10-05 02:00:00 UTC);

struct FixedClock(OffsetDateTime);

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        self.0
    }
}

/// Half a minute after a job scheduled for 03:00 daily fell due.
const DUE: FixedClock = FixedClock(datetime!(2026-10-05 03:00:30 UTC));

fn hosts(fixture: &Fixture) -> (HostContext, SharedSchedulerHost) {
    (
        HostContext {
            attention: contextos_web::job_attention::JobAttentionMemory::default(),
            log_level: None,
            claims_root: Some(fixture.claims_root()),
            clock: std::sync::Arc::new(contextos_web::vault_time::VaultTime::from(
                contextos_web::vault_time::VaultTimeInput {
                    clock: std::sync::Arc::new(contextos_workflows::SystemClock),
                    clients: std::sync::Arc::new(contextos_web::McpClientSet::default()),
                },
            )),
        },
        SharedSchedulerHost {
            store_root: fixture.store_root(),
            jobs_root: fixture.jobs_root(),
            claims_root: fixture.claims_root(),
            runs_root: fixture.runs_root(),
            state: fixture.state(),
            run_retention: time::Duration::days(30),
            owner: None,
            log_level: None,
        },
    )
}

fn run_records(fixture: &Fixture) -> usize {
    fn count(directory: &std::path::Path) -> usize {
        std::fs::read_dir(directory).map_or(0, |entries| {
            entries
                .flatten()
                .map(|entry| {
                    let path = entry.path();
                    if path.is_dir() {
                        count(&path)
                    } else {
                        usize::from(entry.file_name().to_string_lossy().starts_with("R-"))
                    }
                })
                .sum()
        })
    }
    count(&fixture.runs_root())
}

/// The failures the state records for the job's last validation, as `(code, artefact)`.
fn recorded_failures(fixture: &Fixture) -> Result<Vec<(String, String)>, BoxError> {
    let state = fixture.read()?;
    let validation = state
        .job("acme", "sync", "nightly")
        .and_then(JobState::validation)
        .ok_or("no validation was recorded")?;
    Ok(validation
        .failures
        .iter()
        .map(|failure| (failure.code.clone(), failure.artefact.clone()))
        .collect())
}

fn suspension_code(fixture: &Fixture) -> Result<String, BoxError> {
    let state = fixture.read()?;
    let record = state
        .job("acme", "sync", "nightly")
        .and_then(JobState::suspension)
        .ok_or("the job has no suspension record")?;
    Ok(record.code.clone())
}

#[tokio::test]
async fn an_incomplete_release_under_an_enabled_shared_job_refuses_scheduled_runs_and_leaves_the_stage()
-> Result<(), BoxError> {
    let fixture = Fixture::new(READY).await?;
    fixture.enable_on("0 3 * * *", ENABLED_AT).await?;
    let before = fixture.state_text()?;
    std::fs::remove_file(fixture.store_root().join("acme/1.0.0/sync/sync.rhai"))?;
    let (host, shared) = hosts(&fixture);

    let started = start_due_shared_jobs(&fixture.clients, &host, &shared, &DUE).await;
    let polled = poll_shared_once(&fixture.clients, &host, &shared, &DUE).await;

    assert_eq!(started.spawned, 0);
    assert_eq!(polled.spawned, 0);
    assert_eq!(run_records(&fixture), 0, "a refused run leaves no record");
    assert_eq!(
        fixture.lifecycle()?,
        "enabled",
        "an incomplete release says nothing about the job"
    );
    assert_eq!(
        fixture
            .read()?
            .job("acme", "sync", "nightly")
            .and_then(JobState::fingerprint),
        {
            let recorded: contextos_workflows::JobsState = toml::from_str(&before)?;
            recorded
                .job("acme", "sync", "nightly")
                .and_then(JobState::fingerprint)
                .cloned()
        }
        .as_ref(),
        "the recorded version is unchanged"
    );
    Ok(())
}

#[tokio::test]
async fn a_shared_draft_whose_release_has_become_previous_fails_validation_on_its_version() -> Result<(), BoxError> {
    let fixture = Fixture::new(READY).await?;
    fixture.install("1.0.1", OTHER_DIGEST, "log_info(\"b\");")?;
    fixture.record_releases(&[("current", "1.0.1"), ("previous", "1.0.0")])?;

    fixture.pass(FIRST).await?;

    assert_eq!(
        recorded_failures(&fixture)?,
        [("validation/artefact-invalid".to_owned(), "version".to_owned())]
    );
    assert_eq!(fixture.lifecycle()?, "draft");
    Ok(())
}

#[tokio::test]
async fn a_direct_edit_of_an_enabled_shared_jobs_version_suspends_it_as_validation_failed() -> Result<(), BoxError> {
    let fixture = Fixture::new(READY).await?;
    fixture.enable_on("0 3 * * *", ENABLED_AT).await?;
    fixture.install("1.0.1", OTHER_DIGEST, "log_info(\"b\");")?;
    fixture.record_releases(&[("current", "1.0.0"), ("future", "1.0.1")])?;
    fixture.name_release("1.0.1")?;

    fixture.pass(FIRST).await?;

    assert_eq!(fixture.lifecycle()?, "suspended");
    assert_eq!(suspension_code(&fixture)?, "workflow/validation-failed");
    Ok(())
}

#[tokio::test]
async fn a_direct_edit_adding_a_predecessor_to_an_enabled_shared_job_suspends_it_as_validation_failed()
-> Result<(), BoxError> {
    let fixture = Fixture::new(READY).await?;
    fixture.enable_on("0 3 * * *", ENABLED_AT).await?;
    write(&fixture.job_path(), &format!("{READY}predecessor = \"gone\"\n"))?;

    fixture.pass(FIRST).await?;

    assert_eq!(fixture.lifecycle()?, "suspended");
    assert_eq!(suspension_code(&fixture)?, "workflow/validation-failed");
    Ok(())
}

#[tokio::test]
async fn a_shared_draft_naming_a_predecessor_that_does_not_exist_fails_validation_on_its_predecessor()
-> Result<(), BoxError> {
    let fixture = Fixture::new(&format!("{READY}predecessor = \"gone\"\n")).await?;

    fixture.pass(FIRST).await?;

    assert_eq!(
        recorded_failures(&fixture)?,
        [("validation/artefact-invalid".to_owned(), "predecessor".to_owned())]
    );
    Ok(())
}

#[tokio::test]
async fn installing_another_release_leaves_an_enabled_shared_job_on_the_release_it_names() -> Result<(), BoxError> {
    let fixture = Fixture::new(READY).await?;
    fixture.enable_on("0 3 * * *", ENABLED_AT).await?;
    let before = fixture.read()?;
    fixture.install("1.0.1", DIGEST, "log_info(\"b\");")?;

    fixture.pass(FIRST).await?;

    assert_eq!(fixture.lifecycle()?, "enabled");
    assert_eq!(
        fixture
            .read()?
            .job("acme", "sync", "nightly")
            .and_then(JobState::fingerprint),
        before.job("acme", "sync", "nightly").and_then(JobState::fingerprint)
    );
    Ok(())
}

async fn ask(
    fixture: &Fixture,
    job: &SharedJobKey,
    request: OperatorRequest,
) -> Result<OperationOutcome, OperationError> {
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
    perform(&store, request, FIRST).await
}

fn suspension() -> Result<Transition, BoxError> {
    Ok(Transition::Suspend(Suspension::operator(SuspensionReason::try_from(
        "stopped by the operator",
    )?)))
}

/// Takes `job` (a job file already written) to `enabled` through the real pass and transitions.
async fn enable_job(fixture: &Fixture, job: &SharedJobKey) -> Result<(), BoxError> {
    fixture.pass(FIRST).await?;
    let recorded = |fixture: &Fixture| -> Result<_, BoxError> {
        fixture
            .read()?
            .job("acme", "sync", &job.job)
            .and_then(|state| state.fingerprint().cloned())
            .ok_or_else(|| "the job was not validated".into())
    };
    let version = recorded(fixture)?;
    fixture
        .change_job(job, Actor::DryRun, Transition::DryRunCompleted(version), FIRST)
        .await?;
    let version = recorded(fixture)?;
    fixture
        .change_job(
            job,
            Actor::Operator,
            Transition::Enable(JobSchedule::try_from("0 3 * * *")?, version),
            ENABLED_AT,
        )
        .await
}

/// The release the state records for the fixture's job.
fn recorded_release(fixture: &Fixture) -> Result<Option<semver::Version>, BoxError> {
    Ok(fixture
        .read()?
        .job("acme", "sync", "nightly")
        .and_then(JobState::release)
        .cloned())
}

/// A suspended shared job enabled on 1.0.0, with 1.0.1 installed as well.
async fn suspended_job() -> Result<Fixture, BoxError> {
    let fixture = Fixture::new(READY).await?;
    fixture.enable_on("0 3 * * *", ENABLED_AT).await?;
    fixture.change(Actor::Operator, suspension()?, FIRST).await?;
    fixture.install("1.0.1", OTHER_DIGEST, "log_info(\"b\");")?;
    assert_eq!(fixture.lifecycle()?, "suspended");
    Ok(fixture)
}

fn failed_on(fixture: &Fixture, artefact: &str) -> Result<bool, BoxError> {
    Ok(recorded_failures(fixture)?.contains(&("validation/artefact-invalid".to_owned(), artefact.to_owned())))
}

#[tokio::test]
async fn resuming_a_suspended_shared_job_whose_version_was_edited_to_a_retired_release_is_refused_and_it_stays_suspended()
-> Result<(), BoxError> {
    let fixture = suspended_job().await?;
    fixture.record_releases(&[("current", "1.0.0")])?;
    fixture.retire_release("1.0.1")?;
    fixture.name_release("1.0.1")?;

    let refused = ask(&fixture, &key(), OperatorRequest::Resume).await;

    assert!(refused.is_err(), "resume must fail validation: {refused:?}");
    assert!(failed_on(&fixture, "version")?, "{:?}", recorded_failures(&fixture)?);
    assert_eq!(fixture.lifecycle()?, "suspended");
    assert_eq!(recorded_release(&fixture)?, Some(semver::Version::new(1, 0, 0)));
    Ok(())
}

#[tokio::test]
async fn resuming_a_suspended_shared_job_whose_version_was_edited_to_a_future_release_is_refused_and_it_stays_suspended()
-> Result<(), BoxError> {
    let fixture = suspended_job().await?;
    fixture.record_releases(&[("current", "1.0.0"), ("future", "1.0.1")])?;
    fixture.name_release("1.0.1")?;

    let refused = ask(&fixture, &key(), OperatorRequest::Resume).await;

    assert!(refused.is_err(), "resume must fail validation: {refused:?}");
    assert!(failed_on(&fixture, "version")?, "{:?}", recorded_failures(&fixture)?);
    assert_eq!(fixture.lifecycle()?, "suspended");
    assert_eq!(recorded_release(&fixture)?, Some(semver::Version::new(1, 0, 0)));

    fixture.name_release("1.0.0")?;
    // Restoring the file names the release it was enabled on again.
    let text = std::fs::read_to_string(fixture.job_path())?;
    assert!(text.contains("version = \"1.0.0\""), "{text}");
    ask(&fixture, &key(), OperatorRequest::Resume).await?;
    assert_eq!(
        fixture.lifecycle()?,
        "enabled",
        "restoring the file lets the job resume"
    );
    Ok(())
}

#[tokio::test]
async fn resuming_a_suspended_shared_job_whose_predecessor_was_edited_in_is_refused_and_restoring_the_file_lets_it_resume()
-> Result<(), BoxError> {
    let fixture = Fixture::new(READY).await?;
    let other = SharedJobKey {
        job: "other".to_owned(),
        ..key()
    };
    write(&fixture.jobs_root().join("acme/sync/jobs/other.toml"), READY)?;
    enable_job(&fixture, &key()).await?;
    enable_job(&fixture, &other).await?;
    fixture.change(Actor::Operator, suspension()?, FIRST).await?;
    assert_eq!(fixture.lifecycle()?, "suspended");
    write(&fixture.job_path(), &format!("{READY}predecessor = \"other\"\n"))?;

    let refused = ask(&fixture, &key(), OperatorRequest::Resume).await;

    assert!(refused.is_err(), "resume must fail validation: {refused:?}");
    assert!(
        failed_on(&fixture, "predecessor")?,
        "{:?}",
        recorded_failures(&fixture)?
    );
    assert_eq!(fixture.lifecycle()?, "suspended");
    assert_eq!(recorded_release(&fixture)?, Some(semver::Version::new(1, 0, 0)));

    write(&fixture.job_path(), READY)?;
    ask(&fixture, &key(), OperatorRequest::Resume).await?;
    assert_eq!(
        fixture.lifecycle()?,
        "enabled",
        "restoring the file lets the job resume"
    );
    Ok(())
}

#[tokio::test]
async fn resuming_a_suspended_shared_job_whose_file_is_unedited_enables_it_on_the_release_it_was_enabled_on()
-> Result<(), BoxError> {
    let fixture = suspended_job().await?;

    ask(&fixture, &key(), OperatorRequest::Resume).await?;

    assert_eq!(fixture.lifecycle()?, "enabled");
    assert_eq!(recorded_release(&fixture)?, Some(semver::Version::new(1, 0, 0)));
    Ok(())
}

/// A retired shared job that was enabled on 1.0.0, with 1.0.1 installed as well.
async fn retired_job() -> Result<Fixture, BoxError> {
    let fixture = Fixture::new(READY).await?;
    fixture.enable().await?;
    ask(&fixture, &key(), OperatorRequest::Retire).await?;
    fixture.install("1.0.1", OTHER_DIGEST, "log_info(\"b\");")?;
    assert_eq!(fixture.lifecycle()?, "retired");
    Ok(fixture)
}

#[tokio::test]
async fn re_activating_a_retired_shared_job_whose_version_was_edited_to_a_retired_release_is_refused()
-> Result<(), BoxError> {
    let fixture = retired_job().await?;
    fixture.record_releases(&[("current", "1.0.0")])?;
    fixture.retire_release("1.0.1")?;
    fixture.name_release("1.0.1")?;
    let before = fixture.read()?;

    let refused = ask(&fixture, &key(), OperatorRequest::Reactivate).await;

    assert!(refused.is_err(), "re-activation must fail: {refused:?}");
    assert_eq!(fixture.lifecycle()?, "retired");
    assert_eq!(recorded_release(&fixture)?, Some(semver::Version::new(1, 0, 0)));
    assert_eq!(
        fixture
            .read()?
            .package_releases("acme")
            .and_then(|held| held.workflows.get("sync"))
            .map(|held| (held.current.clone(), held.previous.clone())),
        before
            .package_releases("acme")
            .and_then(|held| held.workflows.get("sync"))
            .map(|held| (held.current.clone(), held.previous.clone()))
    );
    Ok(())
}

#[tokio::test]
async fn re_activating_a_retired_shared_job_whose_version_was_edited_to_a_future_release_is_refused_and_restoring_the_file_allows_it()
-> Result<(), BoxError> {
    let fixture = retired_job().await?;
    fixture.record_releases(&[("current", "1.0.0"), ("future", "1.0.1")])?;
    fixture.name_release("1.0.1")?;

    let refused = ask(&fixture, &key(), OperatorRequest::Reactivate).await;

    assert!(refused.is_err(), "re-activation must fail: {refused:?}");
    assert_eq!(fixture.lifecycle()?, "retired");
    assert_eq!(recorded_release(&fixture)?, Some(semver::Version::new(1, 0, 0)));

    fixture.name_release("1.0.0")?;
    ask(&fixture, &key(), OperatorRequest::Reactivate).await?;
    assert_eq!(fixture.lifecycle()?, "draft", "restoring the file allows re-activation");
    Ok(())
}

#[tokio::test]
async fn an_enabled_shared_jobs_version_edited_to_a_release_that_is_not_installed_is_suspended_as_validation_failed()
-> Result<(), BoxError> {
    let fixture = Fixture::new(READY).await?;
    fixture.enable_on("0 3 * * *", ENABLED_AT).await?;
    fixture.name_release("9.9.9")?;

    fixture.pass(FIRST).await?;

    assert_eq!(fixture.lifecycle()?, "suspended");
    assert_eq!(suspension_code(&fixture)?, "workflow/validation-failed");
    Ok(())
}
