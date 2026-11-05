//! Probation against a real `contextos-mcp` session and a real temporary vault: a new version of an
//! enabled job is validated at once and keeps its schedule with its first live run on trial, and how that run ends
//! decides whether the job carries on or is suspended.

mod support;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use contextos_web::McpClientSet;
use contextos_web::cli_run::{CliRunError, CliRunOptions};
use contextos_web::discover_jobs;
use contextos_web::job_lifecycle::{LifecycleChange, change_lifecycle};
use contextos_web::job_probation::{Settlement, settle_first_run};
use contextos_web::job_validation::validation_pass;
use contextos_web::scheduler::{poll_once, start_due_jobs};
use contextos_workflows::{Actor, Clock, JobFingerprint, JobSchedule, JobState, JobsState, RunOutcome, Transition};
use time::OffsetDateTime;
use time::macros::datetime;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT_NAME: &str = "contract-fixture";
const WORKFLOW: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"contextos\"]\n";
const JOB: &str = "name = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";
/// A valid team: `operator` is the one chief of staff.
const TEAM: &str = "[[member]]\nslug = \"operator\"\nname = \"Operator\"\ntype = \"human\"\nchief_of_staff = true\n";
const NEXT_RELEASE: &str = "2.0.0";
const STATE_PATH: &str = ".contextos/jobs.toml";
const ENABLED_AT: OffsetDateTime = datetime!(2026-10-01 03:00:00 UTC);
const LATER: OffsetDateTime = datetime!(2026-10-02 03:00:00 UTC);
const WORKS: &str = "log_info(\"a\");";
const WORKS_DIFFERENTLY: &str = "log_info(\"b\");";
const BROKEN: &str = "let y = (2 +;";
const FAILS: &str = "throw \"the crm is down\";";
const DEGRADES: &str = "flag_degraded(\"a read was retried\");";

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

struct Fixture {
    vault: tempfile::TempDir,
    config: tempfile::TempDir,
    clients: Arc<McpClientSet>,
}

/// The version of the job once `script` is installed as the release its job file names.
fn version_of(script: &str) -> Result<JobFingerprint, BoxError> {
    version_from(script, JOB)
}

/// The version of the job once `script` is installed as a later release, which the job file names too.
fn next_version_of(script: &str) -> Result<JobFingerprint, BoxError> {
    version_from(script, &JOB.replace("1.0.0", NEXT_RELEASE))
}

fn version_from(script: &str, job_toml: &str) -> Result<JobFingerprint, BoxError> {
    support::release_fixture::version_of_source(WORKFLOW, "sync.rhai", script, job_toml)
        .map_err(|error| error.to_string().into())
}

async fn fixture(script: &str) -> Result<Fixture, BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    write(vault.path(), "index.md", "# Root\n")?;
    write(
        vault.path(),
        "registry/workflows/ops/workflows.toml",
        "workflows = [\"sync\"]\n",
    )?;
    write(vault.path(), "registry/workflows/ops/sync/workflow.toml", WORKFLOW)?;
    write(vault.path(), "registry/workflows/ops/sync/sync.rhai", script)?;
    write(vault.path(), "registry/workflows/ops/sync/jobs/nightly.toml", JOB)?;
    write(vault.path(), ".contextos/team.toml", TEAM)?;
    support::release_fixture::install_vault_releases(vault.path()).map_err(|error| error.to_string())?;
    let config_path = support::write_vault_config(config.path(), vault.path())?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    Ok(Fixture { vault, config, clients })
}

impl Fixture {
    async fn change(&self, actor: Actor, transition: Transition, now: OffsetDateTime) -> Result<(), BoxError> {
        let client = self.clients.get("contextos").ok_or("no primary server")?;
        change_lifecycle(
            &client,
            VAULT_NAME,
            LifecycleChange {
                package: "ops",
                workflow_slug: "sync",
                job_slug: "nightly",
                actor,
                transition,
                now,
            },
        )
        .await?;
        Ok(())
    }

