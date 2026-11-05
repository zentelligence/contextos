use serde_json::{Value, json};

use super::*;
use crate::signals_event::EventRow;

fn event(second: u32, kind: &str, severity: &str, extra: Value) -> SignalEvent {
    let mut row = json!({
        "file.path": format!("memory/events/2026/10/2026-10-28-0300{second:02}-{kind}.md"),
        "kind": kind,
        "severity": severity,
        "at": format!("2026-10-28T03:00:{second:02}Z"),
        "entity": "vault",
    });
    if let (Some(row), Value::Object(extra)) = (row.as_object_mut(), extra) {
        row.extend(extra);
    }
    let parsed: Result<EventRow, _> = serde_json::from_value(row);
    parsed
        .ok()
        .and_then(|row| SignalEvent::try_from(row).ok())
        .unwrap_or_else(|| unreachable!("the test row is a valid event"))
}

fn kinds(open: &[&SignalEvent]) -> Vec<String> {
    open.iter()
        .map(|event| format!("{}@{}", event.kind, event.at.second()))
        .collect()
}

const TASK: &str = "[[memory/tasks/a-024|A-024]]";

#[test]
fn with_no_events_nothing_needs_attention() {
    assert_eq!(open_attention(&[], &[]).len(), 0);
}

#[test]
fn an_information_event_never_needs_attention() {
    let events = [event(1, "task.created", "info", json!({"subject": TASK}))];

    assert_eq!(open_attention(&events, &events).len(), 0);
}

#[test]
fn a_warning_with_a_subject_stays_until_a_later_event_about_the_same_subject() {
    let warning = event(1, "task.claim-lapsed", "warning", json!({"subject": TASK}));
    let other_subject = event(
        2,
        "task.state-changed",
        "info",
        json!({"subject": "[[memory/tasks/a-025|A-025]]"}),
    );
    let before = event(0, "task.created", "info", json!({"subject": TASK}));
    let candidates = [warning.clone()];

    let still = [warning.clone(), other_subject, before];
    assert_eq!(kinds(&open_attention(&candidates, &still)), vec!["task.claim-lapsed@1"]);

    let cleared = [
        warning,
        event(3, "task.state-changed", "info", json!({"subject": TASK})),
    ];
    assert_eq!(open_attention(&candidates, &cleared).len(), 0);
}

#[test]
fn an_event_in_the_same_second_with_a_later_path_counts_as_later() {
    let warning = event(1, "task.claim-lapsed", "warning", json!({"subject": TASK}));
    let mut after = event(1, "task.state-changed", "info", json!({"subject": TASK}));
    after.path = "memory/events/2026/10/2026-10-28-030001-zzz.md".to_owned();
    let candidates = [warning.clone()];

    assert_eq!(open_attention(&candidates, &[warning, after]).len(), 0);
}

#[test]
fn a_later_warning_about_the_subject_clears_the_earlier_one_and_stands_itself() {
    let first = event(1, "task.claim-lapsed", "warning", json!({"subject": TASK}));
    let second = event(5, "task.unknown-state", "warning", json!({"subject": TASK}));
    let both = [first, second];

    assert_eq!(kinds(&open_attention(&both, &both)), vec!["task.unknown-state@5"]);
}

#[test]
fn a_warning_without_a_subject_stays_whatever_follows() {
    let warning = event(1, "import.failed", "error", json!({}));
    let later = event(9, "import.completed", "info", json!({}));
    let candidates = [warning.clone()];

    assert_eq!(
        kinds(&open_attention(&candidates, &[warning, later])),
        vec!["import.failed@1"]
    );
}

#[test]
fn vault_degraded_is_cleared_only_by_a_later_recovery_with_the_same_code() {
    let degraded = event(1, "vault.degraded", "warning", json!({"code": "git/commit"}));
    let candidates = [degraded.clone()];
    let other_code = event(2, "vault.recovered", "info", json!({"code": "index/storage"}));
    let earlier_recovery = event(0, "vault.recovered", "info", json!({"code": "git/commit"}));
    let recovery = event(3, "vault.recovered", "info", json!({"code": "git/commit"}));

    let pool = [degraded.clone(), other_code, earlier_recovery];
    assert_eq!(kinds(&open_attention(&candidates, &pool)), vec!["vault.degraded@1"]);

    let pool = [degraded, recovery];
    assert_eq!(open_attention(&candidates, &pool).len(), 0);
}

