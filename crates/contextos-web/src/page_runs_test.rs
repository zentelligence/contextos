use contextos_workflows::{DegradedReasons, RunFailure};
use time::macros::datetime;

use super::*;

const START: OffsetDateTime = datetime!(2026-10-05 03:00:00 UTC);
const END: OffsetDateTime = datetime!(2026-10-05 03:00:05 UTC);

fn table() -> Arc<PageRuns> {
    Arc::new(PageRuns::default())
}

#[test]
fn a_job_nobody_started_from_a_page_has_no_runs() {
    assert_eq!(table().of("work/sync/nightly"), PageRunsOfJob::default());
}

#[test]
fn a_run_is_running_from_when_it_starts_until_it_ends() {
    let runs = table();
    let guard = runs.begin("work/sync/nightly", PageRunKind::Dry, START);

    assert_eq!(runs.of("work/sync/nightly").running, vec![(PageRunKind::Dry, START)]);

    guard.finish(END, &RunOutcome::Succeeded);

    let after = runs.of("work/sync/nightly");
    assert!(
        after.running.is_empty(),
        "`after.running` should be empty, got {:?}",
        after.running
    );
    assert_eq!(
        after.last_ended,
        Some(PageRunEnded {
            kind: PageRunKind::Dry,
            at: END,
            ok: true,
            text: "succeeded".to_owned()
        })
    );
}

#[test]
fn a_failed_run_keeps_the_failure_with_its_code() {
    let runs = table();
    let failure = RunFailure::from("workflow/package-unavailable: the package is not installed".to_owned());

    runs.begin("work/sync/nightly", PageRunKind::Live, START)
        .finish(END, &RunOutcome::Failed(failure));

    let ended = runs.of("work/sync/nightly").last_ended;
    assert!(ended.as_ref().is_some_and(|ended| !ended.ok));
    assert!(ended.is_some_and(|ended| ended.text.starts_with("workflow/package-unavailable")),);
}

#[test]
fn a_run_with_warnings_counts_as_succeeded() {
    let runs = table();

    runs.begin("job", PageRunKind::Live, START).finish(
        END,
        &RunOutcome::Degraded(DegradedReasons {
            kept: vec!["a retried call".to_owned()],
            omitted: 0,
        }),
    );

    assert_eq!(
        runs.of("job").last_ended.map(|ended| (ended.ok, ended.text)),
        Some((true, "succeeded with warnings".to_owned()))
    );
}

#[test]
fn a_cancelled_run_is_not_a_success() {
    let runs = table();

    runs.begin("job", PageRunKind::Live, START)
        .finish(END, &RunOutcome::Cancelled);

    assert_eq!(
        runs.of("job").last_ended.map(|ended| (ended.ok, ended.text)),
        Some((false, "cancelled".to_owned()))
    );
}

#[test]
fn a_run_that_could_not_start_says_why() {
    let runs = table();

    runs.begin("job", PageRunKind::Live, START)
        .fail(END, "the package is not installed".to_owned());

    assert_eq!(
        runs.of("job").last_ended.map(|ended| ended.text),
        Some("the package is not installed".to_owned())
    );
}

#[test]
fn a_run_whose_task_was_dropped_is_not_left_running() {
    let runs = table();

    drop(runs.begin("job", PageRunKind::Dry, START));

    let after = runs.of("job");
    assert!(
        after.running.is_empty(),
        "`after.running` should be empty, got {:?}",
        after.running
    );
    assert_eq!(
        after.last_ended.map(|ended| (ended.ok, ended.text)),
        Some((false, "the run did not finish".to_owned()))
    );
}

#[test]
fn two_runs_of_one_job_are_both_running_until_each_ends() {
    let runs = table();
    let first = runs.begin("job", PageRunKind::Dry, START);
    let second = runs.begin("job", PageRunKind::Dry, END);
    assert_eq!(runs.of("job").running.len(), 2);

    second.finish(END, &RunOutcome::Succeeded);
    assert_eq!(runs.of("job").running.len(), 1, "the first is still running");

    first.finish(END, &RunOutcome::Succeeded);
    assert!(
        runs.of("job").running.is_empty(),
        "job should have no running runs, got {:?}",
        runs.of("job").running
    );
}

#[test]
fn another_jobs_runs_are_its_own() {
    let runs = table();
    runs.begin("one", PageRunKind::Dry, START);

    assert!(
        runs.of("two").running.is_empty(),
        "two should have no running runs, got {:?}",
        runs.of("two").running
    );
}

#[test]
fn the_job_that_started_a_run_longest_ago_is_forgotten_when_the_table_is_full() {
    let runs = table();
    for number in 0..MAX_JOBS {
        let at = START + time::Duration::seconds(i64::try_from(number).unwrap_or_default());
        runs.begin(&format!("job-{number}"), PageRunKind::Dry, at)
            .finish(at, &RunOutcome::Succeeded);
    }

    let _one_more = runs.begin("one-more", PageRunKind::Dry, END + time::Duration::days(1));

    assert!(runs.of("job-0").last_ended.is_none(), "the least recent was forgotten");
    assert!(runs.of("job-1").last_ended.is_some());
    assert_eq!(runs.of("one-more").running.len(), 1);
}

#[test]
fn a_job_with_a_run_in_progress_is_never_forgotten() {
    let runs = table();
    let _in_progress = runs.begin("job-0", PageRunKind::Live, START);
    for number in 1..=MAX_JOBS {
        let at = START + time::Duration::seconds(i64::try_from(number).unwrap_or_default());
        runs.begin(&format!("job-{number}"), PageRunKind::Dry, at)
            .finish(at, &RunOutcome::Succeeded);
    }

    assert_eq!(runs.of("job-0").running.len(), 1);
}
