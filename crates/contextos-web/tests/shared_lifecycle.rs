//! The shared validation pass (`contextos_web::shared_lifecycle::shared_validation_pass`) against a real `contextos-mcp`
//! session and a real package store and host state file: a ready draft job is validated once per version, a validated or
//! registered job whose version changes goes back to draft, an enabled job whose version changes is validated at once and
//! put on probation or suspended, and a suspended job is left alone.

mod support;

use contextos_workflows::{Actor, Suspension, SuspensionCause, SuspensionReason, Transition};
use support::BoxError;
use support::shared::{DIGEST, FIRST, OTHER_DIGEST, SharedFixture as Fixture, write};
use time::OffsetDateTime;
use time::macros::datetime;

const LATER: OffsetDateTime = datetime!(2026-10-05 03:00:00 UTC);
const READY: &str = "name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nready = true\n";
const NOT_READY: &str = "name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";

async fn fixture(job: &str) -> Result<Fixture, BoxError> {
    Fixture::new(job).await
}

#[tokio::test]
async fn a_job_that_is_not_ready_is_left_alone_and_nothing_is_written() -> Result<(), BoxError> {
    let fixture = fixture(NOT_READY).await?;

    fixture.pass(FIRST).await?;

    assert!(!fixture.state().state_path().exists());
    Ok(())
}

#[tokio::test]
async fn a_ready_job_that_passes_becomes_validated_for_its_version() -> Result<(), BoxError> {
    let fixture = fixture(READY).await?;

    let report = fixture.pass(FIRST).await?;

    assert_eq!(report.validated, 1);
    assert_eq!(fixture.lifecycle()?, "validated");
    let state = fixture.read()?;
    let job = state.job("acme", "sync", "nightly");
    assert!(job.and_then(|job| job.fingerprint()).is_some());
    assert_eq!(job.and_then(|job| job.validation()).map(|v| v.passed), Some(true));
    Ok(())
}

#[tokio::test]
async fn a_ready_job_that_fails_stays_a_draft_with_every_failure_recorded_once() -> Result<(), BoxError> {
    let fixture = fixture(READY).await?;
    fixture.install("1.0.0", DIGEST, "let y = (2 +;")?;

    let first = fixture.pass(FIRST).await?;
    let again = fixture.pass(LATER).await?;

    assert_eq!(first.failed, 1);
    assert_eq!(again.failed, 0, "the same version is not validated again");
    assert_eq!(fixture.lifecycle()?, "draft");
    let state = fixture.read()?;
    let stored = state.job("acme", "sync", "nightly").and_then(|job| job.validation());
    assert_eq!(stored.map(|v| v.passed), Some(false));
    assert_eq!(stored.map(|v| v.at), Some(FIRST));
    Ok(())
}

#[tokio::test]
async fn fixing_a_failed_job_validates_the_new_version() -> Result<(), BoxError> {
    let fixture = fixture(READY).await?;
    fixture.install("1.0.0", DIGEST, "let y = (2 +;")?;
    fixture.pass(FIRST).await?;

    fixture.install("1.0.1", OTHER_DIGEST, "log_info(\"fixed\");")?;
    fixture.name_release("1.0.1")?;
    fixture.pass(LATER).await?;

    assert_eq!(fixture.lifecycle()?, "validated");
    Ok(())
}

#[tokio::test]
async fn a_validated_job_whose_job_file_changes_goes_back_to_draft_and_is_validated_afresh() -> Result<(), BoxError> {
    let fixture = fixture(READY).await?;
    fixture.pass(FIRST).await?;
    let before = fixture
        .read()?
        .job("acme", "sync", "nightly")
        .and_then(|j| j.fingerprint().cloned());

    write(
        &fixture.job_path(),
        "name = \"Nightly, renamed\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nready = true\n",
    )?;
    let report = fixture.pass(LATER).await?;

    assert_eq!(report.returned_to_draft, 1);
    assert_eq!(report.validated, 1);
    let after = fixture
        .read()?
        .job("acme", "sync", "nightly")
        .and_then(|j| j.fingerprint().cloned());
    assert_ne!(before, after);
    assert_eq!(fixture.lifecycle()?, "validated");
    Ok(())
}

#[tokio::test]
async fn a_newer_release_changes_nothing_for_a_job_that_names_an_older_one_until_the_job_names_it()
-> Result<(), BoxError> {
    let fixture = fixture(READY).await?;
    fixture.pass(FIRST).await?;

    fixture.install("1.1.0", OTHER_DIGEST, "log_info(\"b\");")?;
    let installed = fixture.pass(LATER).await?;
    fixture.name_release("1.1.0")?;
    let named = fixture.pass(LATER).await?;

    assert_eq!(installed.returned_to_draft, 0, "the job still names 1.0.0");
    assert_eq!(named.returned_to_draft, 1, "naming 1.1.0 is a version nobody validated");
    Ok(())
}

