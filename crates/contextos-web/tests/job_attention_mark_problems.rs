//! A host's examined mark that cannot be used (it is not a number, or it cannot be read) is reported once per job and
//! retried on every pass: the job's run is looked at again as soon as the mark is repaired or removed, and the report
//! is not repeated while the problem stays. Real `contextos` sessions over real temporary vaults, with a fixed clock.

mod support;

use std::path::PathBuf;
use std::time::Duration;

use support::BoxError;
use support::jobs::{FAILING_SCRIPT, JobVault, Spec, VAULT_NAME, enabled_state};
use support::logs::capture;
use time::macros::datetime;
use time::{Duration as Span, OffsetDateTime};

const NOW: OffsetDateTime = datetime!(2026-10-01 03:00:30 UTC);

fn field(text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    text.lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .map(|value| value.trim().trim_matches(|c| c == '\'' || c == '"').to_owned())
}

fn origin(number: u64) -> String {
    format!("contextos-web:run-failed:ops/sync/nightly:R-{number}")
}

async fn runs_finished(vault: &JobVault, count: usize) -> Result<(), BoxError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let records = vault.run_records();
        if records.len() == count && records.iter().all(|(_, text)| !text.contains("status: running")) {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("timed out waiting for the runs to finish".into());
        }
        // A pause, not a bare yield: each check walks the filesystem, blocking the one thread the run needs as well.
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
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

fn mark_path(vault: &JobVault) -> PathBuf {
    vault
        .config
        .path()
        .join("claims")
        .join("vaults")
        .join(VAULT_NAME.to_ascii_lowercase())
        .join("ops")
        .join("sync")
        .join("nightly.run-failed-examined")
}

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

fn seconds(n: i64) -> OffsetDateTime {
    NOW + Span::seconds(n)
}

#[tokio::test]
async fn a_mark_that_is_not_a_number_is_reported_once_and_the_blocked_run_raises_on_the_next_pass_after_it_is_removed()
-> Result<(), BoxError> {
    let vault = failing_scheduled().await?;
    vault.pass(NOW).await;
    runs_finished(&vault, 1).await?;
    std::fs::create_dir_all(mark_path(&vault).parent().ok_or("no folder")?)?;
    std::fs::write(mark_path(&vault), "forty")?;
    let (log, _guard) = capture();

    for n in [5, 10, 15] {
        vault.pass(seconds(n)).await;
    }

    assert!(
        vault.raised_tasks().is_empty(),
        "nothing can be decided: {:?}",
        vault.raised_tasks()
    );
    let warnings = log.at("WARN", "run-failed-examined");
    assert_eq!(warnings.len(), 1, "one report for three passes: {:?}", log.lines());

    std::fs::remove_file(mark_path(&vault))?;
    vault.pass(seconds(20)).await;

    assert_eq!(
        run_failed_origins(&vault),
        vec![origin(1)],
        "the run was held back by the mark alone, so it is looked at again as soon as the mark is gone"
    );
    Ok(())
}

#[tokio::test]
async fn a_mark_that_cannot_be_read_is_reported_once_and_the_blocked_run_raises_once_it_can() -> Result<(), BoxError> {
    let vault = failing_scheduled().await?;
    vault.pass(NOW).await;
    runs_finished(&vault, 1).await?;
    // A folder where the mark should be is a read error that is not "no such file".
    std::fs::create_dir_all(mark_path(&vault))?;
    let (log, _guard) = capture();

    for n in [5, 10, 15] {
        vault.pass(seconds(n)).await;
    }

    assert!(
        vault.raised_tasks().is_empty(),
        "nothing can be decided: {:?}",
        vault.raised_tasks()
    );
    let warnings = log.at("WARN", "run-failed mark");
    assert_eq!(warnings.len(), 1, "one report for three passes: {:?}", log.lines());

    std::fs::remove_dir(mark_path(&vault))?;
    vault.pass(seconds(20)).await;

    assert_eq!(run_failed_origins(&vault), vec![origin(1)]);
    Ok(())
}

#[tokio::test]
async fn a_mark_problem_that_returns_after_a_good_read_is_reported_again() -> Result<(), BoxError> {
    let vault = failing_scheduled().await?;
    vault.pass(NOW).await;
    runs_finished(&vault, 1).await?;
    std::fs::create_dir_all(mark_path(&vault).parent().ok_or("no folder")?)?;
    std::fs::write(mark_path(&vault), "forty")?;
    let (log, _guard) = capture();
    vault.pass(seconds(5)).await;
    vault.pass(seconds(10)).await;
    std::fs::remove_file(mark_path(&vault))?;
    vault.pass(seconds(15)).await;
    assert_eq!(
        run_failed_origins(&vault),
        vec![origin(1)],
        "a good read clears the report"
    );

    // A second failing run, whose examination meets a mark that is broken again.
    vault.pass(seconds(120)).await;
    runs_finished(&vault, 2).await?;
    std::fs::write(mark_path(&vault), "forty")?;
    vault.pass(seconds(130)).await;
    vault.pass(seconds(140)).await;

    assert_eq!(
        log.at("WARN", "run-failed-examined").len(),
        2,
        "one report for each time the problem started: {:?}",
        log.lines()
    );
    Ok(())
}
