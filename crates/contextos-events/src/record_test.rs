use time::macros::datetime;

use crate::Scalar;

use super::*;

fn kind(name: &str) -> KindName {
    KindName::try_from(name).unwrap_or_else(|error| unreachable!("{error}"))
}

fn field(name: &str, value: impl Into<FieldValue>) -> (FieldName, FieldValue) {
    (
        FieldName::try_from(name).unwrap_or_else(|error| unreachable!("{error}")),
        value.into(),
    )
}

fn state_changed() -> EventRecord {
    EventRecord {
        kind: kind("task.state-changed"),
        severity: Severity::Info,
        at: datetime!(2026-10-28 03:15:42 UTC),
        entity: "zenith".to_owned(),
        subject: Some(EventSubject::for_record(
            "memory/tasks/zenith/projects/orion/events/a-024.md",
            "A-024",
            "memory/tasks.base",
        )),
        actor: Some("ellis/S-1042".to_owned()),
        fields: vec![field("from", "next"), field("to", "doing")],
        heading: "A-024: next \u{2192} doing".to_owned(),
        content: None,
    }
}

#[test]
fn a_server_fired_event_renders_the_common_fields_in_order_then_the_kinds_own() {
    let expected = "---\n\
        type: event\n\
        kind: task.state-changed\n\
        severity: info\n\
        at: 2026-10-28T03:15:42Z\n\
        entity: 'zenith'\n\
        subject: '[[a-024|A-024]]'\n\
        subject_id: 'A-024'\n\
        subject_path: 'memory/tasks/zenith/projects/orion/events/a-024.md'\n\
        subject_base: 'memory/tasks.base'\n\
        actor: 'ellis/S-1042'\n\
        from: 'next'\n\
        to: 'doing'\n\
        ---\n\
        \n\
        # A-024: next \u{2192} doing\n";

    assert_eq!(state_changed().to_string(), expected);
}

#[test]
fn an_event_without_subject_or_actor_omits_those_fields_and_carries_no_note_fields() {
    let mut record = state_changed();
    record.subject = None;
    record.actor = None;
    record.fields.clear();

    let text = record.to_string();

    for absent in [
        "subject",
        "actor",
        "title:",
        "created:",
        "updated:",
        "tags:",
        "aliases:",
        "description:",
    ] {
        assert!(!text.contains(absent), "{absent} must not appear in {text}");
    }
    assert!(text.starts_with("---\ntype: event\nkind: task.state-changed\nseverity: info\nat: "));
}

#[test]
fn a_record_whose_file_is_not_named_for_its_key_is_linked_by_its_file_name_labelled_with_its_key() {
    let subject = EventSubject::for_record("memory/tasks/clients/ship-it.md", "A-024", "memory/tasks.base");
    assert_eq!(subject.link, "[[ship-it|A-024]]");
    assert_eq!(subject.id.as_deref(), Some("A-024"));
    assert_eq!(subject.path.as_deref(), Some("memory/tasks/clients/ship-it.md"));
    assert_eq!(subject.base.as_deref(), Some("memory/tasks.base"));
}

#[test]
fn a_subject_without_an_id_omits_subject_id() {
    let mut record = state_changed();
    record.subject = Some(EventSubject::for_note("registry/jobs/nightly.md"));

    let text = record.to_string();

    assert!(
        text.contains("subject: '[[registry/jobs/nightly|nightly]]'\n"),
        "{text}"
    );
    assert!(!text.contains("subject_id"), "{text}");
    assert!(text.contains("subject_path: 'registry/jobs/nightly.md'\n"), "{text}");
    assert!(!text.contains("subject_base"), "{text}");
}

#[test]
fn the_time_is_written_in_utc_whatever_offset_it_carries() {
    let mut record = state_changed();
    record.at = datetime!(2026-10-28 13:15:42 +10:00);

    assert!(record.to_string().contains("\nat: 2026-10-28T03:15:42Z\n"));
}

