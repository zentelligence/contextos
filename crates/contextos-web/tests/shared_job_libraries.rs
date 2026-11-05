//! A shared job whose libraries stop resolving, against a real package store, real job files, the real host state file,
//! and a real `contextos-mcp` session: a library package that is gone is handled as the job's own package being gone, so
//! an `enabled` job is suspended with `workflow/package-unavailable` and a job that is not `enabled` keeps its state.

mod support;

use contextos_web::job_operation::{JobStore, OperationOutcome, OperatorRequest, perform};
use contextos_web::job_operation_shared::SharedJobStore;
use contextos_web::shared_validation::SharedValidationHost;
use support::BoxError;
use support::shared::{DIGEST, FIRST, SharedFixture as Fixture, key};
use time::macros::datetime;

const READY: &str = "name = \"Nightly\"\nready = true\ntimeout_secs = 30\nversion = \"1.0.0\"\n";
const LATER: time::OffsetDateTime = datetime!(2026-10-04 04:00:00 UTC);
const SCRIPT: &str = "import \"acme-text:strings\" as s;\ns::shout();";

/// A fixture whose package `acme` imports from the library `acme-text`, installed.
async fn with_library() -> Result<Fixture, BoxError> {
    let fixture = Fixture::new(READY).await?;
    fixture.install_library("acme-text", "1.0.0", "strings", "fn shout() { 1 }")?;
    fixture.install_depending_on("1.0.0", SCRIPT, &[("acme-text", "^1")])?;
    Ok(fixture)
}

fn remove_library(fixture: &Fixture) -> std::io::Result<()> {
    std::fs::remove_dir_all(fixture.store_root().join("acme-text"))
}

#[tokio::test]
async fn an_enabled_job_whose_library_disappears_is_suspended_as_unavailable_naming_the_library() -> Result<(), BoxError>
{
    let fixture = with_library().await?;
    fixture.enable().await?;

    remove_library(&fixture)?;
    let report = fixture.pass(LATER).await?;

    assert_eq!(fixture.lifecycle()?, "suspended");
    assert_eq!(report.suspended, 1);
    let state = fixture.read()?;
    let suspension = state
        .job("acme", "sync", "nightly")
        .and_then(contextos_workflows::JobState::suspension);
    assert_eq!(
        suspension.map(|suspension| suspension.code.as_str()),
        Some("workflow/package-unavailable")
    );
    assert!(
        suspension.is_some_and(|suspension| suspension.reason.contains("acme-text")),
        "{suspension:?}"
    );
    Ok(())
}

#[tokio::test]
async fn validated_and_registered_jobs_whose_library_disappears_return_to_draft_as_they_do_when_their_package_does()
-> Result<(), BoxError> {
    let validated = with_library().await?;
    validated.pass(FIRST).await?;
    assert_eq!(validated.lifecycle()?, "validated");
    remove_library(&validated)?;
    validated.pass(LATER).await?;
    assert_eq!(validated.lifecycle()?, "draft");

    let registered = with_library().await?;
    registered.register().await?;
    assert_eq!(registered.lifecycle()?, "registered");
    remove_library(&registered)?;
    registered.pass(LATER).await?;
    assert_eq!(registered.lifecycle()?, "draft");
    Ok(())
}

/// Validates the registered job on demand, as the operator does, returning the failures it reports as `(code,
/// artefact, message)` and the lifecycle it is left in.
async fn validate_on_demand(fixture: &Fixture) -> Result<(Vec<(String, String, String)>, String), BoxError> {
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
    let job_key = key();
    let store = SharedJobStore {
        state: &state,
        jobs_root: &jobs_root,
        validation: &validation,
        key: &job_key,
        claims_root: &fixture.claims_root(),
    };

    let outcome = perform(&store, OperatorRequest::Validate, LATER).await?;

    let OperationOutcome::Validated { result, lifecycle } = outcome else {
        return Err("expected a validation outcome".into());
    };
    assert!(!result.passed);
    assert!(
        store.version().await?.is_none(),
        "a job whose code cannot be read has no version"
    );
    let failures = result
        .failures
        .iter()
        .map(|failure| {
            (
                failure.code.as_str().to_owned(),
                failure.artefact.clone(),
                failure.message.clone(),
            )
        })
        .collect();
    Ok((failures, lifecycle.as_str().to_owned()))
}

