//! The live event stream's fan-out: one reader per vault, running only while at least one stream is open.
//!
//! Every host session runs its own `contextos` process, so no single process sees every event to push it. Reading the
//! vault is the one view that sees them all, so the reader polls `contextos-mcp` every interval for events after its
//! cursor's second and sends each one it has not already delivered, rendered once, to every open stream of that vault
//! through a bounded broadcast channel. Events share seconds and their paths do not sort in the order they were written,
//! so a read starts at the start of the newest delivered event's second and the paths delivered since are remembered
//! ([`Delivered`]). The first stream starts the reader and the last stream's end stops it, so nobody watching costs nothing. A
//! stream that falls too far behind is ended rather than buffered without bound: its browser reconnects with the last
//! cursor it saw, and the stream resumes from there.
//!
//! The source of events, the clock, and the rendering are injected, so the rules here are tested with a scripted source
//! and paused time, never with sleeps or a server.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use contextos_workflows::Clock;
use time::OffsetDateTime;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::events_delivered::Delivered;
use crate::mcp_client::McpClientSet;
use crate::signals_attention::may_change_attention;
use crate::signals_event::{Cursor, EVENT_NOTES_FILTER, SignalEvent};
use crate::signals_query::{EventsApi, ReadError, after_filter};
use crate::signals_view::KindCatalogue;

/// The most events a resumed stream is sent; when more were missed it is told to reload its regions instead.
pub const RESUME_LIMIT: usize = 500;
/// The most events one poll reads, across as many queries as that takes. More than this in one second leaves the newest
/// for the polls after.
const POLL_LIMIT: usize = 5_000;
/// The most events one resume reads, so that a gap larger than [`RESUME_LIMIT`] is recognised as one.
const RESUME_READ: usize = 2_000;
/// How many deliveries a slow stream may fall behind before it is ended.
const CHANNEL_CAPACITY: usize = 256;

type Boxed<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What one read of a vault's events found.
#[derive(Debug, Default)]
pub struct EventRead {
    /// The events, oldest first.
    pub events: Vec<SignalEvent>,
    /// What the vault's kind registry declares, read for this one read.
    pub catalogue: KindCatalogue,
    /// Whether more events matched than the read's limit, which left the newest out.
    pub truncated: bool,
}

/// Where a vault's events come from.
pub trait EventSource: Send + Sync {
    /// The events at or after `from` (the start of a second), oldest first, at most `limit`, with the registry read for
    /// them.
    fn events_from(
        &self,
        from: OffsetDateTime,
        now: OffsetDateTime,
        limit: usize,
    ) -> Boxed<'_, Result<EventRead, ReadError>>;

    /// The cursors of the events in the newest event's second, or none when the vault has no events.
    fn newest_moment(&self, now: OffsetDateTime) -> Boxed<'_, Result<Vec<Cursor>, ReadError>>;

    /// Called when a poll found events that may change what needs attention, so anything remembered about the count of
    /// what does is forgotten.
    fn attention_changed(&self) {}
}

/// The event source of each vault.
pub trait EventSources: Send + Sync {
    /// `None` when `vault` is not one this host serves.
    fn for_vault(&self, vault: &str) -> Option<Arc<dyn EventSource>>;
}

/// Renders one event as the fragment a stream carries.
pub type EventRenderer = Arc<dyn Fn(&str, &SignalEvent, &KindCatalogue) -> String + Send + Sync>;

/// One event, rendered once for every stream that will carry it.
#[derive(Debug)]
pub struct LiveEvent {
    pub cursor: Cursor,
    pub entity: String,
    pub kind: String,
    pub html: String,
    /// Whether the event may change what needs attention, so open pages should reload it.
    pub changes_attention: bool,
}

/// What the reader sends every stream.
#[derive(Clone, Debug)]
pub enum Delivery {
    Event(Arc<LiveEvent>),
    /// What a poll read may have changed what needs attention, sent once per poll after its events, with the entities
    /// of the events that may have.
    Attention(Arc<Vec<String>>),
    /// A poll succeeded after one failed.
    Connected,
    /// A poll failed: sent every interval until one succeeds.
    Disconnected,
}

