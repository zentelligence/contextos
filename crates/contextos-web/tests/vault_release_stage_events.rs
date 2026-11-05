//! What events on a vault's releases do to a job in each lifecycle stage: a release removed by force, a release that
//! stops being current, a direct edit of `version` or `predecessor`, and an install at a version. Each runs against a
//! real `contextos-mcp` session and a real temporary vault.

mod support;

use contextos_web::job_operation::{JobStore, OperationError, OperatorRequest, perform};
use contextos_web::job_operation_vault::VaultJobStore;
use contextos_web::vault_source_install::SourceInstall;
use contextos_web::vault_source_read::{SourceKind, SourceTarget};
use contextos_workflows::{JobSchedule, JobState, PackageName, ReleaseKind, TransitionError};
use support::BoxError;
use support::jobs::{
    JobVault, STATE_PATH, Spec, VAULT_NAME, current_enabled_state, current_state, enabled_state, lifecycle_state,
    suspended_state,
};
use time::macros::datetime;

const NOW: time::OffsetDateTime = datetime!(2026-10-02 03:00:00 UTC);
const NIGHTLY: &str = "registry/workflows/ops/sync/jobs/nightly.toml";
/// Release 1.0.1 is current and 1.0.0, which the job names, is previous.
const PREVIOUS_NAMED: &str = "\n[ops.sync]\ncurrent = \"1.0.1\"\nprevious = \"1.0.0\"\n";
/// Release 1.0.0 is current and 1.0.1 is future.
const FUTURE_AVAILABLE: &str = "\n[ops.sync]\ncurrent = \"1.0.0\"\nfuture = \"1.0.1\"\n";

/// A vault whose store holds releases 1.0.0 and 1.0.1 of `ops` with a ledger, with `state` as its job state followed
/// by `releases`.
async fn vault_with_two_releases(state: &str, releases: &str) -> Result<JobVault, BoxError> {
    let vault = JobVault::open_with_releases(&Spec::default(), &["1.0.1"]).await?;
    assert!(vault.root().join(".contextos/packages/ops/1.0.1").is_dir());
    // A job validated on a release has that release recorded with it.
    let recorded = state.replace("[ops.sync.nightly]\n", "[ops.sync.nightly]\nrelease = \"1.0.0\"\n");
    vault.write(STATE_PATH, &format!("{recorded}{releases}"))?;
    Ok(vault)
}

/// A vault whose store holds release 1.0.0 of `ops` with a ledger, with `state` as its job state.
async fn vault_with_ledger(state: Option<&str>) -> Result<JobVault, BoxError> {
    let vault = JobVault::open(&Spec {
        state,
        ..Spec::default()
    })
    .await?;
    vault.rebuild_ledger().await?;
    Ok(vault)
}

/// Validates the job as the operator would and returns the failures the state records, as `(code, artefact)`.
async fn validate(vault: &JobVault) -> Result<Vec<(String, String)>, BoxError> {
    let client = vault.clients.get("contextos").ok_or("no primary server")?;
    let servers = vec!["contextos".to_owned()];
    let store = VaultJobStore {
        client: &client,
        vault_name: VAULT_NAME,
        known_mcp_servers: &servers,
        package: "ops",
        workflow_slug: "sync",
        job_slug: "nightly",
        claims_root: vault.host.claims_root.as_deref(),
    };
    store.validate(NOW).await?;
    // What the state keeps is what the link and release rules concluded; the result of the checks alone leaves them out.
    let state = vault.state()?;
    let recorded = state
        .job("ops", "sync", "nightly")
        .and_then(JobState::validation)
        .ok_or("no validation was recorded")?;
    Ok(recorded
        .failures
        .iter()
        .map(|failure| (failure.code.clone(), failure.artefact.clone()))
        .collect())
}

fn suspension_code(vault: &JobVault) -> Result<String, BoxError> {
    let state = vault.state()?;
    let record = state
        .job("ops", "sync", "nightly")
        .and_then(JobState::suspension)
        .ok_or("the job has no suspension record")?;
    Ok(record.code.clone())
}

