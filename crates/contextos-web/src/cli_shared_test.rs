use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use contextos_workflows::{
    Actor, JobLifecycle, JobSchedule, JobsState, McpCallFailure, RunHeader, RunStatus, SuspensionReason, Transition,
};
use serde_json::{Map, Value};
use tempfile::tempdir;
use time::OffsetDateTime;
use time::macros::datetime;
use tokio_util::sync::CancellationToken;

use super::*;

/// A clock fixed at one instant, so a record's times and the retention window are exact.
struct FixedClock(OffsetDateTime);

impl contextos_workflows::Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        self.0
    }
}

/// Answers every call at once.

#[derive(Default)]
struct Recorder {
    calls: Mutex<usize>,
}

impl McpCaller for Recorder {
    fn call<'a>(
        &'a self,
        _server: &'a str,
        _tool: &'a str,
        _arguments: Map<String, Value>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, McpCallFailure>> + Send + 'a>> {
        Box::pin(async move {
            if let Ok(mut calls) = self.calls.lock() {
                *calls += 1;
            }
            Ok(Value::Null)
        })
    }
}

/// A call that has been issued and never answers, so a test can cancel the run while it is in flight.
struct NeverAnswers {
    issued: Arc<tokio::sync::Notify>,
}

impl McpCaller for NeverAnswers {
    fn call<'a>(
        &'a self,
        _server: &'a str,
        _tool: &'a str,
        _arguments: Map<String, Value>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, McpCallFailure>> + Send + 'a>> {
        self.issued.notify_one();
        Box::pin(std::future::pending())
    }
}

struct Host {
    store: tempfile::TempDir,
    jobs: tempfile::TempDir,
    claims: tempfile::TempDir,
    runs: tempfile::TempDir,
    state: SharedStateStore,
    _state_dir: tempfile::TempDir,
    /// Whether a job added with [`Host::add_job`] is taken to `registered`, so a run by hand is allowed.
    register_jobs: bool,
    recorder: Arc<Recorder>,
    caller: Arc<dyn McpCaller>,
    owner: Option<&'static str>,
    cancel: CancellationToken,
    now: OffsetDateTime,
}

impl Host {
    fn new(manifest_extra: &str, script: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let store = tempdir()?;
        let dir = store.path().join("acme").join("1.0.0");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(
            dir.join("manifest.toml"),
            format!(
                "name = \"acme\"\nversion = \"1.0.0\"\ndigest = \"sha256:{}\"\n\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n{manifest_extra}",
                "a".repeat(64)
            ),
        )?;
        std::fs::write(dir.join("sync.rhai"), script)?;
        crate::release_fixture::seal(&dir)?;
        let recorder = Arc::new(Recorder::default());
        let state_dir = tempdir()?;
        Ok(Self {
            store,
            jobs: tempdir()?,
            claims: tempdir()?,
            runs: tempdir()?,
            state: SharedStateStore::new(state_dir.path().join("state")),
            _state_dir: state_dir,
            register_jobs: true,
            caller: Arc::clone(&recorder) as Arc<dyn McpCaller>,
            recorder,
            owner: None,
            cancel: CancellationToken::new(),
            now: datetime!(2026-10-01 03:00:00 UTC),
        })
    }

    /// The digest the installed release declares, which covers every file of it.
    fn release_digest(&self) -> Result<String, Box<dyn std::error::Error>> {
        let manifest: toml::Table = toml::from_str(&std::fs::read_to_string(
            self.store.path().join("acme/1.0.0/manifest.toml"),
        )?)?;
        Ok(manifest
            .get("digest")
            .and_then(toml::Value::as_str)
            .ok_or("the release declares no digest")?
            .to_owned())
    }

