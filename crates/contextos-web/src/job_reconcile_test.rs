use super::*;
use crate::job_attention::JobAttentionMemory;
use crate::task_sink::{ExistingTask, RaiseOutcome};
use crate::task_sink_fake::FakeTaskSink;
use crate::tasks_config::TasksConfig;

fn a_run(status: &str, trigger: &str, dry_run: bool, run_id: &str) -> RunSummary {
    RunSummary {
        path: "memory/jobs/ops/sync/nightly/2026/10/R-7.md".to_owned(),
        run_id: run_id.to_owned(),
        job: String::new(),
        release: String::new(),
        dry_run,
        trigger: trigger.to_owned(),
        status: status.to_owned(),
        started: "2026-10-01T03:00:00Z".to_owned(),
        completed: "2026-10-01T03:00:05Z".to_owned(),
        error: "the script gave up".to_owned(),
        code: Some("workflow/script-error".to_owned()),
        script_started: crate::job_runs::ScriptStarted::Absent,
    }
}

/// The finished run a record is, or why it is not one. `NotFinished` carries no detail, so there is none to show.
fn finished(record: &RunSummary) -> Result<FinishedRun, &'static str> {
    FinishedRun::try_from(record).map_err(|NotFinished| "the record is not a finished live run")
}

fn key() -> JobKey {
    JobKey {
        package: "ops".to_owned(),
        workflow: "sync".to_owned(),
        job: "nightly".to_owned(),
    }
}

#[test]
fn every_status_a_live_run_ends_with_is_a_finished_run() -> Result<(), &'static str> {
    for (status, expected) in [
        ("succeeded", RunStatus::Succeeded),
        ("degraded", RunStatus::Degraded),
        ("failed", RunStatus::Failed),
        ("cancelled", RunStatus::Cancelled),
        ("interrupted", RunStatus::Interrupted),
    ] {
        let finished = finished(&a_run(status, "scheduled", false, "R-7"))?;

        assert_eq!(finished.status, expected, "{status}");
        assert_eq!(finished.number, 7);
        assert!(!finished.dry_run);
    }
    Ok(())
}

#[test]
fn a_run_still_going_and_a_skipped_tick_and_an_unknown_status_are_not_finished_runs() {
    for status in ["running", "skipped", "", "exploded"] {
        assert!(
            FinishedRun::try_from(&a_run(status, "scheduled", false, "R-7")).is_err(),
            "{status:?}"
        );
    }
}

#[test]
fn only_a_scheduled_or_manual_trigger_makes_a_finished_run() -> Result<(), &'static str> {
    assert_eq!(
        finished(&a_run("failed", "scheduled", false, "R-7"))?.trigger,
        RunTrigger::Scheduled
    );
    assert_eq!(
        finished(&a_run("failed", "manual", false, "R-7"))?.trigger,
        RunTrigger::Manual
    );
    for trigger in ["", "cron", "Scheduled"] {
        assert!(
            FinishedRun::try_from(&a_run("failed", trigger, false, "R-7")).is_err(),
            "{trigger:?}"
        );
    }
    Ok(())
}

#[test]
fn a_dry_run_is_never_a_finished_live_run() {
    assert!(FinishedRun::try_from(&a_run("failed", "manual", true, "R-7")).is_err());
    assert!(FinishedRun::try_from(&a_run("succeeded", "manual", true, "R-7")).is_err());
}

#[test]
fn a_run_with_no_number_is_not_a_finished_run() {
    for run_id in ["", "7", "R-", "R-x", "r-7"] {
        assert!(
            FinishedRun::try_from(&a_run("failed", "scheduled", false, run_id)).is_err(),
            "{run_id:?}"
        );
    }
}

#[test]
fn the_failure_code_is_kept_when_there_is_one_and_absent_when_it_is_empty_or_missing() -> Result<(), &'static str> {
    let mut record = a_run("failed", "scheduled", false, "R-7");
    assert_eq!(
        finished(&record)?.failure_code.as_deref(),
        Some("workflow/script-error")
    );
    record.code = Some(String::new());
    assert_eq!(finished(&record)?.failure_code, None);
    record.code = None;
    assert_eq!(finished(&record)?.failure_code, None);
    Ok(())
}

