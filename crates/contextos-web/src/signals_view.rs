//! How events are shown: one line per event, shared by the Signals feed, the live stream, and the shell's hidden
//! counter, and the three regions of the Signals screen. Pure rendering over events already read; nothing here reads
//! the vault or writes anything.
//!
//! An event is shown from its own fields only: its kind's label, severity, time, subject link, entity, and the fields its
//! kind declares (a server-fired kind's known fields, or the fields the vault's definition of a recorded kind names).
//! A field neither declares is never shown. No note body is read or rendered.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use askama::Template;
use sha2::{Digest, Sha256};
use time::macros::format_description;

use contextos_events::ServerKind;

use crate::signals_event::{Severity, SignalEvent, columns_for};
use crate::urls::VaultUrls;

/// The label each kind of the server's own gives itself, for a vault that declares no definition for it.
const SERVER_KIND_LABELS: [(&str, &str); 9] = [
    ("task.created", "Task created"),
    ("task.state-changed", "Task state changed"),
    ("task.reassigned", "Task reassigned"),
    ("task.claim-lapsed", "Task claim lapsed"),
    ("task.unknown-state", "Task in an unknown state"),
    ("task.moved", "Task moved"),
    ("member.state-changed", "Member state changed"),
    ("vault.degraded", "Vault degraded"),
    ("vault.recovered", "Vault recovered"),
];

/// How many open attention items the panel lists.
pub const ATTENTION_LIMIT: usize = 50;
/// How many events of each kind the background panel lists.
pub const BACKGROUND_PER_KIND: usize = 5;
/// How many events the feed shows to a page.
pub const FEED_PAGE_SIZE: usize = 50;

/// What the vault's `registry/events/` declares about its kinds, with the server's own kinds beneath it: the label each
/// kind gives itself and the fields a page shows for it. A page shows a recorded kind's declared fields and a
/// server-fired kind's known fields, never a field neither declares.
#[derive(Clone, Debug, Default)]
pub struct KindCatalogue {
    labels: HashMap<String, String>,
    fields: HashMap<String, Vec<String>>,
}

impl From<HashMap<String, String>> for KindCatalogue {
    /// A catalogue of labels alone, for a reader that declares no fields.
    fn from(labels: HashMap<String, String>) -> Self {
        Self {
            labels,
            fields: HashMap::new(),
        }
    }
}

impl KindCatalogue {
    /// Records a declared kind: its label, and the fields its definition names in the order they are shown.
    pub fn declare(&mut self, kind: &str, label: &str, fields: Vec<String>) {
        self.labels.insert(kind.to_owned(), label.to_owned());
        self.fields.insert(kind.to_owned(), fields);
    }

    /// The label for `kind`.
    #[must_use]
    pub fn label(&self, kind: &str) -> String {
        self.labels
            .get(kind)
            .cloned()
            .or_else(|| {
                SERVER_KIND_LABELS
                    .iter()
                    .find(|(name, _)| *name == kind)
                    .map(|(_, label)| (*label).to_owned())
            })
            .unwrap_or_else(|| kind.to_owned())
    }

    /// Every kind with a label, sorted: what the feed's kind filter offers before it has seen any event.
    #[must_use]
    pub fn kinds(&self) -> Vec<String> {
        let mut kinds: Vec<String> = self
            .labels
            .keys()
            .cloned()
            .chain(SERVER_KIND_LABELS.iter().map(|(name, _)| (*name).to_owned()))
            .collect();
        kinds.sort();
        kinds.dedup();
        kinds
    }

    /// The fields a page shows for an event of `kind`, in order: a server-fired kind's known fields, else the fields the
    /// vault's definition of the kind declares, else none.
    #[must_use]
    pub fn shown_fields(&self, kind: &str) -> Vec<&str> {
        if let Ok(server) = ServerKind::try_from(kind) {
            return server.fields().to_vec();
        }
        self.fields
            .get(kind)
            .map(|names| names.iter().map(String::as_str).collect())
            .unwrap_or_default()
    }

    /// The query columns that read every declared field, after the common ones.
    #[must_use]
    pub fn columns(&self) -> Vec<String> {
        let mut declared: Vec<String> = self.fields.values().flatten().cloned().collect();
        declared.sort();
        declared.dedup();
        columns_for(&declared)
    }
}

/// One event as a line of the page.
pub struct EventLine {
    pub severity: &'static str,
    pub colour: &'static str,
    pub iso: String,
    pub time: String,
    pub kind: String,
    pub label: String,
    pub subject_href: Option<String>,
    pub subject_label: String,
    pub entity: String,
    pub detail: String,
    /// The list item's DOM id, for the lists a live event can be added to, so a page never shows one twice.
    pub dom_id: Option<DomId>,
}

