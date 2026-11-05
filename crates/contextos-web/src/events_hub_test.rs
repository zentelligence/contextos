use std::sync::atomic::AtomicUsize;

use serde_json::json;
use time::macros::datetime;

use super::*;
use crate::signals_event::EventRow;

const INTERVAL: Duration = Duration::from_secs(5);
const NOW: OffsetDateTime = datetime!(2026-10-28 03:00:00 UTC);

struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        NOW
    }
}

fn event(second: u32, kind: &str, entity: &str) -> SignalEvent {
    let row: EventRow = serde_json::from_value(json!({
        "file.path": format!("memory/events/2026/10/e-{second:03}.md"),
        "kind": kind,
        "severity": "info",
        "at": format!("2026-10-28T03:{:02}:{:02}Z", second / 60, second % 60),
        "entity": entity,
    }))
    .unwrap_or_else(|_| unreachable!("a valid row"));
    SignalEvent::try_from(row).unwrap_or_else(|_| unreachable!("a valid event"))
}

fn event_in_second(second: u32, path: &str) -> SignalEvent {
    let row: EventRow = serde_json::from_value(json!({
        "file.path": path,
        "kind": "task.created",
        "severity": "info",
        "at": format!("2026-10-28T03:00:{second:02}Z"),
        "entity": "acme",
    }))
    .unwrap_or_else(|_| unreachable!("a valid row"));
    SignalEvent::try_from(row).unwrap_or_else(|_| unreachable!("a valid event"))
}

/// A vault's events held in memory, answering as the real source does: every event from the cursor's own second on
/// (`>=`, so the cursor's second is read again and the caller drops what it delivered), oldest first.
#[derive(Default)]
struct FakeSource {
    events: Mutex<Vec<SignalEvent>>,
    unreachable: AtomicBool,
    polls: AtomicUsize,
    registry_reads: AtomicUsize,
    /// Answers as a read that found more matches than its limit, whatever it returns.
    saturated: AtomicBool,
    /// How many times the reader said attention may have changed.
    attention_changes: AtomicUsize,
}

impl FakeSource {
    fn add(&self, event: SignalEvent) {
        if let Ok(mut events) = self.events.lock() {
            events.push(event);
        }
    }

    fn polls(&self) -> usize {
        self.polls.load(Ordering::SeqCst)
    }
}

impl EventSource for FakeSource {
    fn attention_changed(&self) {
        self.attention_changes.fetch_add(1, Ordering::SeqCst);
    }

    fn events_from(
        &self,
        from: OffsetDateTime,
        _now: OffsetDateTime,
        limit: usize,
    ) -> Boxed<'_, Result<EventRead, ReadError>> {
        Box::pin(async move {
            self.polls.fetch_add(1, Ordering::SeqCst);
            self.registry_reads.fetch_add(1, Ordering::SeqCst);
            if self.unreachable.load(Ordering::SeqCst) {
                return Err(ReadError::Unreachable);
            }
            let mut found: Vec<SignalEvent> = self
                .events
                .lock()
                .map(|events| events.clone())
                .unwrap_or_default()
                .into_iter()
                .filter(|event| event.at >= from)
                .collect();
            found.sort_by_key(SignalEvent::cursor);
            let truncated = found.len() > limit || self.saturated.load(Ordering::SeqCst);
            found.truncate(limit);
            Ok(EventRead {
                events: found,
                catalogue: KindCatalogue::default(),
                truncated,
            })
        })
    }

    fn newest_moment(&self, _now: OffsetDateTime) -> Boxed<'_, Result<Vec<Cursor>, ReadError>> {
        Box::pin(async move {
            if self.unreachable.load(Ordering::SeqCst) {
                return Err(ReadError::Unreachable);
            }
            let events = self.events.lock().map(|events| events.clone()).unwrap_or_default();
            let Some(newest) = events.iter().map(|event| event.at).max() else {
                return Ok(Vec::new());
            };
            let second = newest.replace_nanosecond(0).unwrap_or(newest);
            Ok(events
                .iter()
                .filter(|event| event.at >= second)
                .map(SignalEvent::cursor)
                .collect())
        })
    }
}

struct Sources(Arc<FakeSource>);

impl EventSources for Sources {
    fn for_vault(&self, vault: &str) -> Option<Arc<dyn EventSource>> {
        (vault == "vault").then(|| Arc::clone(&self.0) as Arc<dyn EventSource>)
    }
}

fn hub(source: &Arc<FakeSource>) -> EventHub {
    EventHub::new(
        Arc::new(Sources(Arc::clone(source))),
        Arc::new(FixedClock),
        Arc::new(|_, event, _| format!("<li>{}</li>", event.path)),
    )
}

