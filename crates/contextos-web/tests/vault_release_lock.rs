//! The library lock of a workflow release is recorded at the first passing validation of any job on it, in the same
//! write that validates the job, and every later validation reads it and resolves nothing.

mod support;

use contextos_web::job_validation::{JobLocation, validate_and_record};
use semver::Version;
use support::BoxError;
use support::jobs::{JobVault, READY_JOB, Spec, VAULT_NAME};
use time::macros::datetime;

const WORKFLOW: &str = "name = \"Sync\"\nentry = \"sync.rhai\"\nmcp_servers.allow = [\"contextos\"]\n";
const PACKAGE: &str = "workflows = [\"sync\"]\n[[libraries]]\nname = \"crm\"\nrequirement = \"*\"\n";
const LIBRARY: &str = "modules = [\"client.rhai\"]\nexports = [\"client.rhai\"]\n";

async fn vault_with_library() -> Result<JobVault, BoxError> {
    let vault = JobVault::open(&Spec {
        package: PACKAGE,
        workflow: WORKFLOW,
        ..Spec::default()
    })
    .await?;
    vault.write("registry/libraries/crm/library.toml", LIBRARY)?;
    vault.write("registry/libraries/crm/client.rhai", "fn a() { 1 }")?;
    vault.install_releases()?;
    Ok(vault)
}

/// Holds the vault's package store lock as another `contextos-web` operation does.
fn hold_store_lock(vault: &JobVault) -> Result<contextos_fs::HostLockGuard, BoxError> {
    Ok(contextos_fs::lock_host(
        &vault.root().join(".contextos/packages/.lock"),
        std::time::Duration::ZERO,
    )?)
}

async fn validate(vault: &JobVault, file: &str) -> Result<bool, BoxError> {
    let client = vault.clients.get("contextos").ok_or("no primary server")?;
    let location = JobLocation {
        package: "ops".to_owned(),
        directory: "sync".to_owned(),
        file: file.to_owned(),
    };
    let (validation, _) = validate_and_record(
        &client,
        VAULT_NAME,
        &["contextos".to_owned()],
        &location,
        datetime!(2026-10-01 03:00:00 UTC),
    )
    .await?;
    Ok(validation.report.passed())
}

fn locked_library(vault: &JobVault) -> Result<Option<Version>, BoxError> {
    Ok(vault
        .state()?
        .library_lock("ops", "sync", &Version::new(1, 0, 0))
        .and_then(|lock| lock.libraries.get("crm"))
        .map(|library| library.version.clone()))
}

#[tokio::test]
async fn the_first_passing_validation_records_the_library_lock_of_its_workflow_release() -> Result<(), BoxError> {
    let vault = vault_with_library().await?;

    assert!(validate(&vault, "nightly.toml").await?);

    assert_eq!(locked_library(&vault)?, Some(Version::new(1, 0, 0)));
    let job = vault.state()?;
    let entry = job.job("ops", "sync", "nightly").ok_or("no job entry")?;
    assert!(entry.identity().is_some(), "the job is given an identity");
    Ok(())
}

#[tokio::test]
async fn a_later_validation_reads_the_recorded_lock_and_chooses_no_newer_library_release() -> Result<(), BoxError> {
    let vault = vault_with_library().await?;
    assert!(validate(&vault, "nightly.toml").await?);
    vault.write("registry/libraries/crm/client.rhai", "fn a() { 2 }")?;
    let store = vault.root().join(".contextos/packages/crm/1.1.0");
    support::release_fixture::write_whole_release(
        &store,
        ("crm", "1.1.0"),
        "modules = [\"client.rhai\"]\nexports = [\"client.rhai\"]\n",
        &[("client.rhai", b"fn a() { 2 }")],
    )
    .map_err(|error| error.to_string())?;
    vault.write("registry/workflows/ops/sync/jobs/second.toml", READY_JOB)?;

    assert!(validate(&vault, "second.toml").await?);

    assert_eq!(locked_library(&vault)?, Some(Version::new(1, 0, 0)));
    Ok(())
}