/// The id of an event's list item: a digest of its path, so it is a valid, stable DOM id whatever characters the path
/// holds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DomId(String);

impl From<&str> for DomId {
    fn from(path: &str) -> Self {
        let digest = Sha256::digest(path.as_bytes());
        let hex = digest.iter().take(12).fold(String::new(), |mut hex, byte| {
            // Writing to a string cannot fail.
            let _ = write!(hex, "{byte:02x}");
            hex
        });
        Self(format!("event-{hex}"))
    }
}

impl std::fmt::Display for DomId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The line an event is shown as: `from` and `to` first when its kind shows them and it records a change, then the other
/// fields its kind shows, each as `name: value`. A field the kind does not declare is never shown, whatever the note
/// holds.
fn detail_of(event: &SignalEvent, catalogue: &KindCatalogue) -> String {
    let shown = catalogue.shown_fields(&event.kind);
    let value_of = |name: &str| shown.contains(&name).then(|| event.field(name)).flatten();
    let mut parts = Vec::new();
    let changed = match (value_of("from"), value_of("to")) {
        (Some(from), Some(to)) => {
            parts.push(format!("{from} to {to}"));
            true
        }
        (None, Some(to)) => {
            parts.push(format!("now {to}"));
            true
        }
        _ => false,
    };
    for name in &shown {
        if changed && (*name == "from" || *name == "to") {
            continue;
        }
        if let Some(value) = event.field(name) {
            parts.push(format!("{name}: {value}"));
        }
    }
    parts.join("; ")
}

/// Where an event's subject opens: a task on its own page by its key, wherever it is filed; any other subject at the
/// path it had when the event happened, else at the path its link names.
fn subject_href(urls: &VaultUrls, event: &SignalEvent) -> Option<String> {
    if let Some(key) = event.subject_id.as_deref()
        && event.is_about_record(crate::vault_links::TASKS_BASE, key)
    {
        return Some(urls.task(key));
    }
    event
        .subject_path
        .as_deref()
        .map(|path| urls.file(path))
        .or_else(|| event.subject.as_deref().map(|target| urls.file(&subject_path(target))))
}

/// The file route of a subject: Markdown notes are named without their extension in a link.
fn subject_path(target: &str) -> String {
    let name = target.rsplit('/').next().unwrap_or(target);
    if name.contains('.') {
        target.to_owned()
    } else {
        format!("{target}.md")
    }
}

/// A moment as the screens show it: `2026-10-28 03:15 UTC`.
#[must_use]
pub fn format_time(at: time::OffsetDateTime) -> String {
    at.to_offset(time::UtcOffset::UTC)
        .format(format_description!("[year]-[month]-[day] [hour]:[minute] UTC"))
        .unwrap_or_default()
}

impl EventLine {
    /// The line for `event` in the vault `urls` belong to.
    #[must_use]
    pub fn new(urls: &VaultUrls, event: &SignalEvent, labels: &KindCatalogue) -> Self {
        let time = format_time(event.at);
        let iso = event
            .at
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();
        Self {
            severity: event.severity.as_str(),
            colour: SeverityColour::from(event.severity).0,
            iso,
            time,
            kind: event.kind.clone(),
            label: labels.label(&event.kind),
            subject_href: subject_href(urls, event),
            subject_label: event
                .subject_id
                .clone()
                .or_else(|| {
                    event
                        .subject
                        .as_deref()
                        .map(|target| target.rsplit('/').next().unwrap_or(target).to_owned())
                })
                .unwrap_or_default(),
            entity: event.entity.clone(),
            detail: detail_of(event, labels),
            dom_id: Some(DomId::from(event.path.as_str())),
        }
    }

    /// The same line without a DOM id, for a list a live event is never added to and in which the event may also
    /// appear in the feed.
    #[must_use]
    pub fn without_id(mut self) -> Self {
        self.dom_id = None;
        self
    }
}

#[derive(Template)]
#[template(path = "signal_event.html")]
struct SignalEventTemplate<'a> {
    line: &'a EventLine,
}

/// One event as the list item the feed and the stream carry.
#[must_use]
pub fn render_event(urls: &VaultUrls, event: &SignalEvent, labels: &KindCatalogue) -> String {
    let line = EventLine::new(urls, event, labels);
    SignalEventTemplate { line: &line }
        .render()
        .unwrap_or_else(|_| String::new())
}