/// The next delivery, or `None` when nothing arrives within `within` of paused time.
async fn next(subscription: &mut Subscription, within: Duration) -> Option<Delivery> {
    tokio::time::timeout(within, subscription.receiver.recv())
        .await
        .ok()?
        .ok()
}

fn path_of(delivery: &Delivery) -> String {
    match delivery {
        Delivery::Event(event) => event.cursor.path().to_owned(),
        Delivery::Attention(entities) => format!("attention:{}", entities.join(",")),
        Delivery::Connected => "connected".to_owned(),
        Delivery::Disconnected => "disconnected".to_owned(),
    }
}

#[tokio::test(start_paused = true)]
async fn no_reader_runs_and_nothing_is_polled_while_no_stream_is_open() {
    let source = Arc::new(FakeSource::default());
    let hub = hub(&source);

    tokio::time::sleep(INTERVAL * 10).await;

    assert_eq!(hub.active_readers(), 0);
    assert_eq!(source.polls(), 0);
}

#[tokio::test(start_paused = true)]
async fn an_unknown_vault_cannot_be_streamed() {
    let source = Arc::new(FakeSource::default());

    assert!(hub(&source).subscribe("elsewhere", INTERVAL).await.is_none());
}

#[tokio::test(start_paused = true)]
async fn the_first_stream_starts_one_reader_and_new_events_arrive_once_in_order() {
    let source = Arc::new(FakeSource::default());
    source.add(event(1, "task.created", "acme"));
    let hub = hub(&source);
    let Some(mut stream) = hub.subscribe("vault", INTERVAL).await else {
        unreachable!("the vault is served")
    };
    tokio::time::sleep(Duration::from_millis(1)).await;

    source.add(event(3, "task.created", "acme"));
    source.add(event(2, "task.created", "acme"));
    let first = next(&mut stream, INTERVAL * 2).await;
    let second = next(&mut stream, INTERVAL * 2).await;
    let third = next(&mut stream, INTERVAL * 4).await;

    assert_eq!(hub.active_readers(), 1);
    assert_eq!(
        first.as_ref().map(path_of).as_deref(),
        Some("memory/events/2026/10/e-002.md")
    );
    assert_eq!(
        second.as_ref().map(path_of).as_deref(),
        Some("memory/events/2026/10/e-003.md")
    );
    assert!(
        third.is_none(),
        "the event already in the vault when the reader started, and the two sent, are not sent again"
    );
}

#[tokio::test(start_paused = true)]
async fn a_rendered_event_carries_its_cursor_entity_and_fragment() {
    let source = Arc::new(FakeSource::default());
    let hub = hub(&source);
    let Some(mut stream) = hub.subscribe("vault", INTERVAL).await else {
        unreachable!("the vault is served")
    };
    tokio::time::sleep(Duration::from_millis(1)).await;
    source.add(event(7, "task.claim-lapsed", "acme"));

    let Some(Delivery::Event(live)) = next(&mut stream, INTERVAL * 2).await else {
        unreachable!("an event arrives")
    };

    assert_eq!(
        live.cursor.to_string(),
        "2026-10-28T03:00:07Z|memory/events/2026/10/e-007.md"
    );
    assert_eq!(live.entity, "acme");
    assert_eq!(live.html, "<li>memory/events/2026/10/e-007.md</li>");
}

#[tokio::test(start_paused = true)]
async fn two_streams_share_one_reader_and_each_gets_every_event() {
    let source = Arc::new(FakeSource::default());
    let hub = hub(&source);
    let Some(mut one) = hub.subscribe("vault", INTERVAL).await else {
        unreachable!("the vault is served")
    };
    let Some(mut two) = hub.subscribe("vault", INTERVAL).await else {
        unreachable!("the vault is served")
    };
    tokio::time::sleep(Duration::from_millis(1)).await;
    let polls_before = source.polls();
    source.add(event(4, "task.created", "acme"));

    let a = next(&mut one, INTERVAL * 2).await;
    let b = next(&mut two, INTERVAL * 2).await;

    assert_eq!(hub.active_readers(), 1);
    assert_eq!(a.as_ref().map(path_of), b.as_ref().map(path_of));
    assert!(a.is_some());
    assert!(
        source.polls() <= polls_before + 2,
        "one reader polls, not one per stream: {}",
        source.polls()
    );
}

