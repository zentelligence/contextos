use serde_json::{Map, Value, json};
use time::macros::datetime;

use super::*;

const NOW: OffsetDateTime = datetime!(2026-10-28 03:00:00 UTC);

fn frontmatter(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

fn policy(default_days: u32, keep_attention: bool) -> RetentionPolicy {
    RetentionPolicy {
        default_days,
        keep_attention,
    }
}

fn verdict_of(value: Value, policy: RetentionPolicy, kind_days: Option<u32>) -> Verdict {
    Verdict::of(&frontmatter(value), policy, |_| kind_days, NOW)
}

#[test]
fn an_info_event_older_than_the_window_expires_and_a_newer_one_is_kept() {
    let policy = policy(90, true);
    let old = json!({"kind": "task.created", "severity": "info", "at": "2026-07-01T00:00:00Z"});
    let recent = json!({"kind": "task.created", "severity": "info", "at": "2026-09-01T00:00:00Z"});

    assert_eq!(verdict_of(old, policy, None), Verdict::Expired);
    assert_eq!(verdict_of(recent, policy, None), Verdict::Kept);
}

#[test]
fn an_event_exactly_at_the_window_is_kept_and_one_second_past_it_expires() {
    let policy = policy(30, true);
    let at_edge = json!({"kind": "k", "severity": "info", "at": "2026-09-28T03:00:00Z"});
    let past_edge = json!({"kind": "k", "severity": "info", "at": "2026-09-28T02:59:59Z"});

    assert_eq!(verdict_of(at_edge, policy, None), Verdict::Kept);
    assert_eq!(verdict_of(past_edge, policy, None), Verdict::Expired);
}

#[test]
fn warning_and_error_events_are_kept_while_attention_is_kept_and_age_out_otherwise() {
    let old_warning = json!({"kind": "k", "severity": "warning", "at": "2020-01-01T00:00:00Z"});
    let old_error = json!({"kind": "k", "severity": "error", "at": "2020-01-01T00:00:00Z"});

    assert_eq!(verdict_of(old_warning.clone(), policy(90, true), None), Verdict::Kept);
    assert_eq!(verdict_of(old_error.clone(), policy(90, true), None), Verdict::Kept);
    assert_eq!(verdict_of(old_warning, policy(90, false), None), Verdict::Expired);
    assert_eq!(verdict_of(old_error, policy(90, false), None), Verdict::Expired);
}

#[test]
fn a_kinds_own_window_replaces_the_vaults() {
    let event = json!({"kind": "trade.executed", "severity": "info", "at": "2026-01-01T00:00:00Z"});

    assert_eq!(verdict_of(event.clone(), policy(90, true), Some(400)), Verdict::Kept);
    assert_eq!(verdict_of(event, policy(900, true), Some(30)), Verdict::Expired);
}

#[test]
fn the_window_is_asked_for_by_the_events_own_kind() {
    let event = frontmatter(json!({"kind": "trade.executed", "severity": "info", "at": "2026-01-01T00:00:00Z"}));
    let asked = std::cell::RefCell::new(Vec::new());

    let _ = Verdict::of(
        &event,
        policy(90, true),
        |kind| {
            asked.borrow_mut().push(kind.to_owned());
            None
        },
        NOW,
    );

    assert_eq!(asked.into_inner(), vec!["trade.executed".to_owned()]);
}

#[test]
fn an_event_whose_time_cannot_be_read_is_unreadable_and_is_never_expired() {
    let policy = policy(1, false);
    for value in [
        json!({"kind": "k", "severity": "info"}),
        json!({"kind": "k", "severity": "info", "at": "yesterday"}),
        json!({"kind": "k", "severity": "info", "at": 20_260_101}),
    ] {
        assert_eq!(verdict_of(value, policy, None), Verdict::Unreadable);
    }
}

#[test]
fn an_unknown_severity_is_treated_as_attention_so_it_is_not_swept_by_mistake() {
    let event = json!({"kind": "k", "severity": "critical", "at": "2020-01-01T00:00:00Z"});

    assert_eq!(verdict_of(event, policy(1, true), None), Verdict::Kept);
}

#[test]
fn the_sweep_day_round_trips_and_is_due_only_on_a_later_day() -> Result<(), EventsError> {
    let directory = tempfile::tempdir().map_err(|error| EventsError::TableUnwritable {
        reason: error.to_string(),
    })?;
    let path = directory.path().join("sweep.json");
    assert_eq!(SweepDay::load(&path)?, None);

    let today = datetime!(2026-10-28 23:59:59 UTC).date();
    SweepDay::from(today).store(&path)?;

    let loaded = SweepDay::load(&path)?.map(SweepDay::date);
    assert_eq!(loaded, Some(today));
    assert!(!SweepDay::from(today).is_due(today));
    assert!(SweepDay::from(today).is_due(datetime!(2026-10-29 00:00:00 UTC).date()));
    Ok(())
}

#[test]
fn an_events_time_is_read_from_its_at_alone_without_judging_its_age() {
    let readable = frontmatter(json!({"at": "2026-10-28T03:15:42Z", "severity": "bogus"}));
    assert_eq!(Verdict::time_of(&readable), Some(datetime!(2026-10-28 03:15:42 UTC)));
    for unreadable in [json!({}), json!({"at": 5}), json!({"at": "someday"})] {
        assert_eq!(Verdict::time_of(&frontmatter(unreadable)), None);
    }
}

fn note(path: &str, value: Value) -> SweepNote {
    SweepNote {
        path: path.to_owned(),
        frontmatter: frontmatter(value),
    }
}

fn plan(notes: &[SweepNote], keep_attention: bool, kind_days: Option<u32>) -> Vec<Verdict> {
    Verdict::plan(notes, policy(30, keep_attention), |_| kind_days, NOW)
}

fn stuck(path: &str, at: &str) -> SweepNote {
    note(
        path,
        json!({"kind": "task.stuck", "severity": "warning", "at": at, "subject": "[[memory/tasks/t|t]]"}),
    )
}

fn done(path: &str, at: &str) -> SweepNote {
    note(
        path,
        json!({"kind": "task.done", "severity": "info", "at": at, "subject": "[[memory/tasks/t|t]]"}),
    )
}

#[test]
fn an_open_warning_is_kept_however_old_and_a_cleared_one_ages_out_on_its_own_time() {
    let open = note(
        "o",
        json!({"kind": "x.y", "severity": "warning", "at": "2020-01-01T00:00:00Z"}),
    );
    let cleared = stuck("c", "2020-01-01T00:00:00Z");
    let closing = done("d", "2020-01-02T00:00:00Z");

    let verdicts = plan(&[open, cleared, closing], true, None);

    assert_eq!(verdicts, vec![Verdict::Kept, Verdict::Expired, Verdict::Expired]);
}

#[test]
fn a_cleared_warning_inside_the_window_is_kept_with_the_event_that_cleared_it() {
    let cleared = stuck("c", "2026-10-20T00:00:00Z");
    let closing = done("d", "2026-10-21T00:00:00Z");

    assert_eq!(
        plan(&[cleared, closing], true, None),
        vec![Verdict::Kept, Verdict::Kept]
    );
}

#[test]
fn an_event_that_clears_a_surviving_warning_is_not_swept_before_it_even_when_its_own_window_is_shorter() {
    // The warning is older than the clearing event but the kind window of the clearing event is shorter, so the
    // clearing event alone would expire first; it must survive, or the warning would reappear as open.
    let cleared = stuck("c", "2026-10-10T00:00:00Z");
    let closing = done("d", "2026-10-11T00:00:00Z");
    let notes = [cleared, closing];

    let verdicts = Verdict::plan(&notes, policy(30, true), |kind| (kind == "task.done").then_some(1), NOW);

    assert_eq!(verdicts, vec![Verdict::Kept, Verdict::Kept]);
}

#[test]
fn with_attention_not_kept_every_warning_ages_out_whether_open_or_not() {
    let open = note(
        "o",
        json!({"kind": "x.y", "severity": "error", "at": "2020-01-01T00:00:00Z"}),
    );

    assert_eq!(plan(&[open], false, None), vec![Verdict::Expired]);
}

#[test]
fn a_note_whose_event_cannot_be_judged_keeps_the_verdict_it_would_have_had_alone() {
    let unreadable = note("u", json!({"kind": "k", "severity": "info"}));
    let no_kind = note("n", json!({"severity": "warning", "at": "2020-01-01T00:00:00Z"}));

    assert_eq!(
        plan(&[unreadable, no_kind], true, None),
        vec![Verdict::Unreadable, Verdict::Kept]
    );
}