    fn host(&self, clients: Arc<McpClientSet>) -> SharedHost<'_> {
        SharedHost {
            clients,
            primary_server: "contextos",
            store_root: self.store.path(),
            jobs_root: self.jobs.path(),
            claims_root: self.claims.path(),
            runs_root: self.runs.path(),
            state: &self.state,
            run_retention: time::Duration::days(30),
            clock: Arc::new(FixedClock(self.now)),
            caller: Arc::clone(&self.caller),
            owner: self.owner,
            log_level: None,
            cancel: self.cancel.clone(),
        }
    }

    fn record(&self, id: u64) -> Result<String, std::io::Error> {
        std::fs::read_to_string(self.runs.path().join(format!("acme/sync/nightly/2026/10/R-{id}.md")))
    }

    fn key() -> SharedJobKey {
        SharedJobKey {
            package: "acme".to_owned(),
            workflow: "sync".to_owned(),
            job: "nightly".to_owned(),
        }
    }

    fn add_job(&self, text: &str) -> Result<(), Box<dyn std::error::Error>> {
        let dir = self.jobs.path().join("acme").join("sync").join("jobs");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("nightly.toml"), text)?;
        if self.register_jobs {
            self.bring_to(Stage::Registered)?;
        }
        Ok(())
    }

    /// Edits the job file to name `release`, as a change made outside the web interface would, so the job runs a version
    /// nobody has reviewed. The authoring commands refuse this for a standing job.
    fn name_release(&self, release: &str) -> Result<(), Box<dyn std::error::Error>> {
        let path = self.jobs.path().join("acme/sync/jobs/nightly.toml");
        let text = std::fs::read_to_string(&path)?;
        std::fs::write(
            path,
            text.replace("version = \"1.0.0\"", &format!("version = \"{release}\"")),
        )?;
        Ok(())
    }

    /// The job's version as its files stand now.
    fn version(&self) -> Result<contextos_workflows::JobFingerprint, Box<dyn std::error::Error>> {
        let text = std::fs::read_to_string(self.jobs.path().join("acme/sync/jobs/nightly.toml"))?;
        crate::shared_version::shared_job_version(self.store.path(), "acme", &text)
            .ok_or_else(|| "the job has no version".into())
    }

    /// Takes the job through the real transitions to `stage`, for the version it has now.
    fn bring_to(&self, stage: Stage) -> Result<(), Box<dyn std::error::Error>> {
        let version = self.version()?;
        let now = self.now;
        self.state
            .update(|jobs: &mut JobsState| -> Result<(), Box<dyn std::error::Error>> {
                let key = ("acme", "sync", "nightly");
                let mut step = |actor, transition: Transition| {
                    jobs.transition(key.0, key.1, key.2, actor, &transition, now)
                        .map(|_| ())
                };
                step(Actor::Validator, Transition::Validated(version.clone()))?;
                if stage == Stage::Validated {
                    return Ok(());
                }
                step(Actor::DryRun, Transition::DryRunCompleted(version.clone()))?;
                if stage == Stage::Registered {
                    return Ok(());
                }
                step(
                    Actor::Operator,
                    Transition::Enable(JobSchedule::try_from("0 3 * * *")?, version.clone()),
                )?;
                if stage == Stage::EnabledProven {
                    step(Actor::System, Transition::ProbationPassed(version.clone()))?;
                }
                if stage == Stage::Suspended {
                    step(
                        Actor::Operator,
                        Transition::Suspend(contextos_workflows::Suspension::operator(SuspensionReason::try_from(
                            "stopped by the operator",
                        )?)),
                    )?;
                }
                if stage == Stage::Retired {
                    step(Actor::Operator, Transition::Retire)?;
                }
                Ok(())
            })??;
        Ok(())
    }

    fn lifecycle(&self) -> Result<JobLifecycle, Box<dyn std::error::Error>> {
        Ok(self.state.read()?.lifecycle_of("acme", "sync", "nightly"))
    }

    fn on_trial(&self) -> Result<bool, Box<dyn std::error::Error>> {
        Ok(self
            .state
            .read()?
            .job("acme", "sync", "nightly")
            .is_some_and(contextos_workflows::JobState::first_run_pending))
    }

    /// Removes the package, as an uninstall would, so a job that resolved it no longer does.
    fn uninstall(&self) -> Result<(), std::io::Error> {
        std::fs::remove_dir_all(self.store.path().join("acme"))
    }

    /// Ships `text` as the package's scenario `name` for the workflow.
    fn ship_scenario(&self, name: &str, text: &str) -> Result<(), Box<dyn std::error::Error>> {
        let dir = self.store.path().join("acme/1.0.0/sync/tests");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join(format!("{name}.toml")), text)?;
        // A scenario is a file of the release, so it is part of what the release's digest covers.
        crate::release_fixture::seal(&self.store.path().join("acme/1.0.0"))?;
        Ok(())
    }

    fn calls(&self) -> usize {
        self.recorder.calls.lock().map(|calls| *calls).unwrap_or_default()
    }

    async fn run_job(&self) -> Result<RunOutcome, CliSharedError> {
        let clients = Arc::new(
            McpClientSet::connect(&[])
                .await
                .map_err(|_| CliSharedError::NoClients)?,
        );
        run_shared_job_reported(self.host(clients), Self::key(), SharedRunOptions::default(), Vec::new())
            .await
            .map(|report| report.outcome)
    }

    async fn run_with(&self, options: SharedRunOptions) -> Result<RunOutcome, CliSharedError> {
        let clients = Arc::new(
            McpClientSet::connect(&[])
                .await
                .map_err(|_| CliSharedError::NoClients)?,
        );
        run_shared_job_reported(self.host(clients), Self::key(), options, Vec::new())
            .await
            .map(|report| report.outcome)
    }

    async fn run_once_dry(&self, scenario: Option<&str>) -> Result<RunOutcome, CliSharedError> {
        let clients = Arc::new(
            McpClientSet::connect(&[])
                .await
                .map_err(|_| CliSharedError::NoClients)?,
        );
        run_shared_once_reported(
            self.host(clients),
            OneOffRequest {
                package: "acme".to_owned(),
                workflow: "sync".to_owned(),
                release: contextos_workflows::ReleaseChoice::Version(semver::Version::new(1, 0, 0)),
                arguments: Vec::new(),
                dry_run_scenario: Some(scenario.map(str::to_owned)),
            },
        )
        .await
        .map(|report| report.outcome)
    }

    async fn run_once(&self, arguments: &[&str], version: semver::Version) -> Result<RunOutcome, CliSharedError> {
        let clients = Arc::new(
            McpClientSet::connect(&[])
                .await
                .map_err(|_| CliSharedError::NoClients)?,
        );
        run_shared_once_reported(
            self.host(clients),
            OneOffRequest {
                package: "acme".to_owned(),
                workflow: "sync".to_owned(),
                release: contextos_workflows::ReleaseChoice::Version(version),
                arguments: arguments.iter().map(|text| (*text).to_owned()).collect(),
                dry_run_scenario: None,
            },
        )
        .await
        .map(|report| report.outcome)
    }
}