/// A failed run numbered `number`, as far as the fold decision knows it.
fn run_numbered(number: u64) -> FailedRun {
    FailedRun { number }
}

fn host_slot(root: &std::path::Path) -> Result<crate::run_claim::ClaimSlot, crate::run_claim::RunClaimError> {
    crate::run_claim::ClaimSlot::new(root.join("vaults").join("work").join("sync"), "nightly")
}

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Whether the run is one to raise a task for: the decision's `New`, with a folded run and an undecided one both `false`.
async fn run_failure_is_new(
    sink: &dyn crate::task_sink::TaskSink,
    memory: &JobAttentionMemory,
    vault_name: &str,
    key: &JobKey,
    config: &TasksConfig,
    slot: &crate::run_claim::ClaimSlot,
    run: &FailedRun,
) -> bool {
    decide_run_failure(sink, memory, vault_name, key, config, slot, run).await == RunDecision::New
}

#[tokio::test]
async fn a_task_list_that_could_not_be_read_leaves_the_examined_mark_where_it_was_and_the_run_is_raised_next_pass()
-> TestResult {
    let root = tempfile::tempdir()?;
    let slot = host_slot(root.path())?;
    slot.advance_run_failure_mark(4)?;
    let sink = FakeTaskSink::answering(RaiseOutcome::Created("A-1".to_owned()));
    sink.fail_next_listing("unreachable");
    let memory = JobAttentionMemory::default();
    let config = TasksConfig::default();
    assert!(
        memory.run_counter_moved("work", &key(), 6),
        "the first look at the counter"
    );
    assert!(
        !memory.run_counter_moved("work", &key(), 6),
        "and a second look at the same counter would find nothing new"
    );

    let first = run_failure_is_new(&sink, &memory, "work", &key(), &config, &slot, &run_numbered(5)).await;

    assert!(!first, "nothing can be decided while the tasks cannot be listed");
    assert!(
        memory.run_counter_moved("work", &key(), 6),
        "the counter was forgotten, so the run is looked at again on the next pass"
    );
    assert_eq!(
        slot.run_failure_mark()?,
        Some(4),
        "run 5 was not examined, so the mark stays"
    );
    let second = run_failure_is_new(&sink, &memory, "work", &key(), &config, &slot, &run_numbered(5)).await;
    assert!(second, "the listing works the next time, so run 5 is a new failure");
    Ok(())
}

#[tokio::test]
async fn a_failure_folded_into_an_open_task_moves_the_examined_mark_past_it() -> TestResult {
    let root = tempfile::tempdir()?;
    let slot = host_slot(root.path())?;
    let sink = FakeTaskSink::answering(RaiseOutcome::Created("A-1".to_owned()));
    if let Ok(mut existing) = sink.existing.lock() {
        existing.push(ExistingTask {
            origin: "contextos-web:run-failed:ops/sync/nightly:R-3".to_owned(),
            task_type: "action".to_owned(),
            state: "next".to_owned(),
        });
    }

    let new = run_failure_is_new(
        &sink,
        &JobAttentionMemory::default(),
        "work",
        &key(),
        &TasksConfig::default(),
        &slot,
        &run_numbered(5),
    )
    .await;

    assert!(!new);
    assert_eq!(slot.run_failure_mark()?, Some(5), "it was examined and folded");
    Ok(())
}

#[tokio::test]
async fn a_mark_ahead_of_the_records_does_not_suppress_a_failure_numbered_above_it() -> TestResult {
    let root = tempfile::tempdir()?;
    let slot = host_slot(root.path())?;
    slot.advance_run_failure_mark(10)?;
    let sink = FakeTaskSink::answering(RaiseOutcome::Created("A-1".to_owned()));

    let new = run_failure_is_new(
        &sink,
        &JobAttentionMemory::default(),
        "work",
        &key(),
        &TasksConfig::default(),
        &slot,
        &run_numbered(11),
    )
    .await;

    assert!(new, "run 11 was never examined, whatever the records below it held");
    Ok(())
}

