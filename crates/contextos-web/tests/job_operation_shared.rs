//! The operator operation for a shared job, against a real package store, real job files, the real host state file, and
//! a real `contextos-mcp` session, so a refusal that must write nothing is proved to.

mod support;

use contextos_web::job_operation::{
    JobStore, OperationError, OperationOutcome, OperatorRequest, Performed, perform, perform_retaining,
};
use contextos_web::job_operation_shared::SharedJobStore;
use contextos_web::shared_validation::SharedValidationHost;
use contextos_workflows::{Applied, JobLifecycle, JobSchedule, JobState, SuspensionReason};
use support::BoxError;
use support::shared::{DIGEST, FIRST, OTHER_DIGEST, SharedFixture, key};

const JOB: &str = "name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";
/// A job file that says authoring is finished, so the automatic pass validates it.
const READY: &str = "ready = true\nname = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";

impl SharedFixture {
    async fn ask(&self, request: OperatorRequest) -> Result<OperationOutcome, OperationError> {
        self.ask_for(&key(), request).await
    }

    async fn ask_for(
        &self,
        job: &contextos_web::shared_claim::SharedJobKey,
        request: OperatorRequest,
    ) -> Result<OperationOutcome, OperationError> {
        self.ask_retaining_for(job, request, false)
            .await
            .map(|performed| performed.outcome)
    }

    async fn ask_retaining(&self, request: OperatorRequest) -> Result<Performed, OperationError> {
        self.ask_retaining_for(&key(), request, true).await
    }

    async fn ask_retaining_for(
        &self,
        job: &contextos_web::shared_claim::SharedJobKey,
        request: OperatorRequest,
        retaining: bool,
    ) -> Result<Performed, OperationError> {
        let state = self.state();
        let jobs_root = self.jobs_root();
        let store_root = self.store_root();
        let known = ["contextos".to_owned()];
        let validation = SharedValidationHost {
            store_root: &store_root,
            clients: &self.clients,
            primary_server: "contextos",
            known_servers: &known,
        };
        let store = SharedJobStore {
            state: &state,
            jobs_root: &jobs_root,
            validation: &validation,
            key: job,
            claims_root: &self.claims_root(),
        };
        if retaining {
            perform_retaining(&store, request, FIRST).await
        } else {
            perform(&store, request, FIRST).await.map(|outcome| Performed {
                outcome,
                warnings: Vec::new(),
            })
        }
    }
}

#[tokio::test]
async fn an_operator_validates_a_job_that_is_not_ready_and_the_result_is_kept() -> Result<(), BoxError> {
    let fixture = SharedFixture::new(JOB).await?;

    let outcome = fixture.ask(OperatorRequest::Validate).await?;

    let OperationOutcome::Validated { result, lifecycle } = outcome else {
        return Err("expected a validation outcome".into());
    };
    assert!(result.passed, "{result:?}");
    assert_eq!(lifecycle, JobLifecycle::Validated);
    assert_eq!(fixture.lifecycle()?, "validated");
    assert_eq!(
        fixture
            .read()?
            .job("acme", "sync", "nightly")
            .and_then(JobState::validation)
            .map(|v| v.passed),
        Some(true)
    );
    Ok(())
}

#[tokio::test]
async fn validating_a_job_that_would_not_pass_reports_every_failure_and_keeps_it_a_draft() -> Result<(), BoxError> {
    let fixture =
        SharedFixture::new("name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nowner = \"nobody\"\n").await?;
    fixture.install("1.0.0", DIGEST, "let y = (2 +;")?;

    let outcome = fixture.ask(OperatorRequest::Validate).await?;

    let OperationOutcome::Validated { result, lifecycle } = outcome else {
        return Err("expected a validation outcome".into());
    };
    assert!(!result.passed);
    let codes: Vec<&str> = result.failures.iter().map(|failure| failure.code.as_str()).collect();
    assert!(codes.contains(&"validation/script-syntax"), "{codes:?}");
    assert_eq!(lifecycle, JobLifecycle::Draft);
    Ok(())
}

