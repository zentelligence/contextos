//! What a vault command-line run reports for its summary (run number, record location, outputs, registration,
//! overrides), and how it ends when its token is cancelled: real `contextos-mcp` sessions against real temporary vaults.
//! The calls a script makes are real, except in a dry run, where a scenario answers them.

mod support;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use contextos_web::McpClientSet;
use contextos_web::cli_run::{
    CliRunError, CliRunOptions, OneOffVaultRun, run_job_reported, run_workflow_once_reported,
};
use contextos_web::job_lifecycle::{LifecycleChange, change_lifecycle};
use contextos_web::job_validation::validation_pass;
use contextos_web::run_claim::RunClaim;
use contextos_web::run_report::{RunControls, RunReport};
use contextos_web::run_summary::{NotRegistered, RecordLocation, Registration};
use contextos_web::vault_runs::claim_slot;
use contextos_workflows::{
    Actor, JobFingerprint, JobKey, JobSchedule, JobsState, RunOutcome, Transition, WorkflowFailureKind,
};
use serde_json::json;
use time::macros::datetime;
use tokio_util::sync::CancellationToken;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT_NAME: &str = "contract-fixture";
const WORKFLOW: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"contextos\"]\n";
const JOB: &str = "name = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";
/// A workflow that spins until its own short timeout, so a run that is not interrupted ends the test promptly and is
/// reported as timed out, instead of holding the test process for the default five minutes.
const SPINNING: &str =
    "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"contextos\"]\ntimeout_secs = 5\n";
const WITH_TARGET: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"contextos\"]\n\n\
                           [[input]]\nname = \"target\"\nkind = \"string\"\n";
const JOB_WITH_TARGET: &str = "name = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n[inputs]\ntarget = \"from-the-job\"\n";
/// As [`JOB_WITH_TARGET`], marked `ready` (above the `[inputs]` table, which would otherwise take it) so a validation
/// pass validates it.
const READY_JOB_WITH_TARGET: &str = "name = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nready = true\n[inputs]\ntarget = \"from-the-job\"\n";
const WITH_OUTPUTS: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"contextos\"]\n\n\
                            [[output]]\nname = \"label\"\nkind = \"string\"\n";
const ENABLED_AT: time::OffsetDateTime = datetime!(2026-10-01 03:00:00 UTC);
/// Bounds a run that should have been interrupted, so a defect is a failure and not a hang.
const GUARD: Duration = Duration::from_secs(10);

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
    job_toml: String,
}

impl Fixture {
    async fn new(workflow_toml: &str, script: &str, job_toml: &str) -> Result<Self, BoxError> {
        let vault = tempfile::tempdir()?;
        let config = tempfile::tempdir()?;
        write(vault.path(), "index.md", "# Root\n")?;
        write(vault.path(), ".contextos/team.toml", support::jobs::TEAM)?;
        write(
            vault.path(),
            "registry/workflows/ops/workflows.toml",
            "workflows = [\"sync\"]\n",
        )?;
        write(vault.path(), "registry/workflows/ops/sync/workflow.toml", workflow_toml)?;
        write(vault.path(), "registry/workflows/ops/sync/sync.rhai", script)?;
        write(vault.path(), "registry/workflows/ops/sync/jobs/nightly.toml", job_toml)?;
        support::release_fixture::install_vault_releases(vault.path()).map_err(|error| error.to_string())?;
        let config_path = support::write_vault_config(config.path(), vault.path())?;
        let entry = support::real_contextos_entry("contextos", &config_path)?;
        let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
        Ok(Self {
            vault,
            config,
            clients,
            job_toml: job_toml.to_owned(),
        })
    }

    fn version(&self) -> Result<JobFingerprint, BoxError> {
        Ok(
            support::release_fixture::vault_version(self.vault.path(), "ops", &self.job_toml)
                .map_err(|error| error.to_string())?,
        )
    }

