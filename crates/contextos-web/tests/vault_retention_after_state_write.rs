//! Retention runs after the state write it follows. When retention cannot finish (here the store's ledger is missing), the
//! operation reports a warning and its state change stands; the next trigger, once the ledger is back, removes the release
//! nothing keeps and the ledger records the removal as made by retention. Each trigger runs against a real
//! `contextos-mcp` session and a real temporary vault.

mod support;

use contextos_web::job_operation::{OperationOutcome, OperatorRequest, Performed, perform_retaining};
use contextos_web::job_operation_vault::VaultJobStore;
use contextos_web::store_retention::{RetentionWarning, retain_vault};
use contextos_web::vault_job_authoring::VaultJobAuthoring;
use contextos_workflows::JobSchedule;
use support::BoxError;
use support::jobs::{JobVault, STATE_PATH, Spec, VAULT_NAME, current_enabled_state, current_state, lifecycle_state};
use time::macros::datetime;

const NOW: time::OffsetDateTime = datetime!(2026-10-01 03:00:00 UTC);
const LEDGER: &str = ".contextos/packages/ledger.toml";

/// A vault whose store holds `ops` 1.0.0, 1.0.1, and 1.0.2 (the job names 1.0.0), with `state` as its job state and the
/// store's ledger missing, as a retention that cannot finish.
async fn vault_without_ledger(state: Option<&str>) -> Result<JobVault, BoxError> {
    let vault = JobVault::open_with_releases(
        &Spec {
            state,
            ..Spec::default()
        },
        &["1.0.1", "1.0.2"],
    )
    .await?;
    if let Some(state) = state {
        // A job validated on a release has that release recorded with it.
        vault.write(
            STATE_PATH,
            &state.replace("[ops.sync.nightly]\n", "[ops.sync.nightly]\nrelease = \"1.0.0\"\n"),
        )?;
    }
    std::fs::remove_file(vault.root().join(LEDGER))?;
    Ok(vault)
}

fn releases(vault: &JobVault) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(vault.root().join(".contextos/packages/ops"))
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

async fn retaining(vault: &JobVault, request: OperatorRequest) -> Result<Performed, BoxError> {
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
    Ok(perform_retaining(&store, request, NOW).await?)
}

/// The next trigger, with the ledger back: retention removes the release nothing keeps, and the ledger says why.
async fn next_trigger_removes_the_release_nothing_keeps(vault: &JobVault, removed: &str) -> Result<(), BoxError> {
    vault.rebuild_ledger().await?;
    let client = vault.clients.get("contextos").ok_or("no primary server")?;
    retain_vault(&client, VAULT_NAME).await?;
    assert!(
        !releases(vault).iter().any(|held| held == removed),
        "{:?}",
        releases(vault)
    );
    let ledger = vault.read(LEDGER)?;
    assert!(ledger.contains("retention"), "{ledger}");
    Ok(())
}

fn assert_one_warning(performed: &Performed, label: &str) {
    assert_eq!(performed.warnings.len(), 1, "{label}: {:?}", performed.warnings);
    let warning: &RetentionWarning = &performed.warnings[0];
    assert!(!warning.message().is_empty(), "{label}");
    assert_eq!(RetentionWarning::CODE, "packages/retention-failed");
}

#[tokio::test]
async fn enabling_a_job_keeps_the_enabling_when_retention_cannot_finish_and_the_next_trigger_removes_the_release()
-> Result<(), BoxError> {
    let state = current_state(&Spec::default(), "registered");
    let vault = vault_without_ledger(Some(&state)).await?;

    let performed = retaining(&vault, OperatorRequest::Schedule(JobSchedule::try_from("0 5 * * *")?)).await?;

    assert!(
        matches!(performed.outcome, OperationOutcome::Enabled(_)),
        "{:?}",
        performed.outcome
    );
    assert_one_warning(&performed, "enable");
    assert_eq!(vault.lifecycle()?, "enabled");
    assert_eq!(releases(&vault), ["1.0.0", "1.0.1", "1.0.2"]);
    next_trigger_removes_the_release_nothing_keeps(&vault, "1.0.1").await
}