#[test]
fn field_values_are_quoted_so_they_stay_one_line_of_the_type_they_were() {
    let mut record = state_changed();
    record.fields = vec![
        field("reason", "it's  blocked\non review"),
        field("count", 3_i64),
        field("ok", true),
        field(
            "members",
            FieldValue::List(vec![Scalar::from("ellis"), Scalar::from("o'neil")]),
        ),
        field("none", FieldValue::List(Vec::new())),
    ];

    let text = record.to_string();

    assert!(text.contains("reason: 'it''s blocked on review'\n"), "{text}");
    assert!(text.contains("count: 3\n"), "{text}");
    assert!(text.contains("ok: true\n"), "{text}");
    assert!(text.contains("members: ['ellis', 'o''neil']\n"), "{text}");
    assert!(text.contains("none: []\n"), "{text}");
}

#[test]
fn a_recorders_content_follows_the_heading_after_a_blank_line() {
    let mut record = state_changed();
    record.heading = "Import completed".to_owned();
    record.content = Some("Loaded 12 rows.\n".to_owned());

    assert!(
        record
            .to_string()
            .ends_with("\n# Import completed\n\nLoaded 12 rows.\n")
    );
}

#[test]
fn empty_content_adds_no_body_beyond_the_heading() {
    let mut record = state_changed();
    record.content = Some("  \n".to_owned());

    assert!(record.to_string().ends_with("\n\n# A-024: next \u{2192} doing\n"));
}

#[test]
fn the_path_is_the_utc_month_folder_and_stamp_with_the_subjects_id_as_slug() {
    assert_eq!(
        state_changed().relative_path(0),
        "memory/events/2026/10/2026-10-28-031542-a-024.md"
    );
}

#[test]
fn a_taken_path_gains_a_numeric_suffix_from_one() {
    assert_eq!(
        state_changed().relative_path(1),
        "memory/events/2026/10/2026-10-28-031542-a-024-1.md"
    );
    assert_eq!(
        state_changed().relative_path(12),
        "memory/events/2026/10/2026-10-28-031542-a-024-12.md"
    );
}

#[test]
fn the_slug_falls_back_to_the_subject_note_name_then_the_kind() {
    let mut record = state_changed();
    record.subject = Some(EventSubject::for_note("registry/jobs/Nightly Import.md"));
    assert!(
        record.relative_path(0).ends_with("-nightly-import.md"),
        "{}",
        record.relative_path(0)
    );

    record.subject = None;
    assert!(record.relative_path(0).ends_with("-task-state-changed.md"));
}

#[test]
fn a_subject_id_with_no_usable_characters_falls_back_to_the_kind() {
    let mut record = state_changed();
    record.subject = Some(EventSubject {
        link: "[[x|y]]".to_owned(),
        id: Some("!!!".to_owned()),
        path: None,
        base: None,
    });

    assert!(record.relative_path(0).ends_with("-task-state-changed.md"));
}

#[test]
fn the_slug_is_bounded_so_a_long_id_cannot_exceed_a_file_name() {
    let mut record = state_changed();
    record.subject = Some(EventSubject {
        link: "[[x|y]]".to_owned(),
        id: Some("a".repeat(500)),
        path: None,
        base: None,
    });

    let path = record.relative_path(0);

    assert!(path.len() < 140, "{path}");
}

#[test]
fn the_entity_is_the_subjects_then_the_one_given_then_the_recorders_then_vault() {
    assert_eq!(resolve_entity(Some("a"), Some("b"), Some("c")), "a");
    assert_eq!(resolve_entity(None, Some("b"), Some("c")), "b");
    assert_eq!(resolve_entity(None, None, Some("c")), "c");
    assert_eq!(resolve_entity(None, None, None), "vault");
    assert_eq!(resolve_entity(Some("  "), Some(""), Some("c")), "c");
}

#[test]
fn a_heading_is_collapsed_to_one_line() {
    let mut record = state_changed();
    record.heading = "two\nlines".to_owned();

    assert!(record.to_string().ends_with("\n# two lines\n"));
}