async fn operate(vault: &JobVault, request: OperatorRequest) -> Result<(), OperationError> {
    let client = vault
        .clients
        .get("contextos")
        .ok_or(OperationError::Unreadable("client"))?;
    let servers = vec!["contextos".to_owned()];
    let store = VaultJobStore {
        client: &client,
        vault_name: VAULT_NAME,
        known_mcp_servers: &servers,
        package: "ops",
        workflow_slug: "sync",
        job_slug: "nightly",
        claims_root: vault.host.claims_root.as_deref(),
    };
    perform(&store, request, NOW).await.map(|_| ())
}

#[tokio::test]
async fn forcing_the_removal_of_an_enabled_jobs_release_has_the_next_pass_suspend_it_as_package_unavailable()
-> Result<(), BoxError> {
    let spec = Spec::default();
    let state = current_enabled_state(&spec);
    let vault = vault_with_ledger(Some(&state)).await?;

    vault.remove_release_forced("ops", "1.0.0").await?;
    assert_eq!(vault.lifecycle()?, "enabled", "the removal alone moves no job");
    vault.pass(NOW).await;

    assert_eq!(vault.lifecycle()?, "suspended");
    assert_eq!(suspension_code(&vault)?, "workflow/package-unavailable");
    Ok(())
}

#[tokio::test]
async fn a_draft_whose_release_has_become_previous_fails_validation_on_its_version() -> Result<(), BoxError> {
    let vault = vault_with_two_releases("", PREVIOUS_NAMED).await?;

    let validation = validate(&vault).await?;

    assert_eq!(
        validation,
        [("validation/artefact-invalid".to_owned(), "version".to_owned())]
    );
    Ok(())
}

#[tokio::test]
async fn a_registered_job_whose_release_has_become_previous_cannot_be_enabled_and_keeps_its_stage()
-> Result<(), BoxError> {
    let spec = Spec::default();
    let state = current_state(&spec, "registered");
    let vault = vault_with_two_releases(&state, PREVIOUS_NAMED).await?;
    let before = vault.read(STATE_PATH)?;

    let refused = operate(&vault, OperatorRequest::Schedule(JobSchedule::try_from("0 5 * * *")?)).await;

    let Err(OperationError::Refused(TransitionError::KindRefused { kind, .. })) = &refused else {
        return Err(format!("expected the release kind to refuse: {refused:?}").into());
    };
    assert_eq!(*kind, ReleaseKind::Previous);
    assert_eq!(vault.lifecycle()?, "registered");
    assert_eq!(vault.read(STATE_PATH)?, before, "a refusal writes nothing");
    Ok(())
}