    async fn change(&self, actor: Actor, transition: Transition) -> Result<(), BoxError> {
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
                now: ENABLED_AT,
            },
        )
        .await?;
        Ok(())
    }

    /// Takes the job through the real transitions to `registered`.
    async fn registered(&self) -> Result<(), BoxError> {
        self.change(Actor::Validator, Transition::Validated(self.version()?))
            .await?;
        self.change(Actor::DryRun, Transition::DryRunCompleted(self.version()?))
            .await
    }

    /// `registered`, then enabled, which puts its first live run on trial.
    async fn enabled(&self) -> Result<(), BoxError> {
        self.registered().await?;
        self.change(
            Actor::Operator,
            Transition::Enable(JobSchedule::try_from("0 3 * * *")?, self.version()?),
        )
        .await
    }

    /// Validates the job through a real validation pass, which needs it marked `ready`.
    async fn validated(&self) -> Result<(), BoxError> {
        let client = self.clients.get("contextos").ok_or("no primary server")?;
        validation_pass(&client, VAULT_NAME, &["contextos".to_owned()], ENABLED_AT).await?;
        Ok(())
    }

    fn host(&self) -> contextos_web::run_settings::HostContext {
        support::host(self.config.path())
    }

    async fn run(&self, options: CliRunOptions, controls: RunControls) -> Result<RunReport, CliRunError> {
        run_job_reported(
            Arc::clone(&self.clients),
            &self.host(),
            VAULT_NAME,
            &JobKey {
                package: "ops".to_owned(),
                workflow: "sync".to_owned(),
                job: "nightly".to_owned(),
            },
            options,
            controls,
        )
        .await
    }

    async fn live(&self, overrides: &[&str]) -> Result<RunReport, CliRunError> {
        self.run(
            CliRunOptions {
                dry_run_scenario: None,
                record: true,
            },
            overriding(overrides),
        )
        .await
    }

    async fn dry(&self, overrides: &[&str]) -> Result<RunReport, CliRunError> {
        self.run(
            CliRunOptions {
                dry_run_scenario: Some(Some("happy-path".to_owned())),
                record: false,
            },
            overriding(overrides),
        )
        .await
    }

    fn state_text(&self) -> Result<String, std::io::Error> {
        std::fs::read_to_string(self.vault.path().join(".contextos/jobs.toml"))
    }

    fn on_trial(&self) -> Result<bool, BoxError> {
        let state: JobsState = toml::from_str(&self.state_text()?)?;
        Ok(state
            .job("ops", "sync", "nightly")
            .is_some_and(contextos_workflows::JobState::first_run_pending))
    }

    /// The first run record under the job's folder, wherever its date put it.
    fn record_text(&self) -> Option<String> {
        let mut stack = vec![self.vault.path().join("memory/jobs")];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).ok()?.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|extension| extension == "md") {
                    return std::fs::read_to_string(path).ok();
                }
            }
        }
        None
    }

    fn claim_is_free(&self) -> Result<bool, BoxError> {
        let slot = claim_slot(&self.host(), VAULT_NAME, "ops", "sync", "nightly")?;
        Ok(RunClaim::acquire(&slot).is_ok())
    }
}

fn overriding(overrides: &[&str]) -> RunControls {
    RunControls {
        overrides: overrides.iter().map(|argument| (*argument).to_owned()).collect(),
        cancel: CancellationToken::new(),
    }
}

#[tokio::test]
async fn a_vault_run_reports_its_run_id_and_vault_addressed_record() -> Result<(), BoxError> {
    let fixture = Fixture::new(WORKFLOW, "log_info(\"a\");", JOB).await?;
    fixture.registered().await?;

    let report = fixture.live(&[]).await?;

    assert_eq!(report.outcome, RunOutcome::Succeeded);
    assert!(report.started);
    assert_eq!(report.run_id, Some(1));
    let Some(RecordLocation::Vault { vault, path }) = report.record else {
        return Err(format!("expected a vault-addressed record, got {:?}", report.record).into());
    };
    assert_eq!(vault, VAULT_NAME);
    assert!(path.starts_with("memory/jobs/ops/sync/nightly/"), "{path}");
    assert!(path.ends_with("/R-1.md"), "{path}");
    assert!(
        fixture.vault.path().join(&path).is_file(),
        "the path names the record: {path}"
    );
    assert!(
        report.started_at.is_some() && report.ended_at.is_some(),
        "the run's own clock times it"
    );
    Ok(())
}

