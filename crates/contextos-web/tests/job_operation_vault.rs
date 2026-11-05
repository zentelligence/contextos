//! The operator operation for a vault job, against a real `contextos-mcp` session and a real temporary vault: the state
//! file's actual bytes are what is asserted, so a refusal that must write nothing is proved to.

mod support;

use std::path::Path;
use std::sync::Arc;

use contextos_web::McpClientSet;
use contextos_web::job_lifecycle::{LifecycleChange, change_lifecycle};
use contextos_web::job_operation::{
    JobStore, OperationError, OperationOutcome, OperatorRequest, Performed, perform, perform_retaining,
};
use contextos_web::job_operation_vault::VaultJobStore;
use contextos_web::vault_source_install::SourceInstall;
use contextos_web::vault_source_read::{SourceKind, SourceTarget};
use contextos_workflows::PackageName;
use contextos_workflows::{
    Actor, Applied, JobFingerprint, JobLifecycle, JobSchedule, JobState, JobsState, SuspensionReason, Transition,
};
use time::OffsetDateTime;
use time::macros::datetime;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT_NAME: &str = "contract-fixture";
const WORKFLOW: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"contextos\"]\n";
const JOB: &str = "name = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";
/// The same job once it names a later release of its workflow.
const NEXT_JOB: &str = "name = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"2.0.0\"\n";
const SCRIPT: &str = "log_info(\"a\");";
const STATE_PATH: &str = ".contextos/jobs.toml";
const NOW: OffsetDateTime = datetime!(2026-10-01 03:00:00 UTC);

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
    client: Arc<contextos_web::McpClient>,
    known_servers: Vec<String>,
}

async fn fixture(script: &str, job: &str) -> Result<Fixture, BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    write(vault.path(), "index.md", "# Root\n")?;
    write(vault.path(), ".contextos/team.toml", support::jobs::TEAM)?;
    write(
        vault.path(),
        "registry/workflows/ops/workflows.toml",
        "workflows = [\"sync\"]\n",
    )?;
    write(vault.path(), "registry/workflows/ops/sync/workflow.toml", WORKFLOW)?;
    write(vault.path(), "registry/workflows/ops/sync/sync.rhai", script)?;
    write(vault.path(), "registry/workflows/ops/sync/jobs/nightly.toml", job)?;
    support::release_fixture::install_vault_releases(vault.path()).map_err(|error| error.to_string())?;
    let config_path = support::write_vault_config(config.path(), vault.path())?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    let client = clients.get("contextos").ok_or("no primary server")?;
    Ok(Fixture {
        vault,
        _config: config,
        clients,
        client,
        known_servers: vec!["contextos".to_owned()],
    })
}

/// The version of the job `job` once `script` is installed as the release it names.
fn version_of(script: &str, job: &str) -> Result<JobFingerprint, BoxError> {
    support::release_fixture::version_of_source(WORKFLOW, "sync.rhai", script, job)
        .map_err(|error| error.to_string().into())
}

impl Fixture {
    async fn ask(
        &self,
        workflow: &str,
        job: &str,
        request: OperatorRequest,
    ) -> Result<OperationOutcome, OperationError> {
        let client = self
            .clients
            .get("contextos")
            .ok_or_else(|| OperationError::Unavailable("no primary server".to_owned()))?;
        let store = VaultJobStore {
            client: &client,
            vault_name: VAULT_NAME,
            known_mcp_servers: &self.known_servers,
            package: "ops",
            workflow_slug: workflow,
            job_slug: job,
            claims_root: None,
        };
        perform(&store, request, NOW).await
    }

    async fn ask_retaining(&self, request: OperatorRequest) -> Result<Performed, OperationError> {
        let client = self
            .clients
            .get("contextos")
            .ok_or_else(|| OperationError::Unavailable("no primary server".to_owned()))?;
        let store = VaultJobStore {
            client: &client,
            vault_name: VAULT_NAME,
            known_mcp_servers: &self.known_servers,
            package: "ops",
            workflow_slug: "sync",
            job_slug: "nightly",
            claims_root: None,
        };
        perform_retaining(&store, request, NOW).await
    }

