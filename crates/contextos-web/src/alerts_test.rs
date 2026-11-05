use time::macros::date;

use super::*;

const TODAY: Date = date!(2026 - 10 - 29);

fn overdue(id: &str, due: &str) -> OpenTask {
    OpenTask {
        id: id.to_owned(),
        title: format!("Title of {id}"),
        due: Some(due.to_owned()),
        href: format!("/vault/tasks/{}", id.to_lowercase()),
    }
}

#[test]
fn an_open_task_is_overdue_only_once_its_due_date_has_passed() {
    assert!(overdue("T-1", "2026-10-28").is_overdue(TODAY));
    assert!(
        overdue("T-1", "2026-10-28T09:00:00+10:00").is_overdue(TODAY),
        "a timestamp is read by its date"
    );
    assert!(
        !overdue("T-1", "2026-10-29").is_overdue(TODAY),
        "due today is not yet overdue"
    );
    assert!(!overdue("T-1", "2026-11-01").is_overdue(TODAY));
    assert!(
        !overdue("T-1", "next week").is_overdue(TODAY),
        "a due value that is not a date is never guessed at"
    );
    assert!(
        !OpenTask {
            due: None,
            ..overdue("T-1", "2026-10-01")
        }
        .is_overdue(TODAY)
    );
}

#[test]
fn an_overdue_task_alerts_with_its_key_title_and_due_date() {
    let alert = Alert::from(overdue("T-7", "2026-10-20"));

    assert_eq!(alert.kind, AlertKind::Overdue);
    assert_eq!(alert.title, "T-7 Title of T-7");
    assert_eq!(alert.detail, "Overdue since 2026-10-20");
    assert_eq!(alert.href.as_deref(), Some("/vault/tasks/t-7"));
    assert_eq!(alert.icon(), "calendar");
}

#[test]
fn an_open_question_alerts_with_its_key_and_when_it_last_changed() {
    let alert = Alert::from(OpenQuestion {
        id: "Q-3".to_owned(),
        title: "Which region first?".to_owned(),
        updated: Some("2026-10-27T03:00:00Z".to_owned()),
        href: "/vault/settings/decisions/q-003".to_owned(),
    });

    assert_eq!(alert.kind, AlertKind::Question);
    assert_eq!(alert.title, "Q-3 Which region first?");
    assert_eq!(alert.detail, "Open question");
    assert_eq!(alert.when, "2026-10-27");
    assert_eq!(alert.icon(), "circle-help");
}

#[test]
fn an_open_warning_or_error_alerts_with_its_label_subject_and_severity() {
    let line = EventLine {
        severity: "error",
        colour: "red",
        iso: "2026-10-29T01:02:03Z".to_owned(),
        time: "2026-10-29 01:02 UTC".to_owned(),
        kind: "job.failed".to_owned(),
        label: "Job failed".to_owned(),
        subject_href: Some("/vault/runs/r-1".to_owned()),
        subject_label: "run 1".to_owned(),
        entity: "acme".to_owned(),
        detail: "code: io/x".to_owned(),
        dom_id: None,
    };

    let alert = Alert::from(&line);

    assert_eq!(alert.kind, AlertKind::Attention(Severity::Error));
    assert_eq!(alert.title, "Job failed: run 1");
    assert_eq!(alert.detail, "code: io/x");
    assert_eq!(alert.href.as_deref(), Some("/vault/runs/r-1"));
    assert_eq!(alert.when, "2026-10-29 01:02 UTC");
    assert_eq!(alert.icon(), "octagon-alert");
    assert_eq!(
        Alert::from(&EventLine {
            severity: "warning",
            ..line
        })
        .icon(),
        "circle-alert"
    );
}

#[test]
fn alerts_are_listed_most_recent_first_whatever_their_source() {
    let question = Alert::from(OpenQuestion {
        id: "Q-1".to_owned(),
        title: "q".to_owned(),
        updated: Some("2026-10-27".to_owned()),
        href: String::new(),
    });
    let late = Alert::from(overdue("T-1", "2026-10-28"));
    let early = Alert::from(overdue("T-2", "2026-10-01"));

    let sorted = most_recent_first(vec![early, question, late]);

    let titles: Vec<&str> = sorted.iter().map(|alert| alert.title.as_str()).collect();
    assert_eq!(titles, ["T-1 Title of T-1", "Q-1 q", "T-2 Title of T-2"]);
}