#[tokio::test]
async fn a_vault_run_with_overrides_validates_them_against_the_workflow_and_records_their_names() -> Result<(), BoxError>
{
    let script = "if inputs.target != \"override-value-x\" { throw \"the override did not reach the script\"; }";
    let fixture = Fixture::new(WITH_TARGET, script, JOB_WITH_TARGET).await?;
    fixture.registered().await?;

    let report = fixture.live(&["target=override-value-x"]).await?;

    assert_eq!(
        report.outcome,
        RunOutcome::Succeeded,
        "the script saw the overriding value"
    );
    assert_eq!(report.overridden_inputs, vec!["target".to_owned()]);
    let record = fixture.record_text().ok_or("the run left no record")?;
    assert!(record.contains("overrides:\n- target\n"), "{record}");
    assert!(
        !record.contains("override-value-x"),
        "a value is never recorded: {record}"
    );
    Ok(())
}

#[tokio::test]
async fn an_invalid_vault_override_is_refused_before_any_record_and_releases_the_claim() -> Result<(), BoxError> {
    let fixture = Fixture::new(WITH_TARGET, "log_info(\"never reached\");", JOB_WITH_TARGET).await?;
    fixture.registered().await?;

    let report = fixture.live(&["not-declared=1"]).await?;

    assert!(
        matches!(&report.outcome, RunOutcome::Failed(failure) if failure.kind == Some(WorkflowFailureKind::InputInvalid)),
        "{:?}",
        report.outcome
    );
    assert!(!report.started, "no script step ran");
    assert_eq!(report.record, None);
    assert!(fixture.record_text().is_none(), "no record was written");
    assert!(fixture.claim_is_free()?, "the claim taken to find the job was released");
    Ok(())
}

#[tokio::test]
async fn a_vault_run_refused_for_an_invalid_override_never_settles_the_first_run_trial() -> Result<(), BoxError> {
    let fixture = Fixture::new(WITH_TARGET, "log_info(\"never reached\");", JOB_WITH_TARGET).await?;
    fixture.enabled().await?;
    write(
        fixture.vault.path(),
        "registry/workflows/ops/sync/tests/happy-path.toml",
        "[[call]]\nserver = \"contextos\"\ntool = \"vault_info\"\nkind = \"valid\"\nresponse = { vault_count = 1 }\n",
    )?;

    let live = fixture.live(&["not-declared=1"]).await?;
    let dry = fixture.dry(&["not-declared=1"]).await?;

    for report in [&live, &dry] {
        assert!(
            matches!(&report.outcome, RunOutcome::Failed(failure) if failure.kind == Some(WorkflowFailureKind::InputInvalid)),
            "{:?}",
            report.outcome
        );
    }
    assert!(
        fixture.state_text()?.contains("lifecycle = \"enabled\""),
        "the refused runs cannot suspend the job: {}",
        fixture.state_text()?
    );
    assert!(fixture.on_trial()?, "the first live run is still to come");
    Ok(())
}

#[tokio::test]
async fn a_vault_dry_run_that_cannot_find_its_scenario_never_settles_the_first_run_trial() -> Result<(), BoxError> {
    let fixture = Fixture::new(WORKFLOW, "log_info(\"a\");", JOB).await?;
    fixture.enabled().await?;

    let outcome = fixture.dry(&[]).await;

    assert!(
        matches!(
            outcome,
            Err(CliRunError::ScenarioNotFound { .. } | CliRunError::NoScenariosAvailable { .. })
        ),
        "{outcome:?}"
    );
    assert!(fixture.state_text()?.contains("lifecycle = \"enabled\""));
    assert!(fixture.on_trial()?);
    Ok(())
}

#[tokio::test]
async fn a_vault_dry_run_with_overrides_does_not_register_the_job() -> Result<(), BoxError> {
    let fixture = Fixture::new(
        WITH_TARGET,
        "call_mcp(\"contextos\", \"vault_info\", #{});",
        READY_JOB_WITH_TARGET,
    )
    .await?;
    write(
        fixture.vault.path(),
        "registry/workflows/ops/sync/tests/happy-path.toml",
        "[[call]]\nserver = \"contextos\"\ntool = \"vault_info\"\nkind = \"valid\"\nresponse = { vault_count = 1 }\n",
    )?;
    fixture.validated().await?;
    assert!(fixture.state_text()?.contains("lifecycle = \"validated\""));

    let report = fixture.dry(&["target=one-off"]).await?;

    assert_eq!(report.outcome, RunOutcome::Succeeded);
    assert_eq!(
        report.registration,
        Some(Registration::NotRegistered(NotRegistered::InputsOverridden))
    );
    assert!(
        fixture.state_text()?.contains("lifecycle = \"validated\""),
        "the job stays validated"
    );
    Ok(())
}

