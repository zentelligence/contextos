use std::sync::Mutex;

use time::macros::datetime;

use super::*;
use crate::job_runs::ScriptStarted;
use crate::jobs::ResolvedEntity;

const NOW: OffsetDateTime = datetime!(2026-10-28 03:00:00 UTC);

fn job() -> ObservedJob {
    ObservedJob {
        workflow_slug: "sync".to_owned(),
        job_slug: "nightly".to_owned(),
        package: "ops".to_owned(),
        directory: "sync".to_owned(),
        file: "nightly.toml".to_owned(),
        entry: None,
        ready: true,
        version: None,
        declared_owner: None,
        entity: ResolvedEntity::from((Some("acme"), None)),
    }
}

fn run(number: u64, trigger: &str, status: &str, code: Option<&str>, started: &str) -> RunSummary {
    RunSummary {
        path: format!("memory/jobs/ops/sync/nightly/R-{number}.md"),
        run_id: format!("R-{number}"),
        job: String::new(),
        release: String::new(),
        dry_run: false,
        trigger: trigger.to_owned(),
        status: status.to_owned(),
        started: started.to_owned(),
        completed: started.to_owned(),
        error: String::new(),
        code: code.map(str::to_owned),
        script_started: ScriptStarted::Yes,
    }
}

const RECENT: &str = "2026-10-27T10:00:00Z";

/// Where a first reconcile at [`NOW`] puts its floor.
fn not_before() -> OffsetDateTime {
    NOW - FIRST_RECONCILE_LOOKBACK
}

fn kinds(events: &[JobEvent]) -> Vec<(&'static str, Option<&str>)> {
    events
        .iter()
        .map(|event| (event.kind.name(), event.run.as_deref()))
        .collect()
}

#[test]
fn a_failed_scheduled_run_makes_a_failed_event_naming_its_record_code_and_entity() {
    let history = [run(3, "scheduled", "failed", Some("workflow/timed-out"), RECENT)];

    let events = run_events(&job(), &history, None, not_before());

    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.kind, JobEventKind::Failed);
    assert_eq!(event.code.as_deref(), Some("workflow/timed-out"));
    assert_eq!(event.entity, "acme");
    assert_eq!(event.subject, "[[memory/jobs/ops/sync/nightly/R-3|R-3]]");
    assert_eq!(event.id(), "job.failed:ops/sync/nightly:R-3");
}

#[test]
fn a_failed_run_with_no_code_is_recorded_with_the_generic_failure_code() {
    let history = [run(1, "scheduled", "failed", None, RECENT)];

    let events = run_events(&job(), &history, None, not_before());

    assert_eq!(events[0].code.as_deref(), Some("workflow/failed"));
}

#[test]
fn only_a_success_straight_after_a_failure_makes_a_recovered_event() {
    let history = [
        run(1, "scheduled", "succeeded", None, RECENT),
        run(2, "scheduled", "failed", Some("workflow/script-error"), RECENT),
        run(3, "scheduled", "succeeded", None, RECENT),
        run(4, "scheduled", "succeeded", None, RECENT),
    ];

    let events = run_events(&job(), &history, None, not_before());

    assert_eq!(
        kinds(&events),
        vec![("job.failed", Some("R-2")), ("job.recovered", Some("R-3"))]
    );
}

#[test]
fn history_given_newest_first_is_read_in_run_order() {
    let history = [
        run(3, "scheduled", "succeeded", None, RECENT),
        run(2, "scheduled", "failed", Some("workflow/script-error"), RECENT),
    ];

    let events = run_events(&job(), &history, None, not_before());

    assert_eq!(
        kinds(&events),
        vec![("job.failed", Some("R-2")), ("job.recovered", Some("R-3"))]
    );
}

