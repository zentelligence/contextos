//! An event note as the Signals screen, the live stream, the Tasks indicator, and Agents liveness read it.
//!
//! Events are read through `base_query` with an inline definition, so nothing here depends on the vault's
//! `memory/events.base` file. A row that cannot be understood (no readable `at`, no kind, an unknown severity) is not
//! an event: it is counted by the caller and left out.

use std::collections::BTreeMap;
use std::fmt;

use serde::Deserialize;
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

/// The folder events live in.
pub const EVENTS_FOLDER: &str = "memory/events";

/// The base filter every event query starts from.
pub const EVENT_NOTES_FILTER: &str = "file.inFolder(\"memory/events\") && note.type == \"event\"";

/// The columns every event query asks for before it adds a vault's declared fields: the shared event fields, the fields
/// the clearing rules and the Agents screen decide from, and the fields the server's own kinds carry. A recorded
/// kind's declared fields are added from the kind registry (see [`columns_for`]); no other field is requested, so a
/// value placed in an event note by hand never reaches a page.
pub const EVENT_COLUMNS: [&str; 22] = [
    "file.path",
    "kind",
    "severity",
    "at",
    "entity",
    "subject",
    "subject_id",
    "subject_path",
    "subject_base",
    "actor",
    "code",
    "service",
    "workflow",
    "job",
    "run",
    "run_id",
    "member",
    "members",
    "from",
    "to",
    "reason",
    "suspended_at",
];

/// The columns a reader that only decides what is open needs: no display fields, so the rows stay small.
pub const DECISION_COLUMNS: [&str; 12] = [
    "file.path",
    "kind",
    "severity",
    "at",
    "entity",
    "subject",
    "subject_id",
    "subject_path",
    "subject_base",
    "code",
    "workflow",
    "job",
];

/// [`EVENT_COLUMNS`] followed by each of `declared` that is not already among them, in order.
#[must_use]
pub fn columns_for(declared: &[String]) -> Vec<String> {
    let mut columns: Vec<String> = EVENT_COLUMNS.iter().map(|column| (*column).to_owned()).collect();
    for name in declared {
        if !columns.contains(name) {
            columns.push(name.clone());
        }
    }
    columns
}

/// How serious an event is: the one definition `contextos-events` gives it.
pub use contextos_events::Severity;

/// A row that is not a readable event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NotAnEvent;

/// Where an event sits in time: its `at`, then its path, which orders events that share a second. The stream's `id`.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Cursor {
    at: OffsetDateTime,
    path: String,
}

impl Cursor {
    /// The cursor of an event at `path` that happened at `at`.
    #[must_use]
    pub fn new(at: OffsetDateTime, path: &str) -> Self {
        Self {
            at: at.to_offset(UtcOffset::UTC),
            path: path.to_owned(),
        }
    }

    #[must_use]
    pub const fn at(&self) -> OffsetDateTime {
        self.at
    }

    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }
}

impl fmt::Display for Cursor {
    /// `{at as RFC 3339 in UTC}|{path}`: one line, so it is a valid SSE `id`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let at = self.at.format(&Rfc3339).map_err(|_| fmt::Error)?;
        write!(formatter, "{at}|{}", self.path)
    }
}

impl TryFrom<&str> for Cursor {
    type Error = NotAnEvent;

    fn try_from(text: &str) -> Result<Self, NotAnEvent> {
        let (at, path) = text.split_once('|').ok_or(NotAnEvent)?;
        if path.is_empty() || path.contains(['\r', '\n']) {
            return Err(NotAnEvent);
        }
        let at = OffsetDateTime::parse(at, &Rfc3339).map_err(|_| NotAnEvent)?;
        Ok(Self::new(at, path))
    }
}

/// One event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignalEvent {
    /// The note's vault path.
    pub path: String,
    pub kind: String,
    pub severity: Severity,
    pub at: OffsetDateTime,
    pub entity: String,
    /// The target the event's subject link names, without the `.md` of a Markdown note: a record's key, or a note's
    /// vault path.
    pub subject: Option<String>,
    /// The subject's key, when it is a record of a keyed base.
    pub subject_id: Option<String>,
    /// Where the subject was when the event happened, `.md` included.
    pub subject_path: Option<String>,
    /// The base the subject is a record of.
    pub subject_base: Option<String>,
    pub actor: Option<String>,
    /// The fields the query returned beyond the common event fields, by name. Which of them a page shows is decided
    /// by the kind's declaration, not by this map (see `KindCatalogue::shown_fields`).
    pub fields: BTreeMap<String, String>,
}

/// What `base_query` reports for an event row. A property with no value is `null`.
#[derive(Debug, Deserialize)]
pub struct EventRow {
    #[serde(rename = "file.path")]
    path: Option<String>,
    kind: Option<String>,
    severity: Option<String>,
    at: Option<Value>,
    entity: Option<String>,
    subject: Option<String>,
    subject_id: Option<Value>,
    subject_path: Option<String>,
    subject_base: Option<String>,
    actor: Option<String>,
    #[serde(flatten)]
    rest: BTreeMap<String, Value>,
}