#[tokio::test]
async fn a_live_vault_run_with_overrides_leaves_the_first_run_trial_unsettled() -> Result<(), BoxError> {
    let fixture = Fixture::new(WITH_TARGET, "log_info(\"a\");", JOB_WITH_TARGET).await?;
    fixture.enabled().await?;
    assert!(fixture.on_trial()?, "the first live run of this version is on trial");

    let report = fixture.live(&["target=one-off"]).await?;

    assert_eq!(report.outcome, RunOutcome::Succeeded);
    assert!(
        fixture.on_trial()?,
        "a run with other values says nothing about the version on trial"
    );
    Ok(())
}

#[tokio::test]
async fn a_vault_run_cancelled_through_its_token_records_cancelled_and_releases_the_claim() -> Result<(), BoxError> {
    let fixture = Fixture::new(SPINNING, "loop {}", JOB).await?;
    fixture.registered().await?;
    let cancel = CancellationToken::new();
    let canceller = {
        let cancel = cancel.clone();
        let vault = fixture.vault.path().to_path_buf();
        tokio::spawn(async move {
            // The record is written `running` before the script starts, so it says the run is under way.
            for _ in 0..400 {
                if let Some(text) = running_record(&vault) {
                    drop(text);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            cancel.cancel();
        })
    };

    let report = tokio::time::timeout(
        GUARD,
        fixture.run(
            CliRunOptions {
                dry_run_scenario: None,
                record: true,
            },
            RunControls {
                overrides: Vec::new(),
                cancel,
            },
        ),
    )
    .await
    .map_err(|_| "the run was never interrupted by its token")??;
    canceller.await?;

    assert_eq!(report.outcome, RunOutcome::Cancelled);
    let record = fixture.record_text().ok_or("the run left no record")?;
    assert!(record.contains("status: cancelled\n"), "{record}");
    assert!(fixture.claim_is_free()?);
    Ok(())
}

/// The text of a run record that still says `running`.
fn running_record(vault: &Path) -> Option<String> {
    let mut stack = vec![vault.join("memory/jobs")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(text) = std::fs::read_to_string(&path)
                && text.contains("status: running\n")
            {
                return Some(text);
            }
        }
    }
    None
}

#[tokio::test]
async fn a_vault_one_off_run_cancelled_through_its_token_ends_cancelled() -> Result<(), BoxError> {
    let fixture = Fixture::new(SPINNING, "loop {}", JOB).await?;
    let cancel = CancellationToken::new();
    cancel.cancel();

    let report = tokio::time::timeout(
        GUARD,
        run_workflow_once_reported(
            Arc::clone(&fixture.clients),
            "contextos",
            OneOffVaultRun {
                code: contextos_web::cli_run::OneOffCode::Source,
                vault: VAULT_NAME.to_owned(),
                package: "ops".to_owned(),
                workflow_slug: "sync".to_owned(),
                arguments: Vec::new(),
                dry_run_scenario: None,
                log_level: None,
            },
            cancel,
        ),
    )
    .await
    .map_err(|_| "the one-off run was never interrupted by its token")??;

    assert_eq!(report.outcome, RunOutcome::Cancelled);
    Ok(())
}

#[tokio::test]
async fn a_vault_workflow_with_declared_outputs_returns_them_and_its_record_names_them() -> Result<(), BoxError> {
    let fixture = Fixture::new(WITH_OUTPUTS, "set_output(\"label\", \"secret-label-value\");", JOB).await?;
    fixture.registered().await?;

    let report = fixture.live(&[]).await?;

    assert_eq!(report.outcome, RunOutcome::Succeeded);
    assert_eq!(
        report.outputs.as_ref().map(|outputs| json!(outputs)),
        Some(json!({ "label": "secret-label-value" }))
    );
    let record = fixture.record_text().ok_or("the run left no record")?;
    assert!(record.contains("outputs:\n- label\n"), "{record}");
    assert!(
        !record.contains("secret-label-value"),
        "a value is never recorded: {record}"
    );
    Ok(())
}

#[tokio::test]
async fn a_vault_run_refused_after_its_record_was_created_reports_refused_and_the_record_says_failed()
-> Result<(), BoxError> {
    let home = tempfile::tempdir()?;
    let other = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    write(home.path(), "index.md", "# Home\n")?;
    write(home.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write(other.path(), "index.md", "# Other\n")?;
    write(other.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write(
        home.path(),
        "registry/workflows/ops/workflows.toml",
        "workflows = [\"sync\"]\n",
    )?;
    write(home.path(), "registry/workflows/ops/sync/workflow.toml", WORKFLOW)?;
    // An entry script in the installed release that is not text ends the run after its record exists and before its script can start.
    std::fs::create_dir_all(home.path().join("registry/workflows/ops"))?;
    std::fs::write(
        home.path().join("registry/workflows/ops/sync/sync.rhai"),
        [0xff_u8, 0xfe, 0x00, 0xfd],
    )?;
    write(home.path(), "registry/workflows/ops/sync/jobs/nightly.toml", JOB)?;
    write(
        home.path(),
        ".contextos/jobs.toml",
        "[ops.sync.nightly]\nlifecycle = \"registered\"\n",
    )?;
    support::release_fixture::install_vault_releases(home.path()).map_err(|error| error.to_string())?;
    let config_path = support::write_named_vaults_config(
        config.path(),
        &[(VAULT_NAME, home.path()), ("other-vault", other.path())],
    )?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);

    let report = run_job_reported(
        clients,
        &support::host(config.path()),
        VAULT_NAME,
        &JobKey {
            package: "ops".to_owned(),
            workflow: "sync".to_owned(),
            job: "nightly".to_owned(),
        },
        CliRunOptions {
            dry_run_scenario: None,
            record: true,
        },
        RunControls::default(),
    )
    .await?;

    assert!(
        matches!(&report.outcome, RunOutcome::Failed(failure) if failure.kind == Some(WorkflowFailureKind::PackageUnavailable)),
        "{:?}",
        report.outcome
    );
    assert!(
        !report.started,
        "the unreadable release ended the run before its script started"
    );
    assert_eq!(
        report.run_id,
        Some(1),
        "the record was created, so the run has its number"
    );
    assert!(
        matches!(report.record, Some(RecordLocation::Vault { .. })),
        "{:?}",
        report.record
    );
    let record = std::fs::read_dir(home.path().join("memory/jobs/ops/sync/nightly"))
        .ok()
        .and_then(|years| years.flatten().next())
        .and_then(|year| std::fs::read_dir(year.path()).ok())
        .and_then(|months| months.flatten().next())
        .and_then(|month| std::fs::read_dir(month.path()).ok())
        .and_then(|files| files.flatten().next())
        .and_then(|file| std::fs::read_to_string(file.path()).ok())
        .ok_or("the refused run left no record")?;
    assert!(
        record.contains("status: failed\n"),
        "the record keeps its own vocabulary: {record}"
    );
    Ok(())
}

#[tokio::test]
async fn a_first_vault_run_that_fails_before_its_script_starts_leaves_the_job_enabled_and_on_trial()
-> Result<(), BoxError> {
    let fixture = Fixture::new(WORKFLOW, "log_info(\"a\");", JOB).await?;
    fixture.enabled().await?;
    assert!(fixture.on_trial()?, "the first live run of this version is on trial");
    // An entry script in the installed release that is not text ends the run after its record exists and before its script can start.
    std::fs::write(
        fixture.vault.path().join("registry/workflows/ops/sync/sync.rhai"),
        [0xff_u8, 0xfe, 0x00, 0xfd],
    )?;
    support::release_fixture::install_vault_releases(fixture.vault.path()).map_err(|error| error.to_string())?;

    let report = fixture.live(&[]).await?;

    assert!(
        matches!(&report.outcome, RunOutcome::Failed(failure) if failure.kind == Some(WorkflowFailureKind::PackageUnavailable)),
        "{:?}",
        report.outcome
    );
    assert!(!report.started, "the script never started");
    assert!(
        report
            .host_warnings
            .iter()
            .all(|warning| warning.code != "workflow/state-unavailable"),
        "a run that settles nothing cannot have failed to save a settlement: {:?}",
        report.host_warnings
    );
    let record = fixture.record_text().ok_or("the run left no record")?;
    assert!(record.contains("status: failed\n"), "{record}");
    assert!(
        fixture.on_trial()?,
        "a run whose script never started says nothing about the job, so its trial is still pending"
    );
    let state = fixture.state_text()?;
    assert!(
        state.contains("lifecycle = \"enabled\""),
        "the job is neither suspended nor changed: {state}"
    );
    Ok(())
}

#[tokio::test]
async fn a_vault_dry_run_reports_whether_it_registered_the_job() -> Result<(), BoxError> {
    const SCENARIO: &str =
        "[[call]]\nserver = \"contextos\"\ntool = \"vault_info\"\nkind = \"valid\"\nresponse = { vault_count = 1 }\n";
    #[cfg(unix)]
    const CALLING: &str = "call_mcp(\"contextos\", \"vault_info\", #{});";
    let ready_job = format!("{JOB}ready = true\n");

    let completes = Fixture::new(WORKFLOW, CALLING, &ready_job).await?;
    write(
        completes.vault.path(),
        "registry/workflows/ops/sync/tests/happy-path.toml",
        SCENARIO,
    )?;
    completes.validated().await?;

    let draft = Fixture::new(WORKFLOW, CALLING, JOB).await?;
    write(
        draft.vault.path(),
        "registry/workflows/ops/sync/tests/happy-path.toml",
        SCENARIO,
    )?;

    let unused = Fixture::new(WORKFLOW, "log_info(\"no calls\");", &ready_job).await?;
    write(
        unused.vault.path(),
        "registry/workflows/ops/sync/tests/happy-path.toml",
        SCENARIO,
    )?;
    unused.validated().await?;

    let fails = Fixture::new(WORKFLOW, "throw \"boom\";", &ready_job).await?;
    write(
        fails.vault.path(),
        "registry/workflows/ops/sync/tests/happy-path.toml",
        SCENARIO,
    )?;
    fails.validated().await?;

    let expected = [
        (&completes, Registration::Registered),
        (&draft, Registration::NotRegistered(NotRegistered::NotValidated)),
        (&unused, Registration::NotRegistered(NotRegistered::ScenarioCallsUnused)),
        (&fails, Registration::NotRegistered(NotRegistered::DryRunNotCompleted)),
    ];
    for (fixture, registration) in expected {
        let report = fixture.dry(&[]).await?;

        assert_eq!(report.registration, Some(registration), "{:?}", report.outcome);
    }
    Ok(())
}

const CALLING: &str = "call_mcp(\"contextos\", \"vault_info\", #{});";
const SCENARIO: &str =
    "[[call]]\nserver = \"contextos\"\ntool = \"vault_info\"\nkind = \"valid\"\nresponse = { vault_count = 1 }\n";

/// A folder made unwritable for the length of a test, and made writable again when the test ends, however it ends.
#[cfg(unix)]
struct Unwritable(std::path::PathBuf);

#[cfg(unix)]
impl Unwritable {
    fn new(folder: &Path) -> std::io::Result<Self> {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o555))?;
        Ok(Self(folder.to_path_buf()))
    }
}