#[test]
fn a_run_started_by_hand_a_dry_run_and_other_outcomes_make_no_event() {
    let mut dry = run(2, "scheduled", "failed", Some("workflow/script-error"), RECENT);
    dry.dry_run = true;
    let history = [
        run(1, "manual", "failed", Some("workflow/script-error"), RECENT),
        dry,
        run(3, "scheduled", "cancelled", None, RECENT),
        run(4, "scheduled", "interrupted", None, RECENT),
        run(5, "scheduled", "degraded", None, RECENT),
        run(6, "scheduled", "skipped", None, RECENT),
        run(7, "scheduled", "running", None, RECENT),
    ];

    assert_eq!(run_events(&job(), &history, None, not_before()).len(), 0);
}

#[test]
fn a_manual_failure_does_not_stand_between_a_scheduled_failure_and_the_success_that_recovers_it() {
    let history = [
        run(1, "scheduled", "failed", Some("workflow/script-error"), RECENT),
        run(2, "manual", "succeeded", None, RECENT),
        run(3, "scheduled", "succeeded", None, RECENT),
    ];

    let events = run_events(&job(), &history, None, not_before());

    assert_eq!(
        kinds(&events),
        vec![("job.failed", Some("R-1")), ("job.recovered", Some("R-3"))]
    );
}

#[test]
fn a_run_older_than_the_lookback_makes_no_event_but_still_counts_as_the_previous_run() {
    let history = [
        run(
            1,
            "scheduled",
            "failed",
            Some("workflow/script-error"),
            "2026-09-01T10:00:00Z",
        ),
        run(2, "scheduled", "succeeded", None, RECENT),
    ];

    let events = run_events(&job(), &history, None, not_before());

    assert_eq!(kinds(&events), vec![("job.recovered", Some("R-2"))]);
}

#[test]
fn a_failure_after_the_first_reconcile_is_recorded_however_long_ago_that_run_was_whenever_it_is_read() {
    let history = [run(
        1,
        "scheduled",
        "failed",
        Some("workflow/script-error"),
        "2026-09-01T10:00:00Z",
    )];

    let first_reconcile = datetime!(2026-08-28 03:00 UTC);

    let events = run_events(&job(), &history, None, first_reconcile - FIRST_RECONCILE_LOOKBACK);
    let before_it = run_events(&job(), &history, None, datetime!(2026-09-02 00:00 UTC));

    assert_eq!(kinds(&events), vec![("job.failed", Some("R-1"))]);
    assert!(
        before_it.is_empty(),
        "a run before the first reconcile's lookback is never recorded"
    );
}

#[test]
fn a_failure_to_record_is_identified_by_its_job_and_kind_not_its_run() {
    let failed = run_events(
        &job(),
        &[
            run(3, "scheduled", "failed", Some("workflow/script-error"), RECENT),
            run(4, "scheduled", "failed", Some("workflow/other"), RECENT),
        ],
        None,
        not_before(),
    );

    assert_eq!(failed[0].job_and_kind(), failed[1].job_and_kind());
    assert_ne!(failed[0].id(), failed[1].id());
}

#[derive(Default)]
struct FakeSink {
    accepts: bool,
    suspension_unreadable: bool,
    suspension_is_open: bool,
    outcomes: Mutex<Vec<RecordOutcome>>,
    recorded: Mutex<Vec<String>>,
}

impl FakeSink {
    fn accepting(outcomes: Vec<RecordOutcome>) -> Self {
        Self {
            accepts: true,
            suspension_unreadable: false,
            suspension_is_open: false,
            outcomes: Mutex::new(outcomes),
            recorded: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<String> {
        self.recorded.lock().map(|calls| calls.clone()).unwrap_or_default()
    }
}

impl EventSink for FakeSink {
    fn accepts_events<'a>(&'a self) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move { self.accepts })
    }

    fn suspension_open<'a>(
        &'a self,
        _job: &'a JobKey,
    ) -> Pin<Box<dyn Future<Output = Result<bool, SuspensionUnreadable>> + Send + 'a>> {
        Box::pin(async move {
            if self.suspension_unreadable {
                Err(SuspensionUnreadable)
            } else {
                Ok(self.suspension_is_open)
            }
        })
    }

    fn record<'a>(&'a self, event: &'a JobEvent) -> Pin<Box<dyn Future<Output = RecordOutcome> + Send + 'a>> {
        Box::pin(async move {
            if let Ok(mut calls) = self.recorded.lock() {
                calls.push(event.id());
            }
            self.outcomes
                .lock()
                .ok()
                .and_then(|mut outcomes| (!outcomes.is_empty()).then(|| outcomes.remove(0)))
                .unwrap_or(RecordOutcome::Recorded)
        })
    }
}