/// How far through the lifecycle a job is taken.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Stage {
    Validated,
    Registered,
    /// Enabled, so its first live run is on trial.
    Enabled,
    /// Enabled, and its first live run has passed.
    EnabledProven,
    Suspended,
    Retired,
}

const CALLING_SCRIPT: &str = r#"call_mcp("contextos", "vault_info", #{});"#;
const EFFECT: &str = "\n[[workflow.permitted_effects]]\nmcp_server_name = \"contextos\"\ntool = \"vault_info\"\n";

fn failed_with(outcome: &RunOutcome, kind: WorkflowFailureKind) -> bool {
    matches!(outcome, RunOutcome::Failed(failure) if failure.kind == Some(kind))
}

#[tokio::test]
async fn a_saved_shared_job_runs_and_releases_its_claim() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;

    let outcome = host.run_job().await?;

    assert!(matches!(outcome, RunOutcome::Succeeded), "{outcome:?}");
    assert_eq!(host.calls(), 1);
    assert!(
        SharedClaim::acquire(host.claims.path(), &Host::key()).is_ok(),
        "the claim is free once the run has ended"
    );
    Ok(())
}

#[tokio::test]
async fn a_second_run_of_a_job_whose_claim_is_held_is_refused_as_busy_before_any_call()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    let _held = SharedClaim::acquire(host.claims.path(), &Host::key())?;

    let outcome = host.run_job().await?;

    assert!(failed_with(&outcome, WorkflowFailureKind::JobBusy), "{outcome:?}");
    assert_eq!(host.calls(), 0);
    assert!(host.record(1).is_err(), "a run that never started leaves no record");
    Ok(())
}

