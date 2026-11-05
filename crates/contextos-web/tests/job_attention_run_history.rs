//! Which finished runs of a vault job are examined for a run-failed task, and what each is judged by: every run above
//! the host's examined mark in order, not only the latest; the record's own `script_started` field, read together with
//! the rest of the history; and never a run that a suspension or a pending first-run trial already accounts for. Real
//! `contextos` sessions over real temporary vaults, with a fixed clock.

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use contextos_web::cli_run::CliRunOptions;
use support::jobs::{FAILING_SCRIPT, JobVault, Spec, VAULT_NAME, enabled_state};
use support::{BoxError, run_job};
use time::macros::datetime;
use time::{Duration as Span, OffsetDateTime};

const NOW: OffsetDateTime = datetime!(2026-10-01 03:00:30 UTC);

fn field(text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    text.lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .map(|value| value.trim().trim_matches(|c| c == '\'' || c == '"').to_owned())
}

fn minute(n: i64) -> OffsetDateTime {
    NOW + Span::minutes(n)
}

fn origin(number: u64) -> String {
    format!("contextos-web:run-failed:ops/sync/nightly:R-{number}")
}

/// Polls `condition` by yielding to the detached run. The deadline only ends a wait that can never finish.
async fn eventually(what: &str, mut condition: impl FnMut() -> bool) -> Result<(), BoxError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !condition() {
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("timed out waiting for {what}").into());
        }
        // A pause, not a bare yield: each check walks the filesystem, blocking the one thread the run needs as well.
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    Ok(())
}

async fn runs_finished(vault: &JobVault, count: usize) -> Result<(), BoxError> {
    eventually("the runs to finish", || {
        let records = vault.run_records();
        records.len() == count && records.iter().all(|(_, text)| !text.contains("status: running"))
    })
    .await?;
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    Ok(())
}

fn run_failed_origins(vault: &JobVault) -> Vec<String> {
    let mut origins: Vec<String> = vault
        .raised_tasks_for("run-failed")
        .iter()
        .filter_map(|(_, text)| field(text, "origin"))
        .collect();
    origins.sort();
    origins
}

fn claim_folder(vault: &JobVault) -> PathBuf {
    vault
        .config
        .path()
        .join("claims")
        .join("vaults")
        .join(VAULT_NAME.to_ascii_lowercase())
        .join("ops")
        .join("sync")
}

fn mark(vault: &JobVault) -> Option<String> {
    std::fs::read_to_string(claim_folder(vault).join("nightly.run-failed-examined"))
        .ok()
        .map(|text| text.trim().to_owned())
}

/// A vault whose enabled, scheduled job is due on every pass and whose script fails, past its trial.
async fn failing_scheduled() -> Result<JobVault, BoxError> {
    let spec = Spec {
        script: FAILING_SCRIPT,
        ..Spec::default()
    };
    let state = enabled_state(&spec);
    JobVault::open(&Spec {
        state: Some(&state),
        ..spec
    })
    .await
}

/// The enabled state with a schedule that never falls due in these tests.
fn quiet(spec: &Spec<'_>) -> String {
    enabled_state(spec).replace("schedule = \"* * * * *\"", "schedule = \"0 0 1 1 *\"")
}

fn on_trial(state: &str) -> String {
    format!("{state}first_run = true\n")
}

/// Rewrites the lines of run `number`'s record that start with a key in `edits`, whole.
fn edit_record(vault: &JobVault, number: u64, edits: &[(&str, &str)]) -> Result<(), BoxError> {
    let id = format!("id: R-{number}");
    let (path, text) = vault
        .run_records()
        .into_iter()
        .find(|(_, text)| text.lines().any(|line| line == id))
        .ok_or("no such run record")?;
    let rewritten: Vec<String> = text
        .lines()
        .map(|line| {
            edits
                .iter()
                .find(|(key, _)| line.starts_with(&format!("{key}:")))
                .map_or_else(|| line.to_owned(), |(key, value)| format!("{key}: {value}"))
        })
        .collect();
    std::fs::write(path, rewritten.join("\n") + "\n")?;
    Ok(())
}