    /// Takes the job through the real transitions to `registered` for the script it has now.
    async fn registered(&self, script: &str) -> Result<(), BoxError> {
        self.change(Actor::Validator, Transition::Validated(version_of(script)?), ENABLED_AT)
            .await?;
        self.change(
            Actor::DryRun,
            Transition::DryRunCompleted(version_of(script)?),
            ENABLED_AT,
        )
        .await
    }

    /// `registered`, then enabled by the operator, which puts its first live run on trial.
    async fn enabled(&self, script: &str) -> Result<(), BoxError> {
        self.registered(script).await?;
        self.change(
            Actor::Operator,
            Transition::Enable(JobSchedule::try_from("0 3 * * *")?, version_of(script)?),
            ENABLED_AT,
        )
        .await
    }

    /// An enabled job whose first live run has already passed, so its trial is over.
    async fn enabled_and_proven(&self, script: &str) -> Result<(), BoxError> {
        self.enabled(script).await?;
        self.change(
            Actor::System,
            Transition::ProbationPassed(version_of(script)?),
            ENABLED_AT,
        )
        .await
    }

    async fn pass(&self, now: OffsetDateTime) -> Result<(), BoxError> {
        let client = self.clients.get("contextos").ok_or("no primary server")?;
        validation_pass(&client, VAULT_NAME, &["contextos".to_owned()], now).await?;
        Ok(())
    }

    async fn live_run(&self) -> Result<RunOutcome, CliRunError> {
        support::run_job(
            Arc::clone(&self.clients),
            &support::host(self.config.path()),
            VAULT_NAME,
            "ops",
            "sync",
            "nightly",
            CliRunOptions {
                dry_run_scenario: None,
                record: true,
            },
        )
        .await
    }

    fn state(&self) -> Result<JobsState, BoxError> {
        Ok(toml::from_str(&std::fs::read_to_string(
            self.vault.path().join(STATE_PATH),
        )?)?)
    }

    fn lifecycle(&self) -> Result<&'static str, BoxError> {
        Ok(self.state()?.lifecycle_of("ops", "sync", "nightly").as_str())
    }

    fn on_trial(&self) -> Result<bool, BoxError> {
        Ok(self
            .state()?
            .job("ops", "sync", "nightly")
            .is_some_and(contextos_workflows::JobState::first_run_pending))
    }

    fn reason(&self) -> Result<Option<String>, BoxError> {
        Ok(self
            .state()?
            .job("ops", "sync", "nightly")
            .and_then(|job| job.suspension().map(|suspension| suspension.reason.clone())))
    }

    /// The stable code of the structured suspension record, which is stored with the reason and never parsed out of it.
    fn suspension_code(&self) -> Result<Option<String>, BoxError> {
        Ok(self
            .state()?
            .job("ops", "sync", "nightly")
            .and_then(|job| job.suspension().map(|suspension| suspension.code.clone())))
    }

    /// Settles `outcome` for a live run of `version` directly, so what the settlement reported can be seen.
    async fn settle(&self, version: Option<JobFingerprint>, outcome: &RunOutcome) -> Result<Settlement, BoxError> {
        let client = self.clients.get("contextos").ok_or("no primary server")?;
        let job = discover_jobs(&client, VAULT_NAME, &["contextos".to_owned()])
            .await?
            .into_iter()
            .find(|job| job.slug == "nightly")
            .ok_or("the job was not discovered")?;
        Ok(settle_first_run(&client, VAULT_NAME, &job, version, outcome, 1, LATER).await)
    }

    /// Makes every job file of the workflow name a release that is not installed.
    fn name_a_release_that_is_not_installed(&self) -> Result<(), BoxError> {
        self.name_release(NEXT_RELEASE)
    }

    fn name_release(&self, version: &str) -> Result<(), BoxError> {
        let jobs = self.vault.path().join("registry/workflows/ops/sync/jobs");
        for entry in std::fs::read_dir(&jobs)? {
            let path = entry?.path();
            let text = std::fs::read_to_string(&path)?;
            let named = text
                .lines()
                .map(|line| {
                    if line.starts_with("version = ") {
                        format!("version = \"{version}\"\n")
                    } else {
                        format!("{line}\n")
                    }
                })
                .collect::<String>();
            std::fs::write(&path, named)?;
        }
        Ok(())
    }

    /// Releases `script` as a later version of the workflow, which every job of it now names.
    fn publish_script(&self, script: &str) -> Result<(), BoxError> {
        self.publish(WORKFLOW, script)
    }

    /// Releases `workflow_toml` and `script` as a later version of the workflow, which every job of it now names.
    fn publish(&self, workflow_toml: &str, script: &str) -> Result<(), BoxError> {
        write(
            self.vault.path(),
            "registry/workflows/ops/workflows.toml",
            "workflows = [\"sync\"]\n",
        )?;
        write(
            self.vault.path(),
            "registry/workflows/ops/sync/workflow.toml",
            workflow_toml,
        )?;
        write(self.vault.path(), "registry/workflows/ops/sync/sync.rhai", script)?;
        self.name_release(NEXT_RELEASE)?;
        support::release_fixture::install_vault_releases(self.vault.path()).map_err(|error| error.to_string())?;
        Ok(())
    }
}