#[tokio::test]
async fn a_shared_job_whose_script_imports_a_module_its_package_declares_passes_and_a_broken_one_is_named()
-> Result<(), BoxError> {
    let fixture = SharedFixture::new(JOB).await?;
    fixture.install_with_modules(
        "1.0.0",
        DIGEST,
        "import \"lib/text\" as text;\ntext::tidy(\" a \");",
        &[(
            "lib/text.rhai",
            "import \"contextos:text\" as t;\nfn tidy(value) { t::trimmed(value) }",
        )],
    )?;

    let OperationOutcome::Validated { result, .. } = fixture.ask(OperatorRequest::Validate).await? else {
        return Err("expected a validation outcome".into());
    };
    assert!(result.passed, "{:?}", result.failures);

    fixture.install_with_modules(
        "1.0.1",
        DIGEST,
        "import \"lib/text\" as text;",
        &[("lib/text.rhai", "fn tidy( {")],
    )?;
    fixture.name_release("1.0.1")?;
    let OperationOutcome::Validated { result, .. } = fixture.ask(OperatorRequest::Validate).await? else {
        return Err("expected a validation outcome".into());
    };
    assert!(
        result
            .failures
            .iter()
            .any(|failure| failure.code == "validation/script-syntax" && failure.artefact == "lib/text.rhai"),
        "{:?}",
        result.failures
    );
    Ok(())
}

#[tokio::test]
async fn validating_a_shared_job_whose_package_script_imports_an_undeclared_module_refuses_it() -> Result<(), BoxError>
{
    let fixture = SharedFixture::new(JOB).await?;
    fixture.install("1.0.0", DIGEST, "import \"helpers\" as h;\nh::refresh();")?;

    let outcome = fixture.ask(OperatorRequest::Validate).await?;

    let OperationOutcome::Validated { result, lifecycle } = outcome else {
        return Err("expected a validation outcome".into());
    };
    assert!(
        result
            .failures
            .iter()
            .any(|failure| failure.code == "validation/script-syntax" && failure.message.contains("names no module")),
        "{:?}",
        result.failures
    );
    assert_eq!(lifecycle, JobLifecycle::Draft);
    Ok(())
}

#[tokio::test]
async fn an_operator_enables_a_registered_job_by_giving_it_a_schedule() -> Result<(), BoxError> {
    let fixture = SharedFixture::new(READY).await?;
    fixture.register().await?;

    fixture
        .ask(OperatorRequest::Schedule(JobSchedule::try_from("0 3 * * *")?))
        .await?;

    assert_eq!(fixture.lifecycle()?, "enabled");
    assert!(fixture.on_trial()?, "enabling puts the job's first live run on trial");
    assert_eq!(
        fixture.read()?.scheduled_expression("acme", "sync", "nightly"),
        Some("0 3 * * *")
    );
    Ok(())
}

#[tokio::test]
async fn enabling_after_the_job_file_names_a_newer_release_since_validation_is_refused_and_writes_nothing()
-> Result<(), BoxError> {
    let fixture = SharedFixture::new(READY).await?;
    fixture.register().await?;
    fixture.install("1.1.0", OTHER_DIGEST, "log_info(\"newer\");")?;
    fixture.name_release("1.1.0")?;
    let before = fixture.state_text()?;

    let refused = fixture
        .ask(OperatorRequest::Schedule(JobSchedule::try_from("0 3 * * *")?))
        .await;

    let Err(error @ OperationError::Refused(_)) = refused else {
        return Err(format!("{refused:?}").into());
    };
    assert!(error.to_string().contains("validate it again"), "{error}");
    assert_eq!(
        fixture.state_text()?,
        before,
        "the state file is byte for byte as it was"
    );
    Ok(())
}

