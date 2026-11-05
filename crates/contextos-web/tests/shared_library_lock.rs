//! The library lock of a shared workflow release, against a real package store, real job files, the real host state
//! file, and a real `contextos-mcp` session: the first passing validation of any job on the release fixes the lock in the
//! same write that validates the job, and every later validation and version reads it and resolves nothing.

mod support;

use contextos_web::job_operation::{OperatorRequest, perform};
use contextos_web::job_operation_shared::SharedJobStore;
use contextos_web::shared_claim::SharedJobKey;
use contextos_web::shared_jobs::SharedJobFile;
use contextos_web::shared_lifecycle::{SharedJobChange, change_shared_lifecycle};
use contextos_web::shared_validation::{SharedValidationHost, validate_shared_job_against};
use contextos_workflows::{Actor, LibraryLock, Transition, VersionBasis};
use semver::Version;
use support::BoxError;
use support::shared::{FIRST, SharedFixture as Fixture, key, write};
use time::macros::datetime;

const READY: &str = "name = \"Nightly\"\nready = true\ntimeout_secs = 30\nversion = \"1.0.0\"\n";
const SCRIPT: &str = "import \"acme-text:strings\" as s;\ns::shout();";
const LATER: time::OffsetDateTime = datetime!(2026-10-04 04:00:00 UTC);

/// A fixture whose package `acme` imports from the library `acme-text` 1.0.0.
async fn with_library() -> Result<Fixture, BoxError> {
    let fixture = Fixture::new(READY).await?;
    fixture.install_library("acme-text", "1.0.0", "strings", "fn shout() { 1 }")?;
    fixture.install_depending_on("1.0.0", SCRIPT, &[("acme-text", "^1")])?;
    Ok(fixture)
}

fn locked(fixture: &Fixture) -> Result<Option<LibraryLock>, BoxError> {
    Ok(fixture
        .read()?
        .library_lock("acme", "sync", &Version::new(1, 0, 0))
        .cloned())
}

fn locked_names(lock: Option<&LibraryLock>) -> Vec<String> {
    lock.map(|lock| {
        lock.libraries
            .iter()
            .map(|(name, library)| format!("{name} {}", library.version))
            .collect()
    })
    .unwrap_or_default()
}

fn nightly_file(fixture: &Fixture) -> Result<SharedJobFile, BoxError> {
    Ok(SharedJobFile {
        key: key(),
        text: std::fs::read_to_string(fixture.job_path())?,
    })
}

async fn validate_against(
    fixture: &Fixture,
    lock: Option<&LibraryLock>,
) -> Result<contextos_web::job_validation::JobValidation, BoxError> {
    let store_root = fixture.store_root();
    let known = ["contextos".to_owned()];
    let host = SharedValidationHost {
        store_root: &store_root,
        clients: &fixture.clients,
        primary_server: "contextos",
        known_servers: &known,
    };
    Ok(validate_shared_job_against(&host, &nightly_file(fixture)?, lock).await)
}

#[tokio::test]
async fn the_first_passing_validation_on_a_release_records_its_lock_in_the_write_that_validates_the_job()
-> Result<(), BoxError> {
    let fixture = with_library().await?;

    fixture.pass(FIRST).await?;

    assert_eq!(fixture.lifecycle()?, "validated");
    assert_eq!(locked_names(locked(&fixture)?.as_ref()), ["acme-text 1.0.0"]);
    Ok(())
}

#[tokio::test]
async fn a_validation_made_without_a_lock_reports_the_lock_and_link_to_record() -> Result<(), BoxError> {
    let fixture = with_library().await?;

    let validation = validate_against(&fixture, None).await?;

    let basis = validation.made_from.ok_or("a snapshot that was read gives a basis")?;
    assert_eq!(basis.package, "acme");
    assert_eq!(basis.release, Version::new(1, 0, 0));
    assert_eq!(locked_names(Some(&basis.lock)), ["acme-text 1.0.0"]);
    assert_eq!(basis.predecessor, None);
    Ok(())
}

#[tokio::test]
async fn a_job_validated_after_a_newer_library_is_installed_keeps_the_recorded_lock_and_its_version()
-> Result<(), BoxError> {
    let fixture = with_library().await?;
    fixture.pass(FIRST).await?;
    let recorded = fixture.recorded_version()?;

    fixture.install_library("acme-text", "1.1.0", "strings", "fn shout() { 2 }")?;
    // Another job on the same workflow release, validated after the newer library arrived.
    write(
        &fixture.jobs_root().join("acme/sync/jobs/second.toml"),
        "name = \"Second\"\nready = true\ntimeout_secs = 30\nversion = \"1.0.0\"\n",
    )?;
    fixture.pass(LATER).await?;

    assert_eq!(
        fixture.lifecycle()?,
        "validated",
        "the locked job's version did not move"
    );
    assert_eq!(fixture.recorded_version()?, recorded);
    let state = fixture.read()?;
    assert_eq!(state.lifecycle_of("acme", "sync", "second").as_str(), "validated");
    assert_eq!(locked_names(locked(&fixture)?.as_ref()), ["acme-text 1.0.0"]);
    Ok(())
}

#[tokio::test]
async fn a_validation_against_a_recorded_lock_resolves_nothing_and_reports_that_lock() -> Result<(), BoxError> {
    let fixture = with_library().await?;
    fixture.pass(FIRST).await?;
    let lock = locked(&fixture)?.ok_or("a lock was recorded")?;
    fixture.install_library("acme-text", "1.1.0", "strings", "fn shout() { 2 }")?;

    let validation = validate_against(&fixture, Some(&lock)).await?;

    assert!(validation.report.passed(), "{:?}", validation.report);
    let basis: VersionBasis = validation.made_from.ok_or("a basis")?;
    assert_eq!(basis.lock, lock);
    Ok(())
}