#[tokio::test]
async fn a_claim_file_left_by_a_dead_run_does_not_refuse_the_next() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    std::fs::create_dir_all(host.claims.path().join("acme/sync"))?;
    std::fs::write(host.claims.path().join("acme/sync/nightly.lock"), "")?;

    let outcome = host.run_job().await?;

    assert!(matches!(outcome, RunOutcome::Succeeded), "{outcome:?}");
    assert_eq!(host.calls(), 1);
    Ok(())
}

#[tokio::test]
async fn a_job_naming_an_uninstalled_release_fails_as_package_unavailable_and_frees_its_claim()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    host.uninstall()?;

    let outcome = host.run_job().await?;

    assert!(
        failed_with(&outcome, WorkflowFailureKind::PackageUnavailable),
        "{outcome:?}"
    );
    assert_eq!(host.calls(), 0);
    assert!(!host.claims.path().join("acme/sync/nightly.lock").exists());
    Ok(())
}

#[tokio::test]
async fn a_job_with_a_missing_file_is_reported_as_an_error_not_a_run() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new(EFFECT, CALLING_SCRIPT)?;

    assert!(matches!(host.run_job().await, Err(CliSharedError::Job(_))));
    Ok(())
}

#[tokio::test]
async fn a_one_off_run_takes_no_claim_and_runs_beside_a_held_job_claim() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new(EFFECT, CALLING_SCRIPT)?;
    let _held = SharedClaim::acquire(host.claims.path(), &Host::key())?;

    let outcome = host.run_once(&[], semver::Version::new(1, 0, 0)).await?;

    assert!(matches!(outcome, RunOutcome::Succeeded), "{outcome:?}");
    assert_eq!(host.calls(), 1);
    Ok(())
}

#[tokio::test]
async fn a_one_off_run_with_an_invalid_input_ends_before_any_call() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new(
        &format!("{EFFECT}\n[[workflow.inputs]]\nname = \"since\"\nkind = \"date\"\n"),
        CALLING_SCRIPT,
    )?;

    let outcome = host
        .run_once(&["since=\"not a date\""], semver::Version::new(1, 0, 0))
        .await?;

    assert!(failed_with(&outcome, WorkflowFailureKind::InputInvalid), "{outcome:?}");
    assert_eq!(host.calls(), 0);
    Ok(())
}

#[tokio::test]
async fn a_job_naming_a_vault_while_the_primary_server_is_not_connected_fails_as_server_unavailable()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new(
        &format!("{EFFECT}\n[[workflow.inputs]]\nname = \"target\"\nkind = \"vault\"\n"),
        CALLING_SCRIPT,
    )?;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n[inputs]\ntarget = \"client-z\"\n")?;
    // The primary server is configured, but nothing answers at its endpoint.
    let clients = Arc::new(
        McpClientSet::connect(&[crate::config::McpServerConfig::Http {
            name: "contextos".to_owned(),
            endpoint: crate::test_support::unreachable_loopback_url()?,
            token_env: None,
            auth: None,
            headers: std::collections::BTreeMap::new(),
        }])
        .await?,
    );

    let outcome = run_shared_job_reported(host.host(clients), Host::key(), SharedRunOptions::default(), Vec::new())
        .await?
        .outcome;

    assert!(
        failed_with(&outcome, WorkflowFailureKind::ServerUnavailable),
        "the vault is not missing, the server is: {outcome:?}"
    );
    assert_eq!(host.calls(), 0);
    Ok(())
}

#[tokio::test]
async fn a_job_naming_a_vault_the_server_does_not_report_fails_before_any_claim_is_taken()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new(
        &format!("{EFFECT}\n[[workflow.inputs]]\nname = \"target\"\nkind = \"vault\"\n"),
        CALLING_SCRIPT,
    )?;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n[inputs]\ntarget = \"client-z\"\n")?;

    let outcome = host.run_job().await?;

    assert!(
        failed_with(&outcome, WorkflowFailureKind::VaultUnavailable),
        "{outcome:?}"
    );
    assert_eq!(host.calls(), 0);
    assert!(!host.claims.path().join("acme/sync/nightly.lock").exists());
    Ok(())
}