struct Reader {
    sender: broadcast::Sender<Delivery>,
    subscribers: usize,
    stop: CancellationToken,
    connected: Arc<AtomicBool>,
}

struct HubInner {
    sources: Arc<dyn EventSources>,
    clock: Arc<dyn Clock>,
    render: EventRenderer,
    readers: Mutex<HashMap<String, Reader>>,
    /// Cancelled when the host shuts down, so every reader and stream ends promptly.
    closing: CancellationToken,
}

/// The per-vault readers and the streams subscribed to them.
#[derive(Clone)]
pub struct EventHub {
    inner: Arc<HubInner>,
}

/// One stream's hold on its vault's reader. Dropping the last one stops the reader.
pub struct Subscription {
    pub receiver: broadcast::Receiver<Delivery>,
    /// Whether the last poll succeeded when the stream opened.
    pub connected: bool,
    /// Cancelled when the host shuts down: the stream ends rather than hold the server's request drain.
    pub closing: CancellationToken,
    _guard: Guard,
}

struct Guard {
    inner: Arc<HubInner>,
    vault: String,
    sender: broadcast::Sender<Delivery>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let Ok(mut readers) = self.inner.readers.lock() else {
            return;
        };
        let current = readers
            .get_mut(&self.vault)
            .filter(|reader| reader.sender.same_channel(&self.sender));
        let Some(reader) = current else {
            return;
        };
        reader.subscribers = reader.subscribers.saturating_sub(1);
        if reader.subscribers == 0 {
            reader.stop.cancel();
            readers.remove(&self.vault);
        }
    }
}

/// What a resuming stream is sent.
#[derive(Debug)]
pub enum Resume {
    /// The events after its cursor, oldest first.
    Events(Vec<Arc<LiveEvent>>),
    /// More than [`RESUME_LIMIT`] were missed: the page reloads its regions instead.
    Reload,
    /// The vault could not be read: the reader's own polls carry the stream on from there.
    Unavailable,
}

impl EventHub {
    #[must_use]
    pub fn new(sources: Arc<dyn EventSources>, clock: Arc<dyn Clock>, render: EventRenderer) -> Self {
        Self::with_shutdown(sources, clock, render, CancellationToken::new())
    }

    /// A hub whose readers and streams end when `closing` is cancelled.
    #[must_use]
    pub fn with_shutdown(
        sources: Arc<dyn EventSources>,
        clock: Arc<dyn Clock>,
        render: EventRenderer,
        closing: CancellationToken,
    ) -> Self {
        Self {
            inner: Arc::new(HubInner {
                sources,
                clock,
                render,
                readers: Mutex::new(HashMap::new()),
                closing,
            }),
        }
    }

    /// How many vaults have a reader running.
    #[must_use]
    pub fn active_readers(&self) -> usize {
        self.inner.readers.lock().map_or(0, |readers| readers.len())
    }

    /// What a new reader has already delivered: the events in the newest event's second, else nothing, reading from the
    /// current whole second. Whether the vault could be read is returned with it.
    async fn starting_point(&self, source: &dyn EventSource, now: OffsetDateTime) -> (Delivered, bool) {
        match source.newest_moment(now).await {
            Ok(moment) => {
                let mut delivered = Delivered::from(moment.iter().map(Cursor::at).max().unwrap_or(now));
                for cursor in &moment {
                    delivered.record(cursor);
                }
                (delivered, true)
            }
            Err(_) => (Delivered::from(now), false),
        }
    }