    async fn ask_nightly(&self, request: OperatorRequest) -> Result<OperationOutcome, OperationError> {
        self.ask("sync", "nightly", request).await
    }

    async fn step(&self, actor: Actor, transition: Transition) -> Result<Applied, BoxError> {
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
                now: NOW,
            },
        )
        .await?)
    }

    fn state(&self) -> Result<JobsState, BoxError> {
        let path = self.vault.path().join(STATE_PATH);
        if !path.exists() {
            return Ok(JobsState::default());
        }
        Ok(toml::from_str(&std::fs::read_to_string(path)?)?)
    }

    fn state_text(&self) -> String {
        std::fs::read_to_string(self.vault.path().join(STATE_PATH)).unwrap_or_default()
    }

    fn lifecycle(&self) -> Result<JobLifecycle, BoxError> {
        Ok(self.state()?.lifecycle_of("ops", "sync", "nightly"))
    }

    fn rewrite(&self, relative: &str, content: &str) -> Result<(), BoxError> {
        write(self.vault.path(), relative, content)?;
        Ok(())
    }

    /// Releases `script` as a later version of the workflow and makes the job name it.
    fn release(&self, script: &str) -> Result<(), BoxError> {
        write(self.vault.path(), "registry/workflows/ops/sync/sync.rhai", script)?;
        write(
            self.vault.path(),
            "registry/workflows/ops/sync/jobs/nightly.toml",
            NEXT_JOB,
        )?;
        support::release_fixture::install_vault_releases(self.vault.path()).map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Validates on demand, dry-runs, and enables the job on `0 3 * * *`, each through the real operation or transition.
    async fn enable(&self) -> Result<(), BoxError> {
        self.ask_nightly(OperatorRequest::Validate).await?;
        self.step(Actor::DryRun, Transition::DryRunCompleted(version_of(SCRIPT, JOB)?))
            .await?;
        self.ask_nightly(OperatorRequest::Schedule(JobSchedule::try_from("0 3 * * *")?))
            .await?;
        Ok(())
    }
}

#[tokio::test]
async fn an_operator_validates_a_job_that_is_not_ready_and_the_result_is_kept() -> Result<(), BoxError> {
    let fixture = fixture(SCRIPT, JOB).await?;

    let outcome = fixture.ask_nightly(OperatorRequest::Validate).await?;

    let OperationOutcome::Validated { result, lifecycle } = outcome else {
        return Err("expected a validation outcome".into());
    };
    assert!(result.passed, "{result:?}");
    assert_eq!(lifecycle, JobLifecycle::Validated);
    assert_eq!(fixture.lifecycle()?, JobLifecycle::Validated);
    let state = fixture.state()?;
    assert_eq!(
        state.job("ops", "sync", "nightly").and_then(JobState::fingerprint),
        Some(&version_of(SCRIPT, JOB)?)
    );
    Ok(())
}

#[tokio::test]
async fn validating_a_job_that_would_not_register_reports_every_failure() -> Result<(), BoxError> {
    let fixture = fixture(
        "let y = (2 +;",
        "name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nowner = \"nobody\"\n",
    )
    .await?;

    let outcome = fixture.ask_nightly(OperatorRequest::Validate).await?;

    let OperationOutcome::Validated { result, lifecycle } = outcome else {
        return Err("expected a validation outcome".into());
    };
    assert!(!result.passed);
    let codes: Vec<&str> = result.failures.iter().map(|failure| failure.code.as_str()).collect();
    assert!(codes.contains(&"validation/script-syntax"), "{codes:?}");
    assert!(codes.contains(&"validation/owner-unresolved"), "{codes:?}");
    assert_eq!(lifecycle, JobLifecycle::Draft);
    Ok(())
}

