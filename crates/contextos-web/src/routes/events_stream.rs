//! `GET /{{vault_name}}/events/stream`: the vault's new events as Server-Sent Events.
//!
//! One reader per vault ([`crate::events_hub`]) polls `contextos-mcp` while at least one stream is open, and every open
//! stream receives what it reads. Each event is one `signal` message whose `id` is the event's cursor and whose data is
//! the rendered list item; after the events of a poll that may have changed what needs attention, one `attention`
//! message follows, which open pages answer by reloading that region. A `status` message tells the shell panel whether
//! the server can be read. A reconnecting browser sends the last `id` it saw as `Last-Event-ID`, and every event after
//! it is sent first, oldest first, unless more than 500 were missed, in which case one `reload` message makes the page
//! reload its regions. A stream that cannot be read from, including one opened while the server is unreachable, stays
//! open and sends a comment line each interval, so a proxy keeps it alive; a quiet connected one sends a comment at
//! least every 15 seconds. Every stream ends promptly when the host shuts down.
//!
//! The route is behind the same loopback Host check as every other, and sends only the fields an event's kind declares.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::stream::{self, Stream};
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;

use crate::events_delivered::Delivered;
use crate::events_hub::{Delivery, EventHub, LiveEvent, Resume, Subscription};
use crate::mcp_client::McpCallError;
use crate::mcp_client::McpClientSet;
use crate::routes::vault::{not_found, server_not_configured};
use crate::signals_event::Cursor;

/// How often a quiet connected stream sends a comment line, so a proxy keeps it open.
const KEEP_ALIVE: Duration = Duration::from_secs(15);

#[derive(Clone)]
pub struct EventStreamRoutesState {
    pub clients: Arc<McpClientSet>,
    pub web_config_path: Arc<PathBuf>,
    pub hub: EventHub,
}

/// What a stream is opened with.
#[derive(Debug, Default, Deserialize)]
pub struct StreamQuery {
    /// The entity switcher's selection: only events of this entity are sent.
    #[serde(default)]
    pub entity: Option<String>,
    /// The cursor of the newest event the page already shows, for a first connection. A reconnect sends
    /// `Last-Event-ID` instead, which wins.
    #[serde(default)]
    pub after: Option<String>,
}

#[derive(Template)]
#[template(path = "stream_state.html")]
struct StreamStateTemplate {
    connected: bool,
}

/// What a stream sends next.
#[derive(Debug)]
enum Outgoing {
    Status(bool),
    Signal(Arc<LiveEvent>),
    Attention,
    /// More was missed than a resume sends: the page reloads its regions.
    Reload,
    KeepAlive,
}

impl From<Outgoing> for Event {
    fn from(outgoing: Outgoing) -> Self {
        match outgoing {
            Outgoing::Status(connected) => Self::default().event("status").data(
                StreamStateTemplate { connected }
                    .render()
                    .unwrap_or_default()
                    .replace('\n', ""),
            ),
            Outgoing::Signal(live) => Self::default()
                .event("signal")
                .id(live.cursor.to_string())
                .data(live.html.replace(['\r', '\n'], "")),
            Outgoing::Attention => Self::default().event("attention").data("changed"),
            Outgoing::Reload => Self::default().event("reload").data("missed"),
            Outgoing::KeepAlive => Self::default().comment("the server cannot be read"),
        }
    }
}

struct StreamState {
    subscription: Subscription,
    pending: VecDeque<Outgoing>,
    /// What this stream has sent or skipped, so nothing is sent twice when a resume overlaps the live events, and an
    /// event that shares a second with one already sent is still sent when its path sorts lower.
    delivered: Delivered,
    entity: Option<String>,
    connected: bool,
}

impl StreamState {
    /// Whether this stream's entity switcher selection takes `entity`; the comparison is exact, as stored.
    fn wants(&self, entity: &str) -> bool {
        self.entity.as_deref().is_none_or(|selected| entity == selected)
    }

    fn queue(&mut self, live: &Arc<LiveEvent>) {
        if !self.delivered.is_new(&live.cursor) {
            return;
        }
        self.delivered.record(&live.cursor);
        if self.wants(&live.entity) {
            self.pending.push_back(Outgoing::Signal(Arc::clone(live)));
        }
    }

    fn queue_attention(&mut self, entities: &[String]) {
        let wanted = entities.iter().any(|entity| self.wants(entity));
        if wanted
            && !self
                .pending
                .iter()
                .any(|outgoing| matches!(outgoing, Outgoing::Attention))
        {
            self.pending.push_back(Outgoing::Attention);
        }
    }

    /// The next message, waiting for the reader when there is none queued. `None` ends the stream: the reader is gone,
    /// this stream fell too far behind, or the host is shutting down; the browser reconnects from its last cursor.
    async fn next(mut self) -> Option<(Event, Self)> {
        let outgoing = self.next_outgoing().await?;
        Some((Event::from(outgoing), self))
    }

