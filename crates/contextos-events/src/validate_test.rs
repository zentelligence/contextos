use serde_json::json;
use time::macros::datetime;

use super::*;
use crate::{KindDefinitionSource, KindRegistry};

const FINISHED: &str = "---\n\
type: event-kind\n\
kind: session.run-finished\n\
label: Session run finished\n\
description: A scheduled session finished.\n\
severities: [info, warning, error]\n\
default_severity: info\n\
fields:\n  required: [members, review]\n  optional: [note]\n\
subject: optional\n\
---\n";

const STARTED: &str = "---\n\
type: event-kind\n\
kind: session.run-started\n\
label: Session run started\n\
description: A scheduled session started.\n\
severities: [info]\n\
default_severity: info\n\
fields:\n  required: [members]\n\
subject: none\n\
id_field: run_id\n\
---\n";

const FAILED: &str = "---\n\
type: event-kind\n\
kind: job.failed\n\
label: Job failed\n\
description: A job failed.\n\
severities: [error]\n\
default_severity: error\n\
subject: required\n\
---\n";

fn registry() -> Result<KindRegistry, EventsError> {
    let mut registry = KindRegistry::default();
    for (stem, text) in [
        ("session.run-finished", FINISHED),
        ("session.run-started", STARTED),
        ("job.failed", FAILED),
    ] {
        registry.insert(KindDefinition::try_from(KindDefinitionSource { stem, text })?)?;
    }
    Ok(registry)
}

fn request(kind: &str, fields: serde_json::Value) -> RecordRequest {
    RecordRequest {
        kind: Some(kind.to_owned()),
        fields: match fields {
            serde_json::Value::Object(map) => map,
            _ => serde_json::Map::new(),
        },
        ..RecordRequest::default()
    }
}

fn context() -> RecordContext {
    RecordContext {
        at: datetime!(2026-10-28 03:15:42 UTC),
        subject_entity: None,
        subject_note: None,
        recorder_entity: None,
        assigned_key: None,
    }
}

fn code_of(result: Result<EventRecord, EventsError>) -> Option<&'static str> {
    result.err().map(|error| error.code())
}

#[test]
fn a_valid_request_becomes_a_record_with_defaults_stamped_and_fields_in_declared_order() -> Result<(), EventsError> {
    let registry = registry()?;
    let mut ask = request(
        "session.run-finished",
        json!({"note": "all done", "review": 2, "members": ["ellis", "jo"]}),
    );
    ask.content = Some("Worked the list.".to_owned());

    let record = registry.validate(&ask, &context())?;

    assert_eq!(record.kind.as_str(), "session.run-finished");
    assert_eq!(record.severity, Severity::Info);
    assert_eq!(record.at, datetime!(2026-10-28 03:15:42 UTC));
    assert_eq!(record.entity, "vault");
    assert_eq!(record.heading, "Session run finished");
    assert_eq!(record.content.as_deref(), Some("Worked the list."));
    assert_eq!(
        record.fields.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(),
        ["members", "review", "note"],
        "required before optional, each in declared order, whatever order the request used"
    );
    Ok(())
}

#[test]
fn a_given_severity_the_kind_allows_is_used() -> Result<(), EventsError> {
    let mut ask = request("session.run-finished", json!({"members": [], "review": 0}));
    ask.severity = Some("warning".to_owned());

    assert_eq!(registry()?.validate(&ask, &context())?.severity, Severity::Warning);
    Ok(())
}

#[test]
fn a_severity_the_kind_does_not_allow_or_that_is_not_one_is_refused() -> Result<(), EventsError> {
    let registry = registry()?;
    for severity in ["error", "loud", ""] {
        let mut ask = request("session.run-started", json!({"members": []}));
        ask.severity = Some(severity.to_owned());
        assert_eq!(
            code_of(registry.validate(&ask, &context())),
            Some("events/invalid-severity"),
            "{severity:?}"
        );
    }
    Ok(())
}