#[tokio::test]
async fn an_operator_enables_a_registered_job_by_giving_it_a_schedule() -> Result<(), BoxError> {
    let fixture = fixture(SCRIPT, JOB).await?;

    fixture.enable().await?;

    assert_eq!(fixture.lifecycle()?, JobLifecycle::Enabled);
    let text = fixture.state_text();
    assert!(text.contains("schedule = \"0 3 * * *\""), "{text}");
    assert!(text.contains("first_run = true"), "{text}");
    Ok(())
}

#[tokio::test]
async fn enabling_after_the_files_changed_since_validation_is_refused_and_writes_nothing() -> Result<(), BoxError> {
    let fixture = fixture(SCRIPT, JOB).await?;
    fixture.ask_nightly(OperatorRequest::Validate).await?;
    fixture
        .step(Actor::DryRun, Transition::DryRunCompleted(version_of(SCRIPT, JOB)?))
        .await?;
    fixture.release("log_info(\"edited\");")?;
    let before = fixture.state_text();

    let refused = fixture
        .ask_nightly(OperatorRequest::Schedule(JobSchedule::try_from("0 3 * * *")?))
        .await;

    let Err(error @ OperationError::Refused(_)) = refused else {
        return Err(format!("{refused:?}").into());
    };
    assert!(error.to_string().contains("validate it again"), "{error}");
    assert_eq!(
        fixture.state_text(),
        before,
        "the state file is byte for byte as it was"
    );
    assert_eq!(fixture.lifecycle()?, JobLifecycle::Registered);
    Ok(())
}

#[tokio::test]
async fn validating_again_after_the_files_changed_validates_the_job_for_the_files_as_they_are_now()
-> Result<(), BoxError> {
    let fixture = fixture(SCRIPT, JOB).await?;
    fixture.ask_nightly(OperatorRequest::Validate).await?;
    fixture
        .step(Actor::DryRun, Transition::DryRunCompleted(version_of(SCRIPT, JOB)?))
        .await?;
    let edited = "log_info(\"edited\");";
    fixture.release(edited)?;

    let outcome = fixture.ask_nightly(OperatorRequest::Validate).await?;

    let OperationOutcome::Validated { result, lifecycle } = outcome else {
        return Err("expected a validation outcome".into());
    };
    assert!(result.passed, "{result:?}");
    // The dry run was of the old files, so the job is validated for the new ones and waits for a dry run of them.
    assert_eq!(lifecycle, JobLifecycle::Validated);
    let state = fixture.state()?;
    let job = state.job("ops", "sync", "nightly");
    assert_eq!(
        job.and_then(JobState::fingerprint),
        Some(&version_of(edited, NEXT_JOB)?)
    );
    assert_eq!(job.and_then(JobState::dry_run_at), None);
    fixture
        .step(
            Actor::DryRun,
            Transition::DryRunCompleted(version_of(edited, NEXT_JOB)?),
        )
        .await?;
    assert_eq!(fixture.lifecycle()?, JobLifecycle::Registered);
    Ok(())
}

#[tokio::test]
async fn validating_again_after_the_files_changed_to_ones_that_fail_returns_the_job_to_draft() -> Result<(), BoxError> {
    let fixture = fixture(SCRIPT, JOB).await?;
    fixture.ask_nightly(OperatorRequest::Validate).await?;
    fixture.release("let y = (2 +;")?;

    let outcome = fixture.ask_nightly(OperatorRequest::Validate).await?;

    let OperationOutcome::Validated { result, lifecycle } = outcome else {
        return Err("expected a validation outcome".into());
    };
    assert!(!result.passed);
    assert_eq!(lifecycle, JobLifecycle::Draft);
    Ok(())
}

