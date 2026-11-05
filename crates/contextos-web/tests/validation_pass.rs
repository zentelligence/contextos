//! The automatic validation pass (`contextos_web::job_validation::validation_pass`) against a real `contextos-mcp`
//! session and a real temporary vault: a draft job that says it is ready is validated once per version, a job whose
//! version changes goes back to draft, and a job that is not ready is not even looked at beyond its flag.

mod support;

use std::path::Path;
use std::sync::Arc;

use contextos_web::McpClientSet;
use contextos_web::discover_jobs;
use contextos_web::job_lifecycle::{LifecycleChange, change_lifecycle};
use contextos_web::job_operation::{OperatorRequest, perform};
use contextos_web::job_operation_vault::VaultJobStore;
use contextos_web::job_validation::{DryRunRecorded, job_version, record_dry_run, validate_now, validation_pass};
use contextos_workflows::{Actor, Applied, JobFingerprint, JobSchedule, JobsState, Transition};
use time::OffsetDateTime;
use time::macros::datetime;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT_NAME: &str = "contract-fixture";
const WORKFLOW: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"contextos\"]\n";
/// A valid team: `operator` is the one chief of staff.
const TEAM: &str = "[[member]]\nslug = \"operator\"\nname = \"Operator\"\ntype = \"human\"\nchief_of_staff = true\n";
const NEXT_RELEASE: &str = "2.0.0";
const STATE_PATH: &str = ".contextos/jobs.toml";
const FIRST: OffsetDateTime = datetime!(2026-10-01 03:00:00 UTC);
const LATER: OffsetDateTime = datetime!(2026-10-02 03:00:00 UTC);

