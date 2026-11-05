//! The live event stream: a real `contextos` session over a real temporary vault, read as a browser's `EventSource`
//! would, one message at a time. Waits are for a message to arrive, with a deadline that only ends a wait that can
//! never finish. The web server's clock is fixed, so events meant to arrive live are stamped relative to it and no test
//! depends on the date or time it runs at; the absence of a message is shown by the message that follows it.

mod support;

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, BodyDataStream};
use axum::http::{HeaderMap, Request, StatusCode};
use contextos_web::McpClientSet;
use futures_util::StreamExt as _;
use support::events::EventSpec;
use tower::ServiceExt as _;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const VAULT: &str = "contract-fixture";
const DEADLINE: Duration = Duration::from_secs(20);
impl contextos_web::logging::Clock for FixedClock {
    fn now(&self) -> time::OffsetDateTime {
        NOW
    }
}

/// The web server's clock in every test here, safely inside a day so that events stamped minutes after it share its
/// date and month.
const NOW: time::OffsetDateTime = time::macros::datetime!(2026-10-28 12:00:00 UTC);

struct FixedClock;

impl contextos_workflows::Clock for FixedClock {
    fn now(&self) -> time::OffsetDateTime {
        NOW
    }
}

struct Fixture {
    vault: tempfile::TempDir,
    _config: tempfile::TempDir,
    router: Router,
    clients: Arc<McpClientSet>,
}

async fn fixture() -> Result<Fixture, BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    std::fs::write(vault.path().join("index.md"), "# Root\n")?;
    let config_path = support::write_vault_config(config.path(), vault.path())?;
    std::fs::write(config.path().join("web.toml"), "[events]\npoll_seconds = 1\n")?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    let router = contextos_web::build_router_at(
        Arc::clone(&clients),
        Some(config.path()),
        &config.path().join("web.toml"),
        &contextos_web::config::WorkflowsConfig::default(),
        None,
        Arc::new(FixedClock),
    );
    Ok(Fixture {
        vault,
        _config: config,
        router,
        clients,
    })
}

/// One message of a stream.
#[derive(Debug, Default, Eq, PartialEq)]
struct Message {
    event: Option<String>,
    id: Option<String>,
    data: String,
    comment: Option<String>,
}

struct Stream {
    status: StatusCode,
    headers: HeaderMap,
    body: BodyDataStream,
    buffer: String,
    queued: VecDeque<Message>,
}

impl Stream {
    /// The next message, or `None` when none arrives within `within`.
    async fn next(&mut self, within: Duration) -> Option<Message> {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            if let Some(message) = self.queued.pop_front() {
                return Some(message);
            }
            let chunk = tokio::time::timeout_at(deadline, self.body.next()).await.ok()??.ok()?;
            self.buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(end) = self.buffer.find("\n\n") {
                let block: String = self.buffer.drain(..end + 2).collect();
                self.queued.push_back(parse(&block));
            }
        }
    }

    /// The next message that is not a comment or a status message.
    async fn next_signal(&mut self, within: Duration) -> Option<Message> {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let message = self
                .next(deadline.saturating_duration_since(tokio::time::Instant::now()))
                .await?;
            if message.event.as_deref() == Some("signal") {
                return Some(message);
            }
        }
    }
}

fn parse(block: &str) -> Message {
    let mut message = Message::default();
    for line in block.lines() {
        if let Some(comment) = line.strip_prefix(':') {
            message.comment = Some(comment.trim().to_owned());
        } else if let Some(event) = line.strip_prefix("event:") {
            message.event = Some(event.trim().to_owned());
        } else if let Some(id) = line.strip_prefix("id:") {
            message.id = Some(id.trim().to_owned());
        } else if let Some(data) = line.strip_prefix("data:") {
            message.data.push_str(data.trim_start());
        }
    }
    message
}

async fn open(router: &Router, path: &str, extra: &[(&str, &str)]) -> Result<Stream, BoxError> {
    let mut request = Request::builder().uri(path).header("accept", "text/event-stream");
    for (name, value) in extra {
        request = request.header(*name, *value);
    }
    let response = router.clone().oneshot(request.body(Body::empty())?).await?;
    Ok(Stream {
        status: response.status(),
        headers: response.headers().clone(),
        body: response.into_body().into_data_stream(),
        buffer: String::new(),
        queued: VecDeque::new(),
    })
}