#[tokio::test]
async fn validating_a_registered_job_whose_owner_left_the_team_returns_it_to_draft() -> Result<(), BoxError> {
    let fixture = fixture(SCRIPT, JOB).await?;
    fixture.ask_nightly(OperatorRequest::Validate).await?;
    fixture
        .step(Actor::DryRun, Transition::DryRunCompleted(version_of(SCRIPT, JOB)?))
        .await?;
    fixture.rewrite(".contextos/team.toml", support::jobs::TEAM_WITHOUT_VELA)?;

    let outcome = fixture.ask_nightly(OperatorRequest::Validate).await?;

    // The files are unchanged, but the job's own owner no longer resolves, which is the job's to fix, so it cannot be
    // scheduled on the strength of the validation and dry run it had.
    let OperationOutcome::Validated { result, lifecycle } = outcome else {
        return Err("expected a validation outcome".into());
    };
    let codes: Vec<&str> = result.failures.iter().map(|failure| failure.code.as_str()).collect();
    assert!(codes.contains(&"validation/owner-unresolved"), "{codes:?}");
    assert_eq!(lifecycle, JobLifecycle::Draft);
    let refused = fixture
        .ask_nightly(OperatorRequest::Schedule(JobSchedule::try_from("0 3 * * *")?))
        .await;
    assert!(matches!(refused, Err(OperationError::Refused(_))), "{refused:?}");
    Ok(())
}

#[tokio::test]
async fn validating_an_enabled_job_whose_owner_left_the_team_suspends_it_with_the_failure() -> Result<(), BoxError> {
    let fixture = fixture(SCRIPT, JOB).await?;
    fixture.enable().await?;
    fixture.rewrite(".contextos/team.toml", support::jobs::TEAM_WITHOUT_VELA)?;

    let outcome = fixture.ask_nightly(OperatorRequest::Validate).await?;

    let OperationOutcome::Validated { lifecycle, .. } = outcome else {
        return Err("expected a validation outcome".into());
    };
    assert_eq!(lifecycle, JobLifecycle::Suspended);
    let state = fixture.state()?;
    let suspension = state
        .job("ops", "sync", "nightly")
        .and_then(JobState::suspension)
        .ok_or("no suspension")?;
    assert_eq!(suspension.code, "workflow/validation-failed");
    assert!(
        suspension.reason.contains("validation/owner-unresolved"),
        "{}",
        suspension.reason
    );
    Ok(())
}

#[tokio::test]
async fn validating_a_job_whose_script_imports_an_undeclared_module_refuses_it_at_the_import_line()
-> Result<(), BoxError> {
    let fixture = fixture("log_info(\"a\");\nimport \"helpers\" as h;\n", JOB).await?;

    let outcome = fixture.ask_nightly(OperatorRequest::Validate).await?;

    let OperationOutcome::Validated { result, lifecycle } = outcome else {
        return Err("expected a validation outcome".into());
    };
    let failure = result
        .failures
        .iter()
        .find(|failure| failure.code == "validation/script-syntax")
        .ok_or("no script failure")?;
    assert!(failure.message.contains("names no module"), "{}", failure.message);
    assert!(failure.message.contains("line 2"), "{}", failure.message);
    assert_eq!(lifecycle, JobLifecycle::Draft);
    Ok(())
}

#[tokio::test]
async fn resuming_a_job_whose_file_names_another_release_than_it_was_enabled_on_is_refused_and_stays_suspended()
-> Result<(), BoxError> {
    let fixture = fixture(SCRIPT, JOB).await?;
    fixture.enable().await?;
    fixture
        .step(Actor::System, Transition::ProbationPassed(version_of(SCRIPT, JOB)?))
        .await?;
    fixture
        .ask_nightly(OperatorRequest::Suspend(SuspensionReason::try_from("held")?))
        .await?;
    fixture.release("log_info(\"fixed\");")?;

    let refused = fixture.ask_nightly(OperatorRequest::Resume).await;

    let Err(error @ OperationError::NotValid { .. }) = refused else {
        return Err(format!("{refused:?}").into());
    };
    assert!(error.to_string().contains("jobs successor"), "{error}");
    assert_eq!(fixture.lifecycle()?, JobLifecycle::Suspended);
    let state = fixture.state()?;
    let job = state.job("ops", "sync", "nightly");
    assert_eq!(job.and_then(JobState::fingerprint), Some(&version_of(SCRIPT, JOB)?));
    assert!(
        job.is_some_and(|job| job.validation().is_some_and(|stored| !stored.passed)),
        "the failing result is kept so the job page can show it"
    );
    Ok(())
}