    async fn next_outgoing(&mut self) -> Option<Outgoing> {
        loop {
            if let Some(outgoing) = self.pending.pop_front() {
                return Some(outgoing);
            }
            let delivery = tokio::select! {
                () = self.subscription.closing.cancelled() => return None,
                received = self.subscription.receiver.recv() => received,
            };
            match delivery {
                Ok(Delivery::Event(live)) => self.queue(&live),
                Ok(Delivery::Attention(entities)) => self.queue_attention(&entities),
                Ok(Delivery::Connected) => {
                    self.connected = true;
                    self.pending.push_back(Outgoing::Status(true));
                }
                Ok(Delivery::Disconnected) => {
                    if self.connected {
                        self.connected = false;
                        self.pending.push_back(Outgoing::Status(false));
                    }
                    self.pending.push_back(Outgoing::KeepAlive);
                }
                Err(RecvError::Lagged(_) | RecvError::Closed) => return None,
            }
        }
    }
}

/// Whether `headers` accept an event stream: no `Accept` header at all, or one naming `text/event-stream` or anything.
fn accepts_event_stream(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_none_or(|accept| accept.contains("text/event-stream") || accept.contains("*/*"))
}

/// The cursor a stream resumes from: the browser's `Last-Event-ID`, else the page's `after`. One that is not a cursor
/// is ignored, as the browser may hold an id from an older release.
fn resume_cursor(headers: &HeaderMap, query: &StreamQuery) -> Option<Cursor> {
    let last_event_id = headers.get("last-event-id").and_then(|value| value.to_str().ok());
    last_event_id
        .and_then(|text| Cursor::try_from(text).ok())
        .or_else(|| query.after.as_deref().and_then(|text| Cursor::try_from(text).ok()))
}

/// Whether the vault is known to exist. `Ok(false)` only when the server answered that it does not; a server that
/// cannot be reached at all leaves the stream to open disconnected and recover, since the browser's reconnects would
/// otherwise start over without `Last-Event-ID`.
async fn vault_is_known(clients: &McpClientSet, vault_name: &str) -> Result<bool, Box<Response>> {
    match clients.primary() {
        Some(client) => match crate::apps::vault_exists(&client, vault_name).await {
            Ok(exists) => Ok(exists),
            Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => Ok(true),
        },
        None if clients.primary_name().is_some() => Ok(true),
        None => Err(Box::new(server_not_configured())),
    }
}

/// `GET /{{vault_name}}/events/stream`.
pub async fn stream(
    State(state): State<EventStreamRoutesState>,
    Path(vault_name): Path<String>,
    Query(query): Query<StreamQuery>,
    headers: HeaderMap,
) -> Response {
    if !accepts_event_stream(&headers) {
        return StatusCode::NOT_ACCEPTABLE.into_response();
    }
    match vault_is_known(&state.clients, &vault_name).await {
        Ok(true) => {}
        Ok(false) => return not_found(),
        Err(response) => return *response,
    }
    let path = Arc::clone(&state.web_config_path);
    let interval = tokio::task::spawn_blocking(move || crate::config::current_events(&path).poll_interval())
        .await
        .unwrap_or(Duration::from_secs(crate::config::DEFAULT_EVENT_POLL_SECONDS));
    // Subscribing before reading the resumed events means none can fall between the two.
    let Some(subscription) = state.hub.subscribe(&vault_name, interval).await else {
        return not_found();
    };
    let entity = query.entity.clone().filter(|entity| !entity.is_empty());
    let connected = subscription.connected;
    let mut open = StreamState {
        subscription,
        pending: VecDeque::from([Outgoing::Status(connected)]),
        delivered: Delivered::default(),
        entity,
        connected,
    };
    if let Some(cursor) = resume_cursor(&headers, &query) {
        open.delivered = Delivered::from(cursor.at());
        open.delivered.record(&cursor);
        match state.hub.resume(&vault_name, &cursor).await {
            Resume::Events(events) => {
                let mut attention = Vec::new();
                for live in &events {
                    open.queue(live);
                    if live.changes_attention {
                        attention.push(live.entity.clone());
                    }
                }
                open.queue_attention(&attention);
            }
            Resume::Reload => open.pending.push_back(Outgoing::Reload),
            Resume::Unavailable => {}
        }
    }
    let events: std::pin::Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>> =
        Box::pin(stream::unfold(open, |open| async move {
            open.next().await.map(|(event, open)| (Ok(event), open))
        }));
    let mut response = Sse::new(events)
        .keep_alive(KeepAlive::new().interval(KEEP_ALIVE))
        .into_response();
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    // A reverse proxy must pass the stream on as it is written.
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}

#[cfg(test)]
#[path = "events_stream_test.rs"]
mod tests;