#[tokio::test]
async fn a_return_successor_naming_the_previous_release_is_not_failed_on_its_version() -> Result<(), BoxError> {
    let spec = Spec::default();
    let predecessor = current_enabled_state(&spec).replace("[ops.sync.nightly]", "[ops.sync.old]");
    let vault = vault_with_two_releases(
        &predecessor,
        "\n[ops.sync]\ncurrent = \"1.0.1\"\nprevious = \"1.0.0\"\n",
    )
    .await?;
    // `old` is enabled on 1.0.1 (current) in this arrangement; the return successor names previous 1.0.0.
    vault.write(
        "registry/workflows/ops/sync/jobs/old.toml",
        "ready = true\nname = \"Old\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.1\"\n",
    )?;
    vault.write(
        NIGHTLY,
        "ready = true\nname = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\npredecessor = \"old\"\n",
    )?;

    let validation = validate(&vault).await?;

    assert!(
        !validation.iter().any(|(_, artefact)| artefact == "version"),
        "{validation:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_draft_naming_a_predecessor_that_does_not_exist_fails_validation_on_its_predecessor() -> Result<(), BoxError>
{
    let vault = vault_with_ledger(None).await?;
    vault.write(
        NIGHTLY,
        &format!("{}predecessor = \"gone\"\n", support::jobs::READY_JOB),
    )?;

    let validation = validate(&vault).await?;

    assert_eq!(
        validation,
        [("validation/artefact-invalid".to_owned(), "predecessor".to_owned())]
    );
    Ok(())
}

#[tokio::test]
async fn a_direct_edit_of_an_enabled_jobs_version_suspends_it_as_validation_failed() -> Result<(), BoxError> {
    let spec = Spec::default();
    let state = current_enabled_state(&spec);
    let vault = vault_with_two_releases(&state, FUTURE_AVAILABLE).await?;
    vault.name_version("1.0.1")?;

    vault.pass(NOW).await;

    assert_eq!(vault.lifecycle()?, "suspended");
    assert_eq!(suspension_code(&vault)?, "workflow/validation-failed");
    Ok(())
}

#[tokio::test]
async fn a_direct_edit_adding_a_predecessor_to_an_enabled_job_suspends_it_as_validation_failed() -> Result<(), BoxError>
{
    let spec = Spec::default();
    let state = current_enabled_state(&spec);
    let vault = vault_with_two_releases(&state, FUTURE_AVAILABLE).await?;
    vault.write(
        NIGHTLY,
        &format!("{}predecessor = \"gone\"\n", support::jobs::READY_JOB),
    )?;

    vault.pass(NOW).await;

    assert_eq!(vault.lifecycle()?, "suspended");
    assert_eq!(suspension_code(&vault)?, "workflow/validation-failed");
    Ok(())
}

#[tokio::test]
async fn re_activating_a_retired_job_whose_version_was_edited_by_hand_to_a_retired_release_is_refused()
-> Result<(), BoxError> {
    let spec = Spec::default();
    let state = lifecycle_state(&spec, "retired");
    let marker = format!(
        "{FUTURE_AVAILABLE}\n[ops.sync.\"1.0.1\".retired]\ncause = \"superseded\"\nat = \"2026-09-01T00:00:00Z\"\n"
    );
    let vault = vault_with_two_releases(&state, &marker).await?;
    vault.name_version("1.0.1")?;
    let before = vault.read(STATE_PATH)?;

    let refused = operate(&vault, OperatorRequest::Reactivate).await;

    let Err(OperationError::Refused(TransitionError::FileDiffersFromRecord {
        key, recorded, named, ..
    })) = &refused
    else {
        return Err(format!("expected the file to differ from the record: {refused:?}").into());
    };
    assert_eq!((*key, recorded.as_str(), named.as_str()), ("version", "1.0.0", "1.0.1"));
    assert_eq!(vault.lifecycle()?, "retired");
    assert_eq!(vault.read(STATE_PATH)?, before);
    Ok(())
}

/// The failures the state records for the job's last validation, as `(code, artefact)`.
fn recorded_failures(vault: &JobVault) -> Result<Vec<(String, String)>, BoxError> {
    let state = vault.state()?;
    let recorded = state
        .job("ops", "sync", "nightly")
        .and_then(JobState::validation)
        .ok_or("no validation was recorded")?;
    Ok(recorded
        .failures
        .iter()
        .map(|failure| (failure.code.clone(), failure.artefact.clone()))
        .collect())
}

fn recorded_release(vault: &JobVault) -> Result<Option<semver::Version>, BoxError> {
    Ok(vault
        .state()?
        .job("ops", "sync", "nightly")
        .and_then(JobState::release)
        .cloned())
}

/// A suspended job enabled on 1.0.0, with the releases 1.0.0 and 1.0.1 installed, and `releases` recorded.
async fn suspended_on_first_release(releases: &str) -> Result<JobVault, BoxError> {
    let spec = Spec::default();
    vault_with_two_releases(&suspended_state(&spec), releases).await
}

const RETIRED_SECOND: &str = "\n[ops.sync.\"1.0.1\".retired]\ncause = \"superseded\"\nat = \"2026-09-01T00:00:00Z\"\n";

#[tokio::test]
async fn resuming_a_suspended_job_whose_version_was_edited_to_a_retired_release_is_refused_and_it_stays_suspended()
-> Result<(), BoxError> {
    let vault = suspended_on_first_release(&format!("{FUTURE_AVAILABLE}{RETIRED_SECOND}")).await?;
    vault.name_version("1.0.1")?;

    let refused = operate(&vault, OperatorRequest::Resume).await;

    assert!(refused.is_err(), "resume must fail validation: {refused:?}");
    assert_eq!(
        recorded_failures(&vault)?,
        [("validation/artefact-invalid".to_owned(), "version".to_owned())]
    );
    assert_eq!(vault.lifecycle()?, "suspended");
    assert_eq!(recorded_release(&vault)?, Some(semver::Version::new(1, 0, 0)));
    Ok(())
}

#[tokio::test]
async fn resuming_a_suspended_job_whose_version_was_edited_to_a_future_release_is_refused_and_it_stays_suspended()
-> Result<(), BoxError> {
    let vault = suspended_on_first_release(FUTURE_AVAILABLE).await?;
    vault.name_version("1.0.1")?;

    let refused = operate(&vault, OperatorRequest::Resume).await;

    assert!(refused.is_err(), "resume must fail validation: {refused:?}");
    assert_eq!(
        recorded_failures(&vault)?,
        [("validation/artefact-invalid".to_owned(), "version".to_owned())]
    );
    assert_eq!(vault.lifecycle()?, "suspended");
    assert_eq!(recorded_release(&vault)?, Some(semver::Version::new(1, 0, 0)));
    Ok(())
}

#[tokio::test]
async fn resuming_a_suspended_job_whose_predecessor_was_edited_in_is_refused_and_restoring_the_file_lets_it_resume()
-> Result<(), BoxError> {
    let spec = Spec::default();
    let other = enabled_state(&spec).replace("[ops.sync.nightly]", "[ops.sync.other]\nidentity = \"job-other\"");
    let state = format!("{}\n{other}", suspended_state(&spec));
    let vault = vault_with_two_releases(&state, FUTURE_AVAILABLE).await?;
    let original = vault.read(NIGHTLY)?;
    vault.write(NIGHTLY, &format!("{original}predecessor = \"other\"\n"))?;

    let refused = operate(&vault, OperatorRequest::Resume).await;

    assert!(refused.is_err(), "resume must fail validation: {refused:?}");
    assert!(
        recorded_failures(&vault)?.contains(&("validation/artefact-invalid".to_owned(), "predecessor".to_owned())),
        "{:?}",
        recorded_failures(&vault)?
    );
    assert_eq!(vault.lifecycle()?, "suspended");
    assert_eq!(recorded_release(&vault)?, Some(semver::Version::new(1, 0, 0)));

    vault.write(NIGHTLY, &original)?;
    operate(&vault, OperatorRequest::Resume).await?;
    assert_eq!(vault.lifecycle()?, "enabled", "restoring the file lets the job resume");
    Ok(())
}

#[tokio::test]
async fn resuming_a_suspended_job_whose_file_is_unedited_enables_it_on_the_release_it_was_enabled_on()
-> Result<(), BoxError> {
    let vault = suspended_on_first_release(FUTURE_AVAILABLE).await?;

    operate(&vault, OperatorRequest::Resume).await?;

    assert_eq!(vault.lifecycle()?, "enabled");
    assert_eq!(recorded_release(&vault)?, Some(semver::Version::new(1, 0, 0)));
    Ok(())
}

#[tokio::test]
async fn resuming_a_suspended_job_whose_file_was_restored_after_a_version_edit_succeeds() -> Result<(), BoxError> {
    let vault = suspended_on_first_release(FUTURE_AVAILABLE).await?;
    vault.name_version("1.0.1")?;
    assert!(operate(&vault, OperatorRequest::Resume).await.is_err());
    vault.name_version("1.0.0")?;

    operate(&vault, OperatorRequest::Resume).await?;

    assert_eq!(vault.lifecycle()?, "enabled");
    assert_eq!(recorded_release(&vault)?, Some(semver::Version::new(1, 0, 0)));
    Ok(())
}

#[tokio::test]
async fn re_activating_a_retired_job_whose_version_was_edited_to_a_future_release_is_refused_and_restoring_the_file_allows_it()
-> Result<(), BoxError> {
    let spec = Spec::default();
    let state = lifecycle_state(&spec, "retired");
    let vault = vault_with_two_releases(&state, FUTURE_AVAILABLE).await?;
    vault.name_version("1.0.1")?;
    let before = vault.state()?;

    let refused = operate(&vault, OperatorRequest::Reactivate).await;

    assert!(refused.is_err(), "re-activation must fail: {refused:?}");
    assert_eq!(vault.lifecycle()?, "retired");
    assert_eq!(recorded_release(&vault)?, Some(semver::Version::new(1, 0, 0)));
    assert_eq!(moved(&vault.state()?), moved(&before));

    vault.name_version("1.0.0")?;
    operate(&vault, OperatorRequest::Reactivate).await?;
    assert_eq!(vault.lifecycle()?, "draft", "restoring the file allows re-activation");
    Ok(())
}

#[tokio::test]
async fn an_enabled_job_whose_version_was_edited_to_a_release_that_is_not_installed_is_suspended_as_validation_failed()
-> Result<(), BoxError> {
    let spec = Spec::default();
    let state = current_enabled_state(&spec);
    let vault = vault_with_two_releases(&state, FUTURE_AVAILABLE).await?;
    vault.name_version("9.9.9")?;

    vault.pass(NOW).await;

    assert_eq!(vault.lifecycle()?, "suspended");
    assert_eq!(suspension_code(&vault)?, "workflow/validation-failed");
    Ok(())
}

/// The recorded `current` and `previous` of `ops`' `sync`, which only an enabling may move.
fn moved(state: &contextos_workflows::JobsState) -> (Option<semver::Version>, Option<semver::Version>) {
    state
        .package_releases("ops")
        .and_then(|held| held.workflows.get("sync"))
        .map_or((None, None), |versions| {
            (versions.current.clone(), versions.previous.clone())
        })
}

async fn install_at(
    vault: &JobVault,
    version: &str,
) -> Result<contextos_web::vault_source_install::SourceInstalled, BoxError> {
    let client = vault.clients.get("contextos").ok_or("no primary server")?;
    Ok(SourceInstall {
        client: &client,
        vault: VAULT_NAME,
    }
    .install(
        &SourceTarget {
            kind: SourceKind::Workflows,
            name: PackageName::try_from("ops")?,
        },
        Some(&semver::Version::parse(version)?),
    )
    .await?)
}

#[tokio::test]
async fn installing_a_present_release_at_its_version_moves_no_job_in_any_stage() -> Result<(), BoxError> {
    let spec = Spec::default();
    let stages = [
        ("draft", String::new()),
        ("validated", current_state(&spec, "validated")),
        ("registered", current_state(&spec, "registered")),
        ("enabled", current_enabled_state(&spec)),
        ("suspended", suspended_state(&spec)),
        ("retired", lifecycle_state(&spec, "retired")),
    ];
    for (stage, state) in stages {
        let vault = vault_with_ledger((!state.is_empty()).then_some(state.as_str())).await?;
        let before = vault.state()?;
        let lifecycle = vault.lifecycle()?;

        let installed = install_at(&vault, "1.0.0").await?;

        assert_eq!(installed.release.version, "1.0.0", "{stage}");
        assert_eq!(vault.lifecycle()?, lifecycle, "{stage}");
        assert_eq!(
            vault.state()?.job("ops", "sync", "nightly"),
            before.job("ops", "sync", "nightly"),
            "{stage}: install at a version moves no job"
        );
        assert_eq!(
            moved(&vault.state()?),
            moved(&before),
            "{stage}: current and previous stay"
        );
    }
    Ok(())
}

#[tokio::test]
async fn repairing_a_removed_release_at_its_version_moves_no_job_and_restores_it() -> Result<(), BoxError> {
    let spec = Spec::default();
    let state = current_enabled_state(&spec);
    let vault = vault_with_ledger(Some(&state)).await?;
    vault.remove_release_forced("ops", "1.0.0").await?;
    let before = vault.state()?;

    install_at(&vault, "1.0.0").await?;

    assert!(
        vault
            .root()
            .join(".contextos/packages/ops/1.0.0/manifest.toml")
            .is_file()
    );
    assert_eq!(
        vault.state()?.job("ops", "sync", "nightly"),
        before.job("ops", "sync", "nightly")
    );
    assert_eq!(moved(&vault.state()?), moved(&before));
    vault.pass(NOW).await;
    assert_eq!(
        vault.lifecycle()?,
        "enabled",
        "the repaired release keeps the job enabled"
    );
    Ok(())
}