// ---------------------------------------------------------------------
// A new version of an enabled job
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_new_version_that_passes_validation_keeps_the_job_enabled_with_its_first_live_run_on_trial()
-> Result<(), BoxError> {
    let fixture = fixture(WORKS).await?;
    fixture.enabled_and_proven(WORKS).await?;
    assert!(!fixture.on_trial()?);

    fixture.publish_script(WORKS_DIFFERENTLY)?;
    fixture.pass(LATER).await?;

    assert_eq!(fixture.lifecycle()?, "enabled");
    assert!(fixture.on_trial()?);
    let state = fixture.state()?;
    let job = state.job("ops", "sync", "nightly");
    assert_eq!(
        job.and_then(|job| job.fingerprint()),
        Some(&next_version_of(WORKS_DIFFERENTLY)?)
    );
    assert_eq!(
        job.and_then(contextos_workflows::JobState::dry_run_at),
        Some(ENABLED_AT),
        "the dry run is not repeated"
    );
    assert_eq!(state.scheduled_expression("ops", "sync", "nightly"), Some("0 3 * * *"));
    Ok(())
}

#[tokio::test]
async fn a_new_version_that_fails_validation_suspends_the_job_with_every_failure_as_the_reason() -> Result<(), BoxError>
{
    let fixture = fixture(WORKS).await?;
    fixture.enabled_and_proven(WORKS).await?;

    fixture.publish_script(BROKEN)?;
    fixture.pass(LATER).await?;

    assert_eq!(fixture.lifecycle()?, "suspended");
    let reason = fixture.reason()?.ok_or("a suspension must say why")?;
    assert!(reason.contains("validation/script-syntax"), "{reason}");
    assert!(reason.contains("sync.rhai"), "{reason}");
    assert_eq!(
        fixture.suspension_code()?.as_deref(),
        Some("workflow/validation-failed")
    );
    let state = fixture.state()?;
    assert_eq!(
        state.scheduled_expression("ops", "sync", "nightly"),
        None,
        "a suspended job is not scheduled"
    );
    let stored = state.job("ops", "sync", "nightly").and_then(|job| job.validation());
    assert_eq!(
        stored.map(|validation| validation.passed),
        Some(false),
        "the failure is stored too"
    );
    Ok(())
}

#[tokio::test]
async fn a_job_whose_release_is_not_installed_is_suspended_rather_than_left_to_fail_on_its_next_tick()
-> Result<(), BoxError> {
    let fixture = fixture(WORKS).await?;
    fixture.enabled_and_proven(WORKS).await?;

    fixture.name_a_release_that_is_not_installed()?;
    fixture.pass(LATER).await?;

    assert_eq!(fixture.lifecycle()?, "suspended");
    let reason = fixture.reason()?.ok_or("a suspension must say why")?;
    assert!(reason.contains("validation/artefact-missing"), "{reason}");
    assert_eq!(
        fixture.suspension_code()?.as_deref(),
        Some("workflow/package-unavailable")
    );
    Ok(())
}

