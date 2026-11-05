//! The entity a vault job's run records carry, against a real `contextos` session and a real temporary vault: the job's
//! own, else its workflow's, else `vault`, for a live run, a dry run, and a tick skipped because a run held the claim.
//! No record may claim an entity nobody declared.

mod support;

use contextos_web::cli_run::CliRunOptions;
use contextos_web::run_claim::RunClaim;
use contextos_web::scheduler::start_due_jobs;
use contextos_web::vault_runs::claim_slot;
use support::jobs::{FixedClock, JobVault, Spec, VAULT_NAME, WORKFLOW, enabled_state};
use time::macros::datetime;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const ENTITIES: &str = "[[entities]]\nslug = \"zenith\"\nname = \"Zenith\"\naccent = 1\n\n\
                        [[entities]]\nslug = \"personal\"\nname = \"Personal\"\naccent = 2\n";
/// The call the script makes, as a dry run mocks it.
const ONE_CALL: &str = "[[call]]\nserver = \"contextos\"\ntool = \"fs_read_text_file\"\nkind = \"valid\"\n\
                        response = { content = \"mocked\" }\n";
/// A script that reads one file, so a dry run has a call to mock and a live run has a real one.
const READING_SCRIPT: &str =
    "call_mcp(\"contextos\", \"fs_read_text_file\", #{ path: \"contract-fixture://index.md\" });";

/// Produces a skipped, a live, and a dry run record for the vault `spec` describes and checks the entity each carries.
async fn assert_every_record_carries(spec: &Spec<'_>, expected: &str) -> Result<(), BoxError> {
    let vault = JobVault::open(spec).await?;
    vault.write(".contextos/entities.toml", ENTITIES)?;
    vault.write("registry/workflows/ops/sync/tests/happy-path.toml", ONE_CALL)?;
    let clock = FixedClock(datetime!(2026-10-01 03:00:30 UTC));

    // A tick that finds the claim held leaves a skipped record.
    {
        let slot = claim_slot(&vault.host, VAULT_NAME, "ops", "sync", "nightly")?;
        let _running = RunClaim::acquire(&slot)?;
        start_due_jobs(&vault.clients, &vault.host, VAULT_NAME, &clock).await;
    }
    support::run_job(
        vault.clients.clone(),
        &vault.host,
        VAULT_NAME,
        "ops",
        "sync",
        "nightly",
        CliRunOptions {
            dry_run_scenario: None,
            record: true,
        },
    )
    .await?;
    support::run_job(
        vault.clients.clone(),
        &vault.host,
        VAULT_NAME,
        "ops",
        "sync",
        "nightly",
        CliRunOptions {
            dry_run_scenario: Some(Some("happy-path".to_owned())),
            record: true,
        },
    )
    .await?;

    let records = vault.run_records();
    let kinds = |needle: &str| records.iter().filter(|(_, text)| text.contains(needle)).count();
    assert_eq!(kinds("status: skipped"), 1, "one skipped record: {records:?}");
    assert_eq!(kinds("dry_run: true"), 1, "one dry record: {records:?}");
    assert_eq!(kinds("dry_run: false"), 2, "a skipped and a live record: {records:?}");
    assert_eq!(records.len(), 3, "{records:?}");
    for (path, text) in &records {
        let entities: Vec<&str> = text
            .lines()
            .filter_map(|line| line.strip_prefix("entity:"))
            .map(str::trim)
            .collect();
        assert_eq!(entities, [expected], "the entity of {}:\n{text}", path.display());
    }
    Ok(())
}

#[tokio::test]
async fn live_dry_and_skipped_records_carry_the_resolved_entity() -> Result<(), BoxError> {
    let workflow_with_entity = format!("{WORKFLOW}entity = \"personal\"\n");
    let job_with_entity = "ready = true\nname = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nentity = \"personal\"\n";

    // Neither declares one (the case that loads before declared entities are accepted), so the records are the vault's
    // and never another entity's.
    let mut spec = Spec {
        script: READING_SCRIPT,
        ..Spec::default()
    };
    let state = enabled_state(&spec);
    spec.state = Some(&state);
    assert_every_record_carries(&spec, "vault").await?;

    // The job declares its entity, and the workflow another.
    let workflow_other = format!("{WORKFLOW}entity = \"zenith\"\n");
    let mut spec = Spec {
        job: job_with_entity,
        workflow: &workflow_other,
        script: READING_SCRIPT,
        ..Spec::default()
    };
    let state = enabled_state(&spec);
    spec.state = Some(&state);
    assert_every_record_carries(&spec, "personal").await?;

    // Only the workflow declares one.
    let mut spec = Spec {
        workflow: &workflow_with_entity,
        script: READING_SCRIPT,
        ..Spec::default()
    };
    let state = enabled_state(&spec);
    spec.state = Some(&state);
    assert_every_record_carries(&spec, "personal").await?;

    Ok(())
}