#[tokio::test(start_paused = true)]
async fn the_reader_stops_within_an_interval_of_the_last_stream_closing_and_polls_no_more() {
    let source = Arc::new(FakeSource::default());
    let hub = hub(&source);
    let first = hub.subscribe("vault", INTERVAL).await;
    let second = hub.subscribe("vault", INTERVAL).await;
    tokio::time::sleep(INTERVAL * 2).await;

    drop(first);
    tokio::time::sleep(INTERVAL * 2).await;
    assert_eq!(hub.active_readers(), 1, "a stream is still open");

    drop(second);
    tokio::time::sleep(INTERVAL).await;
    let polls_at_stop = source.polls();
    tokio::time::sleep(INTERVAL * 10).await;

    assert_eq!(hub.active_readers(), 0);
    assert_eq!(
        source.polls(),
        polls_at_stop,
        "nothing is polled once no stream is open"
    );
}

#[tokio::test(start_paused = true)]
async fn a_new_stream_after_the_reader_stopped_starts_from_the_newest_event_with_no_replay() {
    let source = Arc::new(FakeSource::default());
    let hub = hub(&source);
    drop(hub.subscribe("vault", INTERVAL).await);
    tokio::time::sleep(INTERVAL * 3).await;
    source.add(event(1, "task.created", "acme"));

    let Some(mut stream) = hub.subscribe("vault", INTERVAL).await else {
        unreachable!("the vault is served")
    };
    let replayed = next(&mut stream, INTERVAL * 3).await;

    assert!(
        replayed.is_none(),
        "events that happened while nobody watched are not replayed to a new stream"
    );
}

#[tokio::test(start_paused = true)]
async fn an_unreachable_server_is_reported_each_interval_and_events_flow_again_without_loss_when_it_returns() {
    let source = Arc::new(FakeSource::default());
    let hub = hub(&source);
    let Some(mut stream) = hub.subscribe("vault", INTERVAL).await else {
        unreachable!("the vault is served")
    };
    tokio::time::sleep(Duration::from_millis(1)).await;

    source.unreachable.store(true, Ordering::SeqCst);
    source.add(event(5, "task.created", "acme"));
    let down_one = next(&mut stream, INTERVAL * 2).await;
    let down_two = next(&mut stream, INTERVAL * 2).await;
    source.unreachable.store(false, Ordering::SeqCst);
    let back = next(&mut stream, INTERVAL * 2).await;
    let caught_up = next(&mut stream, INTERVAL * 2).await;

    assert_eq!(down_one.as_ref().map(path_of).as_deref(), Some("disconnected"));
    assert_eq!(down_two.as_ref().map(path_of).as_deref(), Some("disconnected"));
    assert_eq!(back.as_ref().map(path_of).as_deref(), Some("connected"));
    assert_eq!(
        caught_up.as_ref().map(path_of).as_deref(),
        Some("memory/events/2026/10/e-005.md"),
        "the event that happened during the outage is delivered once the server returns"
    );
}

#[tokio::test(start_paused = true)]
async fn a_stream_opened_during_an_outage_is_told_it_is_disconnected() {
    let source = Arc::new(FakeSource::default());
    let hub = hub(&source);
    let Some(_first) = hub.subscribe("vault", INTERVAL).await else {
        unreachable!("the vault is served")
    };
    source.unreachable.store(true, Ordering::SeqCst);
    tokio::time::sleep(INTERVAL * 2).await;

    let Some(second) = hub.subscribe("vault", INTERVAL).await else {
        unreachable!("the vault is served")
    };

    assert!(!second.connected);
}

#[tokio::test(start_paused = true)]
async fn resuming_sends_every_event_after_the_cursor_oldest_first_however_many_up_to_the_limit() {
    let source = Arc::new(FakeSource::default());
    for second in 0..80 {
        source.add(event(second, "task.created", "acme"));
    }
    let hub = hub(&source);
    let cursor = event(9, "task.created", "acme").cursor();

    let Resume::Events(resumed) = hub.resume("vault", &cursor).await else {
        unreachable!("fewer than the limit were missed")
    };

    assert_eq!(resumed.len(), 70, "all of them, not a capped few");
    assert_eq!(
        resumed.first().map(|live| live.cursor.path().to_owned()).as_deref(),
        Some("memory/events/2026/10/e-010.md")
    );
    assert_eq!(
        resumed.last().map(|live| live.cursor.path().to_owned()).as_deref(),
        Some("memory/events/2026/10/e-079.md")
    );
}

