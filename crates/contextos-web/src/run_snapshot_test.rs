use contextos_workflows::{JobFingerprint, ReleaseFingerprintInput, StoreKind, WorkflowFailureKind};

use super::*;
use crate::fake_vault::FakeVault;
use crate::run_records::Trigger;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const WORKFLOW_TOML: &str = "name = \"Sync\"\nentry = \"workflow.rhai\"\nmcp_servers.allow = []\n";
const JOB_TOML: &str = "name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\nready = true\n";
const STATE_PATH: &str = ".contextos/jobs.toml";

fn job() -> RegisteredJob {
    RegisteredJob {
        templates: contextos_workflows::DeclaredFiles::default(),
        modules: contextos_workflows::DeclaredModules::default(),
        libraries: Vec::new(),
        package: "ops".to_owned(),
        workflow_slug: "sync-crm".to_owned(),
        owner: None,
        declared_owner: None,
        entity: crate::jobs::ResolvedEntity::from((None, None)),
        release: contextos_workflows::JobFileRelease {
            version: contextos_workflows::JobVersion::from(contextos_workflows::JobVersionNumbers {
                major: 1,
                minor: 0,
                patch: 0,
            }),
            predecessor: None,
        },
        slug: "nightly".to_owned(),
        name: "Nightly".to_owned(),
        entry: "workflow.rhai".to_owned(),
        timeout_secs: 60,
        calls: contextos_workflows::call_rules::CallRules::default(),
        inputs: contextos_workflows::ValidatedInputs::default(),
        declared_inputs: contextos_workflows::DeclaredInputs::default(),
        outputs: contextos_workflows::DeclaredOutputs::default(),
        workflow_log_level: None,
        log_level: None,
        ready: true,
        file_name: "nightly.toml".to_owned(),
    }
}

/// The version of a vault job whose installed release has the given digest; another digest is another release.
fn version_of(digest: &str) -> JobFingerprint {
    JobFingerprint::from(&ReleaseFingerprintInput {
        store: StoreKind::Vault,
        package: "ops",
        release: "1.0.0",
        digest,
        job_toml: JOB_TOML,
        release_manifest: WORKFLOW_TOML,
        libraries: &[],
    })
}

fn version() -> JobFingerprint {
    version_of("sha256:0000000000000000000000000000000000000000000000000000000000000000")
}

fn other_version() -> JobFingerprint {
    version_of("sha256:1111111111111111111111111111111111111111111111111111111111111111")
}

fn vault() -> FakeVault {
    FakeVault::new()
        .with_file("registry/workflows/ops/sync-crm/workflow.toml", WORKFLOW_TOML)
        .with_file("registry/workflows/ops/sync-crm/jobs/nightly.toml", JOB_TOML)
}

/// The state of an `enabled` job, with `version` recorded when given.
fn enabled(version: Option<&JobFingerprint>) -> String {
    let recorded = version.map_or_else(String::new, |version| {
        format!("fingerprint = \"{}\"\n", String::from(version.clone()))
    });
    format!(
        "[ops.sync-crm.nightly]\nlifecycle = \"enabled\"\nschedule = \"0 3 * * *\"\n{recorded}\
         lifecycle_changed = \"2026-09-01T00:00:00Z\"\nlifecycle_actor = \"operator\"\n"
    )
}

fn refusal(result: Result<Option<JobFingerprint>, RunFailure>) -> Result<RunFailure, Box<dyn std::error::Error>> {
    match result {
        Err(failure) => Ok(failure),
        Ok(version) => Err(format!("the run was allowed to start with {version:?}").into()),
    }
}

#[tokio::test]
async fn an_enabled_job_whose_code_is_the_reviewed_version_may_start_and_the_run_is_of_that_version() -> TestResult {
    let reviewed = version();
    let vault = vault().with_file(STATE_PATH, &enabled(Some(&reviewed)));
    let client = vault.session().await?;

    let allowed = reviewed_version(&client, "vault", &job(), Some(reviewed.clone())).await;

    assert_eq!(allowed.map_err(|failure| failure.message)?, Some(reviewed));
    Ok(())
}

#[tokio::test]
async fn an_enabled_job_whose_code_is_not_the_reviewed_version_does_not_start() -> TestResult {
    let reviewed = other_version();
    let vault = vault().with_file(STATE_PATH, &enabled(Some(&reviewed)));
    let client = vault.session().await?;

    let failure = refusal(reviewed_version(&client, "vault", &job(), Some(version())).await)?;

    assert_eq!(failure.kind, Some(WorkflowFailureKind::JobNotRunnable));
    assert!(
        failure.message.contains("changed since it was reviewed"),
        "{}",
        failure.message
    );
    Ok(())
}

#[tokio::test]
async fn an_enabled_job_with_no_version_on_record_is_not_blocked_because_there_is_nothing_to_differ_from() -> TestResult
{
    let vault = vault().with_file(STATE_PATH, &enabled(None));
    let client = vault.session().await?;

    let allowed = reviewed_version(&client, "vault", &job(), Some(version())).await;

    assert_eq!(allowed.map_err(|failure| failure.message)?, Some(version()));
    Ok(())
}

