use serde_json::{Value, json};

use super::*;
use crate::signals_event::EventRow;

fn event(kind: &str, severity: &str, second: u32, extra: Value) -> SignalEvent {
    let mut row = json!({
        "file.path": format!("memory/events/2026/10/e-{second:03}.md"),
        "kind": kind,
        "severity": severity,
        "at": format!("2026-10-28T03:15:{second:02}Z"),
        "entity": "acme",
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

fn urls() -> VaultUrls {
    VaultUrls::from("vault")
}

#[test]
fn a_kind_is_labelled_by_the_vault_then_the_server_then_itself() {
    let labels = KindCatalogue::from(HashMap::from([(
        "import.completed".to_owned(),
        "Import finished".to_owned(),
    )]));

    assert_eq!(labels.label("import.completed"), "Import finished");
    assert_eq!(labels.label("task.claim-lapsed"), "Task claim lapsed");
    assert_eq!(labels.label("trade.executed"), "trade.executed");
    let kinds = labels.kinds();
    assert!(kinds.contains(&"import.completed".to_owned()) && kinds.contains(&"member.state-changed".to_owned()));
    assert!(
        kinds.windows(2).all(|pair| pair[0] < pair[1]),
        "sorted and distinct: {kinds:?}"
    );
}

#[test]
fn an_event_line_shows_a_change_then_the_fields_a_reader_decides_from() {
    let line = EventLine::new(
        &urls(),
        &event(
            "task.state-changed",
            "info",
            1,
            json!({
                "from": "next", "to": "doing", "task_type": "build", "code": "x/y", "subject": "[[a-1|A-1]]",
                "subject_id": "A-1", "subject_path": "memory/tasks/clients/a-1.md", "subject_base": "memory/tasks.base"
            }),
        ),
        &KindCatalogue::default(),
    );

    assert_eq!(
        line.detail, "next to doing; task_type: build",
        "a server-fired kind shows its known fields and no other"
    );
    assert_eq!(line.subject_label, "A-1");
    assert_eq!(
        line.subject_href.as_deref(),
        Some("/vault/tasks/a-1"),
        "a task opens on its own page, by its key, wherever it is filed"
    );
    assert_eq!(line.time, "2026-10-28 03:15 UTC");
}

#[test]
fn a_first_transition_reads_as_now_and_a_subject_without_an_id_is_named_by_its_file() {
    let line = EventLine::new(
        &urls(),
        &event(
            "member.state-changed",
            "info",
            1,
            json!({"to": "idle", "subject": "[[registry/agents/ellis|Ellis]]"}),
        ),
        &KindCatalogue::default(),
    );

    assert_eq!(line.detail, "now idle");
    assert_eq!(line.subject_label, "ellis");
    assert_eq!(line.subject_href.as_deref(), Some("/vault/registry/agents/ellis.md"));
}

#[test]
fn a_subject_that_is_not_a_markdown_note_keeps_its_own_extension() {
    let line = EventLine::new(
        &urls(),
        &event(
            "job.suspended",
            "warning",
            1,
            json!({"subject": "[[registry/workflows/ops/sync/jobs/nightly.toml|nightly]]"}),
        ),
        &KindCatalogue::default(),
    );

    assert_eq!(
        line.subject_href.as_deref(),
        Some("/vault/registry/workflows/ops/sync/jobs/nightly.toml")
    );
}

fn declaring(kind: &str, fields: &[&str]) -> KindCatalogue {
    let mut catalogue = KindCatalogue::default();
    catalogue.declare(kind, kind, fields.iter().map(|field| (*field).to_owned()).collect());
    catalogue
}

#[test]
fn a_recorded_kind_shows_the_fields_its_definition_declares_in_that_order_and_no_others() {
    let event = event(
        "booking.made",
        "info",
        1,
        json!({"room": "A", "guest": "Sam", "code": "never-declared", "reason": "never-declared-either"}),
    );

    let declared = EventLine::new(&urls(), &event, &declaring("booking.made", &["guest", "room"]));
    let undeclared = EventLine::new(&urls(), &event, &KindCatalogue::default());

    assert_eq!(declared.detail, "guest: Sam; room: A");
    assert_eq!(undeclared.detail, "", "a kind nothing declares shows no fields");
}

#[test]
fn a_catalogue_asks_for_the_declared_fields_of_every_kind_as_query_columns_once_each() {
    let mut catalogue = declaring("booking.made", &["guest", "room", "code"]);
    catalogue.declare("sale.closed", "Sale", vec!["room".to_owned(), "total".to_owned()]);

    let columns = catalogue.columns();

    for wanted in ["file.path", "kind", "guest", "room", "total"] {
        assert_eq!(columns.iter().filter(|column| *column == wanted).count(), 1, "{wanted}");
    }
}

#[test]
fn a_rendered_event_escapes_whatever_the_note_holds() {
    let html = render_event(
        &urls(),
        &event(
            "import.completed",
            "warning",
            1,
            json!({"reason": "<script>alert(1)</script>", "subject": "[[a\"onmouseover=\"x|y]]"}),
        ),
        &declaring("import.completed", &["reason"]),
    );

    assert!(!html.contains("<script>"), "{html}");
    assert!(html.contains("&#60;script&#62;"), "{html}");
    assert!(!html.contains("\"onmouseover=\""), "{html}");
    assert!(
        html.contains("colour-amber") && html.contains("data-severity=\"warning\""),
        "{html}"
    );
}

#[test]
fn background_groups_keep_each_kinds_newest_few_with_the_most_recent_kind_first() {
    let mut events: Vec<SignalEvent> = (0..8)
        .map(|n| event("import.completed", "info", 10 + n, json!({})))
        .collect();
    events.push(event("trade.executed", "info", 30, json!({})));
    events.sort_by_key(|event| std::cmp::Reverse(event.cursor()));

    let view = BackgroundView::new(urls(), &events, &KindCatalogue::default(), None);

    assert_eq!(view.groups.len(), 2);
    assert_eq!(view.groups[0].kind, "trade.executed");
    assert_eq!(view.groups[1].kind, "import.completed");
    assert_eq!(view.groups[1].total, 8);
    assert_eq!(view.groups[1].lines.len(), BACKGROUND_PER_KIND);
}

#[test]
fn an_empty_region_renders_its_empty_state_and_never_placeholder_content() {
    let background = render_background(&BackgroundView::new(urls(), &[], &KindCatalogue::default(), None));
    let attention = render_attention(&AttentionView {
        urls: urls(),
        refresh_href: urls().signals_region("attention", ""),
        lines: Vec::new(),
        entity: None,
        more: 0,
        unreadable: false,
    });

    assert!(background.contains("No background activity has been recorded."));
    assert!(
        attention.contains("Nothing needs attention.")
            && attention.contains("hx-trigger=\"sse:attention, sse:reload\"")
    );
}

#[test]
fn each_severity_has_its_own_chip_colour_and_the_set_is_the_events_crates() {
    let colours: Vec<&str> = contextos_events::Severity::ALL
        .into_iter()
        .map(|severity| SeverityColour::from(severity).0)
        .collect();

    assert_eq!(colours, ["colour-grey", "colour-amber", "colour-red"]);
}

#[test]
fn a_record_of_another_base_opens_where_it_was_when_the_event_happened() {
    let line = EventLine::new(
        &urls(),
        &event(
            "review.flagged",
            "warning",
            1,
            json!({
                "subject": "[[d-021|D-021]]", "subject_id": "D-021",
                "subject_path": "registry/decisions/d-021.md", "subject_base": "registry/decisions.base"
            }),
        ),
        &KindCatalogue::default(),
    );

    assert_eq!(line.subject_label, "D-021");
    assert_eq!(line.subject_href.as_deref(), Some("/vault/registry/decisions/d-021.md"));
}