#[tokio::test]
async fn a_failed_saved_job_has_no_owner_text_without_an_owner_table() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    host.uninstall()?;

    let outcome = host.run_job().await?;

    assert!(
        matches!(&outcome, RunOutcome::Failed(failure) if !failure.message.contains("owner")),
        "{outcome:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_successful_saved_job_reports_no_owner() -> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.owner = Some("Piper");
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;

    assert!(matches!(host.run_job().await?, RunOutcome::Succeeded));
    Ok(())
}

#[tokio::test]
async fn a_saved_job_leaves_a_record_of_its_run_and_its_calls() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new(
        &format!("{EFFECT}\n[[workflow.inputs]]\nname = \"note\"\nkind = \"string\"\n"),
        CALLING_SCRIPT,
    )?;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n[inputs]\nnote = \"s3cret-value\"\n")?;

    host.run_job().await?;

    let record = host.record(1)?;
    let header = RunHeader::try_from(record.as_str())?;
    assert_eq!(header.status, RunStatus::Succeeded);
    assert!(
        record.contains("| 1 | contextos | vault_info | succeeded |"),
        "{record}"
    );
    assert!(record.contains("inputs: [\"note\"]"), "{record}");
    assert!(
        !record.contains("s3cret-value"),
        "an input is named, never valued:\n{record}"
    );
    Ok(())
}

#[tokio::test]
async fn a_failed_run_is_recorded_failed_with_its_error_and_the_calls_it_made() -> Result<(), Box<dyn std::error::Error>>
{
    let host = Host::new(EFFECT, r#"call_mcp("contextos", "vault_info", #{}); throw "boom";"#)?;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;

    let outcome = host.run_job().await?;

    assert!(matches!(outcome, RunOutcome::Failed(_)), "{outcome:?}");
    let record = host.record(1)?;
    assert!(record.contains("status: failed\n"), "{record}");
    assert!(record.contains("boom"), "{record}");
    assert!(
        record.contains("| 1 | contextos | vault_info | succeeded |"),
        "{record}"
    );
    Ok(())
}

#[tokio::test]
async fn a_refused_call_is_recorded_as_refused() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new(EFFECT, r#"call_mcp("contextos", "fs_delete_file", #{});"#)?;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;

    host.run_job().await?;

    assert!(
        host.record(1)?
            .contains("| 1 | contextos | fs_delete_file | refused: workflow/effect-denied |")
    );
    assert_eq!(host.calls(), 0, "the refused call never reached the server");
    Ok(())
}

#[tokio::test]
async fn a_one_off_run_writes_no_record() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new(EFFECT, CALLING_SCRIPT)?;

    host.run_once(&[], semver::Version::new(1, 0, 0)).await?;

    assert_eq!(std::fs::read_dir(host.runs.path())?.count(), 0);
    Ok(())
}

#[tokio::test]
async fn a_run_that_cannot_be_recorded_still_runs() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    // The runs root is a file, so no record folder can be made under it.
    let unusable = host.runs.path().join("not-a-folder");
    std::fs::write(&unusable, "")?;
    let clients = Arc::new(
        McpClientSet::connect(&[])
            .await
            .map_err(|_| CliSharedError::NoClients)?,
    );
    let mut shared = host.host(clients);
    shared.runs_root = &unusable;

    let outcome = run_shared_job_reported(shared, Host::key(), SharedRunOptions::default(), Vec::new())
        .await?
        .outcome;

    assert!(matches!(outcome, RunOutcome::Succeeded), "{outcome:?}");
    assert_eq!(host.calls(), 1);
    Ok(())
}

#[tokio::test]
async fn the_next_run_marks_a_record_its_dead_predecessor_left_running_as_interrupted()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    host.run_job().await?;
    // Put the first run's record back as it would be had its process been killed mid-run.
    let path = host.runs.path().join("acme/sync/nightly/2026/10/R-1.md");
    let running = std::fs::read_to_string(&path)?
        .replace("status: succeeded", "status: running")
        .replace("completed: 2026-10-01T03:00:00Z", "completed:");
    std::fs::write(&path, running)?;

    host.run_job().await?;

    let first = host.record(1)?;
    assert!(first.contains("status: interrupted\n"), "{first}");
    assert!(host.record(2)?.contains("status: succeeded\n"));
    Ok(())
}