#[tokio::test]
async fn the_ready_key_alone_is_not_a_new_version() -> Result<(), BoxError> {
    let fixture = fixture(READY).await?;
    fixture.pass(FIRST).await?;

    write(&fixture.job_path(), NOT_READY)?;
    let report = fixture.pass(LATER).await?;

    assert_eq!(report.returned_to_draft, 0);
    assert_eq!(fixture.lifecycle()?, "validated");
    Ok(())
}

#[tokio::test]
async fn a_new_version_that_passes_keeps_an_enabled_job_on_probation_without_repeating_the_dry_run()
-> Result<(), BoxError> {
    let fixture = fixture(READY).await?;
    fixture.enable().await?;
    assert!(!fixture.on_trial()?);

    // A changed timeout is a new version of the same release, which the job's file may change in place.
    write(
        &fixture.job_path(),
        &READY.replace("timeout_secs = 30", "timeout_secs = 60"),
    )?;
    let report = fixture.pass(LATER).await?;

    assert_eq!(report.on_probation, 1);
    assert_eq!(fixture.lifecycle()?, "enabled");
    assert!(fixture.on_trial()?);
    let state = fixture.read()?;
    assert!(
        state
            .job("acme", "sync", "nightly")
            .and_then(contextos_workflows::JobState::dry_run_at)
            .is_some()
    );
    assert_eq!(state.scheduled_expression("acme", "sync", "nightly"), Some("0 3 * * *"));
    Ok(())
}

#[tokio::test]
async fn a_new_version_that_fails_suspends_an_enabled_job_with_the_failures_as_the_reason() -> Result<(), BoxError> {
    let fixture = fixture(READY).await?;
    fixture.enable().await?;

    fixture.install("1.1.0", OTHER_DIGEST, "let y = (2 +;")?;
    fixture.name_release("1.1.0")?;
    let report = fixture.pass(LATER).await?;

    assert_eq!(report.suspended, 1);
    assert_eq!(fixture.lifecycle()?, "suspended");
    let state = fixture.read()?;
    let suspension = state
        .job("acme", "sync", "nightly")
        .and_then(contextos_workflows::JobState::suspension);
    assert_eq!(
        suspension.map(|suspension| suspension.code.as_str()),
        Some("workflow/validation-failed")
    );
    assert_eq!(
        suspension.and_then(|suspension| suspension.by),
        Some(contextos_workflows::Actor::System)
    );
    assert!(
        suspension.is_some_and(|suspension| suspension.reason.contains("validation/script-syntax")),
        "{suspension:?}"
    );
    assert_eq!(state.scheduled_expression("acme", "sync", "nightly"), None);
    Ok(())
}

#[tokio::test]
async fn an_enabled_job_whose_package_was_removed_is_suspended() -> Result<(), BoxError> {
    let fixture = fixture(READY).await?;
    fixture.enable().await?;

    std::fs::remove_dir_all(fixture.store_root().join("acme"))?;
    fixture.pass(LATER).await?;

    assert_eq!(fixture.lifecycle()?, "suspended");
    Ok(())
}

#[tokio::test]
async fn an_enabled_job_whose_version_has_not_changed_is_left_untouched() -> Result<(), BoxError> {
    let fixture = fixture(READY).await?;
    fixture.enable().await?;
    let before = fixture.state_text()?;

    fixture.pass(LATER).await?;

    assert_eq!(fixture.state_text()?, before);
    Ok(())
}

#[tokio::test]
async fn a_suspended_job_whose_version_changes_stays_suspended_and_is_not_validated() -> Result<(), BoxError> {
    let fixture = fixture(READY).await?;
    fixture.enable().await?;
    fixture
        .change(
            Actor::Operator,
            Transition::Suspend(contextos_workflows::Suspension::operator(SuspensionReason::try_from(
                "stopped by the operator",
            )?)),
            FIRST,
        )
        .await?;
    let before = fixture.state_text()?;

    fixture.install("1.1.0", OTHER_DIGEST, "let y = (2 +;")?;
    fixture.name_release("1.1.0")?;
    fixture.pass(LATER).await?;

    assert_eq!(fixture.state_text()?, before);
    Ok(())
}