fn pending(runs: &[u64]) -> Vec<(JobKey, JobEvent)> {
    let history: Vec<RunSummary> = runs
        .iter()
        .map(|number| run(*number, "scheduled", "failed", Some("workflow/script-error"), RECENT))
        .collect();
    run_events(&job(), &history, None, not_before())
        .into_iter()
        .map(|event| {
            (
                JobKey {
                    package: "ops".to_owned(),
                    workflow: "sync".to_owned(),
                    job: "nightly".to_owned(),
                },
                event,
            )
        })
        .collect()
}

#[tokio::test]
async fn a_recorded_or_already_recorded_event_is_remembered_and_not_asked_for_again() {
    let memory = JobAttentionMemory::default();
    let sink = FakeSink::accepting(vec![RecordOutcome::Recorded, RecordOutcome::AlreadyRecorded]);

    record_pending(&sink, &memory, "vault", &pending(&[1, 2]), NOW).await;

    assert!(memory.knows_event("vault", "job.failed:ops/sync/nightly:R-1"));
    assert!(memory.knows_event("vault", "job.failed:ops/sync/nightly:R-2"));
}

#[tokio::test]
async fn a_failed_record_is_not_remembered_and_the_jobs_runs_are_read_again() {
    let memory = JobAttentionMemory::default();
    let key = JobKey {
        package: "ops".to_owned(),
        workflow: "sync".to_owned(),
        job: "nightly".to_owned(),
    };
    assert!(memory.event_counter_moved("vault", &key, 4));
    let sink = FakeSink::accepting(vec![RecordOutcome::Failed("io/vault-busy".to_owned())]);

    record_pending(&sink, &memory, "vault", &pending(&[1]), NOW).await;

    assert!(!memory.knows_event("vault", "job.failed:ops/sync/nightly:R-1"));
    assert!(
        memory.event_counter_moved("vault", &key, 4),
        "the runs are read on the next pass"
    );
}

#[tokio::test]
async fn a_vault_that_takes_no_events_is_left_alone_for_a_pause_and_logged_once() {
    let memory = JobAttentionMemory::default();
    let sink = FakeSink::default();

    record_pending(&sink, &memory, "vault", &pending(&[1]), NOW).await;

    assert_eq!(
        sink.calls().len(),
        0,
        "nothing is asked of a vault that accepts no events"
    );
    assert_eq!(memory.events_unsupported_since("vault"), Some(NOW));
    assert!(
        !memory.note_events_unsupported("vault", NOW),
        "the second finding is not news"
    );
}

#[tokio::test]
async fn an_unsupported_kind_stops_the_pass_and_leaves_the_vault_alone() {
    let memory = JobAttentionMemory::default();
    let sink = FakeSink::accepting(vec![RecordOutcome::Unsupported]);

    record_pending(&sink, &memory, "vault", &pending(&[1, 2]), NOW).await;

    assert_eq!(sink.calls().len(), 1, "the rest would be refused the same way");
    assert_eq!(memory.events_unsupported_since("vault"), Some(NOW));
}

#[test]
fn a_system_suspension_with_a_time_makes_a_suspended_event_for_the_job_file() -> Result<(), Box<dyn std::error::Error>>
{
    let state: JobsState = toml::from_str(
        "[ops.sync.nightly]\nlifecycle = \"suspended\"\n\n[ops.sync.nightly.suspension]\n\
         code = \"workflow/validation-failed\"\nby = \"system\"\nat = \"2026-10-27T10:00:00Z\"\nreason = \"x\"\n",
    )?;

    let event = suspension_event(&job(), &state).ok_or("a suspended event")?;

    assert_eq!(event.kind, JobEventKind::Suspended);
    assert_eq!(event.package, "ops");
    assert_eq!(event.code.as_deref(), Some("workflow/validation-failed"));
    assert_eq!(event.suspended_at.as_deref(), Some("2026-10-27T10:00:00Z"));
    assert_eq!(
        event.subject,
        "[[registry/workflows/ops/sync/jobs/nightly.toml|nightly]]"
    );
    Ok(())
}