#[test]
fn an_undeclared_or_server_fired_kind_is_refused_before_anything_else() -> Result<(), EventsError> {
    let registry = registry()?;
    assert_eq!(
        code_of(registry.validate(&request("nope", json!({})), &context())),
        Some("events/unknown-kind")
    );
    assert_eq!(
        code_of(registry.validate(&RecordRequest::default(), &context())),
        Some("events/unknown-kind")
    );
    assert_eq!(
        code_of(registry.validate(&request("task.created", json!({"at": "x"})), &context())),
        Some("events/server-fired-kind")
    );
    Ok(())
}

#[test]
fn a_required_field_that_is_missing_is_refused_naming_it() -> Result<(), EventsError> {
    let error = registry()?
        .validate(&request("session.run-finished", json!({"members": []})), &context())
        .err();

    assert!(
        matches!(
            error,
            Some(EventsError::InvalidFields { ref field, reason: FieldRejection::Missing, .. }) if field == "review"
        ),
        "{error:?}"
    );
    Ok(())
}

#[test]
fn an_undeclared_field_is_refused_whatever_its_name() -> Result<(), EventsError> {
    for name in ["extra", "kind", "severity", "entity", "subject", "actor", "type"] {
        let mut fields = json!({"members": [], "review": 1});
        fields[name] = json!("x");
        let error = registry()?
            .validate(&request("session.run-finished", fields), &context())
            .err();
        assert!(
            matches!(
                error,
                Some(EventsError::InvalidFields {
                    reason: FieldRejection::Undeclared,
                    ..
                })
            ),
            "{name}: {error:?}"
        );
    }
    Ok(())
}

#[test]
fn common_note_fields_are_refused_as_such() -> Result<(), EventsError> {
    for name in ["title", "created", "updated", "tags", "aliases", "description"] {
        let mut fields = json!({"members": [], "review": 1});
        fields[name] = json!("x");
        let error = registry()?
            .validate(&request("session.run-finished", fields), &context())
            .err();
        assert!(
            matches!(
                error,
                Some(EventsError::InvalidFields {
                    reason: FieldRejection::CommonNoteField,
                    ..
                })
            ),
            "{name}: {error:?}"
        );
        assert_eq!(error.map(|error| error.code()), Some("events/invalid-fields"));
    }
    Ok(())
}

#[test]
fn a_field_value_that_cannot_be_frontmatter_is_refused() -> Result<(), EventsError> {
    for bad in [json!(null), json!({"a": 1}), json!("two\nlines")] {
        let error = registry()?
            .validate(
                &request("session.run-finished", json!({"members": bad, "review": 1})),
                &context(),
            )
            .err();
        assert!(
            matches!(
                error,
                Some(EventsError::InvalidFields {
                    reason: FieldRejection::InvalidValue,
                    ..
                })
            ),
            "{error:?}"
        );
    }
    Ok(())
}

#[test]
fn what_the_server_stamps_cannot_be_supplied() -> Result<(), EventsError> {
    let registry = registry()?;
    for name in ["at", "path", "filename"] {
        let mut fields = json!({"members": [], "review": 1});
        fields[name] = json!("x");
        let error = registry
            .validate(&request("session.run-finished", fields), &context())
            .err();
        assert!(
            matches!(&error, Some(EventsError::ServerStamped { field }) if field == name),
            "{name}: {error:?}"
        );
        assert_eq!(error.map(|error| error.code()), Some("events/server-stamped"));
    }
    let error = registry
        .validate(
            &request("session.run-started", json!({"members": [], "run_id": "S-9"})),
            &context(),
        )
        .err();
    assert!(
        matches!(&error, Some(EventsError::ServerStamped { field }) if field == "run_id"),
        "{error:?}"
    );
    Ok(())
}

fn about(path: &str, record: Option<(&str, &str)>) -> RecordContext {
    RecordContext {
        subject_note: Some(SubjectNote {
            path: path.to_owned(),
            record: record.map(|(key, base)| SubjectRecord {
                key: key.to_owned(),
                base: base.to_owned(),
            }),
        }),
        ..context()
    }
}