#[tokio::test]
async fn resuming_a_job_whose_files_no_longer_validate_is_refused_with_the_failures_and_stays_suspended()
-> Result<(), BoxError> {
    let fixture = fixture(SCRIPT, JOB).await?;
    fixture.enable().await?;
    fixture
        .ask_nightly(OperatorRequest::Suspend(SuspensionReason::try_from("held")?))
        .await?;
    fixture.release("let y = (2 +;")?;

    let refused = fixture.ask_nightly(OperatorRequest::Resume).await;

    let Err(error @ OperationError::NotValid { .. }) = refused else {
        return Err(format!("{refused:?}").into());
    };
    assert!(error.to_string().contains("validation/script-syntax"), "{error}");
    assert_eq!(fixture.lifecycle()?, JobLifecycle::Suspended);
    let state = fixture.state()?;
    assert_eq!(
        state
            .job("ops", "sync", "nightly")
            .and_then(JobState::validation)
            .map(|stored| stored.passed),
        Some(false),
        "the failing result is kept so the job page can show it"
    );
    Ok(())
}

#[tokio::test]
async fn a_job_with_no_files_cannot_be_validated_and_the_error_names_where_it_looked() -> Result<(), BoxError> {
    let fixture = fixture(SCRIPT, JOB).await?;

    let refused = fixture.ask("sync", "no-such-job", OperatorRequest::Validate).await;

    let Err(error @ OperationError::NotFound(_)) = refused else {
        return Err(format!("{refused:?}").into());
    };
    assert_eq!(error.code(), "workflow/job-not-found");
    assert!(error.to_string().contains("no-such-job"), "{error}");
    assert!(!fixture.vault.path().join(STATE_PATH).exists());
    Ok(())
}

#[tokio::test]
async fn a_retired_job_whose_files_were_deleted_can_still_be_deleted_from_the_state() -> Result<(), BoxError> {
    let fixture = fixture(SCRIPT, JOB).await?;
    fixture.enable().await?;
    fixture.ask_nightly(OperatorRequest::Retire).await?;
    std::fs::remove_dir_all(fixture.vault.path().join("registry/workflows/ops"))?;

    let outcome = fixture.ask_nightly(OperatorRequest::Delete).await?;

    assert!(matches!(outcome, OperationOutcome::Changed(Applied::Removed)));
    assert!(fixture.state()?.job("ops", "sync", "nightly").is_none());
    Ok(())
}

#[tokio::test]
async fn every_other_jobs_entry_survives_an_operators_change() -> Result<(), BoxError> {
    let fixture = fixture(SCRIPT, JOB).await?;
    fixture.rewrite(STATE_PATH, "[ops.other.keep]\nfavourite = true\n")?;

    fixture.ask_nightly(OperatorRequest::Validate).await?;

    let text = fixture.state_text();
    assert!(text.contains("[ops.other.keep]"), "{text}");
    Ok(())
}

impl Fixture {
    fn store<'a>(&'a self, workflow: &'a str, job: &'a str) -> VaultJobStore<'a> {
        VaultJobStore {
            client: &self.client,
            vault_name: VAULT_NAME,
            known_mcp_servers: &self.known_servers,
            package: "ops",
            workflow_slug: workflow,
            job_slug: job,
            claims_root: None,
        }
    }
}