#[tokio::test]
async fn an_enabled_job_whose_version_has_not_changed_is_left_untouched() -> Result<(), BoxError> {
    let fixture = fixture(WORKS).await?;
    fixture.enabled(WORKS).await?;
    let before = std::fs::read_to_string(fixture.vault.path().join(STATE_PATH))?;

    fixture.pass(LATER).await?;

    assert_eq!(std::fs::read_to_string(fixture.vault.path().join(STATE_PATH))?, before);
    Ok(())
}

#[tokio::test]
async fn a_new_version_does_not_stop_the_job_while_it_is_already_on_trial() -> Result<(), BoxError> {
    let fixture = fixture(WORKS).await?;
    fixture.enabled(WORKS).await?;
    assert!(fixture.on_trial()?);

    fixture.publish_script(WORKS_DIFFERENTLY)?;
    fixture.pass(LATER).await?;

    assert_eq!(fixture.lifecycle()?, "enabled");
    assert!(
        fixture.on_trial()?,
        "the trial now belongs to the version that will run"
    );
    Ok(())
}

#[tokio::test]
async fn a_suspended_job_whose_version_changes_stays_suspended_and_is_not_validated() -> Result<(), BoxError> {
    let fixture = fixture(WORKS).await?;
    fixture.enabled_and_proven(WORKS).await?;
    fixture
        .change(
            Actor::Operator,
            Transition::Suspend(contextos_workflows::Suspension::operator(
                contextos_workflows::SuspensionReason::try_from("operator stopped it")?,
            )),
            ENABLED_AT,
        )
        .await?;
    let before = std::fs::read_to_string(fixture.vault.path().join(STATE_PATH))?;

    fixture.publish_script(WORKS_DIFFERENTLY)?;
    fixture.pass(LATER).await?;

    assert_eq!(std::fs::read_to_string(fixture.vault.path().join(STATE_PATH))?, before);
    Ok(())
}

// ---------------------------------------------------------------------
// The first live run
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_first_live_run_that_succeeds_ends_the_trial_and_the_job_carries_on() -> Result<(), BoxError> {
    let fixture = fixture(WORKS).await?;
    fixture.enabled(WORKS).await?;

    let outcome = fixture.live_run().await?;

    assert!(matches!(outcome, RunOutcome::Succeeded), "{outcome:?}");
    assert_eq!(fixture.lifecycle()?, "enabled");
    assert!(!fixture.on_trial()?);
    Ok(())
}

#[tokio::test]
async fn a_first_live_run_that_degrades_ends_the_trial() -> Result<(), BoxError> {
    let fixture = fixture(DEGRADES).await?;
    fixture.enabled(DEGRADES).await?;

    let outcome = fixture.live_run().await?;

    assert!(matches!(outcome, RunOutcome::Degraded(_)), "{outcome:?}");
    assert_eq!(fixture.lifecycle()?, "enabled");
    assert!(!fixture.on_trial()?);
    Ok(())
}

#[tokio::test]
async fn a_first_live_run_that_fails_suspends_the_job_with_the_failure_and_keeps_the_trial_pending()
-> Result<(), BoxError> {
    let fixture = fixture(FAILS).await?;
    fixture.enabled(FAILS).await?;

    let outcome = fixture.live_run().await?;

    assert!(matches!(outcome, RunOutcome::Failed(_)), "{outcome:?}");
    assert_eq!(fixture.lifecycle()?, "suspended");
    let reason = fixture.reason()?.ok_or("a suspension must say why")?;
    assert!(reason.contains("the crm is down"), "{reason}");
    assert_eq!(fixture.suspension_code()?.as_deref(), Some("workflow/probation-failed"));
    assert!(fixture.on_trial()?);
    assert_eq!(
        fixture.state()?.scheduled_expression("ops", "sync", "nightly"),
        None,
        "it is not retried on a schedule either"
    );
    Ok(())
}

#[tokio::test]
async fn a_job_whose_trial_is_over_is_not_suspended_when_a_later_run_fails() -> Result<(), BoxError> {
    let fixture = fixture(FAILS).await?;
    fixture.enabled_and_proven(FAILS).await?;

    let outcome = fixture.live_run().await?;

    assert!(matches!(outcome, RunOutcome::Failed(_)), "{outcome:?}");
    assert_eq!(fixture.lifecycle()?, "enabled", "only a first run is on trial");
    Ok(())
}