#[tokio::test]
async fn validating_a_validated_job_after_the_job_file_names_a_newer_release_validates_it_for_the_new_release()
-> Result<(), BoxError> {
    let fixture = SharedFixture::new(READY).await?;
    fixture.pass(FIRST).await?;
    let old = fixture.recorded_version()?;
    fixture.install("1.1.0", OTHER_DIGEST, "log_info(\"newer\");")?;
    fixture.name_release("1.1.0")?;

    let outcome = fixture.ask(OperatorRequest::Validate).await?;

    let OperationOutcome::Validated { result, lifecycle } = outcome else {
        return Err("expected a validation outcome".into());
    };
    assert!(result.passed, "{result:?}");
    assert_eq!(lifecycle, JobLifecycle::Validated);
    let new = fixture.recorded_version()?;
    assert_ne!(new, old, "the job is validated for the release installed now");
    assert_eq!(result.fingerprint.as_ref(), Some(&new));
    // The dry run that follows runs the new release, so it can now register the job.
    fixture
        .change(
            contextos_workflows::Actor::DryRun,
            contextos_workflows::Transition::DryRunCompleted(new),
            FIRST,
        )
        .await?;
    assert_eq!(fixture.lifecycle()?, "registered");
    Ok(())
}

#[tokio::test]
async fn validating_a_registered_job_after_the_job_file_names_a_newer_release_lets_it_be_registered_again()
-> Result<(), BoxError> {
    let fixture = SharedFixture::new(READY).await?;
    fixture.register().await?;
    let old = fixture.recorded_version()?;
    fixture.install("1.1.0", OTHER_DIGEST, "log_info(\"newer\");")?;
    fixture.name_release("1.1.0")?;

    fixture.ask(OperatorRequest::Validate).await?;

    // A registered job's dry run was of the old release, so validating the new one takes it back to validated, where a
    // dry run of the new release registers it.
    assert_eq!(fixture.lifecycle()?, "validated");
    let new = fixture.recorded_version()?;
    assert_ne!(new, old);
    fixture
        .change(
            contextos_workflows::Actor::DryRun,
            contextos_workflows::Transition::DryRunCompleted(new),
            FIRST,
        )
        .await?;
    assert_eq!(fixture.lifecycle()?, "registered");
    Ok(())
}

#[tokio::test]
async fn validating_a_validated_job_whose_newly_named_release_fails_returns_it_to_draft() -> Result<(), BoxError> {
    let fixture = SharedFixture::new(READY).await?;
    fixture.pass(FIRST).await?;
    fixture.install("1.1.0", OTHER_DIGEST, "let y = (2 +;")?;
    fixture.name_release("1.1.0")?;

    let outcome = fixture.ask(OperatorRequest::Validate).await?;

    let OperationOutcome::Validated { result, lifecycle } = outcome else {
        return Err("expected a validation outcome".into());
    };
    assert!(!result.passed);
    assert_eq!(
        lifecycle,
        JobLifecycle::Draft,
        "the version it was validated for is gone"
    );
    Ok(())
}

#[tokio::test]
async fn validating_a_validated_job_whose_release_is_unchanged_keeps_it_validated() -> Result<(), BoxError> {
    let fixture = SharedFixture::new(READY).await?;
    fixture.pass(FIRST).await?;
    let version = fixture.recorded_version()?;

    fixture.ask(OperatorRequest::Validate).await?;

    assert_eq!(fixture.lifecycle()?, "validated");
    assert_eq!(fixture.recorded_version()?, version);
    Ok(())
}

#[tokio::test]
async fn resuming_a_job_whose_file_names_another_release_than_it_was_enabled_on_is_refused_and_stays_suspended()
-> Result<(), BoxError> {
    let fixture = SharedFixture::new(READY).await?;
    fixture.enable().await?;
    fixture
        .ask(OperatorRequest::Suspend(SuspensionReason::try_from("held")?))
        .await?;
    fixture.install("1.1.0", OTHER_DIGEST, "log_info(\"fixed\");")?;
    fixture.name_release("1.1.0")?;

    let refused = fixture.ask(OperatorRequest::Resume).await;

    let Err(error @ OperationError::NotValid { .. }) = refused else {
        return Err(format!("{refused:?}").into());
    };
    assert!(error.to_string().contains("jobs successor"), "{error}");
    assert_eq!(fixture.lifecycle()?, "suspended");
    assert!(!fixture.on_trial()?, "nothing was put on trial");
    Ok(())
}