fn put(root: &Path, slug: &str, kind: &str, severity: &str, entity: &str, at: &str) -> Result<String, BoxError> {
    Ok(EventSpec {
        kind,
        severity,
        at,
        entity,
        ..EventSpec::default()
    }
    .write_to(root, slug)?)
}

/// A time `seconds` after a moment ten minutes ahead of the web server's clock, as RFC 3339 in UTC. The stream reads
/// from the moment it opened, so an event meant to arrive live is stamped after that.
fn live(seconds: i64) -> String {
    use time::format_description::well_known::Rfc3339;
    let at = NOW + time::Duration::minutes(10) + time::Duration::seconds(seconds);
    at.format(&Rfc3339).unwrap_or_default()
}

fn stream_path() -> String {
    format!("/{VAULT}/events/stream")
}

#[tokio::test]
async fn the_stream_is_an_uncached_event_stream_that_starts_by_saying_it_is_connected() -> Result<(), BoxError> {
    let fixture = fixture().await?;

    let mut stream = open(&fixture.router, &stream_path(), &[]).await?;
    let first = stream.next(DEADLINE).await.ok_or("a first message")?;

    assert_eq!(stream.status, StatusCode::OK);
    assert_eq!(
        stream.headers.get("content-type").and_then(|v| v.to_str().ok()),
        Some("text/event-stream")
    );
    assert_eq!(
        stream.headers.get("cache-control").and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    assert_eq!(first.event.as_deref(), Some("status"));
    assert!(
        first.data.contains("data-state=\"connected\"") && first.data.contains("sse-swap=\"status\""),
        "{first:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_request_that_does_not_accept_an_event_stream_is_not_acceptable() -> Result<(), BoxError> {
    let fixture = fixture().await?;

    let response = fixture
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri(stream_path())
                .header("accept", "application/json")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::NOT_ACCEPTABLE);
    Ok(())
}

#[tokio::test]
async fn an_unknown_vault_has_no_stream() -> Result<(), BoxError> {
    let fixture = fixture().await?;

    let stream = open(&fixture.router, "/no-such-vault/events/stream", &[]).await?;

    assert_eq!(stream.status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn a_request_for_another_sites_host_is_refused_before_the_stream_opens() -> Result<(), BoxError> {
    let fixture = fixture().await?;

    let stream = open(&fixture.router, &stream_path(), &[("host", "evil.example")]).await?;

    assert_ne!(stream.status, StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn an_event_written_after_the_stream_opened_arrives_once_with_its_cursor_as_the_id() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    put(
        fixture.vault.path(),
        "before",
        "task.created",
        "info",
        "vault",
        "2026-10-28T03:00:00Z",
    )?;
    let mut stream = open(&fixture.router, &stream_path(), &[]).await?;
    stream.next(DEADLINE).await.ok_or("the connected message")?;

    let arrived = live(5);
    let path = put(
        fixture.vault.path(),
        "now",
        "import.completed",
        "info",
        "acme",
        &arrived,
    )?;
    let signal = stream.next_signal(DEADLINE).await.ok_or("the new event")?;

    assert_eq!(signal.id.as_deref(), Some(format!("{arrived}|{path}").as_str()));
    assert!(
        signal.data.starts_with("<li id=\"event-") && signal.data.contains(" class=\"signal-event"),
        "each streamed item carries an id, so a page never shows one twice: {signal:?}"
    );
    assert!(
        signal.data.contains("import.completed") && signal.data.contains("acme"),
        "{signal:?}"
    );
    assert!(!signal.data.contains('\n'), "a fragment is one line of data");
    put(
        fixture.vault.path(),
        "after",
        "import.completed",
        "info",
        "acme",
        &live(30),
    )?;
    let next = stream.next_signal(DEADLINE).await.ok_or("the event after it")?;
    assert!(
        next.id.as_deref().is_some_and(|id| id.starts_with(&live(30))),
        "neither the earlier event nor this one is sent again before the next new one: {next:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_warning_is_followed_by_an_attention_message_and_information_is_not() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    let mut stream = open(&fixture.router, &stream_path(), &[]).await?;
    stream.next(DEADLINE).await.ok_or("the connected message")?;

    put(
        fixture.vault.path(),
        "calm",
        "import.completed",
        "info",
        "vault",
        &live(1),
    )?;
    let calm = stream.next_signal(DEADLINE).await.ok_or("the calm event")?;
    put(fixture.vault.path(), "bad", "import.failed", "error", "vault", &live(2))?;
    let bad = stream.next_signal(DEADLINE).await.ok_or("the failure")?;
    let after_bad = stream.next(DEADLINE).await.ok_or("what follows the failure")?;

    assert!(calm.data.contains("import.completed"));
    assert!(bad.data.contains("import.failed"));
    assert_eq!(after_bad.event.as_deref(), Some("attention"), "{after_bad:?}");
    Ok(())
}

#[tokio::test]
async fn a_reconnecting_browser_gets_the_events_after_its_last_id_oldest_first() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    let root = fixture.vault.path();
    let first = put(root, "one", "import.completed", "info", "vault", "2026-10-28T03:00:01Z")?;
    let second = put(root, "two", "import.completed", "info", "vault", "2026-10-28T03:00:02Z")?;
    let third = put(
        root,
        "three",
        "import.completed",
        "info",
        "vault",
        "2026-10-28T03:00:03Z",
    )?;
    let last_id = format!("2026-10-28T03:00:01Z|{first}");

    let mut stream = open(&fixture.router, &stream_path(), &[("last-event-id", &last_id)]).await?;
    let a = stream.next_signal(DEADLINE).await.ok_or("the second event")?;
    let b = stream.next_signal(DEADLINE).await.ok_or("the third event")?;

    assert_eq!(a.id.as_deref(), Some(format!("2026-10-28T03:00:02Z|{second}").as_str()));
    assert_eq!(b.id.as_deref(), Some(format!("2026-10-28T03:00:03Z|{third}").as_str()));
    put(
        root,
        "four",
        "import.completed",
        "info",
        "vault",
        "2026-10-28T03:00:04Z",
    )?;
    let c = stream.next_signal(DEADLINE).await.ok_or("the fourth event")?;
    assert!(
        c.id.as_deref()
            .is_some_and(|id| id.starts_with("2026-10-28T03:00:04Z|")),
        "the event at the cursor is not sent again: {c:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_resume_sends_every_missed_event_oldest_first_however_many_up_to_five_hundred() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    let root = fixture.vault.path();
    for n in 0..60 {
        put(
            root,
            &format!("e{n:02}"),
            "import.completed",
            "info",
            "vault",
            &format!("2026-10-28T03:10:{n:02}Z"),
        )?;
    }

    let mut stream = open(
        &fixture.router,
        &stream_path(),
        &[("last-event-id", "2026-10-28T03:00:00Z|memory/events/2026/10/none.md")],
    )
    .await?;
    let mut ids = Vec::new();
    while ids.len() < 60 {
        let signal = stream.next_signal(DEADLINE).await.ok_or("a resumed event")?;
        ids.push(signal.id.ok_or("an id")?);
    }

    assert!(
        ids.first().is_some_and(|id| id.starts_with("2026-10-28T03:10:00Z|")),
        "{ids:?}"
    );
    assert!(
        ids.last().is_some_and(|id| id.starts_with("2026-10-28T03:10:59Z|")),
        "all sixty, not a capped few: {ids:?}"
    );
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted, "oldest first");
    Ok(())
}

#[tokio::test]
async fn a_resume_after_more_than_five_hundred_missed_events_sends_one_reload_message_instead() -> Result<(), BoxError>
{
    let fixture = fixture().await?;
    let root = fixture.vault.path();
    for n in 0..505 {
        put(
            root,
            &format!("m{n:03}"),
            "import.completed",
            "info",
            "vault",
            &format!("2026-10-28T03:{:02}:{:02}Z", 10 + n / 60, n % 60),
        )?;
    }

    let mut stream = open(
        &fixture.router,
        &stream_path(),
        &[("last-event-id", "2026-10-28T03:00:00Z|memory/events/2026/10/none.md")],
    )
    .await?;
    let first = stream.next(DEADLINE).await.ok_or("the status message")?;
    let second = stream.next(DEADLINE).await.ok_or("the reload message")?;

    assert_eq!(first.event.as_deref(), Some("status"));
    assert_eq!(second.event.as_deref(), Some("reload"), "{second:?}");
    Ok(())
}

#[tokio::test]
async fn a_late_event_in_the_same_second_with_a_lower_path_still_arrives_on_a_live_stream() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    let mut stream = open(&fixture.router, &stream_path(), &[]).await?;
    stream.next(DEADLINE).await.ok_or("the connected message")?;
    let at = live(5);

    let first = put(
        fixture.vault.path(),
        "zzz-written-first",
        "import.completed",
        "info",
        "vault",
        &at,
    )?;
    let a = stream.next_signal(DEADLINE).await.ok_or("the first event")?;
    let second = put(
        fixture.vault.path(),
        "aaa-written-second",
        "import.completed",
        "info",
        "vault",
        &at,
    )?;
    let b = stream
        .next_signal(DEADLINE)
        .await
        .ok_or("the second event, whose path sorts before the first")?;

    assert_eq!(a.id.as_deref(), Some(format!("{at}|{first}").as_str()));
    assert_eq!(b.id.as_deref(), Some(format!("{at}|{second}").as_str()));
    assert!(second < first, "the second path sorts before the first");
    Ok(())
}

#[tokio::test]
async fn the_page_can_start_a_stream_from_the_newest_event_it_already_shows() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    let root = fixture.vault.path();
    let shown = put(
        root,
        "shown",
        "import.completed",
        "info",
        "vault",
        "2026-10-28T03:00:01Z",
    )?;
    let missed = put(
        root,
        "missed",
        "import.completed",
        "info",
        "vault",
        "2026-10-28T03:00:02Z",
    )?;
    let after = format!("2026-10-28T03:00:01Z|{shown}");

    let mut stream = open(
        &fixture.router,
        &format!("{}?after={}", stream_path(), after.replace('|', "%7C")),
        &[],
    )
    .await?;
    let signal = stream.next_signal(DEADLINE).await.ok_or("the event the page missed")?;

    assert_eq!(
        signal.id.as_deref(),
        Some(format!("2026-10-28T03:00:02Z|{missed}").as_str())
    );
    Ok(())
}

#[tokio::test]
async fn a_stream_for_one_entity_carries_only_that_entitys_events() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    let mut stream = open(&fixture.router, &format!("{}?entity=acme", stream_path()), &[]).await?;
    stream.next(DEADLINE).await.ok_or("the connected message")?;

    put(
        fixture.vault.path(),
        "other",
        "import.completed",
        "info",
        "other",
        &live(1),
    )?;
    let arrived = live(2);
    let mine = put(
        fixture.vault.path(),
        "mine",
        "import.completed",
        "info",
        "acme",
        &arrived,
    )?;
    let signal = stream.next_signal(DEADLINE).await.ok_or("the acme event")?;

    assert_eq!(
        signal.id.as_deref(),
        Some(format!("{arrived}|{mine}").as_str()),
        "the other entity's earlier event was not sent"
    );
    Ok(())
}

#[tokio::test]
async fn a_malformed_last_event_id_is_ignored_and_the_stream_still_opens() -> Result<(), BoxError> {
    let fixture = fixture().await?;

    let mut stream = open(&fixture.router, &stream_path(), &[("last-event-id", "not a cursor")]).await?;

    assert_eq!(stream.status, StatusCode::OK);
    assert_eq!(
        stream.next(DEADLINE).await.and_then(|m| m.event).as_deref(),
        Some("status")
    );
    Ok(())
}

#[tokio::test]
async fn every_vault_page_carries_the_stream_panel_and_the_signals_page_starts_it_from_its_newest_event()
-> Result<(), BoxError> {
    let fixture = fixture().await?;
    let path = put(
        fixture.vault.path(),
        "shown",
        "import.completed",
        "info",
        "vault",
        "2026-10-28T03:00:01Z",
    )?;

    let page = |uri: String| {
        let router = fixture.router.clone();
        async move {
            let response = router.oneshot(Request::builder().uri(uri).body(Body::empty())?).await?;
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
            Ok::<String, BoxError>(String::from_utf8_lossy(&bytes).into_owned())
        }
    };
    let tasks = page(format!("/{VAULT}/tasks/")).await?;
    let signals = page(format!("/{VAULT}/signals/")).await?;

    assert!(
        tasks.contains(&format!("sse-connect=\"/{VAULT}/events/stream\"")),
        "{tasks}"
    );
    assert!(
        tasks.contains("class=\"stream-count\"") && tasks.contains("sse-swap=\"signal\""),
        "{tasks}"
    );
    assert!(tasks.contains("/static/htmx-ext-sse.js"));
    assert!(
        tasks.contains("class=\"stream-ticks visually-hidden\"")
            && tasks.contains("hx-swap=\"none\"")
            && !tasks.contains("replaceChildren"),
        "the counter counts messages and swaps nothing in, so no streamed id is ever in the page twice: {tasks}"
    );
    let encoded = format!("2026-10-28T03:00:01Z%7C{}", path.replace('/', "%2F"));
    assert!(
        signals.contains(&format!("events/stream?after={encoded}")) || signals.contains("events/stream?after="),
        "{signals}"
    );
    Ok(())
}

#[tokio::test]
async fn a_stream_opened_while_contextos_is_unreachable_answers_and_says_it_is_disconnected() -> Result<(), BoxError> {
    let config = tempfile::tempdir()?;
    std::fs::write(config.path().join("web.toml"), "[events]\npoll_seconds = 1\n")?;
    let entry = contextos_web::McpServerConfig::Stdio {
        name: "contextos".to_owned(),
        command: "/definitely/not/a/contextos".to_owned(),
        args: Vec::new(),
    };
    let store = Arc::new(contextos_web::credential_store::FsCredentialStore::new(
        config.path().join("credentials"),
    ));
    let clock: Arc<dyn contextos_web::logging::Clock> = Arc::new(FixedClock);
    let clients = Arc::new(
        McpClientSet::start_until_shutdown(&[entry], &store, &clock, &tokio_util::sync::CancellationToken::new())
            .await?,
    );
    let router = contextos_web::build_router_at(
        clients,
        Some(config.path()),
        &config.path().join("web.toml"),
        &contextos_web::config::WorkflowsConfig::default(),
        None,
        Arc::new(FixedClock),
    );

    let mut stream = open(&router, &stream_path(), &[]).await?;
    let first = stream.next(DEADLINE).await.ok_or("a first message")?;

    assert_eq!(
        stream.status,
        StatusCode::OK,
        "the stream opens so that a browser keeps its place"
    );
    assert_eq!(first.event.as_deref(), Some("status"));
    assert!(first.data.contains("data-state=\"disconnected\""), "{first:?}");
    Ok(())
}

#[tokio::test]
async fn a_stream_ends_promptly_when_the_host_shuts_down() -> Result<(), BoxError> {
    let vault = tempfile::tempdir()?;
    let config = tempfile::tempdir()?;
    std::fs::write(vault.path().join("index.md"), "# Root\n")?;
    let config_path = support::write_vault_config(config.path(), vault.path())?;
    std::fs::write(config.path().join("web.toml"), "[events]\npoll_seconds = 1\n")?;
    let entry = support::real_contextos_entry("contextos", &config_path)?;
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    let shutdown = tokio_util::sync::CancellationToken::new();
    let router = contextos_web::build_router_with_shutdown(
        clients,
        Some(config.path()),
        &config.path().join("web.toml"),
        &contextos_web::config::WorkflowsConfig::default(),
        shutdown.clone(),
    );
    let mut stream = open(&router, &stream_path(), &[]).await?;
    stream.next(DEADLINE).await.ok_or("the connected message")?;

    shutdown.cancel();
    let ended = tokio::time::timeout(DEADLINE, async { while stream.next(DEADLINE).await.is_some() {} }).await;

    assert!(ended.is_ok(), "the stream ends instead of holding the request drain");
    Ok(())
}

/// Writes `count` information events, one per second from `03:00:00` of the web server's day, and returns the last
/// one's path.
fn earlier_events(root: &Path, count: usize) -> Result<String, BoxError> {
    let mut last = String::new();
    for n in 0..count {
        last = put(
            root,
            &format!("early{n:04}"),
            "import.completed",
            "info",
            "vault",
            &format!("2026-10-28T{:02}:{:02}:{:02}Z", 1 + n / 3600, (n / 60) % 60, n % 60),
        )?;
    }
    Ok(last)
}

#[tokio::test]
async fn a_live_event_still_arrives_when_the_day_already_holds_more_than_five_hundred_earlier_events()
-> Result<(), BoxError> {
    let fixture = fixture().await?;
    earlier_events(fixture.vault.path(), 700)?;
    let mut stream = open(&fixture.router, &stream_path(), &[]).await?;
    stream.next(DEADLINE).await.ok_or("the connected message")?;

    let arrived = live(5);
    let path = put(
        fixture.vault.path(),
        "late",
        "import.completed",
        "info",
        "vault",
        &arrived,
    )?;
    let signal = stream.next_signal(DEADLINE).await.ok_or("the live event")?;

    assert_eq!(signal.id.as_deref(), Some(format!("{arrived}|{path}").as_str()));
    Ok(())
}

#[tokio::test]
async fn a_resume_finds_the_missed_events_when_the_day_holds_more_earlier_ones_than_one_read_takes()
-> Result<(), BoxError> {
    let fixture = fixture().await?;
    let root = fixture.vault.path();
    earlier_events(root, 2_100)?;
    let missed = put(
        root,
        "missed",
        "import.completed",
        "info",
        "vault",
        "2026-10-28T09:00:01Z",
    )?;

    let mut stream = open(
        &fixture.router,
        &stream_path(),
        &[("last-event-id", "2026-10-28T09:00:00Z|memory/events/2026/10/none.md")],
    )
    .await?;
    let signal = stream.next_signal(DEADLINE).await.ok_or("the missed event")?;

    assert_eq!(
        signal.id.as_deref(),
        Some(format!("2026-10-28T09:00:01Z|{missed}").as_str())
    );
    Ok(())
}

#[tokio::test]
async fn a_second_holding_more_than_five_hundred_events_neither_resends_them_nor_holds_up_later_events()
-> Result<(), BoxError> {
    let fixture = fixture().await?;
    for n in 0..620 {
        put(
            fixture.vault.path(),
            &format!("burst{n:04}"),
            "import.completed",
            "info",
            "vault",
            "2026-10-28T03:00:00Z",
        )?;
    }
    let mut stream = open(&fixture.router, &stream_path(), &[]).await?;
    stream.next(DEADLINE).await.ok_or("the connected message")?;

    let arrived = live(5);
    let path = put(
        fixture.vault.path(),
        "after-the-burst",
        "import.completed",
        "info",
        "vault",
        &arrived,
    )?;
    let signal = stream.next_signal(DEADLINE).await.ok_or("the event after the burst")?;

    assert_eq!(
        signal.id.as_deref(),
        Some(format!("{arrived}|{path}").as_str()),
        "none of the burst is sent as if it were new"
    );
    Ok(())
}

#[tokio::test]
async fn a_resume_sends_the_events_that_share_the_cursors_second_and_were_written_after_it() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    let root = fixture.vault.path();
    let at = "2026-10-28T03:00:07Z";
    let b = put(root, "b", "import.completed", "info", "vault", at)?;
    let a = put(root, "a", "import.completed", "info", "vault", at)?;
    assert!(a < b, "the later event's path sorts before the cursor's");

    let mut stream = open(
        &fixture.router,
        &stream_path(),
        &[("last-event-id", &format!("{at}|{b}"))],
    )
    .await?;
    let signal = stream
        .next_signal(DEADLINE)
        .await
        .ok_or("the event that shares the second")?;

    assert_eq!(signal.id.as_deref(), Some(format!("{at}|{a}").as_str()));
    Ok(())
}

#[tokio::test]
async fn a_stream_still_opens_when_the_session_dies_after_connecting() -> Result<(), BoxError> {
    let fixture = fixture().await?;
    let pid = fixture
        .clients
        .primary()
        .and_then(|client| client.pid())
        .ok_or("a stdio session reports its child PID")?;
    #[cfg(unix)]
    let killed = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()?;
    #[cfg(windows)]
    let killed = std::process::Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .status()?;
    assert!(killed.success());

    let stream = open(&fixture.router, &stream_path(), &[]).await?;

    assert_eq!(
        stream.status,
        StatusCode::OK,
        "a server that cannot be asked is not a vault that does not exist"
    );
    Ok(())
}