#[tokio::test]
async fn a_new_version_that_could_not_be_fully_checked_leaves_an_enabled_job_as_it_is() -> Result<(), BoxError> {
    let fixture = fixture(READY).await?;
    fixture.enable().await?;
    // The release's script now calls a server that is configured but cannot be reached, which can be neither passed nor
    // failed. The job's file still names the same release.
    let dir = fixture.store_root().join("acme/1.0.0");
    std::fs::remove_dir_all(&dir)?;
    write(
        &dir.join("manifest.toml"),
        &format!(
            "name = \"acme\"\nversion = \"1.0.0\"\ndigest = \"{OTHER_DIGEST}\"\n\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n\n[[workflow.permitted_effects]]\nmcp_server_name = \"crm\"\ntool = \"list_contacts\"\n"
        ),
    )?;
    write(&dir.join("sync.rhai"), "()")?;
    support::release_fixture::seal_io(&dir)?;

    let report = fixture
        .pass_with(LATER, &["contextos".to_owned(), "crm".to_owned()])
        .await?;

    assert_eq!(report.on_probation + report.suspended, 0);
    assert_eq!(fixture.lifecycle()?, "enabled");
    assert!(!fixture.on_trial()?, "it is not on trial for a version nobody accepted");
    Ok(())
}

#[tokio::test]
async fn a_job_is_tracked_under_the_slug_its_manifest_names() -> Result<(), BoxError> {
    let fixture =
        fixture("name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nready = true\nslug = \"renamed\"\n")
            .await?;

    fixture.pass(FIRST).await?;

    let state = fixture.read()?;
    assert_eq!(state.lifecycle_of("acme", "sync", "renamed").as_str(), "validated");
    assert!(state.job("acme", "sync", "nightly").is_none());
    Ok(())
}

#[tokio::test]
async fn a_job_whose_manifest_names_an_unsafe_slug_gets_no_state_entry() -> Result<(), BoxError> {
    let fixture =
        fixture("name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nready = true\nslug = \"../escape\"\n")
            .await?;

    fixture.pass(FIRST).await?;

    assert!(!fixture.state().state_path().exists());
    Ok(())
}

#[tokio::test]
async fn jobs_of_different_packages_are_validated_independently() -> Result<(), BoxError> {
    let fixture = fixture(READY).await?;
    let dir = fixture.store_root().join("globex/1.0.0");
    write(
        &dir.join("manifest.toml"),
        &format!(
            "name = \"globex\"\nversion = \"1.0.0\"\ndigest = \"{DIGEST}\"\n\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n"
        ),
    )?;
    write(&dir.join("sync.rhai"), "let y = (2 +;")?;
    write(&fixture.jobs_root().join("globex/sync/jobs/nightly.toml"), READY)?;

    fixture.pass(FIRST).await?;

    let state = fixture.read()?;
    assert_eq!(state.lifecycle_of("acme", "sync", "nightly").as_str(), "validated");
    assert_eq!(state.lifecycle_of("globex", "sync", "nightly").as_str(), "draft");
    Ok(())
}

const READY_WITH_TIMEOUT: &str = "name = \"Nightly\"\nready = true\ntimeout_secs = 30\nversion = \"1.0.0\"\n";

#[tokio::test]
async fn a_shared_job_suspended_for_a_removed_package_stores_package_unavailable() -> Result<(), BoxError> {
    let fixture = fixture(READY_WITH_TIMEOUT).await?;
    fixture.enable().await?;

    std::fs::remove_dir_all(fixture.store_root().join("acme"))?;
    fixture.pass(LATER).await?;

    assert_eq!(fixture.lifecycle()?, "suspended");
    let state = fixture.read()?;
    let record = state
        .job("acme", "sync", "nightly")
        .and_then(|job| job.suspension())
        .ok_or("the suspension was not stored as a record")?;
    assert_eq!(record.code, "workflow/package-unavailable");
    assert_eq!(record.by, Some(Actor::System));
    assert_eq!(record.at, Some(LATER));
    assert_eq!(record.run, None);
    Ok(())
}

#[tokio::test]
async fn a_shared_first_run_failure_stores_probation_failed_with_its_run_number() -> Result<(), BoxError> {
    let fixture = fixture(READY_WITH_TIMEOUT).await?;
    fixture.enable_on("0 3 * * *", FIRST).await?;
    let version = fixture.recorded_version()?;

    fixture
        .change(
            Actor::System,
            Transition::ProbationFailed(
                version,
                Suspension {
                    cause: SuspensionCause::ProbationFailed {
                        run: Some(7),
                        failure: Some("workflow/script-error".to_owned()),
                    },
                    reason: SuspensionReason::try_from("the first run failed")?,
                },
            ),
            LATER,
        )
        .await?;

    assert_eq!(fixture.lifecycle()?, "suspended");
    let state = fixture.read()?;
    let record = state
        .job("acme", "sync", "nightly")
        .and_then(|job| job.suspension())
        .ok_or("the suspension was not stored as a record")?;
    assert_eq!(record.code, "workflow/probation-failed");
    assert_eq!(record.by, Some(Actor::System));
    assert_eq!(record.run, Some(7));
    assert_eq!(record.failure.as_deref(), Some("workflow/script-error"));
    assert_eq!(record.reason, "the first run failed");
    assert!(fixture.on_trial()?, "the trial stays pending until a run succeeds");
    Ok(())
}