#[tokio::test]
async fn resuming_a_job_that_no_longer_validates_is_refused_with_the_failures_and_stays_suspended()
-> Result<(), BoxError> {
    let fixture = SharedFixture::new(READY).await?;
    fixture.enable().await?;
    fixture
        .ask(OperatorRequest::Suspend(SuspensionReason::try_from("held")?))
        .await?;
    fixture.install("1.1.0", OTHER_DIGEST, "let y = (2 +;")?;
    fixture.name_release("1.1.0")?;

    let refused = fixture.ask(OperatorRequest::Resume).await;

    let Err(error @ OperationError::NotValid { .. }) = refused else {
        return Err(format!("{refused:?}").into());
    };
    assert!(error.to_string().contains("validation/script-syntax"), "{error}");
    assert_eq!(fixture.lifecycle()?, "suspended");
    assert_eq!(
        fixture
            .read()?
            .job("acme", "sync", "nightly")
            .and_then(JobState::validation)
            .map(|v| v.passed),
        Some(false),
        "the failing result is kept so the job page can show it"
    );
    Ok(())
}

#[tokio::test]
async fn a_job_with_no_file_cannot_be_validated_and_the_error_names_it() -> Result<(), BoxError> {
    let fixture = SharedFixture::new(JOB).await?;
    let mut missing = key();
    missing.job = "no-such-job".to_owned();

    let refused = fixture.ask_for(&missing, OperatorRequest::Validate).await;

    let Err(error @ OperationError::NotFound(_)) = refused else {
        return Err(format!("{refused:?}").into());
    };
    assert_eq!(error.code(), "workflow/job-not-found");
    assert!(error.to_string().contains("no-such-job"), "{error}");
    assert!(fixture.state_text().is_err(), "nothing was written");
    Ok(())
}

#[tokio::test]
async fn a_job_is_found_by_the_slug_its_file_names_not_only_by_its_file_name() -> Result<(), BoxError> {
    let fixture =
        SharedFixture::new("name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nslug = \"renamed\"\n").await?;
    let mut renamed = key();
    renamed.job = "renamed".to_owned();

    let outcome = fixture.ask_for(&renamed, OperatorRequest::Validate).await?;

    assert!(matches!(outcome, OperationOutcome::Validated { result, .. } if result.passed));
    assert_eq!(
        fixture.read()?.lifecycle_of("acme", "sync", "renamed"),
        JobLifecycle::Validated
    );
    Ok(())
}

#[tokio::test]
async fn a_retired_job_whose_file_was_deleted_can_still_be_deleted_from_the_state() -> Result<(), BoxError> {
    let fixture = SharedFixture::new(READY).await?;
    fixture.enable().await?;
    fixture.ask(OperatorRequest::Retire).await?;
    std::fs::remove_file(fixture.job_path())?;

    let outcome = fixture.ask(OperatorRequest::Delete).await?;

    assert!(matches!(outcome, OperationOutcome::Changed(Applied::Removed)));
    assert!(fixture.read()?.job("acme", "sync", "nightly").is_none());
    Ok(())
}

#[tokio::test]
async fn a_refused_transition_leaves_the_state_file_untouched() -> Result<(), BoxError> {
    let fixture = SharedFixture::new(READY).await?;
    fixture.enable().await?;
    let before = fixture.state_text()?;

    let refused = fixture.ask(OperatorRequest::Delete).await;

    assert!(matches!(refused, Err(OperationError::Refused(_))), "{refused:?}");
    assert_eq!(fixture.state_text()?, before);
    Ok(())
}