#[tokio::test]
async fn a_manual_run_of_a_registered_job_changes_nothing_whether_it_succeeds_or_fails() -> Result<(), BoxError> {
    for script in [WORKS, FAILS] {
        let fixture = fixture(script).await?;
        fixture.registered(script).await?;
        let before = std::fs::read_to_string(fixture.vault.path().join(STATE_PATH))?;

        fixture.live_run().await?;

        let after = std::fs::read_to_string(fixture.vault.path().join(STATE_PATH))?;
        assert_eq!(fixture.lifecycle()?, "registered", "{script}");
        assert!(!fixture.on_trial()?, "{script}");
        // The run leaves nothing of its own in the job's state.
        assert_eq!(after, before, "{script}");
    }
    Ok(())
}

#[tokio::test]
async fn a_manual_run_of_a_suspended_job_that_succeeds_ends_the_trial_and_leaves_it_suspended() -> Result<(), BoxError>
{
    let fixture = fixture(WORKS).await?;
    fixture.enabled(WORKS).await?;
    fixture
        .change(
            Actor::System,
            Transition::ProbationFailed(
                version_of(WORKS)?,
                contextos_workflows::Suspension {
                    cause: contextos_workflows::SuspensionCause::ProbationFailed {
                        run: Some(1),
                        failure: None,
                    },
                    reason: contextos_workflows::SuspensionReason::try_from("the first live run failed")?,
                },
            ),
            ENABLED_AT,
        )
        .await?;
    assert!(fixture.on_trial()?);

    let outcome = fixture.live_run().await?;

    assert!(matches!(outcome, RunOutcome::Succeeded), "{outcome:?}");
    assert_eq!(fixture.lifecycle()?, "suspended", "resuming is the operator's call");
    assert!(!fixture.on_trial()?);
    Ok(())
}

#[tokio::test]
async fn a_manual_run_of_a_suspended_job_that_fails_leaves_it_suspended_for_its_original_reason() -> Result<(), BoxError>
{
    let fixture = fixture(FAILS).await?;
    fixture.enabled(FAILS).await?;
    fixture
        .change(
            Actor::System,
            Transition::ProbationFailed(
                version_of(FAILS)?,
                contextos_workflows::Suspension {
                    cause: contextos_workflows::SuspensionCause::ProbationFailed {
                        run: Some(1),
                        failure: None,
                    },
                    reason: contextos_workflows::SuspensionReason::try_from("the first live run failed")?,
                },
            ),
            ENABLED_AT,
        )
        .await?;

    fixture.live_run().await?;

    assert_eq!(fixture.lifecycle()?, "suspended");
    assert_eq!(fixture.reason()?.as_deref(), Some("the first live run failed"));
    assert!(fixture.on_trial()?);
    Ok(())
}

#[tokio::test]
async fn a_run_of_a_version_that_is_no_longer_the_one_on_trial_decides_nothing() -> Result<(), BoxError> {
    let fixture = fixture(FAILS).await?;
    fixture.enabled(FAILS).await?;
    // The state already holds a newer version on trial, as a validation pass between the run's start and end would leave.
    fixture
        .change(
            Actor::Validator,
            Transition::OnProbation(version_of(WORKS_DIFFERENTLY)?),
            LATER,
        )
        .await?;

    let outcome = fixture.live_run().await?;

    assert!(matches!(outcome, RunOutcome::Failed(_)), "{outcome:?}");
    assert_eq!(
        fixture.lifecycle()?,
        "enabled",
        "the failed run was of a version that is not on trial"
    );
    assert!(fixture.on_trial()?);
    Ok(())
}

// ---------------------------------------------------------------------
// Runs that are not live
// ---------------------------------------------------------------------

const ONE_CALL: &str = "[[call]]\nserver = \"contextos\"\ntool = \"fs_read_text_file\"\nkind = \"valid\"\nresponse = { content = \"mocked\" }\n";
const READS: &str = "call_mcp(\"contextos\", \"fs_read_text_file\", #{ path: \"does-not-exist.md\" });";