#[tokio::test]
async fn a_locked_library_release_that_was_removed_is_a_missing_artefact_whatever_is_installed_instead()
-> Result<(), BoxError> {
    let fixture = with_library().await?;
    fixture.pass(FIRST).await?;
    let lock = locked(&fixture)?.ok_or("a lock was recorded")?;
    fixture.install_library("acme-text", "1.1.0", "strings", "fn shout() { 2 }")?;
    std::fs::remove_dir_all(fixture.store_root().join("acme-text").join("1.0.0"))?;

    let validation = validate_against(&fixture, Some(&lock)).await?;

    let codes: Vec<&str> = validation
        .report
        .failures
        .iter()
        .map(|failure| failure.code.as_str())
        .collect();
    assert_eq!(codes, ["validation/artefact-missing"], "{:?}", validation.report);
    assert!(validation.made_from.is_none(), "a snapshot that failed records nothing");
    Ok(())
}

#[tokio::test]
async fn a_locked_library_release_reinstalled_with_another_digest_is_an_invalid_artefact() -> Result<(), BoxError> {
    let fixture = with_library().await?;
    fixture.pass(FIRST).await?;
    let lock = locked(&fixture)?.ok_or("a lock was recorded")?;
    let release = fixture.store_root().join("acme-text").join("1.0.0");
    std::fs::remove_dir_all(&release)?;
    write(
        &release.join("manifest.toml"),
        &format!(
            "name = \"acme-text\"\nversion = \"1.0.0\"\ndigest = \"{}\"\nmodules = [\"strings.rhai\"]\nexports = [\"strings.rhai\"]\n",
            support::shared::DIGEST
        ),
    )?;
    write(&release.join("strings.rhai"), "fn shout() { 3 }")?;
    support::release_fixture::seal_io(&release)?;

    let validation = validate_against(&fixture, Some(&lock)).await?;

    let codes: Vec<&str> = validation
        .report
        .failures
        .iter()
        .map(|failure| failure.code.as_str())
        .collect();
    assert_eq!(codes, ["validation/artefact-invalid"], "{:?}", validation.report);
    Ok(())
}

const SUCCESSOR: &str =
    "name = \"Nightly again\"\nready = true\ntimeout_secs = 30\nversion = \"1.1.0\"\npredecessor = \"nightly\"\n";

fn successor_key() -> SharedJobKey {
    SharedJobKey {
        job: "nightly-v2".to_owned(),
        ..key()
    }
}

async fn ask(fixture: &Fixture, job: &SharedJobKey, request: OperatorRequest) -> Result<(), BoxError> {
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
    perform(&store, request, FIRST).await?;
    Ok(())
}

fn successor_version(fixture: &Fixture) -> Result<contextos_workflows::JobFingerprint, BoxError> {
    fixture
        .read()?
        .job("acme", "sync", "nightly-v2")
        .and_then(|job| job.fingerprint().cloned())
        .ok_or_else(|| "the successor was not validated".into())
}

/// An enabled job on release `1.0.0`, and a successor on release `1.1.0` (installed depending on `acme-text`) that
/// has been validated, dry run, and enabled. Its version is new under an enabled job, and the first time `1.1.0` is
/// validated fixes the release's library lock.
async fn enabled_successor_on_a_release_with_a_library() -> Result<Fixture, BoxError> {
    let fixture = Fixture::new(READY).await?;
    fixture.enable().await?;
    fixture.install_library("acme-text", "1.0.0", "strings", "fn shout() { 1 }")?;
    fixture.install_depending_on("1.1.0", SCRIPT, &[("acme-text", "^1")])?;
    write(&fixture.jobs_root().join("acme/sync/jobs/nightly-v2.toml"), SUCCESSOR)?;
    let successor = successor_key();
    ask(&fixture, &successor, OperatorRequest::Validate).await?;
    let version = successor_version(&fixture)?;
    change_shared_lifecycle(
        &fixture.state(),
        SharedJobChange {
            key: &successor,
            actor: Actor::DryRun,
            transition: Transition::DryRunCompleted(version),
            now: FIRST,
        },
    )
    .await?;
    ask(&fixture, &successor, OperatorRequest::Enable).await?;
    Ok(fixture)
}

#[tokio::test]
async fn the_first_passing_validation_of_an_enabled_jobs_successor_fixes_its_releases_library_lock()
-> Result<(), BoxError> {
    let fixture = enabled_successor_on_a_release_with_a_library().await?;

    let lock = fixture
        .read()?
        .library_lock("acme", "sync", &Version::new(1, 1, 0))
        .cloned();

    assert_eq!(
        fixture.read()?.lifecycle_of("acme", "sync", "nightly-v2").as_str(),
        "enabled"
    );
    assert_eq!(locked_names(lock.as_ref()), ["acme-text 1.0.0"]);
    Ok(())
}

#[tokio::test]
async fn a_library_release_installed_after_an_enabled_successors_review_does_not_make_its_version_new_again()
-> Result<(), BoxError> {
    let fixture = enabled_successor_on_a_release_with_a_library().await?;
    let reviewed = successor_version(&fixture)?;
    fixture.install_library("acme-text", "1.1.0", "strings", "fn shout() { 2 }")?;

    let report = fixture.pass(datetime!(2026-10-05 04:00:00 UTC)).await?;

    assert_eq!(report.on_probation, 0, "the lock still names the first library release");
    assert_eq!(successor_version(&fixture)?, reviewed);
    Ok(())
}
