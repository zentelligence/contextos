use super::*;

#[test]
fn every_refusal_has_its_stable_code() {
    let cases = [
        (EventsError::UnknownKind { kind: None }, "events/unknown-kind"),
        (
            EventsError::ServerFiredKind {
                kind: "task.created".to_owned(),
            },
            "events/server-fired-kind",
        ),
        (
            EventsError::SeverityNotAllowed {
                kind: "k".to_owned(),
                severity: "error".to_owned(),
            },
            "events/invalid-severity",
        ),
        (
            EventsError::InvalidFields {
                kind: "k".to_owned(),
                field: "title".to_owned(),
                reason: FieldRejection::CommonNoteField,
            },
            "events/invalid-fields",
        ),
        (
            EventsError::InvalidSubject {
                kind: "k".to_owned(),
                reason: SubjectRejection::Required,
            },
            "events/invalid-subject",
        ),
        (
            EventsError::ServerStamped { field: "at".to_owned() },
            "events/server-stamped",
        ),
        (
            EventsError::TasksConfigUnreadable {
                reason: "expected a table".to_owned(),
            },
            "events/tasks-config-unreadable",
        ),
    ];
    for (error, code) in cases {
        assert_eq!(error.code(), code);
        assert_ne!(error.remediation(), "");
    }
}