#[tokio::test]
async fn validating_on_demand_a_job_whose_library_is_gone_names_the_library_as_a_missing_artefact()
-> Result<(), BoxError> {
    let fixture = with_library().await?;
    fixture.register().await?;
    remove_library(&fixture)?;

    let (failures, lifecycle) = validate_on_demand(&fixture).await?;

    assert!(
        failures
            .iter()
            .any(|(code, _, message)| code == "validation/artefact-missing" && message.contains("acme-text")),
        "{failures:?}"
    );
    assert_eq!(
        lifecycle, "draft",
        "a job with no version returns to draft, as it does when its own package is gone"
    );
    Ok(())
}

/// Replaces an installed release's manifest with `text`.
fn damage_manifest(fixture: &Fixture, package: &str, text: &str) -> std::io::Result<()> {
    support::shared::write(
        &fixture.store_root().join(package).join("1.0.0").join("manifest.toml"),
        text,
    )
}

#[tokio::test]
async fn validating_on_demand_a_job_whose_library_manifest_no_longer_parses_reports_an_invalid_artefact()
-> Result<(), BoxError> {
    let fixture = with_library().await?;
    fixture.register().await?;
    damage_manifest(&fixture, "acme-text", "name = \"acme-text\n[[")?;

    let (failures, lifecycle) = validate_on_demand(&fixture).await?;

    assert!(
        failures
            .iter()
            .any(|(code, _, message)| code == "validation/artefact-invalid" && message.contains("acme-text")),
        "{failures:?}"
    );
    assert!(
        failures
            .iter()
            .all(|(code, _, _)| code != "validation/artefact-missing"),
        "{failures:?}"
    );
    assert_eq!(lifecycle, "draft");
    Ok(())
}

#[tokio::test]
async fn validating_on_demand_a_job_whose_library_manifest_breaks_the_schema_reports_an_invalid_artefact()
-> Result<(), BoxError> {
    let fixture = with_library().await?;
    fixture.register().await?;
    damage_manifest(
        &fixture,
        "acme-text",
        "name = \"acme-text\"\nversion = \"1.0.0\"\nunknown_field = 1\n",
    )?;

    let (failures, _) = validate_on_demand(&fixture).await?;

    assert!(
        failures
            .iter()
            .any(|(code, _, message)| code == "validation/artefact-invalid" && message.contains("acme-text")),
        "{failures:?}"
    );
    Ok(())
}

#[tokio::test]
async fn validating_on_demand_a_job_whose_own_package_manifest_no_longer_parses_reports_an_invalid_artefact_naming_it()
-> Result<(), BoxError> {
    let fixture = with_library().await?;
    fixture.register().await?;
    damage_manifest(&fixture, "acme", "name = \"acme\n[[")?;

    let (failures, lifecycle) = validate_on_demand(&fixture).await?;

    assert!(
        failures
            .iter()
            .any(|(code, artefact, _)| code == "validation/artefact-invalid" && artefact == "acme"),
        "{failures:?}"
    );
    assert!(
        failures
            .iter()
            .all(|(code, _, _)| code != "validation/artefact-missing"),
        "{failures:?}"
    );
    assert_eq!(lifecycle, "draft");
    Ok(())
}

#[tokio::test]
async fn a_damaged_newer_library_release_is_the_one_chosen_and_reported_and_no_older_one_stands_in()
-> Result<(), BoxError> {
    let fixture = with_library().await?;
    fixture.install_library("acme-text", "1.1.0", "strings", "fn shout() { 2 }")?;
    support::shared::write(
        &fixture
            .store_root()
            .join("acme-text")
            .join("1.1.0")
            .join("manifest.toml"),
        "name = \"acme-text\n[[",
    )?;

    fixture.pass(FIRST).await?;

    assert_eq!(fixture.lifecycle()?, "draft");
    let state = fixture.read()?;
    let stored = state
        .job("acme", "sync", "nightly")
        .and_then(contextos_workflows::JobState::validation)
        .ok_or("the failing result was not kept")?;
    assert!(
        stored
            .failures
            .iter()
            .any(|failure| failure.code == "validation/artefact-invalid" && failure.message.contains("acme-text 1.1.0")),
        "{stored:?}"
    );
    Ok(())
}