impl SharedFixture {
    /// Runs `check` with a store for the job `job`.
    async fn with_store<R>(
        &self,
        job: &contextos_web::shared_claim::SharedJobKey,
        check: impl AsyncFnOnce(&SharedJobStore<'_>) -> R,
    ) -> R {
        let state = self.state();
        let jobs_root = self.jobs_root();
        let store_root = self.store_root();
        let known = ["contextos".to_owned()];
        let validation = SharedValidationHost {
            store_root: &store_root,
            clients: &self.clients,
            primary_server: "contextos",
            known_servers: &known,
        };
        let store = SharedJobStore {
            state: &state,
            jobs_root: &jobs_root,
            validation: &validation,
            key: job,
            claims_root: &self.claims_root(),
        };
        check(&store).await
    }
}

#[tokio::test]
async fn a_job_with_a_file_and_nothing_stored_exists_and_has_no_stored_state() -> Result<(), BoxError> {
    let fixture = SharedFixture::new(JOB).await?;

    let (exists, stored) = fixture
        .with_store(&key(), async |store| (store.exists().await, store.stored().await))
        .await;

    assert!(exists?);
    assert!(stored?.is_none());
    Ok(())
}

#[tokio::test]
async fn what_is_stored_for_a_job_is_what_a_page_shows() -> Result<(), BoxError> {
    let fixture = SharedFixture::new(JOB).await?;
    fixture.ask(OperatorRequest::Validate).await?;

    let stored = fixture.with_store(&key(), async |store| store.stored().await).await?;

    let stored = stored.ok_or("nothing stored")?;
    assert_eq!(stored.lifecycle(), JobLifecycle::Validated);
    Ok(())
}

#[tokio::test]
async fn a_retired_job_whose_file_is_gone_still_exists_because_its_state_does() -> Result<(), BoxError> {
    let fixture = SharedFixture::new(READY).await?;
    fixture.enable().await?;
    fixture.ask(OperatorRequest::Retire).await?;
    std::fs::remove_file(fixture.job_path())?;

    let exists = fixture.with_store(&key(), async |store| store.exists().await).await?;

    assert!(exists);
    Ok(())
}

#[tokio::test]
async fn a_job_with_neither_a_file_nor_state_does_not_exist() -> Result<(), BoxError> {
    let fixture = SharedFixture::new(JOB).await?;
    let mut missing = key();
    missing.job = "no-such-job".to_owned();

    let exists = fixture.with_store(&missing, async |store| store.exists().await).await?;

    assert!(!exists);
    Ok(())
}

/// Valid job files for the delete tests, each stating its timeout as every job file will.
const JOB_WITH_TIMEOUT: &str = "name = \"Nightly\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n";
const READY_WITH_TIMEOUT: &str = "ready = true\nname = \"Nightly\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n";

/// Takes the fixture's job to `retired`, which is what a delete needs.
async fn retire(fixture: &SharedFixture) -> Result<(), BoxError> {
    fixture.enable().await?;
    fixture.ask(OperatorRequest::Retire).await?;
    assert_eq!(fixture.lifecycle()?, "retired");
    Ok(())
}

#[tokio::test]
async fn deleting_a_retired_job_removes_its_file_and_then_its_state() -> Result<(), BoxError> {
    let fixture = SharedFixture::new(READY_WITH_TIMEOUT).await?;
    retire(&fixture).await?;

    let outcome = fixture.ask(OperatorRequest::Delete).await?;

    assert!(matches!(outcome, OperationOutcome::Changed(Applied::Removed)));
    assert!(!fixture.job_path().exists(), "the job file is removed with its state");
    assert!(fixture.read()?.job("acme", "sync", "nightly").is_none());
    Ok(())
}

#[tokio::test]
async fn deleting_a_retired_job_finds_the_file_by_the_slug_it_names_and_removes_only_that_file() -> Result<(), BoxError>
{
    let fixture = SharedFixture::new(
        "name = \"Nightly\"\nslug = \"renamed\"\nready = true\ntimeout_secs = 120\nversion = \"1.0.0\"\n",
    )
    .await?;
    let mut renamed = key();
    renamed.job = "renamed".to_owned();
    support::shared::write(
        &fixture.jobs_root().join("acme/sync/jobs/bystander.toml"),
        JOB_WITH_TIMEOUT,
    )?;
    let version = contextos_workflows::JobFingerprint::from(&contextos_workflows::ReleaseFingerprintInput {
        store: contextos_workflows::StoreKind::Host,
        package: "acme",
        release: "1.0.0",
        digest: "sha256:0",
        job_toml: "",
        release_manifest: "",
        libraries: &[],
    });
    let schedule = JobSchedule::try_from("0 3 * * *")?;
    fixture
        .state()
        .update(|jobs| -> Result<(), contextos_workflows::TransitionError> {
            use contextos_workflows::{Actor, Transition};
            let mut at =
                |actor, transition: Transition| jobs.transition("acme", "sync", "renamed", actor, &transition, FIRST);
            at(Actor::Validator, Transition::Validated(version.clone()))?;
            at(Actor::DryRun, Transition::DryRunCompleted(version.clone()))?;
            at(Actor::Operator, Transition::Enable(schedule, version))?;
            at(Actor::Operator, Transition::Retire)?;
            Ok(())
        })??;
    assert_eq!(
        fixture.read()?.lifecycle_of("acme", "sync", "renamed"),
        JobLifecycle::Retired
    );

    fixture.ask_for(&renamed, OperatorRequest::Delete).await?;

    assert!(!fixture.job_path().exists(), "the file the slug names is removed");
    assert!(
        fixture.jobs_root().join("acme/sync/jobs/bystander.toml").exists(),
        "no other job file is touched"
    );
    Ok(())
}

#[tokio::test]
async fn a_delete_the_lifecycle_refuses_removes_no_file_and_writes_no_state() -> Result<(), BoxError> {
    let fixture = SharedFixture::new(READY_WITH_TIMEOUT).await?;
    fixture.enable().await?;
    let before = fixture.state_text()?;

    let refused = fixture.ask(OperatorRequest::Delete).await;

    assert!(matches!(refused, Err(OperationError::Refused(_))), "{refused:?}");
    assert!(fixture.job_path().exists(), "an enabled job keeps its file");
    assert_eq!(fixture.state_text()?, before);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_delete_whose_state_write_fails_leaves_a_retired_job_without_a_file_that_can_be_deleted_again()
-> Result<(), BoxError> {
    use std::os::unix::fs::PermissionsExt as _;

    let fixture = SharedFixture::new(READY_WITH_TIMEOUT).await?;
    retire(&fixture).await?;
    let state_directory = fixture.root.path().join("state");
    let writable = std::fs::metadata(&state_directory)?.permissions();
    let mut read_only = writable.clone();
    read_only.set_mode(0o555);
    std::fs::set_permissions(&state_directory, read_only)?;
    if std::fs::File::create(state_directory.join("probe")).is_ok() {
        // The process is not held back by directory permissions (it runs as the superuser), so the failure cannot be
        // made.
        std::fs::remove_file(state_directory.join("probe"))?;
        std::fs::set_permissions(&state_directory, writable)?;
        return Ok(());
    }

    let failed = fixture.ask(OperatorRequest::Delete).await;
    std::fs::set_permissions(&state_directory, writable)?;

    assert!(matches!(failed, Err(OperationError::Unavailable(_))), "{failed:?}");
    assert!(!fixture.job_path().exists(), "the file goes first");
    assert_eq!(
        fixture.lifecycle()?,
        "retired",
        "the state write failed, so the job is still retired"
    );
    let again = fixture.ask(OperatorRequest::Delete).await?;
    assert!(matches!(again, OperationOutcome::Changed(Applied::Removed)));
    assert!(fixture.read()?.job("acme", "sync", "nightly").is_none());
    Ok(())
}

#[tokio::test]
async fn deleting_a_job_whose_key_two_files_share_is_refused_and_removes_neither() -> Result<(), BoxError> {
    let fixture = SharedFixture::new(READY_WITH_TIMEOUT).await?;
    retire(&fixture).await?;
    let twin = fixture.jobs_root().join("acme/sync/jobs/twin.toml");
    support::shared::write(
        &twin,
        "name = \"Twin\"\nslug = \"nightly\"\ntimeout_secs = 120\nversion = \"1.0.0\"\n",
    )?;
    let before = fixture.state_text()?;

    let refused = fixture.ask(OperatorRequest::Delete).await;

    let Err(error @ OperationError::KeyTaken(_)) = refused else {
        return Err(format!("expected a key-taken refusal, got {refused:?}").into());
    };
    assert_eq!(error.code(), "workflow/job-key-taken");
    let message = error.to_string();
    assert!(message.contains("twin") && message.contains("nightly"), "{message}");
    assert!(fixture.job_path().exists() && twin.exists(), "neither file is removed");
    assert_eq!(fixture.state_text()?, before, "the state is untouched");
    Ok(())
}

#[tokio::test]
async fn deleting_a_retired_job_whose_file_is_too_large_or_not_text_is_refused_and_writes_no_state()
-> Result<(), BoxError> {
    for (label, bytes) in [
        ("too large", vec![b'#'; 64 * 1024 + 1]),
        ("not UTF-8", vec![0xff, 0xfe, 0x00, 0x80]),
    ] {
        let fixture = SharedFixture::new(READY_WITH_TIMEOUT).await?;
        retire(&fixture).await?;
        std::fs::write(fixture.job_path(), &bytes)?;
        let before = fixture.state_text()?;

        let refused = fixture.ask(OperatorRequest::Delete).await;

        let Err(error) = refused else {
            return Err(format!("{label}: expected a refusal, got {refused:?}").into());
        };
        assert_eq!(error.code(), "workflow/job-store-unavailable", "{label}: {error}");
        assert_eq!(
            std::fs::read(fixture.job_path())?,
            bytes,
            "{label}: the file is not removed"
        );
        assert_eq!(fixture.state_text()?, before, "{label}: the state is untouched");
    }
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_delete_that_cannot_remove_the_file_is_reported_as_the_job_store_being_unavailable() -> Result<(), BoxError> {
    use std::os::unix::fs::PermissionsExt;

    let fixture = SharedFixture::new(READY_WITH_TIMEOUT).await?;
    retire(&fixture).await?;
    let folder = fixture.jobs_root().join("acme/sync/jobs");
    let before = fixture.state_text()?;
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o555))?;

    let refused = fixture.ask(OperatorRequest::Delete).await;
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755))?;

    let Err(error) = refused else {
        return Err(format!("expected a refusal, got {refused:?}").into());
    };
    assert_eq!(error.code(), "workflow/job-store-unavailable", "{error}");
    assert!(fixture.job_path().exists(), "the file is still there");
    assert_eq!(fixture.state_text()?, before, "the state is untouched");
    Ok(())
}

