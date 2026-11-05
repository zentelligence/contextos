use time::macros::datetime;

use super::*;
use crate::Severity;

const PATH: &str = "memory/tasks/zenith/a-024.md";
const BASE: &str = "memory/tasks.base";

fn site(path: &str) -> TaskSite<'_> {
    TaskSite { base: BASE, path }
}
const NOW: OffsetDateTime = datetime!(2026-10-28 03:15:42 UTC);
const STATES: [&str; 3] = ["next", "doing", "done"];

fn states() -> Vec<String> {
    STATES.iter().map(|state| (*state).to_owned()).collect()
}

fn task(state: &str) -> TrackedTask {
    TrackedTask {
        task_type: Some("work".to_owned()),
        id: Some("A-024".to_owned()),
        state: Some(state.to_owned()),
        assignee: Some("ellis".to_owned()),
        claimed_by: None,
        claimed_until: None,
        entity: Some("zenith".to_owned()),
    }
}

fn claimed(state: &str, by: &str, until: OffsetDateTime) -> TrackedTask {
    TrackedTask {
        claimed_by: Some(by.to_owned()),
        claimed_until: Some(until),
        ..task(state)
    }
}

fn table_with(task: TrackedTask) -> TaskTable {
    let mut table = TaskTable::default();
    table.tasks.insert(PATH.to_owned(), task);
    table
}

fn update(table: &mut TaskTable, task: TrackedTask) -> Vec<EventRecord> {
    let declared = states();
    table.apply(
        site(PATH),
        TaskChange::Updated {
            task,
            declared_states: Some(&declared),
        },
        NOW,
    )
}

fn kinds(events: &[EventRecord]) -> Vec<&str> {
    events.iter().map(|event| event.kind.as_str()).collect()
}

fn field(event: &EventRecord, name: &str) -> Option<String> {
    event
        .fields
        .iter()
        .find(|(field, _)| field.as_str() == name)
        .map(|(_, value)| value.to_string())
}

#[test]
fn creating_a_task_fires_task_created_with_its_starting_values_and_tracks_it() {
    let mut table = TaskTable::default();
    let declared = states();

    let events = table.apply(
        site(PATH),
        TaskChange::Created {
            task: task("next"),
            origin: Some("workflow".to_owned()),
            declared_states: Some(&declared),
        },
        NOW,
    );

    assert_eq!(kinds(&events), ["task.created"]);
    let created = &events[0];
    assert_eq!(created.severity, Severity::Info);
    assert_eq!(created.at, NOW);
    assert_eq!(created.entity, "zenith");
    assert_eq!(created.actor, None);
    assert_eq!(
        created.subject,
        Some(EventSubject {
            link: "[[a-024|A-024]]".to_owned(),
            id: Some("A-024".to_owned()),
            path: Some(PATH.to_owned()),
            base: Some(BASE.to_owned()),
        })
    );
    assert_eq!(field(created, "task_type").as_deref(), Some("'work'"));
    assert_eq!(field(created, "state").as_deref(), Some("'next'"));
    assert_eq!(field(created, "assignee").as_deref(), Some("'ellis'"));
    assert_eq!(field(created, "origin").as_deref(), Some("'workflow'"));
    assert_eq!(table.get(PATH), Some(&task("next")));
}

#[test]
fn a_state_change_fires_one_event_naming_both_states_and_the_claim_holder_as_actor() {
    let mut table = table_with(task("next"));

    let events = update(
        &mut table,
        claimed("doing", "ellis/S-1042", datetime!(2026-10-28 09:00 UTC)),
    );

    assert_eq!(kinds(&events), ["task.state-changed"]);
    let changed = &events[0];
    assert_eq!(changed.actor.as_deref(), Some("ellis/S-1042"));
    assert_eq!(field(changed, "from").as_deref(), Some("'next'"));
    assert_eq!(field(changed, "to").as_deref(), Some("'doing'"));
    assert_eq!(field(changed, "task_type").as_deref(), Some("'work'"));
    assert_eq!(field(changed, "assignee").as_deref(), Some("'ellis'"));
    assert_eq!(field(changed, "claimed_by").as_deref(), Some("'ellis/S-1042'"));
    assert_eq!(changed.heading, "A-024: next \u{2192} doing");
}

