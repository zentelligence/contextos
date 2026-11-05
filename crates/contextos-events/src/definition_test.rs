use super::*;

const FULL: &str = "---\n\
type: event-kind\n\
kind: session.run-finished\n\
label: Session run finished\n\
description: A scheduled session finished working the task list.\n\
severities: [info, warning, error]\n\
default_severity: info\n\
fields:\n  required: [run_id, members]\n  optional: [note]\n\
subject: optional\n\
id_field: key\n\
retention_days: 90\n\
---\n\
\n\
Record this when a run ends.\n";

fn parse(stem: &str, text: &str) -> Result<KindDefinition, EventsError> {
    KindDefinition::try_from(KindDefinitionSource { stem, text })
}

fn with(replace: &str, by: &str) -> String {
    assert!(FULL.contains(replace), "fixture lacks {replace:?}");
    FULL.replacen(replace, by, 1)
}

fn refusal(text: &str) -> Option<(&'static str, DefinitionRejection)> {
    match parse("session.run-finished", text) {
        Err(EventsError::InvalidDefinition { field, reason }) => Some((field, reason)),
        other => {
            assert!(other.is_err(), "expected a refusal");
            None
        }
    }
}

#[test]
fn a_complete_definition_is_read_in_full() -> Result<(), EventsError> {
    let definition = parse("session.run-finished", FULL)?;

    assert_eq!(definition.kind.as_str(), "session.run-finished");
    assert_eq!(definition.label, "Session run finished");
    assert_eq!(
        definition.description,
        "A scheduled session finished working the task list."
    );
    assert_eq!(definition.severities, Severity::ALL);
    assert_eq!(definition.default_severity, Severity::Info);
    assert_eq!(
        definition.required.iter().map(FieldName::as_str).collect::<Vec<_>>(),
        ["run_id", "members"]
    );
    assert_eq!(
        definition.optional.iter().map(FieldName::as_str).collect::<Vec<_>>(),
        ["note"]
    );
    assert_eq!(definition.subject, SubjectRule::Optional);
    assert_eq!(definition.id_field.as_ref().map(FieldName::as_str), Some("key"));
    assert_eq!(definition.retention_days, Some(90));
    Ok(())
}

#[test]
fn a_minimal_definition_needs_no_fields_key_or_retention() -> Result<(), EventsError> {
    let text = "---\ntype: event-kind\nkind: import.completed\nlabel: Import\ndescription: Loaded.\n\
        severities: [info]\ndefault_severity: info\nsubject: none\n---\n";

    let definition = parse("import.completed", text)?;

    assert!(definition.required.is_empty() && definition.optional.is_empty());
    assert_eq!(definition.subject, SubjectRule::Absent);
    assert_eq!((definition.id_field, definition.retention_days), (None, None));
    Ok(())
}

#[test]
fn a_kind_that_differs_from_the_file_name_is_refused() {
    assert_eq!(
        parse("session.run-started", FULL).err().map(|error| error.code()),
        Some("events/invalid-definition")
    );
    assert_eq!(
        parse("session.run-started", FULL).err().map(|error| error.to_string()),
        Some("the definition's `kind` cannot be used: it differs from the file's name".to_owned())
    );
}

#[test]
fn a_server_fired_kind_cannot_be_defined() {
    let text = with("kind: session.run-finished", "kind: vault.degraded");
    assert_eq!(
        parse("vault.degraded", &text).err().map(|error| error.code()),
        Some("events/server-fired-kind")
    );
}

#[test]
fn the_wrong_note_type_or_missing_frontmatter_is_refused() {
    assert_eq!(
        refusal(&with("type: event-kind", "type: note")),
        Some(("type", DefinitionRejection::NotAllowed))
    );
    assert_eq!(
        refusal("no frontmatter at all\n"),
        Some(("type", DefinitionRejection::Missing))
    );
    assert_eq!(
        refusal("---\nkind: [unclosed\n---\n"),
        Some(("frontmatter", DefinitionRejection::Malformed))
    );
}

#[test]
fn required_text_fields_must_be_present_text() {
    assert_eq!(
        refusal(&with("label: Session run finished\n", "")),
        Some(("label", DefinitionRejection::Missing))
    );
    assert_eq!(
        refusal(&with("label: Session run finished", "label: ''")),
        Some(("label", DefinitionRejection::Empty))
    );
    assert_eq!(
        refusal(&with("label: Session run finished", "label: 3")),
        Some(("label", DefinitionRejection::WrongType))
    );
    assert_eq!(
        refusal(&with(
            "description: A scheduled session finished working the task list.\n",
            ""
        )),
        Some(("description", DefinitionRejection::Missing))
    );
}

#[test]
fn severities_must_be_a_non_empty_distinct_subset_of_the_three() {
    assert_eq!(
        refusal(&with("severities: [info, warning, error]", "severities: []")),
        Some(("severities", DefinitionRejection::Empty))
    );
    assert_eq!(
        refusal(&with("severities: [info, warning, error]", "severities: [info, loud]")),
        Some(("severities", DefinitionRejection::NotAllowed))
    );
    assert_eq!(
        refusal(&with("severities: [info, warning, error]", "severities: [info, info]")),
        Some(("severities", DefinitionRejection::Duplicate))
    );
    assert_eq!(
        refusal(&with("severities: [info, warning, error]", "severities: info")),
        Some(("severities", DefinitionRejection::WrongType))
    );
}

