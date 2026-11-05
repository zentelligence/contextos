//! A template of a vault workflow is read live from source while its release fixes the declared template paths, so a
//! template renamed or removed after install surfaces when it is read, and what that does to a job depends on its stage.

mod support;

use contextos_web::cli_run::CliRunOptions;
use contextos_workflows::{RunOutcome, WorkflowFailureKind};
use support::BoxError;
use support::jobs::{JobVault, STATE_PATH, Spec, VAULT_NAME};
use time::macros::datetime;

const PACKAGE: &str = "workflows = [\"sync\"]\ntemplates = [\"letter.md\"]\n";
const READS_IT: &str = "workflow_file(\"letter.md\");";
const SCENARIO: &str = "registry/workflows/ops/sync/tests/happy-path.toml";
const SOME_CALL: &str = "[[call]]\nserver = \"contextos\"\ntool = \"fs_read_text_file\"\nkind = \"valid\"\nresponse = { content = \"x\" }\n";

/// A vault whose release declares `letter.md`, which source holds at first and then loses, with its job in `lifecycle`
/// (`draft` for none) for the version the release gives it.
async fn vault_losing_its_template(lifecycle: &str, trial: bool) -> Result<JobVault, BoxError> {
    let vault = JobVault::open(&Spec {
        package: PACKAGE,
        script: READS_IT,
        ..Spec::default()
    })
    .await?;
    vault.write("registry/workflows/ops/letter.md", "Dear you\n")?;
    vault.install_releases()?;
    if lifecycle != "draft" {
        let version = String::from(vault.current_version()?);
        let schedule = if lifecycle == "enabled" {
            "schedule = \"* * * * *\"\n"
        } else {
            ""
        };
        let first_run = if trial { "first_run = true\n" } else { "" };
        vault.write(
            STATE_PATH,
            &format!(
                "[ops.sync.nightly]\nlifecycle = \"{lifecycle}\"\n{schedule}fingerprint = \"{version}\"\n{first_run}\
                 lifecycle_changed = \"2026-09-01T00:00:00Z\"\nlifecycle_actor = \"operator\"\n"
            ),
        )?;
    }
    vault.write(SCENARIO, SOME_CALL)?;
    std::fs::remove_file(vault.root().join("registry/workflows/ops/letter.md"))?;
    Ok(vault)
}

async fn run(vault: &JobVault, dry: bool) -> Result<RunOutcome, BoxError> {
    Ok(support::run_job(
        std::sync::Arc::clone(&vault.clients),
        &vault.host,
        VAULT_NAME,
        "ops",
        "sync",
        "nightly",
        CliRunOptions {
            dry_run_scenario: dry.then(|| Some("happy-path".to_owned())),
            record: false,
        },
    )
    .await?)
}

/// Whether the run's script started and failed reading the template: the failure carries the file-not-found kind the
/// read gives, which a failure to discover or validate the workflow never does.
fn failed_reading_the_template(outcome: &RunOutcome) -> bool {
    matches!(
        outcome,
        RunOutcome::Failed(failure)
            if failure.kind == Some(WorkflowFailureKind::FileNotFound)
                && failure.message.contains("workflow/file-not-found")
                && failure.message.contains("letter.md")
    )
}

#[tokio::test]
async fn a_dry_run_of_a_draft_job_fails_naming_the_missing_template() -> Result<(), BoxError> {
    let vault = vault_losing_its_template("draft", false).await?;

    let outcome = run(&vault, true).await?;

    assert!(failed_reading_the_template(&outcome), "{outcome:?}");
    assert_eq!(vault.lifecycle()?, "draft");
    Ok(())
}

#[tokio::test]
async fn a_dry_run_or_a_run_by_hand_of_a_registered_job_fails_and_leaves_its_stage() -> Result<(), BoxError> {
    let vault = vault_losing_its_template("registered", false).await?;

    let dry = run(&vault, true).await?;
    let live = run(&vault, false).await?;

    assert!(failed_reading_the_template(&dry), "{dry:?}");
    assert!(failed_reading_the_template(&live), "{live:?}");
    assert_eq!(vault.lifecycle()?, "registered");
    Ok(())
}

#[tokio::test]
async fn a_run_of_an_enabled_job_on_trial_fails_and_suspends_it() -> Result<(), BoxError> {
    let vault = vault_losing_its_template("enabled", true).await?;

    let outcome = run(&vault, false).await?;

    assert!(failed_reading_the_template(&outcome), "{outcome:?}");
    assert_eq!(vault.lifecycle()?, "suspended");
    Ok(())
}

const DUE: time::OffsetDateTime = datetime!(2026-10-01 03:00:00 UTC);

/// Waits, yielding rather than sleeping, until `count` run records exist and none is still running. The deadline only
/// ends a wait that can never finish.
async fn runs_finished(vault: &JobVault, count: usize) -> Result<(), BoxError> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let records = vault.run_records();
        if records.len() == count && records.iter().all(|(_, text)| !text.contains("status: running")) {
            for _ in 0..50 {
                tokio::task::yield_now().await;
            }
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("timed out waiting for the run to finish".into());
        }
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn a_scheduled_run_of_an_enabled_job_past_its_trial_fails_reading_the_template_and_raises_the_run_failed_task()
-> Result<(), BoxError> {
    let vault = vault_losing_its_template("enabled", false).await?;

    vault.pass(DUE).await;
    runs_finished(&vault, 1).await?;
    // The scheduler raises the task for a finished run on the pass that next notices it.
    vault.pass(DUE + time::Duration::seconds(20)).await;

    let records = vault.run_records();
    let [(_, record)] = records.as_slice() else {
        return Err(format!("expected one run record, found {}", records.len()).into());
    };
    assert!(record.contains("status: failed"), "{record}");
    assert!(record.contains("workflow/file-not-found"), "{record}");
    assert!(record.contains("script_started: true"), "{record}");
    assert_eq!(vault.lifecycle()?, "enabled");
    assert_eq!(vault.raised_tasks_for("run-failed").len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_schedule_does_not_run_a_suspended_or_retired_job_so_its_missing_template_is_never_read()
-> Result<(), BoxError> {
    for lifecycle in ["suspended", "retired"] {
        let vault = vault_losing_its_template(lifecycle, false).await?;

        vault.pass(DUE).await;
        for _ in 0..500 {
            tokio::task::yield_now().await;
        }

        assert!(vault.run_records().is_empty(), "{lifecycle}: {:?}", vault.run_records());
        assert!(vault.raised_tasks_for("run-failed").is_empty(), "{lifecycle}");
        assert_eq!(vault.lifecycle()?, lifecycle);
    }
    Ok(())
}