#[test]
fn a_subject_naming_a_record_by_its_key_is_stamped_with_its_key_path_and_base() -> Result<(), EventsError> {
    let registry = registry()?;
    let mut ask = request("job.failed", json!({}));
    ask.subject = Some("[[a-024|A-024]]".to_owned());

    let record = registry.validate(
        &ask,
        &about("memory/tasks/clients/a-024.md", Some(("A-024", "memory/tasks.base"))),
    )?;

    assert_eq!(
        record.subject,
        Some(EventSubject {
            link: "[[a-024|A-024]]".to_owned(),
            id: Some("A-024".to_owned()),
            path: Some("memory/tasks/clients/a-024.md".to_owned()),
            base: Some("memory/tasks.base".to_owned()),
        })
    );
    Ok(())
}

#[test]
fn a_subject_naming_a_record_by_its_path_is_refused() -> Result<(), EventsError> {
    let registry = registry()?;
    let mut ask = request("job.failed", json!({}));
    ask.subject = Some("[[memory/tasks/clients/a-024|A-024]]".to_owned());

    let refused = registry.validate(
        &ask,
        &about("memory/tasks/clients/a-024.md", Some(("A-024", "memory/tasks.base"))),
    );

    assert!(
        matches!(
            refused,
            Err(EventsError::InvalidSubject {
                reason: SubjectRejection::NotByKey,
                ..
            })
        ),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn a_subject_naming_a_note_that_is_no_record_is_stamped_with_its_path_only() -> Result<(), EventsError> {
    let registry = registry()?;
    let mut ask = request("job.failed", json!({}));
    ask.subject = Some("[[registry/jobs/nightly|nightly]]".to_owned());

    let record = registry.validate(&ask, &about("registry/jobs/nightly.md", None))?;

    assert_eq!(
        record.subject,
        Some(EventSubject {
            link: "[[registry/jobs/nightly|nightly]]".to_owned(),
            id: None,
            path: Some("registry/jobs/nightly.md".to_owned()),
            base: None,
        })
    );
    Ok(())
}

#[test]
fn a_subject_is_checked_against_the_kinds_rule() -> Result<(), EventsError> {
    let registry = registry()?;

    let mut required = request("job.failed", json!({}));
    assert_eq!(
        code_of(registry.validate(&required, &context())),
        Some("events/invalid-subject")
    );
    required.subject = Some("[[registry/jobs/nightly|nightly]]".to_owned());
    let record = registry.validate(&required, &context())?;
    assert_eq!(
        record.subject,
        Some(EventSubject {
            link: "[[registry/jobs/nightly|nightly]]".to_owned(),
            id: None,
            path: None,
            base: None,
        }),
        "a link that names no note is kept as given, with nothing stamped"
    );

    let mut absent = request("session.run-started", json!({"members": []}));
    absent.subject = Some("[[x]]".to_owned());
    assert_eq!(
        code_of(registry.validate(&absent, &context())),
        Some("events/invalid-subject")
    );

    for stamped in ["subject_id", "subject_path", "subject_base"] {
        let supplied = request(
            "session.run-finished",
            json!({"members": [], "review": 0, stamped: "A-1"}),
        );
        assert_eq!(
            code_of(registry.validate(&supplied, &context())),
            Some("events/server-stamped"),
            "{stamped}"
        );
    }

    for bad in [
        "",
        "  ",
        "two\nlines",
        "memory/tasks/a-001",
        "![[memory/tasks/a-001]]",
        "see [[memory/tasks/a-001]]",
    ] {
        let mut malformed = request("session.run-finished", json!({"members": [], "review": 0}));
        malformed.subject = Some(bad.to_owned());
        assert_eq!(
            code_of(registry.validate(&malformed, &context())),
            Some("events/invalid-subject"),
            "{bad:?}"
        );
    }
    Ok(())
}

#[test]
fn the_entity_follows_the_resolution_order() -> Result<(), EventsError> {
    let registry = registry()?;
    let mut ask = request("session.run-finished", json!({"members": [], "review": 0}));
    let mut with_context = context();
    with_context.recorder_entity = Some("recorder".to_owned());
    assert_eq!(registry.validate(&ask, &with_context)?.entity, "recorder");

    ask.entity = Some("given".to_owned());
    assert_eq!(registry.validate(&ask, &with_context)?.entity, "given");

    with_context.subject_entity = Some("subject".to_owned());
    assert_eq!(registry.validate(&ask, &with_context)?.entity, "subject");
    Ok(())
}

#[test]
fn an_assigned_key_is_placed_first_among_the_kinds_own_fields() -> Result<(), EventsError> {
    let mut with_key = context();
    with_key.assigned_key = Some("S-7".to_owned());

    let record = registry()?.validate(&request("session.run-started", json!({"members": ["a"]})), &with_key)?;

    assert_eq!(
        record.fields.first(),
        Some(&(FieldName::try_from("run_id")?, FieldValue::from("S-7")))
    );
    assert_eq!(record.fields.len(), 2);
    Ok(())
}

#[test]
fn a_key_must_be_assigned_exactly_when_the_kind_declares_a_key_field() -> Result<(), EventsError> {
    let registry = registry()?;
    assert_eq!(
        code_of(registry.validate(&request("session.run-started", json!({"members": []})), &context())),
        Some("events/key-mismatch")
    );
    let mut with_key = context();
    with_key.assigned_key = Some("S-1".to_owned());
    let mut failed = request("job.failed", json!({}));
    failed.subject = Some("[[registry/jobs/nightly|nightly]]".to_owned());
    assert_eq!(
        code_of(registry.validate(&failed, &with_key)),
        Some("events/key-mismatch")
    );
    Ok(())
}

#[test]
fn a_refused_request_produces_no_record() -> Result<(), EventsError> {
    let mut ask = request("session.run-finished", json!({"members": []}));
    ask.severity = Some("error".to_owned());
    assert!(registry()?.validate(&ask, &context()).is_err());
    Ok(())
}

#[test]
fn a_keyed_kind_takes_the_series_the_sidecar_maps_to_it() -> Result<(), EventsError> {
    let registry = registry()?;
    let started = registry.lookup(Some("session.run-started"))?;

    assert_eq!(started.select_series(Some("S"), &["S"])?, Some("S".to_owned()));
    Ok(())
}

#[test]
fn a_keyed_kind_refuses_a_missing_prefix_and_a_series_not_mapped_to_it() -> Result<(), EventsError> {
    let registry = registry()?;
    let started = registry.lookup(Some("session.run-started"))?;

    for (requested, mapped, rejection) in [
        (None, vec!["S"], SeriesRejection::Missing),
        (Some("T"), vec!["S"], SeriesRejection::NotMapped),
        (Some("S"), vec![], SeriesRejection::NotMapped),
    ] {
        let error = started
            .select_series(requested, &mapped)
            .err()
            .ok_or(EventsError::UnknownKind { kind: None })?;
        assert_eq!(error.code(), "events/invalid-series");
        assert!(matches!(error, EventsError::InvalidSeries { reason, .. } if reason == rejection));
    }
    Ok(())
}

#[test]
fn a_kind_without_a_key_field_refuses_a_prefix_and_assigns_no_series() -> Result<(), EventsError> {
    let registry = registry()?;
    let failed = registry.lookup(Some("job.failed"))?;

    assert_eq!(failed.select_series(None, &[])?, None);
    let error = failed
        .select_series(Some("S"), &["S"])
        .err()
        .ok_or(EventsError::UnknownKind { kind: None })?;
    assert!(matches!(
        error,
        EventsError::InvalidSeries {
            reason: SeriesRejection::NotAccepted,
            ..
        }
    ));
    Ok(())
}
