use time::macros::datetime;

use super::*;
use crate::team::{MemberState, TeamMember, TeamMemberKind};

const NOW: time::OffsetDateTime = datetime!(2026-10-29 10:00:00 UTC);

fn member(slug: &str, chief: bool, state: MemberState) -> TeamMember {
    TeamMember {
        slug: slug.to_owned(),
        name: format!("Name of {slug}"),
        kind: TeamMemberKind::Agent,
        domain: None,
        team: None,
        chief_of_staff: chief,
        state,
    }
}

#[test]
fn the_chief_of_staff_is_the_member_the_team_flags_and_none_without_one() {
    let team = [
        member("vela", false, MemberState::Idle),
        member("atlas", true, MemberState::Working),
    ];
    assert_eq!(chief_of_staff(&team).map(|chief| chief.slug.as_str()), Some("atlas"));
    assert!(chief_of_staff(&team[..1]).is_none());
}

#[test]
fn the_chief_card_states_only_what_the_vault_records() {
    let chief = member("atlas", true, MemberState::Blocked);
    let liveness = MemberLiveness::default();

    let card = ChiefCard::from(ChiefFacts {
        member: &chief,
        liveness: Some(&liveness),
        assigned: Some(3),
    });

    assert_eq!(card.name, "Name of atlas");
    assert_eq!(card.state, "blocked");
    let facts: Vec<(&str, &str)> = card
        .facts
        .iter()
        .map(|fact| (fact.label, fact.value.as_str()))
        .collect();
    assert_eq!(
        facts,
        [
            ("Open tasks", "3"),
            ("Claimed", "0"),
            ("Last session", "No session recorded"),
            ("Lapsed claims", "0"),
        ]
    );
}

#[test]
fn the_chief_card_says_when_liveness_could_not_be_read() {
    let chief = member("atlas", true, MemberState::Idle);

    let card = ChiefCard::from(ChiefFacts {
        member: &chief,
        liveness: None,
        assigned: None,
    });

    assert!(
        card.facts
            .iter()
            .any(|fact| fact.label == "Last session" && fact.value == "Unknown"),
        "{:?}",
        card.facts
    );
}

#[test]
fn a_tally_counts_the_last_day_of_events_by_severity() {
    let tally = EventTally::from([Severity::Info, Severity::Warning, Severity::Info, Severity::Error].as_slice());

    assert_eq!((tally.info, tally.warning, tally.error), (2, 1, 1));
    assert_eq!(tally.total(), 4);
}

#[test]
fn an_inbox_row_shows_what_is_waiting_and_is_idle_when_empty() {
    let waiting = InputRow::inbox(Some(5), "/vault/inbox/".to_owned());
    assert_eq!(
        (waiting.label.as_str(), waiting.meta.as_str(), waiting.state),
        ("Inbox", "5 waiting", "working")
    );

    let empty = InputRow::inbox(Some(0), String::new());
    assert_eq!((empty.meta.as_str(), empty.state), ("Nothing waiting", "idle"));

    let unreadable = InputRow::inbox(None, String::new());
    assert_eq!((unreadable.meta.as_str(), unreadable.state), ("Unknown", "unknown"));
}

#[test]
fn a_recorded_source_row_names_its_kind_and_its_newest_event() {
    let row = InputRow::recorded(&RecordedSource {
        kind: "import.completed".to_owned(),
        label: "Import completed".to_owned(),
        latest: NOW,
        severity: Severity::Warning,
        href: "/vault/signals/?kind=import.completed".to_owned(),
    });

    assert_eq!(row.label, "Import completed");
    assert_eq!(row.meta, "2026-10-29 10:00 UTC");
    assert_eq!(row.state, "attention");
    assert_eq!(row.icon, "radio");
}

#[test]
fn the_newest_event_of_each_recorded_kind_is_its_source_and_server_kinds_are_not_sources() {
    let event = |kind: &str, at: time::OffsetDateTime, severity: Severity| (kind.to_owned(), at, severity);
    let sources = newest_per_kind(vec![
        event("import.completed", NOW - time::Duration::hours(2), Severity::Info),
        event("import.completed", NOW, Severity::Warning),
        event("task.created", NOW, Severity::Info),
        event("session.run-started", NOW - time::Duration::hours(1), Severity::Info),
    ]);

    let kinds: Vec<(&str, Severity)> = sources
        .iter()
        .map(|(kind, (_, severity))| (kind.as_str(), *severity))
        .collect();
    assert_eq!(
        kinds,
        [
            ("import.completed", Severity::Warning),
            ("session.run-started", Severity::Info)
        ]
    );
}
