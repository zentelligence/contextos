//! Retention runs after the state write it follows. When retention cannot finish (here the host store's ledger is
//! missing), the operation reports a warning and its change stands; the next trigger, once the ledger is back, removes the
//! release nothing keeps and the ledger records the removal as made by retention. Against a real package store, real job
//! files, the real host state file, and a real `contextos-mcp` session.

mod support;

use contextos_web::job_operation::{OperationOutcome, OperatorRequest, Performed, perform_retaining};
use contextos_web::job_operation_shared::SharedJobStore;
use contextos_web::saved_jobs::SavedJobs;
use contextos_web::shared_claim::SharedJobKey;
use contextos_web::shared_job_authoring::{AuthoredOutcome, SharedJobAuthoring};
use contextos_web::shared_validation::SharedValidationHost;
use contextos_web::store_retention::{RetentionWarning, retain_host};
use contextos_workflows::JobSchedule;
use support::BoxError;
use support::shared::{DIGEST, FIRST, SharedFixture, key};

const READY: &str = "ready = true\nname = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";

/// A host store holding `acme` 1.0.0, 1.0.1, and 1.0.2 (the job names 1.0.0) and no ledger, as a retention that cannot
/// finish.
async fn fixture_without_ledger() -> Result<SharedFixture, BoxError> {
    let fixture = SharedFixture::new(READY).await?;
    fixture.install("1.0.1", DIGEST, "let a = 1;")?;
    fixture.install("1.0.2", DIGEST, "let a = 2;")?;
    Ok(fixture)
}

fn releases(fixture: &SharedFixture) -> Vec<String> {
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

async fn retaining(
    fixture: &SharedFixture,
    job: &SharedJobKey,
    request: OperatorRequest,
) -> Result<Performed, BoxError> {
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
    Ok(perform_retaining(&store, request, FIRST).await?)
}

fn assert_one_warning(performed: &Performed, label: &str) {
    assert_eq!(performed.warnings.len(), 1, "{label}: {:?}", performed.warnings);
    assert!(!performed.warnings[0].message().is_empty(), "{label}");
    assert_eq!(RetentionWarning::CODE, "packages/retention-failed");
}

/// The next trigger, with the ledger back: retention removes the release nothing keeps, and the ledger says why.
fn next_trigger_removes_the_release_nothing_keeps(fixture: &SharedFixture, removed: &str) -> Result<(), BoxError> {
    contextos_web::package_store::rebuild_ledger(&fixture.store_root())?;
    retain_host(
        &fixture.store_root(),
        &SavedJobs {
            jobs_root: fixture.jobs_root(),
            state: fixture.state(),
        },
    )?;
    assert!(
        !releases(fixture).iter().any(|held| held == removed),
        "{:?}",
        releases(fixture)
    );
    let ledger = std::fs::read_to_string(fixture.store_root().join("ledger.toml"))?;
    assert!(ledger.contains("retention"), "{ledger}");
    Ok(())
}

#[tokio::test]
async fn enabling_a_shared_job_keeps_the_enabling_when_retention_cannot_finish_and_the_next_trigger_removes_the_release()
-> Result<(), BoxError> {
    let fixture = fixture_without_ledger().await?;
    fixture.register().await?;

    let performed = retaining(
        &fixture,
        &key(),
        OperatorRequest::Schedule(JobSchedule::try_from("0 5 * * *")?),
    )
    .await?;

    assert!(
        matches!(performed.outcome, OperationOutcome::Enabled(_)),
        "{:?}",
        performed.outcome
    );
    assert_one_warning(&performed, "enable");
    assert_eq!(fixture.lifecycle()?, "enabled");
    assert_eq!(releases(&fixture), ["1.0.0", "1.0.1", "1.0.2"]);
    next_trigger_removes_the_release_nothing_keeps(&fixture, "1.0.1")
}

#[tokio::test]
async fn retiring_a_shared_job_keeps_the_retirement_when_retention_cannot_finish_and_the_next_trigger_removes_the_release()
-> Result<(), BoxError> {
    let fixture = fixture_without_ledger().await?;
    fixture.enable().await?;

    let performed = retaining(&fixture, &key(), OperatorRequest::Retire).await?;

    assert_one_warning(&performed, "retire");
    assert_eq!(fixture.lifecycle()?, "retired");
    assert_eq!(releases(&fixture), ["1.0.0", "1.0.1", "1.0.2"]);
    next_trigger_removes_the_release_nothing_keeps(&fixture, "1.0.1")
}

#[tokio::test]
async fn deleting_a_shared_job_keeps_the_deletion_when_retention_cannot_finish_and_the_next_trigger_removes_the_release()
-> Result<(), BoxError> {
    let fixture = fixture_without_ledger().await?;
    fixture.enable().await?;
    retaining(&fixture, &key(), OperatorRequest::Retire).await?;

    let performed = retaining(&fixture, &key(), OperatorRequest::Delete).await?;

    assert_one_warning(&performed, "delete");
    assert!(
        fixture.read()?.job("acme", "sync", "nightly").is_none(),
        "the job's state is gone"
    );
    assert!(!fixture.job_path().exists(), "the job's file is gone");
    assert_eq!(releases(&fixture), ["1.0.0", "1.0.1", "1.0.2"]);
    next_trigger_removes_the_release_nothing_keeps(&fixture, "1.0.1")
}

#[tokio::test]
async fn a_first_shared_validation_keeps_its_result_when_retention_cannot_finish_and_the_next_trigger_removes_the_release()
-> Result<(), BoxError> {
    let fixture = fixture_without_ledger().await?;

    let performed = retaining(&fixture, &key(), OperatorRequest::Validate).await?;

    assert!(
        matches!(performed.outcome, OperationOutcome::Validated { ref result, .. } if result.passed),
        "{:?}",
        performed.outcome
    );
    assert_one_warning(&performed, "validate");
    assert_eq!(fixture.lifecycle()?, "validated");
    assert_eq!(releases(&fixture), ["1.0.0", "1.0.1", "1.0.2"]);
    next_trigger_removes_the_release_nothing_keeps(&fixture, "1.0.1")
}

#[tokio::test]
async fn changing_a_shared_jobs_version_in_place_keeps_the_replacement_when_retention_cannot_finish_and_the_next_trigger_removes_the_release()
-> Result<(), BoxError> {
    let fixture = fixture_without_ledger().await?;
    fixture.record_releases(&[("future", "1.0.2")])?;
    let authoring = SharedJobAuthoring {
        jobs_root: fixture.jobs_root(),
        store_root: fixture.store_root(),
        state: fixture.state(),
    };
    let shown = authoring.show(&key())?;
    let renamed = shown.content.replace("1.0.0", "1.0.1");

    let replaced = authoring.replace(&key(), renamed.as_bytes(), &shown.hash)?;

    assert_eq!(replaced.outcome, AuthoredOutcome::Replaced);
    assert_eq!(replaced.warnings.len(), 1, "{:?}", replaced.warnings);
    assert!(
        std::fs::read_to_string(fixture.job_path())?.contains("version = \"1.0.1\""),
        "the replacement stands"
    );
    assert_eq!(releases(&fixture), ["1.0.0", "1.0.1", "1.0.2"]);
    next_trigger_removes_the_release_nothing_keeps(&fixture, "1.0.0")
}