impl From<&EventRow> for crate::signals_query::UnreadableRow {
    /// Where the row sits, as far as it says: its path and a parseable `at`.
    fn from(row: &EventRow) -> Self {
        Self {
            path: row.path.clone().filter(|path| !path.is_empty()),
            at: row
                .at
                .as_ref()
                .and_then(Value::as_str)
                .and_then(|at| OffsetDateTime::parse(at, &Rfc3339).ok())
                .map(|at| at.to_offset(UtcOffset::UTC)),
        }
    }
}

/// A scalar as it is written, a list as its items separated by commas, and a map as `{name: value, ...}`, as one line of
/// text. A value with no text, such as `null` or an empty string, is `None`.
fn line_of(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.trim().is_empty() => Some(text.trim().to_owned()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().filter_map(line_of).collect();
            (!parts.is_empty()).then(|| parts.join(", "))
        }
        Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .filter_map(|(name, item)| line_of(item).map(|text| format!("{name}: {text}")))
                .collect();
            (!parts.is_empty()).then(|| format!("{{{}}}", parts.join(", ")))
        }
        Value::Null | Value::String(_) => None,
    }
}

/// The vault path a `[[target|label]]` link names, or `None` for text that is not a link.
#[must_use]
pub fn link_target(link: &str) -> Option<String> {
    contextos_events::link_target(link)
}

/// The label of a `[[target|label]]` link, else its target.
#[must_use]
pub fn link_label(link: &str) -> Option<String> {
    let inner = link.trim().strip_prefix("[[")?.strip_suffix("]]")?;
    let label = inner.split_once('|').map_or(inner, |(_, label)| label).trim();
    (!label.is_empty()).then(|| label.to_owned())
}

impl TryFrom<EventRow> for SignalEvent {
    type Error = NotAnEvent;

    fn try_from(row: EventRow) -> Result<Self, NotAnEvent> {
        // The path becomes a stream `id`, which is one line; a path with a line break is not an event note this
        // product wrote.
        let path = row
            .path
            .filter(|path| !path.is_empty() && !path.contains(['\r', '\n']))
            .ok_or(NotAnEvent)?;
        let kind = row.kind.filter(|kind| !kind.trim().is_empty()).ok_or(NotAnEvent)?;
        let severity = Severity::try_from(row.severity.as_deref().ok_or(NotAnEvent)?).map_err(|_| NotAnEvent)?;
        let at = row.at.as_ref().and_then(Value::as_str).ok_or(NotAnEvent)?;
        let at = OffsetDateTime::parse(at, &Rfc3339).map_err(|_| NotAnEvent)?;
        let fields = row
            .rest
            .iter()
            .filter_map(|(name, value)| line_of(value).map(|line| (name.clone(), line)))
            .collect();
        Ok(Self {
            path,
            kind: kind.trim().to_owned(),
            severity,
            at: at.to_offset(UtcOffset::UTC),
            // As stored, not trimmed: every filter compares the stored value (`entity == "..."` in a query, the
            // switcher's selection on a stream), so an event is of the same entity wherever it is looked at.
            entity: row
                .entity
                .filter(|entity| !entity.trim().is_empty())
                .unwrap_or_else(|| "vault".to_owned()),
            subject: row.subject.as_deref().and_then(link_target),
            subject_id: row.subject_id.as_ref().and_then(line_of),
            subject_path: row.subject_path.filter(|path| !path.trim().is_empty()),
            subject_base: row.subject_base.filter(|base| !base.trim().is_empty()),
            actor: row.actor.filter(|actor| !actor.trim().is_empty()),
            fields,
        })
    }
}

impl SignalEvent {
    /// What the event is about, as the clearing rules compare it: a record by its base and key, any other note by its
    /// path.
    #[must_use]
    pub fn subject_identity(&self) -> Option<contextos_events::SubjectIdentity> {
        let link = self.subject.as_ref().map(|target| format!("[[{target}]]"));
        contextos_events::SubjectIdentity::try_from(contextos_events::SubjectFields {
            link: link.as_deref(),
            id: self.subject_id.as_deref(),
            path: self.subject_path.as_deref(),
            base: self.subject_base.as_deref(),
        })
        .ok()
    }

    /// Whether the event is about the record `key` of the base `base`, comparing keys without regard to case.
    #[must_use]
    pub fn is_about_record(&self, base: &str, key: &str) -> bool {
        matches!(
            self.subject_identity(),
            Some(contextos_events::SubjectIdentity::Record { base: found, key: found_key })
                if found == base && found_key.eq_ignore_ascii_case(key)
        )
    }

    /// Where the event sits in time.
    #[must_use]
    pub fn cursor(&self) -> Cursor {
        Cursor::new(self.at, &self.path)
    }

    /// One of the kind's own fields.
    #[must_use]
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }

    /// Whether the event is of a kind the product ships: fired by the server, or recorded by the scaffold's own
    /// scheduler and session skills. Any other kind is background activity from the vault or an external source.
    #[must_use]
    pub fn is_shipped_kind(&self) -> bool {
        contextos_events::is_shipped_kind(&self.kind)
    }
}

#[cfg(test)]
#[path = "signals_event_test.rs"]
mod tests;