#[tokio::test]
async fn the_gate_reads_only_the_job_state_and_never_the_jobs_files() -> TestResult {
    let reviewed = version();
    let vault = vault().with_file(STATE_PATH, &enabled(Some(&reviewed)));
    let client = vault.session().await?;

    let allowed = reviewed_version(&client, "vault", &job(), Some(reviewed.clone())).await;

    assert_eq!(allowed.map_err(|failure| failure.message)?, Some(reviewed));
    assert_eq!(
        vault.calls(),
        vec![format!("fs_attach_file vault://{STATE_PATH}")],
        "the gate must compare the version it was handed with the state alone"
    );
    Ok(())
}

#[tokio::test]
async fn a_server_that_drops_while_the_state_is_read_ends_the_run_as_state_unavailable() -> TestResult {
    let reviewed = version();
    // The connection drops on the first call, which is the read of the job state itself.
    let vault = vault()
        .with_file(STATE_PATH, &enabled(Some(&reviewed)))
        .dropping_after(0);
    let client = vault.session().await?;

    let failure = refusal(reviewed_version(&client, "vault", &job(), Some(version())).await)?;

    assert_eq!(
        failure.kind,
        Some(WorkflowFailureKind::StateUnavailable),
        "{}",
        failure.message
    );
    Ok(())
}

#[tokio::test]
async fn a_state_file_that_cannot_be_read_refuses_the_run_rather_than_letting_it_through_unchecked() -> TestResult {
    let reviewed = version();
    let vault = vault()
        .with_file(STATE_PATH, &enabled(Some(&reviewed)))
        .unreadable(STATE_PATH);
    let client = vault.session().await?;

    let failure = refusal(reviewed_version(&client, "vault", &job(), Some(version())).await)?;

    assert_eq!(
        failure.kind,
        Some(WorkflowFailureKind::StateUnavailable),
        "{}",
        failure.message
    );
    assert!(failure.message.contains("state"), "{}", failure.message);
    Ok(())
}

#[tokio::test]
async fn a_state_file_that_does_not_parse_refuses_the_run_rather_than_letting_it_through_unchecked() -> TestResult {
    let vault = vault().with_file(STATE_PATH, "this is [not toml");
    let client = vault.session().await?;

    let failure = refusal(reviewed_version(&client, "vault", &job(), Some(version())).await)?;

    assert_eq!(
        failure.kind,
        Some(WorkflowFailureKind::StateUnavailable),
        "{}",
        failure.message
    );
    assert!(failure.message.contains("state"), "{}", failure.message);
    Ok(())
}

#[tokio::test]
async fn a_job_that_has_no_state_file_yet_may_start() -> TestResult {
    let vault = vault();
    let client = vault.session().await?;

    let allowed = reviewed_version(&client, "vault", &job(), Some(version())).await;

    assert!(allowed.is_ok(), "{:?}", allowed.map_err(|failure| failure.message));
    Ok(())
}

#[tokio::test]
async fn a_run_that_requires_review_does_not_start_on_code_that_was_not_reviewed() -> TestResult {
    let reviewed = other_version();
    let vault = vault().with_file(STATE_PATH, &enabled(Some(&reviewed)));
    let client = vault.session().await?;

    let failure = refusal(version_for_run(&client, "vault", &job(), Some(version()), ReviewGate::Required).await)?;

    assert_eq!(failure.kind, Some(WorkflowFailureKind::JobNotRunnable));
    Ok(())
}

#[tokio::test]
async fn a_run_that_does_not_require_review_starts_on_code_that_was_not_reviewed_and_is_of_the_version_it_read()
-> TestResult {
    let reviewed = other_version();
    let vault = vault().with_file(STATE_PATH, &enabled(Some(&reviewed)));
    let client = vault.session().await?;

    let run = version_for_run(&client, "vault", &job(), Some(version()), ReviewGate::NotRequired).await;

    assert_eq!(run.map_err(|failure| failure.message)?, Some(version()));
    Ok(())
}

#[tokio::test]
async fn a_run_that_does_not_require_review_never_reads_the_job_state() -> TestResult {
    let vault = vault().with_file(STATE_PATH, "this is [not toml");
    let client = vault.session().await?;

    let run = version_for_run(&client, "vault", &job(), Some(version()), ReviewGate::NotRequired).await;

    assert!(run.is_ok(), "{:?}", run.map_err(|failure| failure.message));
    assert_eq!(vault.calls_to("fs_attach_file", STATE_PATH), 0, "{:?}", vault.calls());
    Ok(())
}

#[test]
fn only_a_scheduled_run_requires_its_code_to_be_the_reviewed_version() {
    assert_eq!(ReviewGate::from(Trigger::Scheduled), ReviewGate::Required);
    assert_eq!(ReviewGate::from(Trigger::Manual), ReviewGate::NotRequired);
}