/// The feed region: its filters, one page of events, and the note of rows that were left out.
pub struct FeedView {
    pub urls: VaultUrls,
    pub lines: Vec<EventLine>,
    pub kinds: Vec<String>,
    pub selected_kind: String,
    pub selected_severity: String,
    pub entity: Option<String>,
    pub skipped: usize,
    pub page: usize,
    pub has_older: bool,
    /// The queries that keep the filters while paging, already encoded.
    pub older_query: String,
    pub newer_query: String,
    /// The cursor of the newest event on the first page, so the live stream can start from it.
    pub newest_cursor: Option<String>,
    pub filtered: bool,
    /// Where the region reloads itself from when the stream says events were missed.
    pub refresh_href: String,
}

#[derive(Template)]
#[template(path = "signals_feed.html")]
struct FeedTemplate<'a> {
    view: &'a FeedView,
}

#[must_use]
pub fn render_feed(view: &FeedView) -> String {
    FeedTemplate { view }
        .render()
        .unwrap_or_else(|_| "<p>The feed could not be rendered.</p>".to_owned())
}

/// The attention region.
pub struct AttentionView {
    pub urls: VaultUrls,
    /// Where the region reloads itself from when the stream says attention may have changed.
    pub refresh_href: String,
    pub lines: Vec<EventLine>,
    pub entity: Option<String>,
    pub more: usize,
    pub unreadable: bool,
}

#[derive(Template)]
#[template(path = "signals_attention.html")]
struct AttentionTemplate<'a> {
    view: &'a AttentionView,
}

#[must_use]
pub fn render_attention(view: &AttentionView) -> String {
    AttentionTemplate { view }
        .render()
        .unwrap_or_else(|_| "<p>Attention could not be rendered.</p>".to_owned())
}

/// One kind's recent events in the background panel.
pub struct BackgroundGroup {
    pub kind: String,
    pub label: String,
    pub total: usize,
    pub lines: Vec<EventLine>,
}

/// The background activity region.
pub struct BackgroundView {
    pub urls: VaultUrls,
    pub groups: Vec<BackgroundGroup>,
    pub entity: Option<String>,
    pub unreadable: bool,
    /// Where the region reloads itself from when the stream says events were missed.
    pub refresh_href: String,
}

#[derive(Template)]
#[template(path = "signals_background.html")]
struct BackgroundTemplate<'a> {
    view: &'a BackgroundView,
}

#[must_use]
pub fn render_background(view: &BackgroundView) -> String {
    BackgroundTemplate { view }
        .render()
        .unwrap_or_else(|_| "<p>Background activity could not be rendered.</p>".to_owned())
}

impl BackgroundView {
    /// Groups `events` (newest first) by kind, the most recently active kind first, each keeping its newest few.
    #[must_use]
    pub fn new(urls: VaultUrls, events: &[SignalEvent], labels: &KindCatalogue, entity: Option<String>) -> Self {
        let mut by_kind: BTreeMap<&str, Vec<&SignalEvent>> = BTreeMap::new();
        for event in events {
            by_kind.entry(event.kind.as_str()).or_default().push(event);
        }
        let mut groups: Vec<BackgroundGroup> = by_kind
            .into_iter()
            .map(|(kind, members)| BackgroundGroup {
                kind: kind.to_owned(),
                label: labels.label(kind),
                total: members.len(),
                lines: members
                    .iter()
                    .take(BACKGROUND_PER_KIND)
                    .map(|event| EventLine::new(&urls, event, labels).without_id())
                    .collect(),
            })
            .collect();
        let newest = |group: &BackgroundGroup| group.lines.first().map(|line| line.iso.clone()).unwrap_or_default();
        groups.sort_by_key(|group| std::cmp::Reverse(newest(group)));
        let refresh_query = entity
            .as_deref()
            .map(|entity| format!("entity={}", crate::inbox_outbox::encode_query_value(entity)))
            .unwrap_or_default();
        Self {
            refresh_href: urls.signals_region("background", &refresh_query),
            urls,
            groups,
            entity,
            unreadable: false,
        }
    }
}

/// The Signals screen: its three regions, already rendered, in one page.
pub struct SignalsPage {
    pub urls: VaultUrls,
    pub attention: String,
    pub feed: String,
    pub background: String,
}

#[derive(Template)]
#[template(path = "signals.html")]
struct PageTemplate<'a> {
    page: &'a SignalsPage,
}

#[must_use]
pub fn render_page(page: &SignalsPage) -> String {
    PageTemplate { page }
        .render()
        .unwrap_or_else(|_| "<p>The Signals screen could not be rendered.</p>".to_owned())
}

/// The status chip colour class of a severity: grey for information, amber for a warning, red for an error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SeverityColour(pub &'static str);

impl From<Severity> for SeverityColour {
    fn from(severity: Severity) -> Self {
        Self(match severity {
            Severity::Info => "colour-grey",
            Severity::Warning => "colour-amber",
            Severity::Error => "colour-red",
        })
    }
}

#[cfg(test)]
#[path = "signals_view_test.rs"]
mod tests;