/// A pass over a vault whose `enabled` job was last reviewed for another version, so the version it has now is a new one
/// under an enabled job, which the pass validates and puts on trial.
async fn pass_over_an_enabled_job_with_a_new_version() -> Result<JobVault, BoxError> {
    let earlier = Spec::default();
    let state = support::jobs::enabled_state(&earlier);
    let vault = JobVault::open(&Spec {
        package: PACKAGE,
        workflow: WORKFLOW,
        state: Some(&state),
        tasks: false,
        ..Spec::default()
    })
    .await?;
    vault.write("registry/libraries/crm/library.toml", LIBRARY)?;
    vault.write("registry/libraries/crm/client.rhai", "fn a() { 1 }")?;
    vault.install_releases()?;
    // The entry script does not use the library; naming it is enough for the release to depend on it.
    let client = vault.clients.get("contextos").ok_or("no primary server")?;
    contextos_web::job_validation::validation_pass(
        &client,
        VAULT_NAME,
        &["contextos".to_owned()],
        datetime!(2026-10-01 03:00:00 UTC),
    )
    .await?;
    Ok(vault)
}

#[tokio::test]
async fn the_new_version_of_an_enabled_job_that_passes_fixes_its_releases_library_lock() -> Result<(), BoxError> {
    let vault = pass_over_an_enabled_job_with_a_new_version().await?;

    assert_eq!(vault.lifecycle()?, "enabled");
    assert_eq!(locked_library(&vault)?, Some(Version::new(1, 0, 0)));
    Ok(())
}

#[tokio::test]
async fn a_library_release_installed_after_an_enabled_jobs_review_does_not_make_its_version_new_again()
-> Result<(), BoxError> {
    let vault = pass_over_an_enabled_job_with_a_new_version().await?;
    let reviewed = vault
        .state()?
        .job("ops", "sync", "nightly")
        .and_then(|job| job.fingerprint().cloned());
    support::release_fixture::write_whole_release(
        &vault.root().join(".contextos/packages/crm/1.1.0"),
        ("crm", "1.1.0"),
        "modules = [\"client.rhai\"]\nexports = [\"client.rhai\"]\n",
        &[("client.rhai", b"fn a() { 2 }")],
    )
    .map_err(|error| error.to_string())?;
    let client = vault.clients.get("contextos").ok_or("no primary server")?;

    let pass = contextos_web::job_validation::validation_pass(
        &client,
        VAULT_NAME,
        &["contextos".to_owned()],
        datetime!(2026-10-02 03:00:00 UTC),
    )
    .await?;

    assert_eq!(
        pass.on_probation, 0,
        "the reviewed lock still names the first library release"
    );
    let after = vault
        .state()?
        .job("ops", "sync", "nightly")
        .and_then(|job| job.fingerprint().cloned());
    assert_eq!(after, reviewed);
    Ok(())
}

#[tokio::test]
async fn a_validation_made_while_another_operation_holds_the_vault_store_lock_is_not_recorded() -> Result<(), BoxError>
{
    let vault = vault_with_library().await?;
    let client = vault.clients.get("contextos").ok_or("no primary server")?;
    let held = hold_store_lock(&vault)?;
    let location = JobLocation {
        package: "ops".to_owned(),
        directory: "sync".to_owned(),
        file: "nightly.toml".to_owned(),
    };

    let (_, recording) = validate_and_record(
        &client,
        VAULT_NAME,
        &["contextos".to_owned()],
        &location,
        datetime!(2026-10-01 03:00:00 UTC),
    )
    .await?;

    assert!(
        matches!(
            recording,
            contextos_web::job_validation::Recording::NotRecorded { conflict: true, .. }
        ),
        "{recording:?}"
    );
    assert_eq!(locked_library(&vault)?, None);
    drop(held);
    assert!(validate(&vault, "nightly.toml").await?);
    assert_eq!(locked_library(&vault)?, Some(Version::new(1, 0, 0)));
    Ok(())
}

#[tokio::test]
async fn a_pass_made_while_another_operation_holds_the_vault_store_lock_validates_nothing() -> Result<(), BoxError> {
    let vault = vault_with_library().await?;
    vault.write("registry/workflows/ops/sync/jobs/ready.toml", READY_JOB)?;
    let client = vault.clients.get("contextos").ok_or("no primary server")?;
    let held = hold_store_lock(&vault)?;
    let at = datetime!(2026-10-01 03:00:00 UTC);

    let report =
        contextos_web::job_validation::validation_pass(&client, VAULT_NAME, &["contextos".to_owned()], at).await?;

    assert_eq!(report.validated, 0, "{report:?}");
    assert_eq!(locked_library(&vault)?, None);
    drop(held);
    let report =
        contextos_web::job_validation::validation_pass(&client, VAULT_NAME, &["contextos".to_owned()], at).await?;
    assert_eq!(report.validated, 2, "{report:?}");
    assert_eq!(locked_library(&vault)?, Some(Version::new(1, 0, 0)));
    Ok(())
}