async fn manual_run(vault: &JobVault) -> Result<(), BoxError> {
    run_job(
        Arc::clone(&vault.clients),
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
    Ok(())
}

/// A vault whose job never falls due and whose history holds the records the test writes. R-1 is a failed scheduled run
/// whose script started; more records come from [`add_record`].
async fn quiet_vault_with_a_failed_scheduled_first_record() -> Result<JobVault, BoxError> {
    let spec = Spec {
        script: FAILING_SCRIPT,
        ..Spec::default()
    };
    let state = quiet(&spec);
    let vault = JobVault::open(&Spec {
        state: Some(&state),
        ..spec
    })
    .await?;
    manual_run(&vault).await?;
    edit_record(
        &vault,
        1,
        &[("trigger", "scheduled"), ("started", "2026-10-01T03:00:30Z")],
    )?;
    Ok(vault)
}

/// Writes a record for run `number` beside R-1's, like it but with `status`, `code`, and the start time given, and moves
/// the job's run counter past it.
fn add_record(vault: &JobVault, number: u64, status: &str, code: &str, started: &str) -> Result<(), BoxError> {
    let (path, text) = vault
        .run_records()
        .into_iter()
        .find(|(_, text)| text.lines().any(|line| line == "id: R-1"))
        .ok_or("no first record to copy")?;
    let rewritten: Vec<String> = text
        .lines()
        .map(|line| {
            if line.starts_with("id:") {
                format!("id: R-{number}")
            } else if line.starts_with("title:") {
                format!("title: 'Run: ops/sync/nightly R-{number}'")
            } else if line.starts_with("status:") {
                format!("status: {status}")
            } else if line.starts_with("started:") {
                format!("started: {started}")
            } else if line.starts_with("code:") {
                format!("code: {code}")
            } else {
                line.to_owned()
            }
        })
        .collect();
    let sibling = path.with_file_name(format!("R-{number}.md"));
    std::fs::write(sibling, rewritten.join("\n") + "\n")?;
    std::fs::write(
        claim_folder(vault).join("nightly.next-run-id"),
        (number + 1).to_string(),
    )?;
    Ok(())
}

// ---------------------------------------------------------------------
// The record's own field decides whether the script started
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_run_recorded_as_started_raises_a_task_even_when_its_code_is_one_a_run_that_never_started_ends_with()
-> Result<(), BoxError> {
    let vault = failing_scheduled().await?;
    vault.pass(NOW).await;
    runs_finished(&vault, 1).await?;
    edit_record(&vault, 1, &[("code", "workflow/server-unavailable")])?;

    vault.pass(NOW + Span::seconds(10)).await;

    assert_eq!(
        run_failed_origins(&vault),
        vec![origin(1)],
        "the stored field says the script started, so the code is not consulted"
    );
    Ok(())
}

#[tokio::test]
async fn a_run_recorded_as_never_started_raises_nothing_whatever_its_code() -> Result<(), BoxError> {
    let vault = failing_scheduled().await?;
    vault.pass(NOW).await;
    runs_finished(&vault, 1).await?;
    edit_record(&vault, 1, &[("script_started", "false")])?;

    vault.pass(NOW + Span::seconds(10)).await;
    vault.pass(NOW + Span::seconds(20)).await;

    assert!(
        vault.raised_tasks_for("run-failed").is_empty(),
        "the stored field says the script never started, so a script error code does not make it a failure: {:?}",
        vault.raised_tasks()
    );
    Ok(())
}

#[tokio::test]
async fn a_record_whose_started_field_cannot_be_read_leaves_the_failure_to_be_raised_once_it_is_repaired()
-> Result<(), BoxError> {
    let vault = failing_scheduled().await?;
    vault.pass(NOW).await;
    runs_finished(&vault, 1).await?;
    // The code alone would say the run never started, so reading the field badly must not fall back to it.
    edit_record(
        &vault,
        1,
        &[("code", "workflow/server-unavailable"), ("script_started", "maybe")],
    )?;

    vault.pass(NOW + Span::seconds(10)).await;
    assert!(
        vault.raised_tasks_for("run-failed").is_empty(),
        "nothing can be decided yet"
    );
    assert_eq!(mark(&vault), None, "an unreadable record is not an examined run");

    edit_record(&vault, 1, &[("script_started", "true")])?;
    vault.pass(NOW + Span::seconds(20)).await;

    assert_eq!(
        run_failed_origins(&vault),
        vec![origin(1)],
        "the run is looked at again once its record can be read"
    );
    Ok(())
}

#[tokio::test]
async fn a_run_history_that_cannot_be_read_leaves_the_mark_unmoved_and_the_failure_is_raised_on_a_later_pass()
-> Result<(), BoxError> {
    let vault = failing_scheduled().await?;
    vault.pass(NOW).await;
    runs_finished(&vault, 1).await?;
    // A job run record with no status makes the whole history unusable.
    let stray = vault.root().join("memory/jobs/ops/sync/nightly/2026/10/01/R-9.md");
    std::fs::write(&stray, "---\ntype: job-run\nid: R-9\n---\n")?;

    vault.pass(NOW + Span::seconds(10)).await;
    assert_eq!(vault.raised_tasks_for("run-failed").len(), 0);
    assert_eq!(mark(&vault), None, "nothing was examined");

    std::fs::remove_file(stray)?;
    vault.pass(NOW + Span::seconds(20)).await;

    assert_eq!(run_failed_origins(&vault), vec![origin(1)]);
    Ok(())
}

// ---------------------------------------------------------------------
// Every finished run above the mark is examined
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_failure_followed_by_a_success_between_two_passes_still_raises_a_task_for_the_failure() -> Result<(), BoxError>
{
    let vault = quiet_vault_with_a_failed_scheduled_first_record().await?;
    add_record(&vault, 2, "succeeded", "~", "2026-10-01T04:00:30Z")?;

    vault.pass(NOW).await;
    vault.pass(NOW + Span::seconds(10)).await;

    assert_eq!(
        run_failed_origins(&vault),
        vec![origin(1)],
        "R-1 failed and was never examined, whatever ran after it"
    );
    assert_eq!(mark(&vault).as_deref(), Some("2"), "both runs were examined");
    Ok(())
}

#[tokio::test]
async fn several_unexamined_failures_raise_one_task_for_the_earliest_and_fold_the_rest_into_it() -> Result<(), BoxError>
{
    let vault = quiet_vault_with_a_failed_scheduled_first_record().await?;
    add_record(&vault, 2, "failed", "workflow/script-error", "2026-10-01T04:00:30Z")?;
    add_record(&vault, 3, "failed", "workflow/script-error", "2026-10-01T05:00:30Z")?;

    vault.pass(NOW).await;
    vault.pass(NOW + Span::seconds(10)).await;

    assert_eq!(
        run_failed_origins(&vault),
        vec![origin(1)],
        "the first unfolded failure raises, and the later ones fold into it"
    );
    assert_eq!(
        mark(&vault).as_deref(),
        Some("3"),
        "the mark moves to the highest examined run"
    );
    Ok(())
}

// ---------------------------------------------------------------------
// A failure a suspension handles is not raised again
// ---------------------------------------------------------------------

/// A job on trial whose scheduled first run fails, so the run suspends it. Returns the vault once it is suspended.
async fn suspended_by_a_failed_first_run() -> Result<JobVault, BoxError> {
    let spec = Spec {
        script: FAILING_SCRIPT,
        ..Spec::default()
    };
    let state = on_trial(&enabled_state(&spec));
    let vault = JobVault::open(&Spec {
        state: Some(&state),
        ..spec
    })
    .await?;
    vault.pass(NOW).await;
    runs_finished(&vault, 1).await?;
    eventually("the failed first run to suspend the job", || {
        vault.lifecycle().is_ok_and(|lifecycle| lifecycle == "suspended")
    })
    .await?;
    // The suspension is recorded before the mark moves to the run it names, so the mark is waited for as well.
    eventually("the mark to move to the suspending run", || mark(&vault).is_some()).await?;
    Ok(vault)
}

#[tokio::test]
async fn recording_a_suspension_that_names_a_run_moves_the_examined_mark_to_that_run() -> Result<(), BoxError> {
    let vault = suspended_by_a_failed_first_run().await?;

    assert_eq!(
        mark(&vault).as_deref(),
        Some("1"),
        "the suspension handles R-1's failure, so it is examined before any pass can look at it"
    );
    Ok(())
}

#[tokio::test]
async fn a_failed_first_run_that_suspended_the_job_raises_no_run_failed_task_after_the_job_is_resumed()
-> Result<(), BoxError> {
    let vault = suspended_by_a_failed_first_run().await?;
    vault.pass(NOW + Span::seconds(10)).await;
    assert_eq!(
        vault.raised_tasks_for("suspended").len(),
        1,
        "the suspension raises its one task"
    );

    // A resumed job: enabled, trial over, and not due again in this test.
    let spec = Spec {
        script: FAILING_SCRIPT,
        ..Spec::default()
    };
    vault.write(support::jobs::STATE_PATH, &quiet(&spec))?;
    vault.pass(minute(2)).await;
    vault.pass(minute(2) + Span::seconds(10)).await;

    assert!(
        vault.raised_tasks_for("run-failed").is_empty(),
        "the failure was handled by the suspension: {:?}",
        vault.raised_tasks()
    );
    assert_eq!(vault.raised_tasks().len(), 1, "only the suspended task exists");
    Ok(())
}

#[tokio::test]
async fn a_run_of_the_version_on_trial_is_not_examined_until_the_trial_ends() -> Result<(), BoxError> {
    // A failed scheduled run is finished and recorded, but its outcome has not been applied to the trial yet.
    let vault = quiet_vault_with_a_failed_scheduled_first_record().await?;
    let spec = Spec {
        script: FAILING_SCRIPT,
        ..Spec::default()
    };
    vault.write(support::jobs::STATE_PATH, &on_trial(&quiet(&spec)))?;

    vault.pass(NOW).await;
    vault.pass(NOW + Span::seconds(10)).await;

    assert!(
        vault.raised_tasks_for("run-failed").is_empty(),
        "settlement owns this outcome: {:?}",
        vault.raised_tasks()
    );
    assert_eq!(mark(&vault), None, "the run was not examined, so the mark is untouched");

    vault.write(support::jobs::STATE_PATH, &quiet(&spec))?;
    vault.pass(NOW + Span::seconds(20)).await;
    assert_eq!(
        run_failed_origins(&vault),
        vec![origin(1)],
        "the counter was untouched too, so the run is looked at as soon as the trial is over"
    );
    Ok(())
}

/// A job suspended by its failed first run, which the host has not yet marked examined: the failed scheduled run is
/// recorded, and the state names it.
async fn suspended_naming_a_failed_run_the_mark_has_not_reached() -> Result<JobVault, BoxError> {
    let vault = quiet_vault_with_a_failed_scheduled_first_record().await?;
    let spec = Spec {
        script: FAILING_SCRIPT,
        ..Spec::default()
    };
    let state = format!(
        "{}first_run = true\n\n[ops.sync.nightly.suspension]\ncode = \"workflow/probation-failed\"\n\
         by = \"system\"\nat = \"2026-10-01T03:00:31Z\"\nreason = \"the first live run failed\"\nrun = 1\n\
         failure = \"workflow/script-error\"\n",
        support::jobs::suspended_state(&spec)
    );
    vault.write(support::jobs::STATE_PATH, &state)?;
    Ok(vault)
}

#[tokio::test]
async fn a_pass_that_sees_a_suspension_naming_a_failed_run_raises_no_run_failed_task_and_moves_the_mark_to_it()
-> Result<(), BoxError> {
    let vault = suspended_naming_a_failed_run_the_mark_has_not_reached().await?;

    vault.pass(NOW).await;

    assert_eq!(
        mark(&vault).as_deref(),
        Some("1"),
        "the suspension covers R-1, so it is examined"
    );
    assert!(
        vault.raised_tasks_for("run-failed").is_empty(),
        "the suspension's task covers the failure: {:?}",
        vault.raised_tasks()
    );

    // Resumed, with the trial over and not due again: the failure still never surfaces as a run-failed task.
    let spec = Spec {
        script: FAILING_SCRIPT,
        ..Spec::default()
    };
    vault.write(support::jobs::STATE_PATH, &quiet(&spec))?;
    vault.pass(minute(2)).await;
    vault.pass(minute(2) + Span::seconds(10)).await;
    assert!(
        vault.raised_tasks_for("run-failed").is_empty(),
        "{:?}",
        vault.raised_tasks()
    );
    Ok(())
}

// ---------------------------------------------------------------------
// A task that could not be raised is raised on a later pass
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_run_failed_task_whose_creation_fails_once_is_raised_exactly_once_on_the_following_pass()
-> Result<(), BoxError> {
    let vault = failing_scheduled().await?;
    vault.pass(NOW).await;
    runs_finished(&vault, 1).await?;
    // With a file where the tasks folder should be, creating a task note is refused.
    std::fs::write(vault.root().join("memory/tasks"), "not a folder")?;

    vault.pass(NOW + Span::seconds(10)).await;
    assert!(vault.raised_tasks().is_empty(), "the create failed");
    assert_eq!(mark(&vault), None, "a run whose task was not raised is not examined");

    std::fs::remove_file(vault.root().join("memory/tasks"))?;
    vault.pass(NOW + Span::seconds(20)).await;
    vault.pass(NOW + Span::seconds(30)).await;

    assert_eq!(
        run_failed_origins(&vault),
        vec![origin(1)],
        "exactly one task, after the retry"
    );
    assert_eq!(mark(&vault).as_deref(), Some("1"));
    Ok(())
}