#[test]
fn an_operators_suspension_and_a_job_that_is_not_suspended_make_no_event() -> Result<(), Box<dyn std::error::Error>> {
    let operators: JobsState = toml::from_str(
        "[ops.sync.nightly]\nlifecycle = \"suspended\"\n\n[ops.sync.nightly.suspension]\n\
         code = \"workflow/operator-suspended\"\nby = \"operator\"\nat = \"2026-10-27T10:00:00Z\"\nreason = \"x\"\n",
    )?;
    let enabled: JobsState = toml::from_str("[ops.sync.nightly]\nlifecycle = \"enabled\"\n")?;

    assert!(suspension_event(&job(), &operators).is_none());
    assert!(suspension_event(&job(), &enabled).is_none());
    Ok(())
}

#[test]
fn the_first_scheduled_success_after_a_recorded_suspension_was_resumed_makes_a_recovered_event() {
    let history = vec![
        run(1, "scheduled", "succeeded", None, "2026-10-27T10:00:00Z"),
        run(2, "scheduled", "succeeded", None, "2026-10-27T11:00:00Z"),
        run(3, "scheduled", "succeeded", None, "2026-10-27T12:00:00Z"),
    ];
    let resumed = datetime!(2026-10-27 10:30 UTC);

    let events = run_events(&job(), &history, Some(resumed), not_before());

    assert_eq!(kinds(&events), vec![("job.recovered", Some("R-2"))]);
}

#[test]
fn a_success_before_the_resumption_makes_no_recovered_event() {
    let history = vec![run(1, "scheduled", "succeeded", None, "2026-10-27T10:00:00Z")];

    assert_eq!(
        run_events(&job(), &history, Some(datetime!(2026-10-27 10:30 UTC)), not_before()).len(),
        0
    );
}

#[test]
fn a_failure_stays_unrecovered_across_runs_that_were_cancelled_interrupted_skipped_or_degraded() {
    let history = [
        run(1, "scheduled", "failed", Some("workflow/script-error"), RECENT),
        run(2, "scheduled", "cancelled", None, RECENT),
        run(3, "scheduled", "interrupted", None, RECENT),
        run(4, "scheduled", "degraded", None, RECENT),
        run(5, "scheduled", "skipped", None, RECENT),
        run(6, "scheduled", "succeeded", None, RECENT),
        run(7, "scheduled", "succeeded", None, RECENT),
    ];

    let events = run_events(&job(), &history, None, not_before());

    assert_eq!(
        kinds(&events),
        vec![("job.failed", Some("R-1")), ("job.recovered", Some("R-6"))],
        "the first success after the failure recovers it, and only that one"
    );
}

#[test]
fn a_degraded_or_cancelled_run_with_no_failure_before_it_recovers_nothing() {
    let history = [
        run(1, "scheduled", "succeeded", None, RECENT),
        run(2, "scheduled", "degraded", None, RECENT),
        run(3, "scheduled", "cancelled", None, RECENT),
        run(4, "scheduled", "succeeded", None, RECENT),
    ];

    assert_eq!(run_events(&job(), &history, None, not_before()).len(), 0);
}