#[tokio::test]
async fn a_finished_run_sweeps_its_jobs_records_older_than_the_window() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    host.run_job().await?;
    let mut later = Host::new(EFFECT, CALLING_SCRIPT)?;
    later.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    later.runs = host.runs;
    later.now = datetime!(2026-11-15 03:00:00 UTC);

    later.run_job().await?;

    assert!(
        !later.runs.path().join("acme/sync/nightly/2026/10/R-1.md").exists(),
        "a record 45 days old is past the 30 day window"
    );
    assert!(later.runs.path().join("acme/sync/nightly/2026/11/R-2.md").exists());
    Ok(())
}

#[tokio::test]
async fn a_run_cancelled_before_it_starts_is_recorded_cancelled_and_frees_its_claim()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    host.cancel.cancel();

    let outcome = host.run_job().await?;

    assert_eq!(outcome, RunOutcome::Cancelled);
    assert_eq!(host.calls(), 0);
    assert!(host.record(1)?.contains("status: cancelled\n"));
    assert!(SharedClaim::acquire(host.claims.path(), &Host::key()).is_ok());
    Ok(())
}

#[tokio::test]
async fn cancelling_a_run_with_a_call_in_flight_records_that_call_as_outcome_unknown()
-> Result<(), Box<dyn std::error::Error>> {
    let issued = Arc::new(tokio::sync::Notify::new());
    let mut host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.caller = Arc::new(NeverAnswers {
        issued: Arc::clone(&issued),
    });
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    let cancel = host.cancel.clone();
    let canceller = tokio::spawn(async move {
        issued.notified().await;
        cancel.cancel();
    });

    // A bound that only turns a run that was never interrupted into a failure instead of a hang.
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(30), host.run_job())
        .await
        .map_err(|_| "the run was never interrupted")??;
    canceller.await?;

    assert_eq!(outcome, RunOutcome::Cancelled);
    let record = host.record(1)?;
    assert!(record.contains("status: cancelled\n"), "{record}");
    assert!(
        record.contains("| 1 | contextos | vault_info | abandoned in flight, outcome unknown |"),
        "{record}"
    );
    assert!(SharedClaim::acquire(host.claims.path(), &Host::key()).is_ok());
    Ok(())
}

#[tokio::test]
async fn a_run_asked_to_stop_beside_its_claim_ends_cancelled_and_frees_its_claim()
-> Result<(), Box<dyn std::error::Error>> {
    let issued = Arc::new(tokio::sync::Notify::new());
    let mut host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.caller = Arc::new(NeverAnswers {
        issued: Arc::clone(&issued),
    });
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    let slot = Host::key().claim_slot(host.claims.path())?;
    let asker = {
        let slot = slot.clone();
        tokio::spawn(async move {
            // A call in flight means the run has said which run it is.
            issued.notified().await;
            match slot.state() {
                Ok(crate::run_claim::ClaimState::Held(Some(active))) => {
                    slot.request_cancel(active.run_id).map_err(|error| error.to_string())
                }
                other => Err(format!("the run was not active: {other:?}")),
            }
        })
    };

    // A bound that only turns a run that was never asked to stop into a failure instead of a hang.
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(30), host.run_job())
        .await
        .map_err(|_| "the run was never asked to stop")?;
    asker.await??;

    assert_eq!(outcome?, RunOutcome::Cancelled);
    let record = host.record(1)?;
    assert!(record.contains("status: cancelled\n"), "{record}");
    assert_eq!(slot.state()?, crate::run_claim::ClaimState::Idle);
    Ok(())
}

/// Answers every call with a secret, so a test can check that no result ever reaches a record.
struct AnswersWithASecret;

impl McpCaller for AnswersWithASecret {
    fn call<'a>(
        &'a self,
        _server: &'a str,
        _tool: &'a str,
        _arguments: Map<String, Value>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, McpCallFailure>> + Send + 'a>> {
        Box::pin(async { Ok(Value::String("s3cret-result".to_owned())) })
    }
}