#[tokio::test]
async fn a_failing_run_numbered_at_or_below_the_mark_is_already_examined_and_the_mark_stays() -> TestResult {
    let root = tempfile::tempdir()?;
    let slot = host_slot(root.path())?;
    slot.advance_run_failure_mark(10)?;
    let sink = FakeTaskSink::answering(RaiseOutcome::Created("A-1".to_owned()));

    for number in [3, 10] {
        let new = run_failure_is_new(
            &sink,
            &JobAttentionMemory::default(),
            "work",
            &key(),
            &TasksConfig::default(),
            &slot,
            &run_numbered(number),
        )
        .await;

        assert!(!new, "run {number}");
    }
    assert_eq!(slot.run_failure_mark()?, Some(10));
    Ok(())
}

#[tokio::test]
async fn a_mark_that_is_not_a_number_decides_nothing_and_is_left_alone() -> TestResult {
    let root = tempfile::tempdir()?;
    let slot = host_slot(root.path())?;
    slot.advance_run_failure_mark(1)?;
    std::fs::write(slot.directory().join("nightly.run-failed-examined"), "forty")?;
    let sink = FakeTaskSink::answering(RaiseOutcome::Created("A-1".to_owned()));

    let new = run_failure_is_new(
        &sink,
        &JobAttentionMemory::default(),
        "work",
        &key(),
        &TasksConfig::default(),
        &slot,
        &run_numbered(2),
    )
    .await;

    assert!(!new);
    assert_eq!(
        std::fs::read_to_string(slot.directory().join("nightly.run-failed-examined"))?,
        "forty"
    );
    Ok(())
}

#[tokio::test]
async fn a_mark_that_is_not_a_number_forgets_the_run_counter_so_the_run_is_looked_at_again() -> TestResult {
    let root = tempfile::tempdir()?;
    let slot = host_slot(root.path())?;
    slot.advance_run_failure_mark(1)?;
    std::fs::write(slot.directory().join("nightly.run-failed-examined"), "forty")?;
    let sink = FakeTaskSink::answering(RaiseOutcome::Created("A-1".to_owned()));
    let memory = JobAttentionMemory::default();
    assert!(memory.run_counter_moved("work", &key(), 3));
    assert!(!memory.run_counter_moved("work", &key(), 3));

    let new = run_failure_is_new(
        &sink,
        &memory,
        "work",
        &key(),
        &TasksConfig::default(),
        &slot,
        &run_numbered(2),
    )
    .await;

    assert!(!new);
    assert!(
        memory.run_counter_moved("work", &key(), 3),
        "the run waits for the mark to be repaired or removed, so the counter must not count as examined"
    );
    Ok(())
}

#[tokio::test]
async fn a_mark_that_cannot_be_read_forgets_the_run_counter_so_the_run_is_looked_at_again() -> TestResult {
    let root = tempfile::tempdir()?;
    let slot = host_slot(root.path())?;
    slot.advance_run_failure_mark(1)?;
    // A folder where the mark file should be cannot be read as text.
    let mark = slot.directory().join("nightly.run-failed-examined");
    std::fs::remove_file(&mark)?;
    std::fs::create_dir(&mark)?;
    let sink = FakeTaskSink::answering(RaiseOutcome::Created("A-1".to_owned()));
    let memory = JobAttentionMemory::default();
    assert!(memory.run_counter_moved("work", &key(), 3));
    assert!(!memory.run_counter_moved("work", &key(), 3));

    let new = run_failure_is_new(
        &sink,
        &memory,
        "work",
        &key(),
        &TasksConfig::default(),
        &slot,
        &run_numbered(2),
    )
    .await;

    assert!(!new);
    assert!(memory.run_counter_moved("work", &key(), 3), "the counter was forgotten");
    Ok(())
}