#[test]
fn a_reassignment_fires_task_reassigned_with_both_assignees() {
    let mut table = table_with(task("next"));
    let mut moved_on = task("next");
    moved_on.assignee = Some("jo".to_owned());

    let events = update(&mut table, moved_on);

    assert_eq!(kinds(&events), ["task.reassigned"]);
    assert_eq!(field(&events[0], "from").as_deref(), Some("'ellis'"));
    assert_eq!(field(&events[0], "to").as_deref(), Some("'jo'"));
}

#[test]
fn a_write_that_changes_nothing_tracked_fires_nothing_even_in_an_undeclared_state() {
    let mut table = table_with(task("limbo"));

    assert_eq!(kinds(&update(&mut table, task("limbo"))), Vec::<&str>::new());
    assert_eq!(table.get(PATH), Some(&task("limbo")));
}

#[test]
fn several_changes_in_one_write_fire_in_the_documented_order_sharing_one_time() {
    let lapsed_claim = claimed("doing", "ellis/S-1", datetime!(2026-10-28 01:00 UTC));
    let mut table = table_with(lapsed_claim);
    let mut next = claimed("limbo", "jo/S-2", datetime!(2026-10-28 09:00 UTC));
    next.assignee = Some("jo".to_owned());

    let events = update(&mut table, next);

    assert_eq!(
        kinds(&events),
        [
            "task.state-changed",
            "task.reassigned",
            "task.claim-lapsed",
            "task.unknown-state"
        ]
    );
    assert!(events.iter().all(|event| event.at == NOW));
    assert_eq!(events[2].severity, Severity::Warning);
    assert_eq!(events[3].severity, Severity::Warning);
}

#[test]
fn a_fresh_claim_after_the_previous_one_expired_fires_claim_lapsed_with_the_previous_values() {
    let until = datetime!(2026-10-28 01:00 UTC);
    let mut table = table_with(claimed("doing", "ellis/S-1", until));

    let events = update(&mut table, claimed("doing", "jo/S-2", datetime!(2026-10-28 09:00 UTC)));

    assert_eq!(kinds(&events), ["task.claim-lapsed"]);
    assert_eq!(field(&events[0], "claimed_by").as_deref(), Some("'ellis/S-1'"));
    assert_eq!(
        field(&events[0], "claimed_until").as_deref(),
        Some("'2026-10-28T01:00:00Z'")
    );
    assert_eq!(events[0].actor.as_deref(), Some("jo/S-2"));
}

#[test]
fn leaving_doing_after_the_claim_expired_fires_claim_lapsed() {
    let mut table = table_with(claimed("doing", "ellis/S-1", datetime!(2026-10-28 01:00 UTC)));

    let events = update(&mut table, task("next"));

    assert_eq!(kinds(&events), ["task.state-changed", "task.claim-lapsed"]);
}

#[test]
fn a_claim_that_has_not_expired_never_lapses() {
    for until in [datetime!(2026-10-28 03:15:42 UTC), datetime!(2026-10-28 04:00 UTC)] {
        let mut table = table_with(claimed("doing", "ellis/S-1", until));
        assert_eq!(
            kinds(&update(
                &mut table,
                claimed("doing", "jo/S-2", datetime!(2026-10-28 09:00 UTC))
            )),
            Vec::<&str>::new(),
            "claim until {until}"
        );
        let mut table = table_with(claimed("doing", "ellis/S-1", until));
        assert_eq!(kinds(&update(&mut table, task("done"))), ["task.state-changed"]);
    }
}