#[test]
fn a_failed_or_suspended_job_is_cleared_only_by_a_later_recovery_of_the_same_job() {
    let failed = event(
        1,
        "job.failed",
        "error",
        json!({"workflow": "sync", "job": "nightly", "subject": "[[memory/jobs/ops/sync/nightly/R-1|R-1]]"}),
    );
    let suspended = event(
        2,
        "job.suspended",
        "warning",
        json!({"workflow": "sync", "job": "nightly", "subject": "[[registry/workflows/ops/sync/jobs/nightly.toml|nightly]]"}),
    );
    let candidates = [failed.clone(), suspended.clone()];
    let other_job = event(3, "job.recovered", "info", json!({"workflow": "sync", "job": "weekly"}));
    let other_workflow = event(
        3,
        "job.recovered",
        "info",
        json!({"workflow": "other", "job": "nightly"}),
    );
    let recovered = event(
        4,
        "job.recovered",
        "info",
        json!({"workflow": "sync", "job": "nightly"}),
    );

    let pool = [failed.clone(), suspended.clone(), other_job, other_workflow];
    assert_eq!(
        kinds(&open_attention(&candidates, &pool)),
        vec!["job.failed@1", "job.suspended@2"]
    );

    let pool = [failed, suspended, recovered];
    assert_eq!(open_attention(&candidates, &pool).len(), 0);
}

#[test]
fn a_recovery_that_names_no_workflow_clears_no_job() {
    let failed = event(1, "job.failed", "error", json!({"workflow": "sync", "job": "nightly"}));
    let without_workflow = event(2, "job.recovered", "info", json!({"job": "nightly"}));
    let candidates = [failed.clone()];

    assert_eq!(
        kinds(&open_attention(&candidates, &[failed, without_workflow])),
        vec!["job.failed@1"]
    );
}

#[test]
fn a_recovery_before_a_failure_does_not_clear_it() {
    let recovered = event(
        1,
        "job.recovered",
        "info",
        json!({"workflow": "sync", "job": "nightly"}),
    );
    let failed = event(2, "job.failed", "error", json!({"workflow": "sync", "job": "nightly"}));
    let candidates = [failed.clone()];

    assert_eq!(
        kinds(&open_attention(&candidates, &[recovered, failed])),
        vec!["job.failed@2"]
    );
}

#[test]
fn the_order_of_the_candidates_is_kept() {
    let a = event(5, "import.failed", "error", json!({}));
    let b = event(2, "batch.failed", "warning", json!({}));
    let candidates = [a, b];

    assert_eq!(
        kinds(&open_attention(&candidates, &candidates)),
        vec!["import.failed@5", "batch.failed@2"]
    );
}

#[test]
fn a_task_with_no_events_shows_no_indicator() {
    assert_eq!(task_indicator(&[]), None);
}

#[test]
fn a_task_shows_the_severity_of_its_most_recent_event_when_it_needs_attention() {
    let events = [
        event(1, "task.created", "info", json!({"subject": TASK})),
        event(2, "task.claim-lapsed", "warning", json!({"subject": TASK})),
    ];

    assert_eq!(task_indicator(&events), Some(Severity::Warning));
}

#[test]
fn any_later_event_clears_a_tasks_indicator_whatever_order_the_events_arrive_in() {
    let events = [
        event(3, "task.state-changed", "info", json!({"subject": TASK})),
        event(2, "task.unknown-state", "error", json!({"subject": TASK})),
    ];

    assert_eq!(task_indicator(&events), None);
}

#[test]
fn an_event_may_change_attention_when_it_raises_some_or_may_clear_some() {
    assert!(may_change_attention(&event(1, "vault.degraded", "warning", json!({}))));
    assert!(may_change_attention(&event(
        1,
        "task.state-changed",
        "info",
        json!({"subject": TASK})
    )));
    assert!(may_change_attention(&event(
        1,
        "vault.recovered",
        "info",
        json!({"code": "x"})
    )));
    assert!(may_change_attention(&event(
        1,
        "session.run-finished",
        "info",
        json!({})
    )));
    assert!(may_change_attention(&event(
        1,
        "member.state-changed",
        "info",
        json!({})
    )));
    assert!(!may_change_attention(&event(1, "import.completed", "info", json!({}))));
}
