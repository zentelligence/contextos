use serde_json::json;

use super::*;

fn row(extra: Value) -> Result<EventRow, serde_json::Error> {
    let mut base = json!({
        "file.path": "memory/events/2026/10/2026-10-28-031542-a-024.md",
        "kind": "task.state-changed",
        "severity": "info",
        "at": "2026-10-28T03:15:42Z",
        "entity": "zenith",
        "subject": "[[memory/tasks/a-024|A-024]]",
        "subject_id": "A-024",
        "actor": "ellis/S-001",
        "from": "next",
        "to": "doing"
    });
    if let (Some(base), Value::Object(extra)) = (base.as_object_mut(), extra) {
        base.extend(extra);
    }
    serde_json::from_value(base)
}

#[test]
fn a_full_row_becomes_an_event_with_its_subject_target_and_own_fields() -> Result<(), Box<dyn std::error::Error>> {
    let event = SignalEvent::try_from(row(json!({}))?).map_err(|_| "readable")?;

    assert_eq!(event.kind, "task.state-changed");
    assert_eq!(event.severity, Severity::Info);
    assert_eq!(event.entity, "zenith");
    assert_eq!(event.subject.as_deref(), Some("memory/tasks/a-024"));
    assert_eq!(event.subject_id.as_deref(), Some("A-024"));
    assert_eq!(event.actor.as_deref(), Some("ellis/S-001"));
    assert_eq!(event.field("from"), Some("next"));
    assert_eq!(event.field("to"), Some("doing"));
    Ok(())
}

#[test]
fn a_row_without_a_readable_at_kind_or_severity_is_not_an_event() -> Result<(), Box<dyn std::error::Error>> {
    for bad in [
        json!({"at": null}),
        json!({"at": "yesterday"}),
        json!({"kind": null}),
        json!({"kind": "  "}),
        json!({"severity": "fatal"}),
        json!({"severity": null}),
        json!({"file.path": null}),
    ] {
        assert!(SignalEvent::try_from(row(bad.clone())?).is_err(), "{bad}");
    }
    Ok(())
}

#[test]
fn an_entity_is_read_as_stored_so_a_query_and_a_stream_agree_on_which_events_are_its_own()
-> Result<(), Box<dyn std::error::Error>> {
    let padded = SignalEvent::try_from(row(json!({"entity": " acme "}))?).map_err(|_| "readable")?;
    let blank = SignalEvent::try_from(row(json!({"entity": "  "}))?).map_err(|_| "readable")?;

    assert_eq!(padded.entity, " acme ");
    assert_eq!(blank.entity, "vault", "an entity of only spaces is no entity");
    Ok(())
}

#[test]
fn a_missing_entity_reads_as_the_vault() -> Result<(), Box<dyn std::error::Error>> {
    let event = SignalEvent::try_from(row(json!({"entity": null}))?).map_err(|_| "readable")?;

    assert_eq!(event.entity, "vault");
    Ok(())
}

#[test]
fn a_list_field_reads_as_one_comma_separated_line() -> Result<(), Box<dyn std::error::Error>> {
    let event = SignalEvent::try_from(row(json!({"members": ["ellis", "vela"]}))?).map_err(|_| "readable")?;

    assert_eq!(event.field("members"), Some("ellis, vela"));
    Ok(())
}

#[test]
fn a_link_names_its_target_without_label_anchor_or_markdown_extension() {
    assert_eq!(link_target("[[a/b.md|B]]").as_deref(), Some("a/b"));
    assert_eq!(link_target("[[a/b#section]]").as_deref(), Some("a/b"));
    assert_eq!(
        link_target("[[registry/jobs/x.toml|x]]").as_deref(),
        Some("registry/jobs/x.toml")
    );
    assert_eq!(link_target("plain text"), None);
    assert_eq!(link_target("[[|x]]"), None);
    assert_eq!(link_label("[[a/b|Bee]]").as_deref(), Some("Bee"));
    assert_eq!(link_label("[[a/b]]").as_deref(), Some("a/b"));
}

#[test]
fn a_cursor_round_trips_orders_by_time_then_path_and_refuses_malformed_text() -> Result<(), Box<dyn std::error::Error>>
{
    let early = Cursor::try_from("2026-10-28T03:15:42Z|memory/events/a.md").map_err(|_| "parses")?;
    let same_second = Cursor::try_from("2026-10-28T03:15:42Z|memory/events/b.md").map_err(|_| "parses")?;
    let later = Cursor::try_from("2026-10-28T03:15:43+00:00|memory/events/a.md").map_err(|_| "parses")?;

    assert!(early < same_second && same_second < later);
    assert_eq!(early.to_string(), "2026-10-28T03:15:42Z|memory/events/a.md");
    assert_eq!(
        Cursor::try_from(early.to_string().as_str()).map_err(|_| "round trip")?,
        early
    );
    for bad in [
        "",
        "no separator",
        "2026-10-28T03:15:42Z|",
        "yesterday|a.md",
        "2026-10-28T03:15:42Z|a\nb",
        "2026-10-28T03:15:42Z|a\rb",
        "2026-10-28T03:15:42Z|a\r",
    ] {
        assert!(Cursor::try_from(bad).is_err(), "{bad:?}");
    }
    Ok(())
}

#[test]
fn shipped_kinds_are_those_the_server_and_scaffold_own() -> Result<(), Box<dyn std::error::Error>> {
    for (kind, shipped) in [
        ("task.created", true),
        ("member.state-changed", true),
        ("vault.degraded", true),
        ("job.failed", true),
        ("session.run-finished", true),
        ("agent.delegated", true),
        ("import.completed", false),
        ("trade.executed", false),
        ("my-task.created", false),
        ("task.created-elsewhere", false),
        ("job.custom", false),
    ] {
        let event = SignalEvent::try_from(row(json!({"kind": kind}))?).map_err(|_| "readable")?;
        assert_eq!(event.is_shipped_kind(), shipped, "{kind}");
    }
    Ok(())
}

#[test]
fn a_row_whose_path_holds_a_line_break_is_not_an_event_because_it_would_corrupt_a_stream_id()
-> Result<(), Box<dyn std::error::Error>> {
    for path in ["memory/events/a\nb.md", "memory/events/a\rb.md"] {
        assert!(
            SignalEvent::try_from(row(json!({"file.path": path}))?).is_err(),
            "{path:?}"
        );
    }
    Ok(())
}