#[tokio::test(start_paused = true)]
async fn resuming_after_more_than_the_limit_were_missed_asks_the_page_to_reload_instead() {
    let source = Arc::new(FakeSource::default());
    for second in 0..=u32::try_from(RESUME_LIMIT).unwrap_or(0) + 1 {
        source.add(event(second, "task.created", "acme"));
    }
    let hub = hub(&source);

    let resumed = hub.resume("vault", &event(0, "task.created", "acme").cursor()).await;

    assert!(matches!(resumed, Resume::Reload), "{resumed:?}");
}

#[tokio::test(start_paused = true)]
async fn resuming_when_the_server_cannot_be_read_sends_nothing() {
    let source = Arc::new(FakeSource::default());
    source.add(event(1, "task.created", "acme"));
    source.unreachable.store(true, Ordering::SeqCst);
    let hub = hub(&source);

    assert!(matches!(
        hub.resume("vault", &event(0, "task.created", "acme").cursor()).await,
        Resume::Unavailable
    ));
}

#[tokio::test(start_paused = true)]
async fn a_stream_that_falls_too_far_behind_is_lagged_rather_than_buffered_without_bound() {
    let source = Arc::new(FakeSource::default());
    let hub = hub(&source);
    let Some(mut slow) = hub.subscribe("vault", INTERVAL).await else {
        unreachable!("the vault is served")
    };
    tokio::time::sleep(Duration::from_millis(1)).await;
    for second in 0..(CHANNEL_CAPACITY + 50) {
        source.add(event(u32::try_from(second).unwrap_or(0), "task.created", "acme"));
    }
    tokio::time::sleep(INTERVAL * 2).await;

    let received = slow.receiver.recv().await;

    assert!(
        matches!(received, Err(broadcast::error::RecvError::Lagged(_))),
        "{received:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn an_event_later_in_the_same_second_with_a_lower_path_is_still_delivered() {
    let source = Arc::new(FakeSource::default());
    let hub = hub(&source);
    let Some(mut stream) = hub.subscribe("vault", INTERVAL).await else {
        unreachable!("the vault is served")
    };
    tokio::time::sleep(Duration::from_millis(1)).await;
    source.add(event_in_second(5, "memory/events/2026/10/z-written-first.md"));
    let first = next(&mut stream, INTERVAL * 2).await;

    source.add(event_in_second(5, "memory/events/2026/10/a-written-second.md"));
    let second = next(&mut stream, INTERVAL * 2).await;
    let again = next(&mut stream, INTERVAL * 3).await;

    assert_eq!(
        first.as_ref().map(path_of).as_deref(),
        Some("memory/events/2026/10/z-written-first.md")
    );
    assert_eq!(
        second.as_ref().map(path_of).as_deref(),
        Some("memory/events/2026/10/a-written-second.md"),
        "a path that sorts before one already sent, in the same second, is not dropped"
    );
    assert!(again.is_none(), "and nothing is sent twice");
}

#[tokio::test(start_paused = true)]
async fn a_new_reader_does_not_replay_the_other_events_of_the_newest_events_second() {
    let source = Arc::new(FakeSource::default());
    source.add(event_in_second(5, "memory/events/2026/10/b.md"));
    source.add(event_in_second(5, "memory/events/2026/10/a.md"));
    let hub = hub(&source);

    let Some(mut stream) = hub.subscribe("vault", INTERVAL).await else {
        unreachable!("the vault is served")
    };
    let replayed = next(&mut stream, INTERVAL * 3).await;

    assert!(replayed.is_none(), "both were already there: {replayed:?}");
}

#[tokio::test(start_paused = true)]
async fn one_poll_that_finds_several_attention_events_sends_one_attention_message() {
    let source = Arc::new(FakeSource::default());
    let hub = hub(&source);
    let Some(mut stream) = hub.subscribe("vault", INTERVAL).await else {
        unreachable!("the vault is served")
    };
    tokio::time::sleep(Duration::from_millis(1)).await;
    for second in 1..=3 {
        source.add(event(second, "vault.recovered", "acme"));
    }

    let mut seen = Vec::new();
    while let Some(delivery) = next(&mut stream, INTERVAL * 2).await {
        seen.push(path_of(&delivery));
    }

    let attention = seen.iter().filter(|text| text.starts_with("attention")).count();
    assert_eq!(seen.len(), 4, "three events and one attention message: {seen:?}");
    assert_eq!(attention, 1, "{seen:?}");
    assert_eq!(seen.last().map(String::as_str), Some("attention:acme"));
}

#[tokio::test(start_paused = true)]
async fn a_stream_opened_while_the_server_is_unreachable_starts_disconnected_and_recovers() {
    let source = Arc::new(FakeSource::default());
    source.unreachable.store(true, Ordering::SeqCst);
    let hub = hub(&source);

    let Some(mut stream) = hub.subscribe("vault", INTERVAL).await else {
        unreachable!("the vault is served")
    };
    assert!(!stream.connected);
    tokio::time::sleep(Duration::from_millis(1)).await;
    source.unreachable.store(false, Ordering::SeqCst);
    let first = next(&mut stream, INTERVAL * 3).await;
    let second = next(&mut stream, INTERVAL * 3).await;

    assert_eq!(first.as_ref().map(path_of).as_deref(), Some("disconnected"));
    assert_eq!(second.as_ref().map(path_of).as_deref(), Some("connected"));
}

#[tokio::test(start_paused = true)]
async fn shutting_down_stops_the_reader_and_signals_every_stream_to_end() {
    let source = Arc::new(FakeSource::default());
    let closing = CancellationToken::new();
    let hub = EventHub::with_shutdown(
        Arc::new(Sources(Arc::clone(&source))),
        Arc::new(FixedClock),
        Arc::new(|_, event, _| format!("<li>{}</li>", event.path)),
        closing.clone(),
    );
    let Some(stream) = hub.subscribe("vault", INTERVAL).await else {
        unreachable!("the vault is served")
    };
    tokio::time::sleep(INTERVAL * 2).await;

    closing.cancel();
    tokio::time::sleep(Duration::from_millis(1)).await;
    let polls_at_close = source.polls();
    tokio::time::sleep(INTERVAL * 5).await;

    assert!(stream.closing.is_cancelled());
    assert_eq!(
        source.polls(),
        polls_at_close,
        "the reader polls no more once the host closes"
    );
}

#[tokio::test(start_paused = true)]
async fn an_event_written_the_moment_a_stream_opens_is_not_taken_for_one_that_was_already_there() {
    let source = Arc::new(FakeSource::default());
    source.add(event(1, "task.created", "acme"));
    let hub = hub(&source);

    let Some(mut stream) = hub.subscribe("vault", INTERVAL).await else {
        unreachable!("the vault is served")
    };
    source.add(event(2, "task.created", "acme"));
    let arrived = next(&mut stream, INTERVAL * 2).await;

    assert_eq!(
        arrived.as_ref().map(path_of).as_deref(),
        Some("memory/events/2026/10/e-002.md")
    );
}

#[tokio::test(start_paused = true)]
async fn resuming_after_a_read_that_left_events_out_asks_the_page_to_reload_however_few_it_returned() {
    let source = Arc::new(FakeSource::default());
    source.add(event(20, "task.created", "acme"));
    source.saturated.store(true, Ordering::SeqCst);
    let hub = hub(&source);

    let resumed = hub.resume("vault", &event(0, "task.created", "acme").cursor()).await;

    assert!(matches!(resumed, Resume::Reload), "{resumed:?}");
}

#[tokio::test(start_paused = true)]
async fn resuming_sends_the_events_of_the_cursors_second_written_after_it_but_not_the_cursors_own() {
    let source = Arc::new(FakeSource::default());
    source.add(event_in_second(7, "memory/events/2026/10/b.md"));
    source.add(event_in_second(7, "memory/events/2026/10/a.md"));
    source.add(event_in_second(8, "memory/events/2026/10/c.md"));
    let hub = hub(&source);
    let cursor = event_in_second(7, "memory/events/2026/10/b.md").cursor();

    let Resume::Events(resumed) = hub.resume("vault", &cursor).await else {
        unreachable!("few were missed")
    };

    let paths: Vec<String> = resumed.iter().map(|live| live.cursor.path().to_owned()).collect();
    assert_eq!(paths, ["memory/events/2026/10/a.md", "memory/events/2026/10/c.md"]);
}

#[tokio::test(start_paused = true)]
async fn the_source_is_told_once_per_poll_that_attention_may_have_changed_and_not_for_a_quiet_event() {
    let source = Arc::new(FakeSource::default());
    let hub = hub(&source);
    let Some(mut stream) = hub.subscribe("vault", INTERVAL).await else {
        unreachable!("the vault is served")
    };
    tokio::time::sleep(Duration::from_millis(1)).await;
    source.add(event(1, "import.completed", "acme"));
    while next(&mut stream, INTERVAL * 2).await.is_some() {}
    assert_eq!(
        source.attention_changes.load(Ordering::SeqCst),
        0,
        "nothing about it changes attention"
    );

    for second in 2..=4 {
        source.add(event(second, "vault.recovered", "acme"));
    }
    while next(&mut stream, INTERVAL * 2).await.is_some() {}

    assert_eq!(source.attention_changes.load(Ordering::SeqCst), 1);
}