    /// Opens a stream on `vault`, starting its reader (polling every `interval`) when no stream was open. A new reader
    /// first finds the newest event there is, so an event written once this returns is never mistaken for one that was
    /// already there. When the vault cannot be read the stream still opens, disconnected, and recovers when a poll
    /// succeeds. `None` when the vault is not one this host serves.
    pub async fn subscribe(&self, vault: &str, interval: Duration) -> Option<Subscription> {
        let source = self.inner.sources.for_vault(vault)?;
        let has_reader = self.inner.readers.lock().ok()?.contains_key(vault);
        let now = self.inner.clock.now();
        let (delivered, readable) = if has_reader {
            (Delivered::from(now), true)
        } else {
            self.starting_point(source.as_ref(), now).await
        };
        let mut readers = self.inner.readers.lock().ok()?;
        let reader = readers.entry(vault.to_owned()).or_insert_with(|| {
            let (sender, _) = broadcast::channel(CHANNEL_CAPACITY);
            let reader = Reader {
                sender: sender.clone(),
                subscribers: 0,
                stop: CancellationToken::new(),
                connected: Arc::new(AtomicBool::new(readable)),
            };
            tokio::spawn(run_reader(ReaderContext {
                vault: vault.to_owned(),
                source,
                clock: Arc::clone(&self.inner.clock),
                render: Arc::clone(&self.inner.render),
                sender,
                connected: Arc::clone(&reader.connected),
                stop: reader.stop.clone(),
                closing: self.inner.closing.clone(),
                interval,
                delivered,
            }));
            reader
        });
        reader.subscribers += 1;
        let subscription = Subscription {
            receiver: reader.sender.subscribe(),
            connected: reader.connected.load(Ordering::Relaxed),
            closing: self.inner.closing.clone(),
            _guard: Guard {
                inner: Arc::clone(&self.inner),
                vault: vault.to_owned(),
                sender: reader.sender.clone(),
            },
        };
        Some(subscription)
    }

    /// The events after `cursor`, oldest first, rendered for a stream that is resuming; or [`Resume::Reload`] when more
    /// than [`RESUME_LIMIT`] were missed, so nothing missed is lost silently.
    pub async fn resume(&self, vault: &str, cursor: &Cursor) -> Resume {
        let Some(source) = self.inner.sources.for_vault(vault) else {
            return Resume::Unavailable;
        };
        let Ok(read) = source
            .events_from(
                cursor.at().replace_nanosecond(0).unwrap_or(cursor.at()),
                self.inner.clock.now(),
                RESUME_READ,
            )
            .await
        else {
            return Resume::Unavailable;
        };
        // The whole of the cursor's second is sent again but for the cursor's own event: an event written after it in
        // that second may have a path that sorts before it. A stream and a page both ignore one they already have.
        let second = cursor.at().replace_nanosecond(0).unwrap_or(cursor.at());
        let missed: Vec<&SignalEvent> = read
            .events
            .iter()
            .filter(|event| event.at >= second && event.path != cursor.path())
            .collect();
        if missed.len() > RESUME_LIMIT || read.truncated {
            return Resume::Reload;
        }
        Resume::Events(
            missed
                .into_iter()
                .map(|event| Arc::new(live_event(&self.inner.render, vault, event, &read.catalogue)))
                .collect(),
        )
    }
}

fn live_event(render: &EventRenderer, vault: &str, event: &SignalEvent, labels: &KindCatalogue) -> LiveEvent {
    LiveEvent {
        cursor: event.cursor(),
        entity: event.entity.clone(),
        kind: event.kind.clone(),
        html: render(vault, event, labels),
        changes_attention: may_change_attention(event),
    }
}

struct ReaderContext {
    vault: String,
    source: Arc<dyn EventSource>,
    clock: Arc<dyn Clock>,
    render: EventRenderer,
    sender: broadcast::Sender<Delivery>,
    connected: Arc<AtomicBool>,
    stop: CancellationToken,
    closing: CancellationToken,
    interval: Duration,
    /// What was delivered before the reader started, or nothing.
    delivered: Delivered,
}

/// Polls until stopped. Reading is async I/O through the MCP session, so nothing here blocks the executor.
async fn run_reader(context: ReaderContext) {
    let mut delivered = context.delivered.clone();
    tracing::debug!(vault = %context.vault, "the event reader started");
    loop {
        poll_once(&context, &mut delivered).await;
        tokio::select! {
            () = context.stop.cancelled() => break,
            () = context.closing.cancelled() => break,
            () = tokio::time::sleep(context.interval) => {}
        }
    }
    tracing::debug!(vault = %context.vault, "the event reader stopped");
}