#[tokio::test]
async fn a_dry_run_never_settles_the_trial_whatever_it_finds() -> Result<(), BoxError> {
    for script in [READS, FAILS] {
        let fixture = fixture(script).await?;
        write(
            fixture.vault.path(),
            "registry/workflows/ops/sync/tests/happy-path.toml",
            ONE_CALL,
        )?;
        fixture.enabled(script).await?;
        let before = std::fs::read_to_string(fixture.vault.path().join(STATE_PATH))?;

        support::run_job(
            Arc::clone(&fixture.clients),
            &support::host(fixture.config.path()),
            VAULT_NAME,
            "ops",
            "sync",
            "nightly",
            CliRunOptions {
                dry_run_scenario: Some(Some("happy-path".to_owned())),
                record: false,
            },
        )
        .await?;

        assert_eq!(fixture.lifecycle()?, "enabled", "{script}");
        assert!(fixture.on_trial()?, "{script}");
        assert_eq!(
            std::fs::read_to_string(fixture.vault.path().join(STATE_PATH))?,
            before,
            "{script}"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------
// A scheduled first run
// ---------------------------------------------------------------------

/// 03:00:30, inside the look-back window of a job scheduled for 03:00 daily.
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        datetime!(2026-10-01 03:00:30 UTC)
    }
}

#[tokio::test]
async fn a_scheduled_first_run_that_fails_suspends_the_job() -> Result<(), BoxError> {
    let fixture = fixture(FAILS).await?;
    fixture.enabled(FAILS).await?;
    // Counted from the moment it was enabled, a tick at 03:00 has not yet fallen due.
    fixture
        .change(
            Actor::Operator,
            Transition::SetSchedule(JobSchedule::try_from("* * * * *")?),
            datetime!(2026-10-01 02:55:00 UTC),
        )
        .await?;
    let host = support::host(fixture.config.path());

    poll_once(&fixture.clients, &host, &FixedClock).await;

    // The run is detached, so its end is awaited by polling until the job is suspended, and no outcome depends on the
    // length of the pause. The deadline only ends a wait that can never finish.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while fixture.lifecycle()? != "suspended" {
        if tokio::time::Instant::now() >= deadline {
            return Err("the failed first run did not suspend the job".into());
        }
        // A pause, not a bare yield: each check walks the filesystem, blocking the one thread the run needs as well.
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let reason = fixture.reason()?.ok_or("a suspension must say why")?;
    assert!(reason.contains("the crm is down"), "{reason}");
    assert_eq!(fixture.suspension_code()?.as_deref(), Some("workflow/probation-failed"));
    let run_records = run_record_names(&fixture.vault.path().join("memory/jobs/ops/sync/nightly"));
    let [record] = run_records.as_slice() else {
        return Err(format!("expected the one run that failed, found {run_records:?}").into());
    };
    let number: u64 = record
        .trim_start_matches("R-")
        .trim_end_matches(".md")
        .parse()
        .map_err(|_| format!("a run record is named R-<number>.md: {record}"))?;
    let state = fixture.state()?;
    let stored = state
        .job("ops", "sync", "nightly")
        .and_then(JobState::suspension)
        .ok_or("the suspension must be stored as a record")?;
    assert_eq!(
        stored.run,
        Some(number),
        "the suspension names the run that failed the trial, which the scheduler's own run supplies"
    );
    Ok(())
}

// ---------------------------------------------------------------------
// What settling reports
// ---------------------------------------------------------------------

fn failed_run() -> RunOutcome {
    RunOutcome::Failed(contextos_workflows::RunFailure::from("it broke".to_owned()))
}

#[tokio::test]
async fn settling_reports_what_it_did_for_a_job_on_trial() -> Result<(), BoxError> {
    let passing = fixture(WORKS).await?;
    passing.enabled(WORKS).await?;
    assert_eq!(
        passing.settle(Some(version_of(WORKS)?), &RunOutcome::Succeeded).await?,
        Settlement::Passed
    );

    let failing = fixture(WORKS).await?;
    failing.enabled(WORKS).await?;
    assert_eq!(
        failing.settle(Some(version_of(WORKS)?), &failed_run()).await?,
        Settlement::Suspended
    );
    Ok(())
}

#[tokio::test]
async fn settling_reports_that_there_was_nothing_to_settle_when_the_outcome_cannot_apply() -> Result<(), BoxError> {
    // A cancelled run decides nothing.
    let cancelled = fixture(WORKS).await?;
    cancelled.enabled(WORKS).await?;
    assert_eq!(
        cancelled
            .settle(Some(version_of(WORKS)?), &RunOutcome::Cancelled)
            .await?,
        Settlement::NothingToSettle
    );
    assert!(cancelled.on_trial()?);

    // A job whose trial is over has nothing on trial.
    let proven = fixture(WORKS).await?;
    proven.enabled_and_proven(WORKS).await?;
    assert_eq!(
        proven.settle(Some(version_of(WORKS)?), &failed_run()).await?,
        Settlement::NothingToSettle
    );

    // A run of another version than the one on trial.
    let other = fixture(WORKS).await?;
    other.enabled(WORKS).await?;
    assert_eq!(
        other
            .settle(Some(version_of(WORKS_DIFFERENTLY)?), &failed_run())
            .await?,
        Settlement::NothingToSettle
    );

    // A job that is not enabled or suspended.
    let registered = fixture(WORKS).await?;
    registered.registered(WORKS).await?;
    assert_eq!(
        registered.settle(Some(version_of(WORKS)?), &failed_run()).await?,
        Settlement::NothingToSettle
    );
    Ok(())
}

#[tokio::test]
async fn settling_a_failure_of_a_job_that_is_already_suspended_is_not_reported_as_a_fault() -> Result<(), BoxError> {
    let fixture = fixture(WORKS).await?;
    fixture.enabled(WORKS).await?;
    assert_eq!(
        fixture.settle(Some(version_of(WORKS)?), &failed_run()).await?,
        Settlement::Suspended
    );

    let again = fixture.settle(Some(version_of(WORKS)?), &failed_run()).await?;

    assert_eq!(again, Settlement::NothingToSettle);
    Ok(())
}

#[tokio::test]
async fn settling_without_the_version_that_ran_applies_nothing_and_says_so() -> Result<(), BoxError> {
    let fixture = fixture(WORKS).await?;
    fixture.enabled(WORKS).await?;

    let settled = fixture.settle(None, &failed_run()).await?;

    assert_eq!(settled, Settlement::NotRecorded);
    assert_eq!(
        fixture.lifecycle()?,
        "enabled",
        "a failure is not applied to a version nobody knows"
    );
    Ok(())
}

// ---------------------------------------------------------------------
// Which jobs a change reaches
// ---------------------------------------------------------------------

const WEEKLY: &str = "name = \"Weekly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";

impl Fixture {
    /// Adds a second job to the workflow and takes it, like the first, to `enabled` with its trial already passed.
    async fn add_proven_job(&self, slug: &str, job_toml: &str, script: &str) -> Result<(), BoxError> {
        write(
            self.vault.path(),
            &format!("registry/workflows/ops/sync/jobs/{slug}.toml"),
            job_toml,
        )?;
        let version = version_from(script, job_toml)?;
        let client = self.clients.get("contextos").ok_or("no primary server")?;
        for (actor, transition) in [
            (Actor::Validator, Transition::Validated(version.clone())),
            (Actor::DryRun, Transition::DryRunCompleted(version.clone())),
            (
                Actor::Operator,
                Transition::Enable(JobSchedule::try_from("0 3 * * *")?, version.clone()),
            ),
            (Actor::System, Transition::ProbationPassed(version)),
        ] {
            change_lifecycle(
                &client,
                VAULT_NAME,
                LifecycleChange {
                    package: "ops",
                    workflow_slug: "sync",
                    job_slug: slug,
                    actor,
                    transition,
                    now: ENABLED_AT,
                },
            )
            .await?;
        }
        Ok(())
    }

    fn job_on_trial(&self, slug: &str) -> Result<bool, BoxError> {
        Ok(self
            .state()?
            .job("ops", "sync", slug)
            .is_some_and(contextos_workflows::JobState::first_run_pending))
    }

    fn job_lifecycle(&self, slug: &str) -> Result<&'static str, BoxError> {
        Ok(self.state()?.lifecycle_of("ops", "sync", slug).as_str())
    }
}

