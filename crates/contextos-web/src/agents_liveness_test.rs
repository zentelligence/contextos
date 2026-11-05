use serde_json::{Value, json};
use time::macros::datetime;

use super::*;
use crate::signals_event::EventRow;

const NOW: OffsetDateTime = datetime!(2026-10-28 12:00:00 UTC);
const OVERDUE: Duration = Duration::minutes(120);

fn event(kind: &str, severity: &str, at: &str, extra: Value) -> SignalEvent {
    let mut row = json!({
        "file.path": format!("memory/events/2026/10/{at}-{kind}.md"),
        "kind": kind,
        "severity": severity,
        "at": at,
        "entity": "vault",
    });
    if let (Some(row), Value::Object(extra)) = (row.as_object_mut(), extra) {
        row.extend(extra);
    }
    let parsed: Result<EventRow, _> = serde_json::from_value(row);
    parsed
        .ok()
        .and_then(|row| SignalEvent::try_from(row).ok())
        .unwrap_or_else(|| unreachable!("a valid event"))
}

fn started(at: &str, run: &str, members: &str) -> SignalEvent {
    event(
        "session.run-started",
        "info",
        at,
        json!({"run_id": run, "members": members}),
    )
}

fn finished(at: &str, run: &str, members: &str, severity: &str) -> SignalEvent {
    event(
        "session.run-finished",
        severity,
        at,
        json!({"run_id": run, "members": members}),
    )
}

fn claim(id: &str, claimed_by: &str, until: Option<&str>) -> ClaimedTask {
    ClaimedTask {
        path: format!("memory/tasks/{}.md", id.to_ascii_lowercase()),
        id: id.to_owned(),
        assignee: Some("ellis".to_owned()),
        claimed_by: Some(claimed_by.to_owned()),
        claimed_until: until
            .and_then(|until| OffsetDateTime::parse(until, &time::format_description::well_known::Rfc3339).ok()),
    }
}

fn liveness_of(events: &[SignalEvent], tasks: &[ClaimedTask]) -> MemberLiveness {
    liveness("ellis", events, tasks, NOW, OVERDUE)
}

#[test]
fn a_member_with_no_session_events_shows_no_liveness_and_no_amber() {
    let found = liveness_of(&[], &[claim("A-1", "ellis/S-001", Some("2026-10-28T11:00:00Z"))]);

    assert!(!found.shows_liveness());
    assert_eq!(found.overdue.len(), 0);
}

#[test]
fn the_last_finish_of_a_run_that_names_the_member_gives_its_time_and_severity() {
    let events = [
        finished("2026-10-28T08:00:00Z", "S-001", "ellis, vela", "info"),
        finished("2026-10-28T10:00:00Z", "S-002", "vela", "error"),
        finished("2026-10-28T09:00:00Z", "S-003", "ellis", "warning"),
    ];

    let found = liveness_of(&events, &[]);

    assert_eq!(
        found.last_finish,
        Some((datetime!(2026-10-28 09:00:00 UTC), Severity::Warning))
    );
    assert!(found.shows_liveness());
}

#[test]
fn a_member_is_matched_by_whole_slug_not_by_part_of_one() {
    let events = [finished("2026-10-28T09:00:00Z", "S-001", "ellis-two", "info")];

    assert!(liveness_of(&events, &[]).last_finish.is_none());
}

#[test]
fn the_number_of_claims_counts_tasks_claimed_for_the_member_that_have_not_lapsed() {
    let tasks = [
        claim("A-1", "ellis/S-001", Some("2026-10-28T13:00:00Z")),
        claim("A-2", "ellis/S-002", None),
        claim("A-3", "ellis/S-003", Some("2026-10-28T11:00:00Z")),
        claim("A-4", "vela/S-001", Some("2026-10-28T13:00:00Z")),
        claim("A-5", "ellis-two/S-001", Some("2026-10-28T13:00:00Z")),
    ];

    let found = liveness_of(&[finished("2026-10-28T09:00:00Z", "S-009", "ellis", "info")], &tasks);

    assert_eq!(found.claimed, 2);
}

#[test]
fn a_started_run_whose_claims_have_all_lapsed_shows_amber_naming_the_run_and_its_start() {
    let events = [started("2026-10-28T11:30:00Z", "S-007", "ellis")];
    let tasks = [
        claim("A-1", "ellis/S-007", Some("2026-10-28T11:45:00Z")),
        claim("A-2", "vela/S-007", Some("2026-10-28T11:50:00Z")),
    ];

    let found = liveness_of(&events, &tasks);

    assert_eq!(
        found.overdue,
        vec![OverdueRun {
            run_id: "S-007".to_owned(),
            started: datetime!(2026-10-28 11:30:00 UTC),
        }],
        "started only half an hour ago, but every claim carrying the run id is in the past"
    );
}

