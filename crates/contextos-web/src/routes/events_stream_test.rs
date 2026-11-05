use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde_json::json;
use time::OffsetDateTime;
use time::macros::datetime;

use super::*;
use crate::events_hub::{EventRead, EventSource, EventSources};
use crate::signals_event::{EventRow, SignalEvent};

const INTERVAL: Duration = Duration::from_secs(5);

struct FixedClock;

impl contextos_workflows::Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        datetime!(2026-10-28 03:00:00 UTC)
    }
}

/// A vault that is always unreachable, so the reader sends only what the test provokes.
struct Silent;

impl EventSource for Silent {
    fn events_from<'a>(
        &'a self,
        _from: OffsetDateTime,
        _now: OffsetDateTime,
        _limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<EventRead, crate::signals_query::ReadError>> + Send + 'a>> {
        Box::pin(async { Ok(EventRead::default()) })
    }

    fn newest_moment(
        &self,
        _now: OffsetDateTime,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Cursor>, crate::signals_query::ReadError>> + Send + '_>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}

struct Sources;

impl EventSources for Sources {
    fn for_vault(&self, _vault: &str) -> Option<Arc<dyn EventSource>> {
        Some(Arc::new(Silent))
    }
}

fn hub() -> EventHub {
    EventHub::new(
        Arc::new(Sources),
        Arc::new(FixedClock),
        Arc::new(|_, _, _| String::new()),
    )
}

fn live(second: u32, entity: &str, changes_attention: bool) -> Arc<LiveEvent> {
    let row: EventRow = serde_json::from_value(json!({
        "file.path": format!("memory/events/2026/10/e-{second:03}.md"),
        "kind": "task.created",
        "severity": "info",
        "at": format!("2026-10-28T03:00:{second:02}Z"),
        "entity": entity,
    }))
    .unwrap_or_else(|_| unreachable!("a valid row"));
    let event = SignalEvent::try_from(row).unwrap_or_else(|_| unreachable!("a valid event"));
    Arc::new(LiveEvent {
        cursor: event.cursor(),
        entity: entity.to_owned(),
        kind: event.kind,
        html: format!("<li>{second}</li>"),
        changes_attention,
    })
}

async fn state(entity: Option<&str>, seen: Option<Cursor>) -> StreamState {
    let Some(subscription) = hub().subscribe("vault", INTERVAL).await else {
        unreachable!("the vault is served")
    };
    StreamState {
        connected: subscription.connected,
        subscription,
        pending: VecDeque::new(),
        delivered: seen.map_or_else(Delivered::default, |cursor| {
            let mut delivered = Delivered::from(cursor.at());
            delivered.record(&cursor);
            delivered
        }),
        entity: entity.map(str::to_owned),
    }
}

fn describe(outgoing: &Outgoing) -> String {
    match outgoing {
        Outgoing::Status(connected) => format!("status {connected}"),
        Outgoing::Signal(live) => format!("signal {}", live.html),
        Outgoing::Attention => "attention".to_owned(),
        Outgoing::Reload => "reload".to_owned(),
        Outgoing::KeepAlive => "keep-alive".to_owned(),
    }
}

fn drain(state: &mut StreamState) -> Vec<String> {
    state.pending.drain(..).map(|outgoing| describe(&outgoing)).collect()
}

#[tokio::test(start_paused = true)]
async fn an_event_is_queued_and_one_attention_message_is_queued_for_all_that_may_change_attention() {
    let mut open = state(None, None).await;

    open.queue(&live(1, "acme", false));
    open.queue(&live(2, "acme", true));
    open.queue(&live(3, "acme", true));
    open.queue_attention(&["acme".to_owned(), "acme".to_owned()]);
    open.queue_attention(&["acme".to_owned()]);

    assert_eq!(
        drain(&mut open),
        vec![
            "signal <li>1</li>",
            "signal <li>2</li>",
            "signal <li>3</li>",
            "attention"
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn an_event_already_sent_is_not_sent_again_but_one_in_the_same_second_with_a_lower_path_is() {
    let mut open = state(None, Some(live(5, "acme", false).cursor.clone())).await;

    open.queue(&live(4, "acme", false));
    open.queue(&live(5, "acme", false));
    open.queue(&live(6, "acme", false));
    open.queue(&live(6, "acme", false));
    let mut lower = live(6, "acme", false);
    if let Some(lower) = Arc::get_mut(&mut lower) {
        lower.cursor = Cursor::new(lower.cursor.at(), "memory/events/2026/10/a-lower.md");
        lower.html = "<li>lower</li>".to_owned();
    }
    open.queue(&lower);

    assert_eq!(drain(&mut open), vec!["signal <li>6</li>", "signal <li>lower</li>"]);
}

#[tokio::test(start_paused = true)]
async fn an_event_of_another_entity_is_skipped_but_still_counts_as_seen() {
    let mut open = state(Some("acme"), None).await;

    open.queue(&live(1, "other", true));
    open.queue(&live(2, "acme", false));
    open.queue(&live(1, "acme", false));
    open.queue(&live(3, "ACME", false));

    assert_eq!(
        drain(&mut open),
        vec!["signal <li>2</li>"],
        "the entity is matched exactly, as stored, as the Signals screen does"
    );
}

#[tokio::test(start_paused = true)]
async fn attention_is_queued_only_when_an_event_of_the_selected_entity_may_change_it() {
    let mut open = state(Some("acme"), None).await;

    open.queue_attention(&["other".to_owned()]);
    assert_eq!(drain(&mut open), Vec::<String>::new());
    open.queue_attention(&["other".to_owned(), "acme".to_owned()]);

    assert_eq!(drain(&mut open), vec!["attention"]);
}

#[test]
fn a_reload_message_is_its_own_event_name() {
    let text = format!("{:?}", Event::from(Outgoing::Reload));

    assert!(text.contains("reload"), "{text}");
}

#[tokio::test(start_paused = true)]
async fn a_stream_with_no_entity_sends_every_entitys_events() {
    let mut open = state(None, None).await;

    open.queue(&live(1, "other", false));
    open.queue(&live(2, "acme", false));

    assert_eq!(drain(&mut open), vec!["signal <li>1</li>", "signal <li>2</li>"]);
}

#[test]
fn a_status_message_is_the_shell_panels_own_fragment_on_one_line() {
    let connected = StreamStateTemplate { connected: true }
        .render()
        .unwrap_or_default()
        .replace('\n', "");
    let down = StreamStateTemplate { connected: false }
        .render()
        .unwrap_or_default()
        .replace('\n', "");

    assert!(connected.contains("data-state=\"connected\"") && connected.contains("sse-swap=\"status\""));
    assert!(down.contains("data-state=\"disconnected\""));
}

#[test]
fn a_cursor_in_the_last_event_id_header_wins_over_the_pages_after_parameter() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "last-event-id",
        HeaderValue::from_static("2026-10-28T03:00:09Z|memory/events/b.md"),
    );
    let query = StreamQuery {
        entity: None,
        after: Some("2026-10-28T03:00:01Z|memory/events/a.md".to_owned()),
    };

    assert_eq!(
        resume_cursor(&headers, &query)
            .map(|cursor| cursor.to_string())
            .as_deref(),
        Some("2026-10-28T03:00:09Z|memory/events/b.md")
    );
    assert_eq!(
        resume_cursor(&HeaderMap::new(), &query)
            .map(|cursor| cursor.to_string())
            .as_deref(),
        Some("2026-10-28T03:00:01Z|memory/events/a.md")
    );
    headers.insert("last-event-id", HeaderValue::from_static("garbage"));
    assert_eq!(
        resume_cursor(&headers, &query)
            .map(|cursor| cursor.to_string())
            .as_deref(),
        Some("2026-10-28T03:00:01Z|memory/events/a.md"),
        "a header that is not a cursor falls back to the page's own"
    );
}

#[test]
fn only_a_request_that_accepts_an_event_stream_is_served() {
    let accept = |value: Option<&'static str>| {
        let mut headers = HeaderMap::new();
        if let Some(value) = value {
            headers.insert(header::ACCEPT, HeaderValue::from_static(value));
        }
        accepts_event_stream(&headers)
    };

    assert!(accept(None) && accept(Some("text/event-stream")) && accept(Some("*/*")));
    assert!(!accept(Some("text/html")) && !accept(Some("application/json")));
}

#[tokio::test(start_paused = true)]
async fn a_quiet_connected_stream_sends_a_comment_line_at_least_every_fifteen_seconds() {
    use futures_util::StreamExt as _;

    let config = tempfile::tempdir().unwrap_or_else(|_| unreachable!("a temporary directory"));
    // A session that cannot start still gives the set a primary server, which is all the vault check needs here.
    let entry = crate::McpServerConfig::Stdio {
        name: "contextos".to_owned(),
        command: "/definitely/not/a/contextos".to_owned(),
        args: Vec::new(),
    };
    let store = Arc::new(crate::credential_store::FsCredentialStore::new(
        config.path().join("credentials"),
    ));
    let clock: Arc<dyn crate::logging::Clock> = Arc::new(crate::logging::SystemClock);
    let clients = crate::McpClientSet::start_until_shutdown(
        &[entry],
        &store,
        &clock,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap_or_else(|_| unreachable!("a lenient start keeps a server that cannot start"));
    let state = EventStreamRoutesState {
        clients: Arc::new(clients),
        web_config_path: Arc::new(config.path().join("web.toml")),
        hub: hub(),
    };

    let response = stream(
        State(state),
        Path("vault".to_owned()),
        Query(StreamQuery::default()),
        HeaderMap::new(),
    )
    .await;
    let mut body = response.into_body().into_data_stream();
    let first = body.next().await;
    assert!(
        first
            .as_ref()
            .is_some_and(|chunk| chunk.as_ref().is_ok_and(|bytes| bytes.starts_with(b"event: status"))),
        "{first:?}"
    );

    let before = tokio::time::timeout(Duration::from_secs(14), body.next()).await;
    assert!(
        before.is_err(),
        "nothing is sent while the stream is quiet and inside fifteen seconds"
    );
    let after = tokio::time::timeout(Duration::from_secs(2), body.next()).await;

    assert!(
        after
            .ok()
            .flatten()
            .is_some_and(|chunk| chunk.is_ok_and(|bytes| bytes.starts_with(b":"))),
        "a comment line keeps proxies from closing the connection"
    );
}