async fn two_proven_jobs() -> Result<Fixture, BoxError> {
    let fixture = fixture(WORKS).await?;
    fixture.enabled_and_proven(WORKS).await?;
    fixture.add_proven_job("weekly", WEEKLY, WORKS).await?;
    Ok(fixture)
}

#[tokio::test]
async fn a_change_to_the_workflow_script_puts_every_enabled_job_of_the_workflow_on_probation() -> Result<(), BoxError> {
    let fixture = two_proven_jobs().await?;

    fixture.publish_script(WORKS_DIFFERENTLY)?;
    fixture.pass(LATER).await?;

    for slug in ["nightly", "weekly"] {
        assert_eq!(fixture.job_lifecycle(slug)?, "enabled", "{slug}");
        assert!(fixture.job_on_trial(slug)?, "{slug}");
    }
    Ok(())
}

#[tokio::test]
async fn a_change_to_the_workflow_manifest_puts_every_enabled_job_of_the_workflow_on_probation() -> Result<(), BoxError>
{
    let fixture = two_proven_jobs().await?;

    fixture.publish(&WORKFLOW.replace("Sync", "Sync everything"), WORKS)?;
    fixture.pass(LATER).await?;

    for slug in ["nightly", "weekly"] {
        assert_eq!(fixture.job_lifecycle(slug)?, "enabled", "{slug}");
        assert!(fixture.job_on_trial(slug)?, "{slug}");
    }
    Ok(())
}