#[tokio::test]
async fn neither_a_calls_arguments_nor_its_result_reach_the_record() -> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(
        EFFECT,
        r#"call_mcp("contextos", "vault_info", #{ token: "s3cret-argument" });"#,
    )?;
    host.caller = Arc::new(AnswersWithASecret);
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;

    host.run_job().await?;

    let record = host.record(1)?;
    assert!(
        record.contains("| 1 | contextos | vault_info | succeeded |"),
        "{record}"
    );
    assert!(
        !record.contains("s3cret"),
        "no argument or result is recorded:\n{record}"
    );
    Ok(())
}

#[tokio::test]
async fn a_failure_message_is_recorded_cut_so_the_record_stays_readable() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new(EFFECT, r#"throw "x".repeat(50000);"#)?;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;

    host.run_job().await?;

    let record = host.record(1)?;
    assert!(record.len() < 8_000, "{} bytes", record.len());
    assert!(record.contains("status: failed\n"));
    Ok(())
}

/// A caller that fails the way a bug in a host would: it panics instead of answering.
struct PanicsOnCall;

impl McpCaller for PanicsOnCall {
    fn call<'a>(
        &'a self,
        _server: &'a str,
        _tool: &'a str,
        _arguments: Map<String, Value>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, McpCallFailure>> + Send + 'a>> {
        std::panic::resume_unwind(Box::new("the caller panicked"))
    }
}

fn claim_is_free(host: &Host) -> bool {
    SharedClaim::acquire(host.claims.path(), &Host::key()).is_ok()
}

#[tokio::test]
async fn a_call_stuck_on_a_hung_server_ends_the_run_at_its_timeout_and_is_recorded()
-> Result<(), Box<dyn std::error::Error>> {
    let issued = Arc::new(tokio::sync::Notify::new());
    let mut host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.caller = Arc::new(NeverAnswers { issued });
    host.add_job("name = \"Nightly\"\ntimeout_secs = 1\nversion = \"1.0.0\"\n")?;

    // A bound that only turns a call the timeout never abandoned into a failure instead of a hang.
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(60), host.run_job())
        .await
        .map_err(|_| "the timeout never ended the run")??;

    assert!(failed_with(&outcome, WorkflowFailureKind::TimedOut), "{outcome:?}");
    let record = host.record(1)?;
    assert!(record.contains("status: failed\n"), "{record}");
    assert!(record.contains("error: \"workflow/timed-out"), "{record}");
    assert!(
        record.contains("| 1 | contextos | vault_info | abandoned in flight, outcome unknown |"),
        "{record}"
    );
    assert!(claim_is_free(&host), "the claim is free once the run timed out");
    Ok(())
}

#[tokio::test]
async fn a_script_that_never_returns_ends_at_its_timeout_with_its_claim_freed() -> Result<(), Box<dyn std::error::Error>>
{
    let host = Host::new(EFFECT, "loop {}")?;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 1\nversion = \"1.0.0\"\n")?;

    let outcome = host.run_job().await?;

    assert!(failed_with(&outcome, WorkflowFailureKind::TimedOut), "{outcome:?}");
    assert!(host.record(1)?.contains("error: \"workflow/timed-out"));
    assert!(claim_is_free(&host));
    Ok(())
}

#[tokio::test]
async fn a_run_whose_caller_panics_is_recorded_failed_and_frees_its_claim() -> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.caller = Arc::new(PanicsOnCall);
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;

    let outcome = host.run_job().await?;

    assert!(matches!(outcome, RunOutcome::Failed(_)), "{outcome:?}");
    let record = host.record(1)?;
    assert!(record.contains("status: failed\n"), "{record}");
    assert!(
        record.contains("| 1 | contextos | vault_info | abandoned in flight, outcome unknown |"),
        "the call that panicked is logged with its outcome unknown:\n{record}"
    );
    assert!(claim_is_free(&host), "the claim is free once the run panicked");
    Ok(())
}

#[path = "cli_shared_lifecycle_test.rs"]
mod lifecycle;

#[path = "cli_shared_report_test.rs"]
mod report;

#[path = "cli_shared_release_race_test.rs"]
mod release_race;

#[path = "cli_shared_scheduled_snapshot_test.rs"]
mod scheduled_snapshot;

#[path = "cli_shared_once_test.rs"]
mod once;