#[tokio::test]
async fn a_job_with_files_and_nothing_stored_exists_and_has_no_stored_state() -> Result<(), BoxError> {
    let fixture = fixture(SCRIPT, JOB).await?;
    let store = fixture.store("sync", "nightly");

    assert!(store.exists().await?);
    assert!(store.stored().await?.is_none());
    Ok(())
}

#[tokio::test]
async fn what_is_stored_for_a_job_is_what_a_page_shows() -> Result<(), BoxError> {
    let fixture = fixture(SCRIPT, JOB).await?;
    fixture.ask_nightly(OperatorRequest::Validate).await?;
    let store = fixture.store("sync", "nightly");

    let stored = store.stored().await?.ok_or("nothing stored")?;

    assert_eq!(stored.lifecycle(), JobLifecycle::Validated);
    assert!(stored.validation().is_some_and(|validation| validation.passed));
    Ok(())
}

#[tokio::test]
async fn a_retired_job_whose_files_are_gone_still_exists_because_its_state_does() -> Result<(), BoxError> {
    let fixture = fixture(SCRIPT, JOB).await?;
    fixture.enable().await?;
    fixture.ask_nightly(OperatorRequest::Retire).await?;
    std::fs::remove_dir_all(fixture.vault.path().join("registry/workflows/ops"))?;
    let store = fixture.store("sync", "nightly");

    assert!(store.exists().await?);
    Ok(())
}

#[tokio::test]
async fn a_job_with_neither_files_nor_state_does_not_exist() -> Result<(), BoxError> {
    let fixture = fixture(SCRIPT, JOB).await?;
    let store = fixture.store("sync", "no-such-job");

    assert!(!store.exists().await?);
    assert!(store.stored().await?.is_none());
    Ok(())
}

fn releases_in_store(fixture: &Fixture) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(fixture.vault.path().join(".contextos/packages/ops"))
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    found.sort();
    found
}

/// Installs the vault's `ops` source as the next release.
async fn install_source(fixture: &Fixture) -> Result<(), BoxError> {
    let client = fixture.clients.get("contextos").ok_or("no primary server")?;
    SourceInstall {
        client: &client,
        vault: VAULT_NAME,
    }
    .install(
        &SourceTarget {
            kind: SourceKind::Workflows,
            name: PackageName::try_from("ops")?,
        },
        None,
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn a_validation_that_passes_removes_the_releases_nothing_keeps_and_leaves_the_rest() -> Result<(), BoxError> {
    let fixture = fixture(SCRIPT, JOB).await?;
    // The store is made again by installs, which keep a ledger: the fixture's releases were written without one.
    std::fs::remove_dir_all(fixture.vault.path().join(".contextos/packages"))?;
    install_source(&fixture).await?;
    // A job file names the release before the next is installed, so it is kept, and is removed after.
    let holder = "registry/workflows/ops/sync/jobs/holder.toml";
    for (script, naming) in [("log_info(\"b\");", "1.0.0"), ("log_info(\"c\");", "1.0.1")] {
        write(
            fixture.vault.path(),
            holder,
            &format!("name = \"Holder\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"{naming}\"\n"),
        )?;
        write(fixture.vault.path(), "registry/workflows/ops/sync/sync.rhai", script)?;
        install_source(&fixture).await?;
    }
    std::fs::remove_file(fixture.vault.path().join(holder))?;
    assert_eq!(releases_in_store(&fixture), ["1.0.0", "1.0.1", "1.0.2"]);

    let performed = fixture.ask_retaining(OperatorRequest::Validate).await?;

    assert!(performed.warnings.is_empty(), "{:?}", performed.warnings);
    // The job names 1.0.0, and 1.0.2 is the newest future release.
    assert_eq!(releases_in_store(&fixture), ["1.0.0", "1.0.2"]);
    Ok(())
}