#[tokio::test]
async fn a_change_to_one_job_file_puts_only_that_job_on_probation() -> Result<(), BoxError> {
    let fixture = two_proven_jobs().await?;

    write(
        fixture.vault.path(),
        "registry/workflows/ops/sync/jobs/nightly.toml",
        "name = \"Nightly, renamed\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    fixture.pass(LATER).await?;

    assert!(fixture.job_on_trial("nightly")?);
    assert!(!fixture.job_on_trial("weekly")?);
    assert_eq!(fixture.job_lifecycle("weekly")?, "enabled");
    Ok(())
}

// ---------------------------------------------------------------------
// A version nobody has reviewed is not started
// ---------------------------------------------------------------------

/// The file names of every run record at or beneath `folder`, wherever the record is filed within it.
fn run_record_names(folder: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![folder.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if let Some(name) = path.file_name().and_then(|name| name.to_str())
                && name.starts_with("R-")
            {
                found.push(name.to_owned());
            }
        }
    }
    found
}

fn run_records(vault: &Path) -> usize {
    let mut found = 0;
    let mut pending = vec![vault.join("memory/jobs")];
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
                found += 1;
            }
        }
    }
    found
}

#[tokio::test]
async fn a_due_job_whose_version_has_not_been_reviewed_is_not_started_so_a_failure_cannot_go_unsettled()
-> Result<(), BoxError> {
    let fixture = fixture(WORKS).await?;
    fixture.enabled_and_proven(WORKS).await?;
    fixture
        .change(
            Actor::Operator,
            Transition::SetSchedule(JobSchedule::try_from("* * * * *")?),
            datetime!(2026-10-01 02:55:00 UTC),
        )
        .await?;
    // The script changed, and the review that would put the new version on trial has not happened, as it would not have
    // if the validation pass had been dropped.
    fixture.publish_script(FAILS)?;
    let host = support::host(fixture.config.path());

    start_due_jobs(&fixture.clients, &host, VAULT_NAME, &FixedClock).await;

    assert_eq!(
        run_records(fixture.vault.path()),
        0,
        "nothing was started for an unreviewed version"
    );
    assert_eq!(fixture.lifecycle()?, "enabled");
    assert!(!fixture.on_trial()?, "nothing was reviewed either");
    Ok(())
}