#[test]
fn a_task_with_no_previous_claim_cannot_lapse() {
    let mut table = table_with(task("next"));

    let events = update(&mut table, claimed("doing", "jo/S-2", datetime!(2026-10-28 09:00 UTC)));

    assert_eq!(kinds(&events), ["task.state-changed"]);
}

#[test]
fn the_same_holder_renewing_an_expired_claim_counts_as_claiming_afresh_but_an_untouched_claim_does_not() {
    let expired = claimed("doing", "ellis/S-1", datetime!(2026-10-28 01:00 UTC));
    let mut table = table_with(expired.clone());
    let renewed = claimed("doing", "ellis/S-1", datetime!(2026-10-28 09:00 UTC));
    assert_eq!(kinds(&update(&mut table, renewed)), ["task.claim-lapsed"]);

    let mut table = table_with(expired.clone());
    assert_eq!(kinds(&update(&mut table, expired)), Vec::<&str>::new());
}

#[test]
fn an_undeclared_state_fires_unknown_state_on_create_and_when_the_state_changes_to_it() {
    let declared = states();
    let mut table = TaskTable::default();
    let created = table.apply(
        site(PATH),
        TaskChange::Created {
            task: task("limbo"),
            origin: None,
            declared_states: Some(&declared),
        },
        NOW,
    );
    assert_eq!(kinds(&created), ["task.created", "task.unknown-state"]);
    assert_eq!(field(&created[1], "state").as_deref(), Some("'limbo'"));
    assert_eq!(field(&created[1], "task_type").as_deref(), Some("'work'"));

    let mut table = table_with(task("next"));
    assert_eq!(
        kinds(&update(&mut table, task("limbo"))),
        ["task.state-changed", "task.unknown-state"]
    );
}

#[test]
fn unknown_state_needs_the_types_declared_states_to_judge() {
    let mut table = table_with(task("next"));

    let events = table.apply(
        site(PATH),
        TaskChange::Updated {
            task: task("limbo"),
            declared_states: None,
        },
        NOW,
    );

    assert_eq!(kinds(&events), ["task.state-changed"]);
}

#[test]
fn a_task_the_table_has_not_seen_is_seeded_silently() {
    let mut table = TaskTable::default();

    let events = update(&mut table, task("limbo"));

    assert_eq!(kinds(&events), Vec::<&str>::new());
    assert_eq!(table.get(PATH), Some(&task("limbo")));
}

#[test]
fn a_moved_task_keeps_its_entry_and_fires_task_moved_naming_both_paths() {
    let mut table = table_with(task("next"));
    let new_path = "memory/tasks/zenith/archive/a-024.md";

    let events = table.apply(site(new_path), TaskChange::Moved { from: PATH }, NOW);

    assert_eq!(kinds(&events), ["task.moved"]);
    assert_eq!(events[0].severity, Severity::Info);
    assert_eq!(field(&events[0], "from").as_deref(), Some(format!("'{PATH}'").as_str()));
    assert_eq!(
        field(&events[0], "to").as_deref(),
        Some(format!("'{new_path}'").as_str())
    );
    assert_eq!(
        events[0].subject,
        Some(EventSubject::for_record(new_path, "A-024", BASE)),
        "the move is about the task where it now is, referenced by its key"
    );
    assert_eq!((table.get(PATH), table.get(new_path)), (None, Some(&task("next"))));

    let declared = states();
    let later = table.apply(
        site(new_path),
        TaskChange::Updated {
            task: task("doing"),
            declared_states: Some(&declared),
        },
        NOW,
    );
    assert_eq!(kinds(&later), ["task.state-changed"]);
    assert_eq!(field(&later[0], "from").as_deref(), Some("'next'"));
    assert_eq!(
        later[0].subject.as_ref().map(|subject| subject.link.as_str()),
        Some("[[a-024|A-024]]")
    );
    assert_eq!(
        later[0].subject.as_ref().and_then(|subject| subject.path.as_deref()),
        Some(new_path)
    );
}