#[cfg(unix)]
impl Drop for Unwritable {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

#[cfg(unix)]
fn warning_codes(report: &RunReport) -> Vec<&str> {
    report
        .host_warnings
        .iter()
        .map(|warning| warning.code.as_str())
        .collect()
}

#[cfg(unix)]
#[tokio::test]
async fn a_vault_dry_run_that_cannot_save_its_registration_warns_that_the_state_is_unavailable() -> Result<(), BoxError>
{
    if support::directory_permissions_are_ignored()? {
        return Ok(());
    }
    let ready_job = format!("{JOB}ready = true\n");
    let fixture = Fixture::new(WORKFLOW, CALLING, &ready_job).await?;
    write(
        fixture.vault.path(),
        "registry/workflows/ops/sync/tests/happy-path.toml",
        SCENARIO,
    )?;
    fixture.validated().await?;
    let _locked = Unwritable::new(&fixture.vault.path().join(".contextos"))?;

    let report = fixture.dry(&[]).await?;

    assert_eq!(report.outcome, RunOutcome::Succeeded, "the dry run itself completes");
    assert_eq!(
        report.registration,
        Some(Registration::NotRegistered(NotRegistered::StateUnavailable))
    );
    assert!(
        warning_codes(&report).contains(&"workflow/state-unavailable"),
        "the summary says the host could not save the state: {:?}",
        report.host_warnings
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_vault_first_run_that_cannot_settle_its_trial_warns_that_the_state_is_unavailable() -> Result<(), BoxError> {
    if support::directory_permissions_are_ignored()? {
        return Ok(());
    }
    let fixture = Fixture::new(WORKFLOW, "log_info(\"a\");", JOB).await?;
    fixture.enabled().await?;
    let _locked = Unwritable::new(&fixture.vault.path().join(".contextos"))?;

    let report = fixture.live(&[]).await?;

    assert_eq!(report.outcome, RunOutcome::Succeeded, "the run itself is unaffected");
    assert!(
        warning_codes(&report).contains(&"workflow/state-unavailable"),
        "the summary says the trial could not be settled: {:?}",
        report.host_warnings
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_vault_run_whose_record_cannot_be_completed_warns_that_the_record_is_unavailable() -> Result<(), BoxError> {
    if support::directory_permissions_are_ignored()? {
        return Ok(());
    }
    let fixture = Fixture::new(SPINNING, "loop {}", JOB).await?;
    fixture.registered().await?;
    let cancel = CancellationToken::new();
    let locking = {
        let cancel = cancel.clone();
        let vault = fixture.vault.path().to_path_buf();
        tokio::spawn(async move {
            // The record exists once it says `running`. Its folder is then made unwritable, so the run cannot finish it.
            // The poll is bounded (10 s) and stays because the run offers no marker to wait on: the record's `running` state
            // is only visible from outside, in the vault, and the run is held by a script that never ends until cancelled.
            for _ in 0..400 {
                if running_record(&vault).is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            let folder = record_folder(&vault);
            let locked = folder.as_deref().map(Unwritable::new).transpose();
            cancel.cancel();
            locked
        })
    };

    let report = tokio::time::timeout(
        GUARD,
        fixture.run(
            CliRunOptions {
                dry_run_scenario: None,
                record: true,
            },
            RunControls {
                overrides: Vec::new(),
                cancel,
            },
        ),
    )
    .await
    .map_err(|_| "the run was never interrupted by its token")??;
    let _locked = locking.await??;

    assert_eq!(report.outcome, RunOutcome::Cancelled);
    assert!(
        warning_codes(&report).contains(&"workflow/record-unavailable"),
        "{:?} {:?}",
        report.host_warnings,
        fixture.record_text()
    );
    Ok(())
}

/// The folder holding the first run record under the job's folder.
#[cfg(unix)]
fn record_folder(vault: &Path) -> Option<std::path::PathBuf> {
    let mut stack = vec![vault.join("memory/jobs")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|extension| extension == "md") {
                return path.parent().map(Path::to_path_buf);
            }
        }
    }
    None
}

#[tokio::test]
async fn a_cancelled_vault_dry_run_leaves_a_record_that_says_cancelled() -> Result<(), BoxError> {
    let fixture = Fixture::new(SPINNING, "loop {}", JOB).await?;
    write(
        fixture.vault.path(),
        "registry/workflows/ops/sync/tests/happy-path.toml",
        SCENARIO,
    )?;
    fixture.registered().await?;
    let cancel = CancellationToken::new();
    cancel.cancel();

    let report = tokio::time::timeout(
        GUARD,
        fixture.run(
            CliRunOptions {
                dry_run_scenario: Some(Some("happy-path".to_owned())),
                record: true,
            },
            RunControls {
                overrides: Vec::new(),
                cancel,
            },
        ),
    )
    .await
    .map_err(|_| "the dry run was never interrupted by its token")??;

    assert_eq!(report.outcome, RunOutcome::Cancelled);
    assert!(
        matches!(report.record, Some(RecordLocation::Vault { .. })),
        "{:?}",
        report.record
    );
    let record = fixture.record_text().ok_or("the cancelled dry run left no record")?;
    assert!(record.contains("status: cancelled\n"), "{record}");
    assert!(record.contains("dry_run: true\n"), "{record}");
    Ok(())
}