#[tokio::test]
async fn deleting_a_retired_job_whose_tracked_file_has_an_unusable_file_of_its_own_stem_is_refused_with_key_taken_and_removes_nothing()
-> Result<(), BoxError> {
    for (label, bytes) in [
        ("too large", vec![b'#'; 64 * 1024 + 1]),
        ("not UTF-8", vec![0xff, 0xfe, 0x00, 0x80]),
    ] {
        // `nightly.toml` names the job `renamed`, and `renamed.toml` cannot be read as a job file.
        let fixture = SharedFixture::new(
            "name = \"Nightly\"\nslug = \"renamed\"\nready = true\ntimeout_secs = 120\nversion = \"1.0.0\"\n",
        )
        .await?;
        let mut renamed = key();
        renamed.job = "renamed".to_owned();
        let version = contextos_workflows::JobFingerprint::from(&contextos_workflows::ReleaseFingerprintInput {
            store: contextos_workflows::StoreKind::Host,
            package: "acme",
            release: "1.0.0",
            digest: "sha256:0",
            job_toml: "",
            release_manifest: "",
            libraries: &[],
        });
        let schedule = JobSchedule::try_from("0 3 * * *")?;
        fixture
            .state()
            .update(|jobs| -> Result<(), contextos_workflows::TransitionError> {
                use contextos_workflows::{Actor, Transition};
                let mut at = |actor, transition: Transition| {
                    jobs.transition("acme", "sync", "renamed", actor, &transition, FIRST)
                };
                at(Actor::Validator, Transition::Validated(version.clone()))?;
                at(Actor::DryRun, Transition::DryRunCompleted(version.clone()))?;
                at(Actor::Operator, Transition::Enable(schedule, version))?;
                at(Actor::Operator, Transition::Retire)?;
                Ok(())
            })??;
        let unusable = fixture.jobs_root().join("acme/sync/jobs/renamed.toml");
        std::fs::write(&unusable, &bytes)?;
        let before = fixture.state_text()?;

        let refused = fixture.ask_for(&renamed, OperatorRequest::Delete).await;

        let Err(error @ OperationError::KeyTaken(_)) = refused else {
            return Err(format!("{label}: expected a key-taken refusal, got {refused:?}").into());
        };
        assert_eq!(error.code(), "workflow/job-key-taken", "{label}");
        let message = error.to_string();
        assert!(
            message.contains("nightly.toml") && message.contains("renamed.toml"),
            "{label}: both files are named: {message}"
        );
        assert!(
            !message.contains(&fixture.root.path().to_string_lossy().into_owned()),
            "{label}: by name only: {message}"
        );
        assert!(fixture.job_path().exists(), "{label}: the tracked file is not removed");
        assert_eq!(
            std::fs::read(&unusable)?,
            bytes,
            "{label}: the unusable file is not removed"
        );
        assert_eq!(fixture.state_text()?, before, "{label}: no state is written");
    }
    Ok(())
}