#[test]
fn a_move_that_leaves_a_task_where_it_was_fires_nothing() {
    let mut table = table_with(task("next"));

    assert_eq!(
        kinds(&table.apply(site(PATH), TaskChange::Moved { from: PATH }, NOW)),
        Vec::<&str>::new()
    );
    assert_eq!(table.get(PATH), Some(&task("next")));
}

#[test]
fn moving_a_task_the_table_never_saw_changes_nothing() {
    let mut table = TaskTable::default();

    assert_eq!(
        kinds(&table.apply(site("b.md"), TaskChange::Moved { from: "a.md" }, NOW)),
        Vec::<&str>::new()
    );
    assert!(table.is_empty());
}

#[test]
fn a_deleted_task_leaves_the_table_without_an_event() {
    let mut table = table_with(task("next"));

    assert_eq!(
        kinds(&table.apply(site(PATH), TaskChange::Deleted, NOW)),
        Vec::<&str>::new()
    );
    assert!(table.is_empty());
    assert!(
        table.apply(site(PATH), TaskChange::Deleted, NOW).is_empty(),
        "deleting an unknown task is quiet too"
    );
}

#[test]
fn a_task_without_an_id_is_named_by_its_note_in_subject_slug_and_heading() {
    let mut table = table_with(TrackedTask {
        id: None,
        ..task("next")
    });

    let events = update(
        &mut table,
        TrackedTask {
            id: None,
            ..task("doing")
        },
    );

    assert_eq!(events[0].subject, Some(EventSubject::for_note(PATH)));
    assert_eq!(events[0].heading, "a-024: next \u{2192} doing");
}

#[test]
fn an_event_about_a_task_with_no_entity_falls_back_to_vault() {
    let mut table = table_with(TrackedTask {
        entity: None,
        ..task("next")
    });

    let events = update(
        &mut table,
        TrackedTask {
            entity: None,
            ..task("doing")
        },
    );

    assert_eq!(events[0].entity, "vault");
}

#[test]
fn every_fired_event_renders_as_an_event_note_without_note_fields() {
    let mut table = table_with(task("next"));
    let events = update(&mut table, claimed("limbo", "jo/S-2", datetime!(2026-10-28 09:00 UTC)));

    for event in &events {
        let text = event.to_string();
        assert!(text.starts_with("---\ntype: event\nkind: task."), "{text}");
        assert!(!text.contains("\ntitle:") && !text.contains("\ntags:"), "{text}");
    }
}

#[test]
fn the_table_round_trips_through_its_file_and_is_written_whole() -> Result<(), EventsError> {
    let directory = tempfile::tempdir().map_err(|error| EventsError::TableUnwritable {
        reason: error.to_string(),
    })?;
    let path = directory.path().join("state").join("tasks.json");
    let table = table_with(claimed("doing", "ellis/S-1", datetime!(2026-10-28 01:00 UTC)));

    table.store(&path)?;
    let loaded = TaskTable::load(&path)?;

    assert_eq!(loaded, Some(table));
    Ok(())
}

#[test]
fn a_missing_table_file_loads_as_nothing_so_the_caller_seeds_silently() -> Result<(), EventsError> {
    let directory = tempfile::tempdir().map_err(|error| EventsError::TableUnreadable {
        reason: error.to_string(),
    })?;

    assert_eq!(TaskTable::load(&directory.path().join("tasks.json"))?, None);
    Ok(())
}

#[test]
fn an_unreadable_table_file_is_reported_not_trusted() {
    let Ok(directory) = tempfile::tempdir() else { return };
    let path = directory.path().join("tasks.json");
    for contents in ["not json", "{\"version\":999,\"tasks\":{}}", "{\"tasks\":{}}"] {
        assert!(std::fs::write(&path, contents).is_ok());
        assert_eq!(
            TaskTable::load(&path).err().map(|error| error.code()),
            Some("events/table-unreadable"),
            "{contents}"
        );
    }
}