#[test]
fn a_started_run_with_a_claim_still_running_is_not_overdue() {
    let events = [started("2026-10-28T06:00:00Z", "S-007", "ellis")];
    let tasks = [
        claim("A-1", "ellis/S-007", Some("2026-10-28T11:45:00Z")),
        claim("A-2", "vela/S-007", Some("2026-10-28T12:30:00Z")),
    ];

    assert_eq!(
        liveness_of(&events, &tasks).overdue.len(),
        0,
        "one claim is still within its window"
    );
}

#[test]
fn a_started_run_no_task_claims_is_overdue_once_older_than_the_window() {
    let old = [started("2026-10-28T09:59:00Z", "S-007", "ellis")];
    let recent = [started("2026-10-28T10:01:00Z", "S-007", "ellis")];

    assert_eq!(liveness_of(&old, &[]).overdue.len(), 1);
    assert_eq!(
        liveness_of(&recent, &[]).overdue.len(),
        0,
        "two hours have not yet passed"
    );
}

#[test]
fn a_run_whose_tasks_were_all_released_falls_under_the_window_not_under_the_claims() {
    let events = [started("2026-10-28T06:00:00Z", "S-007", "ellis")];
    let tasks = [claim("A-1", "ellis/S-006", Some("2026-10-28T13:00:00Z"))];

    assert_eq!(liveness_of(&events, &tasks).overdue.len(), 1);
}

#[test]
fn a_finished_run_is_never_overdue() {
    let events = [
        started("2026-10-28T06:00:00Z", "S-007", "ellis"),
        finished("2026-10-28T06:30:00Z", "S-007", "ellis", "info"),
    ];

    assert_eq!(liveness_of(&events, &[]).overdue.len(), 0);
}

#[test]
fn a_run_that_does_not_name_the_member_is_not_theirs_to_be_overdue() {
    let events = [started("2026-10-28T06:00:00Z", "S-007", "vela")];

    assert_eq!(liveness_of(&events, &[]).overdue.len(), 0);
}

#[test]
fn overdue_runs_are_listed_newest_first() {
    let events = [
        started("2026-10-28T01:00:00Z", "S-001", "ellis"),
        started("2026-10-28T03:00:00Z", "S-002", "ellis"),
    ];

    let runs: Vec<String> = liveness_of(&events, &[])
        .overdue
        .into_iter()
        .map(|run| run.run_id)
        .collect();

    assert_eq!(runs, vec!["S-002", "S-001"]);
}

#[test]
fn recent_lapsed_claims_on_the_members_tasks_are_listed_and_older_ones_and_others_are_not() {
    let tasks = [
        ClaimedTask {
            path: "memory/tasks/a-1.md".to_owned(),
            id: "A-1".to_owned(),
            assignee: Some("Ellis".to_owned()),
            claimed_by: None,
            claimed_until: None,
        },
        ClaimedTask {
            path: "memory/tasks/a-2.md".to_owned(),
            id: "A-2".to_owned(),
            assignee: Some("vela".to_owned()),
            claimed_by: None,
            claimed_until: None,
        },
    ];
    let lapse = |at: &str, task: &str| {
        event(
            "task.claim-lapsed",
            "warning",
            at,
            json!({
                "subject": format!("[[{task}|{}]]", task.to_ascii_uppercase()),
                "subject_id": task.to_ascii_uppercase(),
                // Where the task was when the claim lapsed; it has been filed elsewhere since.
                "subject_path": format!("memory/tasks/old/{task}.md"),
                "subject_base": "memory/tasks.base"
            }),
        )
    };
    let events = [
        lapse("2026-10-27T10:00:00Z", "a-1"),
        lapse("2026-10-20T11:59:59Z", "a-1"),
        lapse("2026-10-28T09:00:00Z", "a-2"),
    ];

    let found = liveness_of(&events, &tasks);

    assert_eq!(found.lapsed.len(), 1);
    assert_eq!(found.lapsed[0].task_id, "A-1");
    assert_eq!(found.lapsed[0].at, datetime!(2026-10-27 10:00:00 UTC));
    assert!(
        !found.shows_liveness(),
        "a lapsed claim is listed, but it is not a session event"
    );
}