#[test]
fn every_job_event_is_unique_by_its_kind_as_well_as_its_job_and_run_or_suspension() {
    let failed = run_events(
        &job(),
        &[run(3, "scheduled", "failed", Some("workflow/script-error"), RECENT)],
        None,
        not_before(),
    );
    let recovered = JobEvent {
        kind: JobEventKind::Recovered,
        ..failed[0].clone()
    };
    let suspended = JobEvent {
        kind: JobEventKind::Suspended,
        run: None,
        suspended_at: Some("2026-10-27T10:00:00Z".to_owned()),
        ..failed[0].clone()
    };

    assert_eq!(
        failed[0].unique_fields(),
        vec!["kind", "package", "workflow", "job", "run"]
    );
    assert_eq!(
        recovered.unique_fields(),
        vec!["kind", "package", "workflow", "job", "run"]
    );
    assert_eq!(
        suspended.unique_fields(),
        vec!["kind", "package", "workflow", "job", "suspended_at"]
    );
}

#[tokio::test]
async fn a_suspension_that_cannot_be_read_is_an_unsettled_decision_and_not_a_finding_of_no_recovery()
-> Result<(), Box<dyn std::error::Error>> {
    let state: JobsState =
        toml::from_str("[ops.sync.nightly]\nlifecycle = \"enabled\"\nlifecycle_changed = \"2026-10-27T10:30:00Z\"\n")?;
    let history = [run(2, "scheduled", "succeeded", None, "2026-10-27T11:00:00Z")];
    let unreadable = FakeSink {
        suspension_unreadable: true,
        ..FakeSink::accepting(Vec::new())
    };
    let open = FakeSink {
        suspension_is_open: true,
        ..FakeSink::accepting(Vec::new())
    };
    let closed = FakeSink::accepting(Vec::new());

    assert_eq!(
        resumption(&unreadable, &job(), &state, &history).await,
        Err(SuspensionUnreadable)
    );
    assert_eq!(
        resumption(&open, &job(), &state, &history).await,
        Ok(Some(datetime!(2026-10-27 10:30 UTC)))
    );
    assert_eq!(resumption(&closed, &job(), &state, &history).await, Ok(None));
    Ok(())
}

#[tokio::test]
async fn a_suspension_that_cannot_be_read_arranges_for_the_jobs_runs_to_be_read_again_on_the_next_pass()
-> Result<(), Box<dyn std::error::Error>> {
    let state: JobsState =
        toml::from_str("[ops.sync.nightly]\nlifecycle = \"enabled\"\nlifecycle_changed = \"2026-10-27T10:30:00Z\"\n")?;
    let history = [run(2, "scheduled", "succeeded", None, "2026-10-27T11:00:00Z")];
    let key = JobKey {
        package: "ops".to_owned(),
        workflow: "sync".to_owned(),
        job: "nightly".to_owned(),
    };
    let memory = JobAttentionMemory::default();
    let unreadable = FakeSink {
        suspension_unreadable: true,
        ..FakeSink::accepting(Vec::new())
    };
    assert!(
        memory.event_counter_moved("work", &key, 2),
        "the first look is a change"
    );
    assert!(
        !memory.event_counter_moved("work", &key, 2),
        "and an unchanged counter is not"
    );

    let resumed = resumption_or_retry(&unreadable, &memory, "work", &key, &job(), &state, &history).await;

    assert_eq!(resumed, None);
    assert!(
        memory.event_counter_moved("work", &key, 2),
        "the counter was forgotten, so the runs are read again"
    );
    Ok(())
}

#[tokio::test]
async fn a_suspension_that_is_read_leaves_the_counter_alone() -> Result<(), Box<dyn std::error::Error>> {
    let state: JobsState =
        toml::from_str("[ops.sync.nightly]\nlifecycle = \"enabled\"\nlifecycle_changed = \"2026-10-27T10:30:00Z\"\n")?;
    let history = [run(2, "scheduled", "succeeded", None, "2026-10-27T11:00:00Z")];
    let key = JobKey {
        package: "ops".to_owned(),
        workflow: "sync".to_owned(),
        job: "nightly".to_owned(),
    };
    let memory = JobAttentionMemory::default();
    let readable = FakeSink {
        suspension_is_open: true,
        ..FakeSink::accepting(Vec::new())
    };
    assert!(memory.event_counter_moved("work", &key, 2));

    let resumed = resumption_or_retry(&readable, &memory, "work", &key, &job(), &state, &history).await;

    assert_eq!(resumed, Some(datetime!(2026-10-27 10:30 UTC)));
    assert!(!memory.event_counter_moved("work", &key, 2), "nothing to read again");
    Ok(())
}