#[test]
fn storing_over_an_existing_table_replaces_it() -> Result<(), EventsError> {
    let directory = tempfile::tempdir().map_err(|error| EventsError::TableUnwritable {
        reason: error.to_string(),
    })?;
    let path = directory.path().join("tasks.json");
    table_with(task("next")).store(&path)?;
    let newer = table_with(task("done"));

    newer.store(&path)?;

    assert_eq!(TaskTable::load(&path)?, Some(newer));
    Ok(())
}

fn frontmatter(text: &str) -> serde_json::Map<String, serde_json::Value> {
    match serde_json::from_str(text) {
        Ok(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    }
}

#[test]
fn a_task_note_is_read_from_its_frontmatter() -> Result<(), NotATask> {
    let task = TrackedTask::try_from(&frontmatter(
        r#"{"type":"task","task_type":"action","id":"A-024","state":"doing","assignee":"ellis",
            "claimed_by":"ellis/S-1","claimed_until":"2026-10-28T04:00:00+10:00","entity":"zenith"}"#,
    ))?;

    assert_eq!(task.task_type.as_deref(), Some("action"));
    assert_eq!(task.id.as_deref(), Some("A-024"));
    assert_eq!(task.state.as_deref(), Some("doing"));
    assert_eq!(task.assignee.as_deref(), Some("ellis"));
    assert_eq!(task.claimed_by.as_deref(), Some("ellis/S-1"));
    assert_eq!(task.claimed_until, Some(datetime!(2026-10-27 18:00:00 UTC)));
    assert_eq!(task.entity.as_deref(), Some("zenith"));
    Ok(())
}

#[test]
fn a_note_that_is_not_type_task_is_not_a_task() {
    for text in [
        "{}",
        r#"{"type":"note","state":"next"}"#,
        r#"{"type":["task"]}"#,
        r#"{"state":"next"}"#,
    ] {
        assert!(TrackedTask::try_from(&frontmatter(text)).is_err(), "{text}");
    }
}

#[test]
fn blank_or_non_text_values_and_an_unparsable_claim_time_read_as_absent() -> Result<(), NotATask> {
    let task = TrackedTask::try_from(&frontmatter(
        r#"{"type":"task","state":"  ","assignee":7,"claimed_by":null,"claimed_until":"tomorrow"}"#,
    ))?;

    assert_eq!(task.state, None);
    assert_eq!(task.assignee, None);
    assert_eq!(task.claimed_by, None);
    assert_eq!(task.claimed_until, None);
    Ok(())
}

#[test]
fn the_table_knows_whether_it_tracks_any_task_below_a_folder() {
    let table = table_with(task("next"));

    assert!(table.holds_below("memory/tasks/zenith"));
    assert!(table.holds_below("memory/tasks"));
    assert!(table.holds_below("memory"));
    assert!(
        !table.holds_below("memory/tasks/zen"),
        "a name that merely starts alike is another folder"
    );
    assert!(
        !table.holds_below("memory/tasks/zenith/a-024.md"),
        "a task is not a folder"
    );
    assert!(!table.holds_below("elsewhere"));
}

#[test]
fn the_paths_below_a_folder_are_the_tracked_tasks_beneath_it_and_no_neighbour() {
    let mut table = TaskTable::default();
    for path in [
        "memory/tasks/inbox/a-1.md",
        "memory/tasks/inbox/deep/a-2.md",
        "memory/tasks/inboxes/a-3.md",
        "memory/tasks/a-4.md",
    ] {
        table.tasks.insert(path.to_owned(), task("next"));
    }

    assert_eq!(
        table.paths_below("memory/tasks/inbox"),
        ["memory/tasks/inbox/a-1.md", "memory/tasks/inbox/deep/a-2.md"]
    );
    assert_eq!(table.paths_below("memory/tasks/none"), Vec::<String>::new());
}