#[tokio::test]
async fn validating_a_job_whose_library_manifest_names_another_release_reports_an_invalid_artefact()
-> Result<(), BoxError> {
    let fixture = with_library().await?;
    fixture.register().await?;
    damage_manifest(
        &fixture,
        "acme-text",
        &format!(
            "name = \"acme-text\"\nversion = \"9.9.9\"\ndigest = \"{DIGEST}\"\nmodules = [\"strings.rhai\"]\nexports = [\"strings.rhai\"]\n"
        ),
    )?;

    let (failures, _) = validate_on_demand(&fixture).await?;

    assert!(
        failures
            .iter()
            .any(|(code, _, message)| code == "validation/artefact-invalid" && message.contains("acme-text")),
        "{failures:?}"
    );
    Ok(())
}

#[tokio::test]
async fn an_enabled_job_whose_library_manifest_is_damaged_is_suspended_as_validation_failed() -> Result<(), BoxError> {
    let fixture = with_library().await?;
    fixture.enable().await?;

    damage_manifest(&fixture, "acme-text", "name = \"acme-text\n[[")?;
    fixture.pass(LATER).await?;

    assert_eq!(fixture.lifecycle()?, "suspended");
    let state = fixture.read()?;
    let suspension = state
        .job("acme", "sync", "nightly")
        .and_then(contextos_workflows::JobState::suspension);
    assert_eq!(
        suspension.map(|suspension| suspension.code.as_str()),
        Some("workflow/validation-failed")
    );
    let stored = state
        .job("acme", "sync", "nightly")
        .and_then(contextos_workflows::JobState::validation)
        .ok_or("no validation was stored")?;
    assert!(
        stored
            .failures
            .iter()
            .any(|failure| failure.code == "validation/artefact-invalid"
                && failure.artefact == "acme"
                && failure.message.contains("acme-text")),
        "{:?}",
        stored.failures
    );
    Ok(())
}

#[tokio::test]
async fn an_enabled_job_whose_own_package_manifest_is_damaged_is_suspended_as_validation_failed() -> Result<(), BoxError>
{
    let fixture = with_library().await?;
    fixture.enable().await?;

    damage_manifest(&fixture, "acme", "name = \"acme\n[[")?;
    fixture.pass(LATER).await?;

    assert_eq!(fixture.lifecycle()?, "suspended");
    let state = fixture.read()?;
    let suspension = state
        .job("acme", "sync", "nightly")
        .and_then(contextos_workflows::JobState::suspension);
    assert_eq!(
        suspension.map(|suspension| suspension.code.as_str()),
        Some("workflow/validation-failed")
    );
    let stored = state
        .job("acme", "sync", "nightly")
        .and_then(contextos_workflows::JobState::validation)
        .ok_or("no validation was stored")?;
    assert!(
        stored
            .failures
            .iter()
            .any(|failure| failure.code == "validation/artefact-invalid" && failure.artefact == "acme"),
        "{:?}",
        stored.failures
    );
    Ok(())
}

#[tokio::test]
async fn an_enabled_job_edited_to_name_a_release_that_is_not_installed_is_suspended_as_validation_failed()
-> Result<(), BoxError> {
    let fixture = with_library().await?;
    fixture.enable().await?;

    support::shared::write(&fixture.job_path(), &READY.replace("1.0.0", "9.9.9"))?;
    fixture.pass(LATER).await?;

    assert_eq!(fixture.lifecycle()?, "suspended");
    let state = fixture.read()?;
    let suspension = state
        .job("acme", "sync", "nightly")
        .and_then(contextos_workflows::JobState::suspension);
    assert_eq!(
        suspension.map(|suspension| suspension.code.as_str()),
        Some("workflow/validation-failed")
    );
    Ok(())
}