fn releases_in_store(fixture: &SharedFixture) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(fixture.store_root().join("acme"))
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

#[tokio::test]
async fn a_validation_that_passes_removes_the_releases_nothing_keeps_and_leaves_the_rest() -> Result<(), BoxError> {
    let fixture = SharedFixture::new(JOB).await?;
    fixture.install("1.0.1", DIGEST, "let a = 1;")?;
    fixture.install("1.0.2", DIGEST, "let a = 2;")?;
    contextos_web::package_store::rebuild_ledger(&fixture.store_root())?;
    assert_eq!(releases_in_store(&fixture), ["1.0.0", "1.0.1", "1.0.2"]);

    let performed = fixture.ask_retaining(OperatorRequest::Validate).await?;

    assert!(performed.warnings.is_empty(), "{:?}", performed.warnings);
    // The job names 1.0.0 and 1.0.2 is the newest future release, so only 1.0.1 is kept by nothing.
    assert_eq!(releases_in_store(&fixture), ["1.0.0", "1.0.2"]);
    Ok(())
}

#[tokio::test]
async fn retiring_a_job_runs_retention_which_keeps_the_current_and_the_newest_future_release() -> Result<(), BoxError> {
    let fixture = SharedFixture::new(READY_WITH_TIMEOUT).await?;
    fixture.enable().await?;
    fixture.install("1.0.1", DIGEST, "let a = 1;")?;
    fixture.install("1.0.2", DIGEST, "let a = 2;")?;
    contextos_web::package_store::rebuild_ledger(&fixture.store_root())?;

    fixture.ask_retaining(OperatorRequest::Retire).await?;

    // 1.0.0 is the current release, 1.0.2 the newest future one, and nothing keeps 1.0.1.
    assert_eq!(releases_in_store(&fixture), ["1.0.0", "1.0.2"]);
    Ok(())
}

#[tokio::test]
async fn a_store_with_no_ledger_gives_the_operation_a_warning_and_the_operation_still_succeeds() -> Result<(), BoxError>
{
    let fixture = SharedFixture::new(JOB).await?;
    fixture.install("1.0.1", DIGEST, "let a = 1;")?;

    let performed = fixture.ask_retaining(OperatorRequest::Validate).await?;

    assert_eq!(fixture.lifecycle()?, "validated");
    assert_eq!(performed.warnings.len(), 1, "{:?}", performed.warnings);
    assert_eq!(releases_in_store(&fixture), ["1.0.0", "1.0.1"]);
    Ok(())
}