#[test]
fn the_default_severity_must_be_one_the_kind_allows() {
    let text = with("severities: [info, warning, error]", "severities: [warning, error]");
    assert_eq!(
        refusal(&text),
        Some(("default_severity", DefinitionRejection::NotAllowed))
    );
}

#[test]
fn subject_must_be_required_optional_or_none() {
    assert_eq!(
        refusal(&with("subject: optional", "subject: sometimes")),
        Some(("subject", DefinitionRejection::NotAllowed))
    );
    assert_eq!(
        refusal(&with("subject: optional\n", "")),
        Some(("subject", DefinitionRejection::Missing))
    );
}

#[test]
fn field_names_may_not_collide_with_event_note_or_stamped_names() {
    for reserved in [
        "at",
        "entity",
        "subject_id",
        "actor",
        "title",
        "tags",
        "description",
        "path",
        "filename",
        "type",
    ] {
        let text = with("optional: [note]", &format!("optional: [{reserved}]"));
        assert_eq!(
            refusal(&text),
            Some(("fields", DefinitionRejection::ReservedName)),
            "{reserved}"
        );
    }
}

#[test]
fn field_names_must_be_valid_and_declared_once() {
    assert_eq!(
        refusal(&with("optional: [note]", "optional: [Bad Name]")),
        Some(("fields", DefinitionRejection::InvalidName))
    );
    assert_eq!(
        refusal(&with("optional: [note]", "optional: [run_id]")),
        Some(("fields", DefinitionRejection::Duplicate))
    );
    assert_eq!(
        refusal(&with("required: [run_id, members]", "required: [run_id, run_id]")),
        Some(("fields", DefinitionRejection::Duplicate))
    );
    assert_eq!(
        refusal(&with("optional: [note]", "optional: note")),
        Some(("fields", DefinitionRejection::WrongType))
    );
}

#[test]
fn the_key_field_is_server_assigned_so_it_may_not_also_be_a_declared_field() {
    assert_eq!(
        refusal(&with("id_field: key", "id_field: run_id")),
        Some(("id_field", DefinitionRejection::DeclaredAsField))
    );
    assert_eq!(
        refusal(&with("id_field: key", "id_field: at")),
        Some(("id_field", DefinitionRejection::ReservedName))
    );
    assert_eq!(
        refusal(&with("id_field: key", "id_field: Bad")),
        Some(("id_field", DefinitionRejection::InvalidName))
    );
}

#[test]
fn retention_days_must_be_a_positive_whole_number() {
    for bad in ["0", "-3", "1.5", "'ninety'"] {
        assert!(
            refusal(&with("retention_days: 90", &format!("retention_days: {bad}"))).is_some(),
            "{bad}"
        );
    }
    assert_eq!(
        refusal(&with("retention_days: 90", "retention_days: 0")),
        Some(("retention_days", DefinitionRejection::NotPositive))
    );
}

#[test]
fn a_registry_finds_a_declared_kind_and_refuses_everything_else() -> Result<(), EventsError> {
    let mut registry = KindRegistry::default();
    registry.insert(parse("session.run-finished", FULL)?)?;

    assert!(registry.lookup(Some("session.run-finished")).is_ok());
    let unknown = [
        None,
        Some(""),
        Some("  "),
        Some("import.completed"),
        Some("Session.Run-Finished"),
    ];
    for kind in unknown {
        assert_eq!(
            registry.lookup(kind).err().map(|error| error.code()),
            Some("events/unknown-kind"),
            "{kind:?}"
        );
    }
    assert_eq!(
        registry.lookup(Some("task.created")).err().map(|error| error.code()),
        Some("events/server-fired-kind")
    );
    Ok(())
}

#[test]
fn defining_a_kind_twice_is_refused_and_keeps_the_first() -> Result<(), EventsError> {
    let mut registry = KindRegistry::default();
    registry.insert(parse("session.run-finished", FULL)?)?;

    let again = registry.insert(parse("session.run-finished", FULL)?);

    assert_eq!(
        again.err().map(|error| error.code()),
        Some("events/duplicate-definition")
    );
    assert_eq!(registry.len(), 1);
    Ok(())
}

#[test]
fn a_registry_lists_the_retention_windows_only_of_the_kinds_that_declare_one() -> Result<(), EventsError> {
    let mut registry = KindRegistry::default();
    for (stem, extra) in [("trade.executed", "retention_days: 400\n"), ("order.placed", "")] {
        let text = format!(
            "---\ntype: event-kind\nkind: {stem}\nlabel: L\ndescription: d\nseverities: [info]\ndefault_severity: info\nsubject: none\n{extra}---\n"
        );
        registry.insert(KindDefinition::try_from(KindDefinitionSource { stem, text: &text })?)?;
    }

    assert_eq!(registry.retention_windows(), vec![("trade.executed".to_owned(), 400)]);
    Ok(())
}