#[tokio::test]
async fn retiring_a_job_keeps_the_retirement_when_retention_cannot_finish_and_the_next_trigger_removes_the_release()
-> Result<(), BoxError> {
    let state = current_enabled_state(&Spec::default());
    let vault = vault_without_ledger(Some(&state)).await?;

    let performed = retaining(&vault, OperatorRequest::Retire).await?;

    assert_one_warning(&performed, "retire");
    assert_eq!(vault.lifecycle()?, "retired");
    assert_eq!(releases(&vault), ["1.0.0", "1.0.1", "1.0.2"]);
    next_trigger_removes_the_release_nothing_keeps(&vault, "1.0.1").await
}

#[tokio::test]
async fn deleting_a_job_keeps_the_deletion_when_retention_cannot_finish_and_the_next_trigger_removes_the_release()
-> Result<(), BoxError> {
    let state = lifecycle_state(&Spec::default(), "retired");
    let vault = vault_without_ledger(Some(&state)).await?;
    // The job's files are gone, which a deletion of a retired job's state is usually made after.
    std::fs::remove_file(vault.root().join("registry/workflows/ops/sync/jobs/nightly.toml"))?;

    let performed = retaining(&vault, OperatorRequest::Delete).await?;

    assert_one_warning(&performed, "delete");
    assert_eq!(vault.state()?.lifecycle_of("ops", "sync", "nightly").as_str(), "draft");
    assert!(
        vault.state()?.job("ops", "sync", "nightly").is_none(),
        "the job's state is gone"
    );
    assert_eq!(releases(&vault), ["1.0.0", "1.0.1", "1.0.2"]);
    next_trigger_removes_the_release_nothing_keeps(&vault, "1.0.1").await
}

#[tokio::test]
async fn a_first_validation_keeps_its_result_when_retention_cannot_finish_and_the_next_trigger_removes_the_release()
-> Result<(), BoxError> {
    let vault = vault_without_ledger(None).await?;

    let performed = retaining(&vault, OperatorRequest::Validate).await?;

    assert!(
        matches!(performed.outcome, OperationOutcome::Validated { ref result, .. } if result.passed),
        "{:?}",
        performed.outcome
    );
    assert_one_warning(&performed, "validate");
    assert_eq!(vault.lifecycle()?, "validated");
    assert_eq!(releases(&vault), ["1.0.0", "1.0.1", "1.0.2"]);
    next_trigger_removes_the_release_nothing_keeps(&vault, "1.0.1").await
}

#[tokio::test]
async fn changing_a_jobs_version_in_place_keeps_the_replacement_when_retention_cannot_finish_and_the_next_trigger_removes_the_release()
-> Result<(), BoxError> {
    let vault = vault_without_ledger(Some("[ops.sync]\nfuture = \"1.0.2\"\n")).await?;
    let client = vault.clients.get("contextos").ok_or("no primary server")?;
    let servers = vec!["contextos".to_owned()];
    let authoring = VaultJobAuthoring {
        client: &client,
        vault: VAULT_NAME,
        known_servers: &servers,
    };
    let shown = authoring.show("ops", "sync", "nightly").await?;
    let renamed = shown.content.replace("1.0.0", "1.0.1");

    let replaced = authoring
        .replace("ops", "sync", "nightly", renamed.as_bytes(), &shown.hash)
        .await?;

    assert_eq!(replaced.warnings.len(), 1, "{:?}", replaced.warnings);
    assert!(
        vault
            .read("registry/workflows/ops/sync/jobs/nightly.toml")?
            .contains("version = \"1.0.1\""),
        "the replacement stands"
    );
    assert_eq!(releases(&vault), ["1.0.0", "1.0.1", "1.0.2"]);
    next_trigger_removes_the_release_nothing_keeps(&vault, "1.0.0").await
}