fn write(dir: &Path, relative: &str, content: &str) -> std::io::Result<()> {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

struct Fixture {
    vault: tempfile::TempDir,
    _config: tempfile::TempDir,
    clients: Arc<McpClientSet>,
}

async fn fixture(script: &str, job: &str) -> Result<Fixture, BoxError> {
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
    write(vault.path(), "registry/workflows/ops/sync/jobs/nightly.toml", job)?;
    write(vault.path(), ".contextos/team.toml", TEAM)?;
    support::release_fixture::install_vault_releases(vault.path()).map_err(|error| error.to_string())?;
    let config_path = support::write_vault_config(config.path(), vault.path())?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    Ok(Fixture {
        vault,
        _config: config,
        clients,
    })
}

impl Fixture {
    async fn pass(&self, now: OffsetDateTime) -> Result<(), BoxError> {
        let client = self.clients.get("contextos").ok_or("no primary server")?;
        validation_pass(&client, VAULT_NAME, &["contextos".to_owned()], now).await?;
        Ok(())
    }

    fn state(&self) -> Result<JobsState, BoxError> {
        let path = self.vault.path().join(STATE_PATH);
        if !path.exists() {
            return Ok(JobsState::default());
        }
        Ok(toml::from_str(&std::fs::read_to_string(path)?)?)
    }

    fn rewrite(&self, relative: &str, content: &str) -> Result<(), BoxError> {
        write(self.vault.path(), relative, content)?;
        Ok(())
    }

    /// An operator's request for the job, made through the operation that applies the rules of the job's release.
    async fn operate(&self, request: OperatorRequest, now: OffsetDateTime) -> Result<(), BoxError> {
        let client = self.clients.get("contextos").ok_or("no primary server")?;
        let known = vec!["contextos".to_owned()];
        let store = VaultJobStore {
            client: &client,
            vault_name: VAULT_NAME,
            known_mcp_servers: &known,
            package: "ops",
            workflow_slug: "sync",
            job_slug: "nightly",
            claims_root: None,
        };
        perform(&store, request, now).await.map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Installs the vault's source as releases again, so an edit to source made since is a release.
    fn install(&self) -> Result<(), BoxError> {
        support::release_fixture::install_vault_releases(self.vault.path()).map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Makes the job file name `version`, leaving the rest of it as it is.
    fn name_version(&self, version: &str) -> Result<(), BoxError> {
        let path = "registry/workflows/ops/sync/jobs/nightly.toml";
        let text = std::fs::read_to_string(self.vault.path().join(path))?;
        self.rewrite(
            path,
            &text.replace("version = \"1.0.0\"", &format!("version = \"{version}\"")),
        )
    }

    /// Releases `script` as a later version of the workflow, which the job file now names.
    fn release(&self, script: &str) -> Result<(), BoxError> {
        self.rewrite("registry/workflows/ops/sync/sync.rhai", script)?;
        self.name_version(NEXT_RELEASE)?;
        self.install()
    }

    async fn change(&self, actor: Actor, transition: Transition, now: OffsetDateTime) -> Result<Applied, BoxError> {
        let client = self.clients.get("contextos").ok_or("no primary server")?;
        Ok(change_lifecycle(
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
        .await?)
    }
}

fn lifecycle(fixture: &Fixture) -> Result<&'static str, BoxError> {
    Ok(fixture.state()?.lifecycle_of("ops", "sync", "nightly").as_str())
}

/// The version of the job `job` once `script` is installed as the release it names.
fn version_of(script: &str, job: &str) -> Result<JobFingerprint, BoxError> {
    support::release_fixture::version_of_source(WORKFLOW, "sync.rhai", script, job)
        .map_err(|error| error.to_string().into())
}

/// `job`, naming the later release a script edit is made into.
fn later(job: &str) -> String {
    job.replace("version = \"1.0.0\"", &format!("version = \"{NEXT_RELEASE}\""))
}

#[tokio::test]
async fn a_job_that_is_not_ready_is_left_alone_and_nothing_is_written() -> Result<(), BoxError> {
    let fixture = fixture(
        "log_info(\"a\");",
        "name = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )
    .await?;

    fixture.pass(FIRST).await?;

    assert!(!fixture.vault.path().join(STATE_PATH).exists());
    Ok(())
}

#[tokio::test]
async fn a_ready_job_that_passes_becomes_validated_for_its_version() -> Result<(), BoxError> {
    let fixture = fixture(
        "log_info(\"a\");",
        "ready = true\nname = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )
    .await?;

    fixture.pass(FIRST).await?;

    let state = fixture.state()?;
    assert_eq!(lifecycle(&fixture)?, "validated");
    let job = state.job("ops", "sync", "nightly");
    assert_eq!(
        job.and_then(|job| job.fingerprint()),
        Some(&version_of(
            "log_info(\"a\");",
            "name = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n"
        )?),
        "the version excludes the ready key"
    );
    assert_eq!(job.and_then(|job| job.validation()).map(|v| v.passed), Some(true));
    Ok(())
}

#[tokio::test]
async fn a_ready_job_that_fails_stays_a_draft_with_every_failure_recorded() -> Result<(), BoxError> {
    let fixture = fixture(
        "let y = (2 +;",
        "ready = true\nname = \"Nightly\"\nowner = \"nobody\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )
    .await?;

    fixture.pass(FIRST).await?;

    assert_eq!(lifecycle(&fixture)?, "draft");
    let state = fixture.state()?;
    let stored = state.job("ops", "sync", "nightly").and_then(|job| job.validation());
    assert_eq!(stored.map(|v| v.passed), Some(false));
    let codes: Vec<&str> = stored
        .map(|v| v.failures.iter().map(|f| f.code.as_str()).collect())
        .unwrap_or_default();
    assert!(codes.contains(&"validation/script-syntax"), "{codes:?}");
    assert!(codes.contains(&"validation/owner-unresolved"), "{codes:?}");
    Ok(())
}

#[tokio::test]
async fn the_same_version_is_not_validated_again() -> Result<(), BoxError> {
    let fixture = fixture(
        "let y = (2 +;",
        "ready = true\nname = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )
    .await?;
    fixture.pass(FIRST).await?;

    fixture.pass(LATER).await?;

    let state = fixture.state()?;
    assert_eq!(
        state
            .job("ops", "sync", "nightly")
            .and_then(|job| job.validation())
            .map(|v| v.at),
        Some(FIRST),
        "a second pass over an unchanged version leaves the first result alone"
    );
    Ok(())
}

#[tokio::test]
async fn fixing_a_failed_job_validates_the_new_version() -> Result<(), BoxError> {
    let fixture = fixture(
        "let y = (2 +;",
        "ready = true\nname = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )
    .await?;
    fixture.pass(FIRST).await?;
    assert_eq!(lifecycle(&fixture)?, "draft");

    fixture.release("log_info(\"fixed\");")?;
    fixture.pass(LATER).await?;

    assert_eq!(lifecycle(&fixture)?, "validated");
    Ok(())
}

#[tokio::test]
async fn a_validated_job_whose_script_changes_goes_back_to_draft_and_is_validated_afresh() -> Result<(), BoxError> {
    let fixture = fixture(
        "log_info(\"a\");",
        "ready = true\nname = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )
    .await?;
    fixture.pass(FIRST).await?;
    assert_eq!(lifecycle(&fixture)?, "validated");

    fixture.release("log_info(\"b\");")?;
    fixture.pass(LATER).await?;

    let state = fixture.state()?;
    assert_eq!(
        state.job("ops", "sync", "nightly").and_then(|job| job.fingerprint()),
        Some(&version_of(
            "log_info(\"b\");",
            &later("name = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n")
        )?),
        "it was returned to draft and validated for the new version in the same pass"
    );
    Ok(())
}

#[tokio::test]
async fn a_changed_script_that_is_no_longer_ready_leaves_the_job_a_draft() -> Result<(), BoxError> {
    let fixture = fixture(
        "log_info(\"a\");",
        "ready = true\nname = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )
    .await?;
    fixture.pass(FIRST).await?;

    fixture.release("log_info(\"b\");")?;
    fixture.rewrite(
        "registry/workflows/ops/sync/jobs/nightly.toml",
        &later("name = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n"),
    )?;
    fixture.pass(LATER).await?;

    assert_eq!(lifecycle(&fixture)?, "draft");
    Ok(())
}

#[tokio::test]
async fn a_registered_job_is_not_disturbed_by_the_ready_key_but_is_by_a_real_change() -> Result<(), BoxError> {
    let fixture = fixture(
        "log_info(\"a\");",
        "ready = true\nname = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )
    .await?;
    fixture.pass(FIRST).await?;
    let version = version_of(
        "log_info(\"a\");",
        "name = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    fixture
        .change(Actor::DryRun, Transition::DryRunCompleted(version), FIRST)
        .await?;
    assert_eq!(lifecycle(&fixture)?, "registered");

    fixture.rewrite(
        "registry/workflows/ops/sync/jobs/nightly.toml",
        "ready = false\nname = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    fixture.pass(LATER).await?;
    assert_eq!(lifecycle(&fixture)?, "registered", "the ready key is not a new version");

    fixture.rewrite(
        "registry/workflows/ops/sync/jobs/nightly.toml",
        "name = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n[inputs]\n",
    )?;
    fixture.release("log_info(\"changed\");")?;
    fixture.pass(LATER).await?;
    assert_eq!(lifecycle(&fixture)?, "draft", "a changed script voids the registration");
    Ok(())
}

#[tokio::test]
async fn an_enabled_job_is_left_to_the_probation_that_handles_a_new_version() -> Result<(), BoxError> {
    let fixture = fixture(
        "log_info(\"a\");",
        "ready = true\nname = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )
    .await?;
    fixture.pass(FIRST).await?;
    let version = version_of(
        "log_info(\"a\");",
        "name = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    fixture
        .change(Actor::DryRun, Transition::DryRunCompleted(version.clone()), FIRST)
        .await?;
    fixture
        .operate(OperatorRequest::Schedule(JobSchedule::try_from("0 3 * * *")?), FIRST)
        .await?;

    // A changed timeout is a new version of the same release, which the job's file may change in place.
    fixture.rewrite(
        "registry/workflows/ops/sync/jobs/nightly.toml",
        "ready = true\nname = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n",
    )?;
    fixture.pass(LATER).await?;

    assert_eq!(
        lifecycle(&fixture)?,
        "enabled",
        "this pass does not touch an enabled job"
    );
    Ok(())
}

#[tokio::test]
async fn an_operator_can_validate_a_job_that_is_not_ready() -> Result<(), BoxError> {
    let fixture = fixture(
        "log_info(\"a\");",
        "name = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )
    .await?;
    let client = fixture.clients.get("contextos").ok_or("no primary server")?;

    let validation = validate_now(
        &client,
        VAULT_NAME,
        &["contextos".to_owned()],
        contextos_web::jobs::WorkflowFolder {
            package: "ops",
            directory: "sync",
        },
        "nightly.toml",
        FIRST,
    )
    .await?;

    assert!(validation.report.passed(), "{:?}", validation.report);
    assert_eq!(lifecycle(&fixture)?, "validated");
    Ok(())
}

#[tokio::test]
async fn validating_on_demand_records_a_failure_and_leaves_the_job_a_draft() -> Result<(), BoxError> {
    let fixture = fixture(
        "let y = (2 +;",
        "name = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )
    .await?;
    let client = fixture.clients.get("contextos").ok_or("no primary server")?;

    let validation = validate_now(
        &client,
        VAULT_NAME,
        &["contextos".to_owned()],
        contextos_web::jobs::WorkflowFolder {
            package: "ops",
            directory: "sync",
        },
        "nightly.toml",
        FIRST,
    )
    .await?;

    assert!(!validation.report.passed());
    assert_eq!(lifecycle(&fixture)?, "draft");
    let state = fixture.state()?;
    assert_eq!(
        state
            .job("ops", "sync", "nightly")
            .and_then(|job| job.validation())
            .map(|v| v.passed),
        Some(false)
    );
    Ok(())
}

#[tokio::test]
async fn validating_a_job_that_is_already_validated_for_its_version_changes_nothing_but_the_record()
-> Result<(), BoxError> {
    let fixture = fixture(
        "log_info(\"a\");",
        "ready = true\nname = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )
    .await?;
    fixture.pass(FIRST).await?;
    let client = fixture.clients.get("contextos").ok_or("no primary server")?;

    validate_now(
        &client,
        VAULT_NAME,
        &["contextos".to_owned()],
        contextos_web::jobs::WorkflowFolder {
            package: "ops",
            directory: "sync",
        },
        "nightly.toml",
        LATER,
    )
    .await?;

    assert_eq!(lifecycle(&fixture)?, "validated");
    Ok(())
}

#[tokio::test]
async fn validating_on_demand_after_an_edit_does_not_strand_the_job_as_a_draft() -> Result<(), BoxError> {
    let fixture = fixture(
        "log_info(\"a\");",
        "ready = true\nname = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )
    .await?;
    fixture.pass(FIRST).await?;
    fixture.release("log_info(\"b\");")?;
    let client = fixture.clients.get("contextos").ok_or("no primary server")?;
    validate_now(
        &client,
        VAULT_NAME,
        &["contextos".to_owned()],
        contextos_web::jobs::WorkflowFolder {
            package: "ops",
            directory: "sync",
        },
        "nightly.toml",
        FIRST,
    )
    .await?;

    fixture.pass(LATER).await?;

    assert_eq!(lifecycle(&fixture)?, "validated");
    assert_eq!(
        fixture
            .state()?
            .job("ops", "sync", "nightly")
            .and_then(|job| job.fingerprint()),
        Some(&version_of(
            "log_info(\"b\");",
            &later("ready = true\nname = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n")
        )?)
    );
    Ok(())
}

#[tokio::test]
async fn a_reactivated_job_with_unchanged_files_is_validated_again() -> Result<(), BoxError> {
    let fixture = fixture(
        "log_info(\"a\");",
        "ready = true\nname = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )
    .await?;
    fixture.pass(FIRST).await?;
    fixture
        .change(
            Actor::DryRun,
            Transition::DryRunCompleted(version_of(
                "log_info(\"a\");",
                "ready = true\nname = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
            )?),
            FIRST,
        )
        .await?;
    fixture
        .operate(OperatorRequest::Schedule(JobSchedule::try_from("0 3 * * *")?), FIRST)
        .await?;
    fixture.operate(OperatorRequest::Retire, FIRST).await?;
    fixture.operate(OperatorRequest::Reactivate, FIRST).await?;
    assert_eq!(lifecycle(&fixture)?, "draft");

    fixture.pass(LATER).await?;

    assert_eq!(lifecycle(&fixture)?, "validated");
    Ok(())
}

#[tokio::test]
async fn a_dry_run_is_recorded_for_the_version_that_ran_and_a_later_edit_still_returns_the_job_to_draft()
-> Result<(), BoxError> {
    let fixture = fixture(
        "log_info(\"a\");",
        "ready = true\nname = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )
    .await?;
    fixture.pass(FIRST).await?;
    let client = fixture.clients.get("contextos").ok_or("no primary server")?;
    let jobs = discover_jobs(&client, VAULT_NAME, &["contextos".to_owned()]).await?;
    let job = jobs.first().ok_or("job not discovered")?;
    let ran = job_version(&client, VAULT_NAME, job).await?.ok_or("no version")?;
    fixture.release("log_info(\"b\");")?;

    let recorded = record_dry_run(&client, VAULT_NAME, job, ran, FIRST).await?;

    assert_eq!(recorded, DryRunRecorded::Registered);
    fixture.pass(LATER).await?;
    assert_eq!(
        lifecycle(&fixture)?,
        "validated",
        "the edited version was validated afresh, not registered"
    );
    Ok(())
}

#[tokio::test]
async fn a_dry_run_of_a_version_that_was_never_validated_registers_nothing() -> Result<(), BoxError> {
    let fixture = fixture(
        "log_info(\"a\");",
        "ready = true\nname = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )
    .await?;
    fixture.pass(FIRST).await?;
    fixture.release("log_info(\"b\");")?;
    let client = fixture.clients.get("contextos").ok_or("no primary server")?;
    let jobs = discover_jobs(&client, VAULT_NAME, &["contextos".to_owned()]).await?;
    let job = jobs.first().ok_or("job not discovered")?;
    let ran = job_version(&client, VAULT_NAME, job).await?.ok_or("no version")?;

    let recorded = record_dry_run(&client, VAULT_NAME, job, ran, LATER).await?;

    assert!(matches!(recorded, DryRunRecorded::NotRecorded(_)), "{recorded:?}");
    assert_eq!(lifecycle(&fixture)?, "validated");
    Ok(())
}

#[tokio::test]
async fn a_job_naming_a_slug_that_is_not_url_safe_gets_no_state_entry() -> Result<(), BoxError> {
    let fixture = fixture(
        "log_info(\"a\");",
        "ready = true\nname = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nslug = \"Not Safe\"\n",
    )
    .await?;

    fixture.pass(FIRST).await?;

    assert!(!fixture.vault.path().join(STATE_PATH).exists());
    Ok(())
}

#[tokio::test]
async fn a_ready_job_naming_a_release_that_is_not_installed_stores_a_failure_with_no_version_and_is_validated_once_it_is()
-> Result<(), BoxError> {
    let fixture = fixture(
        "log_info(\"a\");",
        "ready = true\nname = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )
    .await?;
    fixture.name_version(NEXT_RELEASE)?;

    fixture.pass(FIRST).await?;

    let stored = fixture.state()?;
    let validation = stored
        .job("ops", "sync", "nightly")
        .and_then(|job| job.validation())
        .ok_or("no validation was stored")?;
    assert!(!validation.passed);
    assert_eq!(validation.fingerprint, None);
    assert_eq!(validation.failures[0].code, "validation/artefact-missing");
    let first_pass = std::fs::read_to_string(fixture.vault.path().join(STATE_PATH))?;

    fixture.pass(LATER).await?;
    assert_eq!(
        std::fs::read_to_string(fixture.vault.path().join(STATE_PATH))?,
        first_pass,
        "the same unreadable state is not recorded again"
    );

    fixture.release("log_info(\"a\");")?;
    fixture.pass(LATER).await?;
    assert_eq!(lifecycle(&fixture)?, "validated");
    Ok(())
}

#[tokio::test]
async fn a_validated_job_whose_named_release_is_not_installed_returns_to_draft_with_the_failure_recorded()
-> Result<(), BoxError> {
    let fixture = fixture(
        "log_info(\"a\");",
        "ready = true\nname = \"Nightly\"\nowner = \"operator\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )
    .await?;
    fixture.pass(FIRST).await?;
    assert_eq!(lifecycle(&fixture)?, "validated");
    fixture.name_version(NEXT_RELEASE)?;

    fixture.pass(LATER).await?;

    assert_eq!(lifecycle(&fixture)?, "draft");
    let stored = fixture.state()?;
    let validation = stored
        .job("ops", "sync", "nightly")
        .and_then(|job| job.validation())
        .ok_or("no validation was stored")?;
    assert!(!validation.passed);
    assert_eq!(validation.fingerprint, None);
    Ok(())
}