#[test]
fn run_events_name_the_package_of_the_job() {
    let history = [
        run(1, "scheduled", "failed", Some("workflow/script-error"), RECENT),
        run(2, "scheduled", "succeeded", None, RECENT),
    ];

    let events = run_events(&job(), &history, None, not_before());

    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|event| event.package == "ops"), "{events:?}");
}

#[test]
fn the_same_workflow_job_and_run_in_two_packages_are_two_events() {
    let history = [run(3, "scheduled", "failed", Some("workflow/script-error"), RECENT)];
    let other = ObservedJob {
        package: "billing".to_owned(),
        ..job()
    };

    let ops = run_events(&job(), &history, None, not_before());
    let billing = run_events(&other, &history, None, not_before());

    assert_ne!(ops[0].id(), billing[0].id());
    assert_ne!(ops[0].job_and_kind(), billing[0].job_and_kind());
    assert_eq!(billing[0].id(), "job.failed:billing/sync/nightly:R-3");
}

#[test]
fn a_recorded_event_carries_its_package_in_its_frontmatter() {
    let history = [run(3, "scheduled", "failed", Some("workflow/script-error"), RECENT)];
    let events = run_events(&job(), &history, None, not_before());

    let frontmatter = Map::from(&events[0]);

    assert_eq!(frontmatter.get("package"), Some(&Value::String("ops".to_owned())));
    assert_eq!(frontmatter.get("workflow"), Some(&Value::String("sync".to_owned())));
}

const RETIRED_BY_SUCCESSOR: &str = "[ops.sync.nightly]\nlifecycle = \"retired\"\n\n[ops.sync.nightly.retirement]\n\
    by = \"system\"\ncause = \"successor-enabled\"\nat = \"2026-10-27T10:00:00Z\"\n\
    replaced_by = { job = \"nightly-v2\", identity = \"job-2\" }\n";

#[test]
fn a_job_retired_by_its_successors_enabling_makes_a_retired_event_naming_the_successor()
-> Result<(), Box<dyn std::error::Error>> {
    let state: JobsState = toml::from_str(RETIRED_BY_SUCCESSOR)?;

    let event = retirement_event(&job(), &state).ok_or("a retired event")?;

    assert_eq!(event.kind, JobEventKind::Retired);
    assert_eq!(event.kind.name(), "job.retired");
    assert_eq!(event.package, "ops");
    assert_eq!(event.replaced_by.as_deref(), Some("nightly-v2"));
    assert_eq!(event.retired_at.as_deref(), Some("2026-10-27T10:00:00Z"));
    assert_eq!(
        event.subject,
        "[[registry/workflows/ops/sync/jobs/nightly.toml|nightly]]"
    );
    assert_eq!(event.id(), "job.retired:ops/sync/nightly:2026-10-27T10:00:00Z");
    assert_eq!(
        event.unique_fields(),
        vec!["kind", "package", "workflow", "job", "retired_at"]
    );
    let frontmatter = Map::from(&event);
    assert_eq!(
        frontmatter.get("replaced_by"),
        Some(&Value::String("nightly-v2".to_owned()))
    );
    assert_eq!(
        frontmatter.get("retired_at"),
        Some(&Value::String("2026-10-27T10:00:00Z".to_owned()))
    );
    Ok(())
}

#[test]
fn a_job_the_operator_retired_and_a_job_that_is_not_retired_make_no_retired_event()
-> Result<(), Box<dyn std::error::Error>> {
    let operators: JobsState = toml::from_str("[ops.sync.nightly]\nlifecycle = \"retired\"\n")?;
    let enabled: JobsState = toml::from_str("[ops.sync.nightly]\nlifecycle = \"enabled\"\n")?;

    assert!(retirement_event(&job(), &operators).is_none());
    assert!(retirement_event(&job(), &enabled).is_none());
    Ok(())
}