/// One poll: reads from the start of the newest delivered second, delivers each event not delivered before, then says
/// once whether attention may have changed.
async fn poll_once(context: &ReaderContext, delivered: &mut Delivered) {
    let now = context.clock.now();
    let from = delivered.read_from().unwrap_or(now);
    let Ok(read) = context.source.events_from(from, now, POLL_LIMIT).await else {
        if context.connected.swap(false, Ordering::Relaxed) {
            tracing::warn!(vault = %context.vault, "the event reader cannot reach contextos-mcp, so streams wait");
        }
        let _ = context.sender.send(Delivery::Disconnected);
        return;
    };
    if !context.connected.swap(true, Ordering::Relaxed) {
        let _ = context.sender.send(Delivery::Connected);
    }
    let mut attention: Vec<String> = Vec::new();
    for event in &read.events {
        if !delivered.is_new(&event.cursor()) {
            continue;
        }
        delivered.record(&event.cursor());
        let live = live_event(&context.render, &context.vault, event, &read.catalogue);
        if live.changes_attention && !attention.contains(&live.entity) {
            attention.push(live.entity.clone());
        }
        // No receiver is not a failure: the last stream may have just closed.
        let _ = context.sender.send(Delivery::Event(Arc::new(live)));
    }
    if !attention.is_empty() {
        context.source.attention_changed();
        let _ = context.sender.send(Delivery::Attention(Arc::new(attention)));
    }
}

/// The MCP-backed sources: every vault of the primary session.
pub struct McpEventSources {
    pub clients: Arc<McpClientSet>,
}

struct McpEventSource {
    clients: Arc<McpClientSet>,
    vault: String,
}

impl EventSources for McpEventSources {
    fn for_vault(&self, vault: &str) -> Option<Arc<dyn EventSource>> {
        Some(Arc::new(McpEventSource {
            clients: Arc::clone(&self.clients),
            vault: vault.to_owned(),
        }))
    }
}

impl EventSource for McpEventSource {
    fn events_from(
        &self,
        from: OffsetDateTime,
        now: OffsetDateTime,
        limit: usize,
    ) -> Boxed<'_, Result<EventRead, ReadError>> {
        Box::pin(async move {
            let client = self.clients.primary().ok_or(ReadError::Unreachable)?;
            let api = EventsApi {
                client: &client,
                vault_name: &self.vault,
            };
            // The registry is read once for this read, so the declared fields arrive as columns of the event query.
            let catalogue = api.kind_catalogue().await?;
            let batch = api
                .oldest_since(EVENT_NOTES_FILTER, Some(from), now, limit, &catalogue.columns())
                .await?;
            let events = batch.events.into_iter().filter(|event| event.at >= from).collect();
            Ok(EventRead {
                events,
                catalogue,
                truncated: batch.truncated,
            })
        })
    }

    fn attention_changed(&self) {
        if let Some(client) = self.clients.primary() {
            client.open_counts().invalidate(&self.vault);
        }
    }

    fn newest_moment(&self, now: OffsetDateTime) -> Boxed<'_, Result<Vec<Cursor>, ReadError>> {
        Box::pin(async move {
            let client = self.clients.primary().ok_or(ReadError::Unreachable)?;
            let api = EventsApi {
                client: &client,
                vault_name: &self.vault,
            };
            let columns = crate::signals_load::decision_columns();
            let newest = api.newest_first(&after_filter(None, now), 1, &columns).await?;
            let Some(newest) = newest.events.first() else {
                return Ok(Vec::new());
            };
            let second = newest.at.replace_nanosecond(0).unwrap_or(newest.at);
            let moment = api
                .oldest_since(EVENT_NOTES_FILTER, Some(second), now, POLL_LIMIT, &columns)
                .await?;
            Ok(moment
                .events
                .iter()
                .filter(|event| event.at >= second)
                .map(SignalEvent::cursor)
                .collect())
        })
    }
}

#[cfg(test)]
#[path = "events_hub_test.rs"]
mod tests;
