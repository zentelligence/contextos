//! Which events the retention sweep deletes, and when a vault was last swept.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use time::format_description::well_known::{Iso8601, Rfc3339};
use time::{Date, Duration, OffsetDateTime, UtcOffset};

use crate::persist::{read_json, write_json};
use crate::{EventsError, Occurrence, Severity};

/// How long a vault keeps events.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetentionPolicy {
    /// Days an event is kept when its kind declares no window of its own.
    pub default_days: u32,
    /// Whether `warning` and `error` events are kept until deleted by hand.
    pub keep_attention: bool,
}

/// What the sweep does with one event note.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Verdict {
    /// Leave it.
    Kept,
    /// Delete it: it is older than its window.
    Expired,
    /// Its time cannot be read, so it is left and reported.
    Unreadable,
}

/// One event note as the sweep reads it: where it is and what its frontmatter holds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SweepNote {
    /// The note's vault path.
    pub path: String,
    /// Its frontmatter.
    pub frontmatter: Map<String, Value>,
}

impl Verdict {
    /// The time an event note records in `at`, or `None` when it has none or cannot be read: exactly the notes the
    /// sweep reports as [`Self::Unreadable`].
    pub fn time_of(frontmatter: &Map<String, Value>) -> Option<OffsetDateTime> {
        frontmatter
            .get("at")
            .and_then(Value::as_str)
            .and_then(|text| OffsetDateTime::parse(text, &Rfc3339).ok())
    }

    /// Judges every note in `notes` at `now`, in the same order, by the one set of rules for what is still open
    /// ([`Occurrence::is_open`]): with `keep_attention` a warning or error is kept only while open, and once cleared it
    /// ages out on its own `at`. An event that clears a warning which survives is kept too, so a cleared warning never
    /// reappears as open because the event that cleared it was swept. A note that cannot be judged as an event is
    /// treated as open, so it is kept as attention is.
    pub fn plan(
        notes: &[SweepNote],
        policy: RetentionPolicy,
        kind_days: impl Fn(&str) -> Option<u32>,
        now: OffsetDateTime,
    ) -> Vec<Self> {
        let occurrences: Vec<Option<Occurrence>> = notes
            .iter()
            .map(|note| Occurrence::try_from((note.path.as_str(), &note.frontmatter)).ok())
            .collect();
        let pool: Vec<Occurrence> = occurrences.iter().flatten().cloned().collect();
        let mut verdicts: Vec<Self> = notes
            .iter()
            .zip(&occurrences)
            .map(|(note, occurrence)| {
                let is_open = occurrence.as_ref().is_none_or(|occurrence| occurrence.is_open(&pool));
                let judged = RetentionPolicy {
                    keep_attention: policy.keep_attention && is_open,
                    ..policy
                };
                Self::of(&note.frontmatter, judged, &kind_days, now)
            })
            .collect();
        // Withhold the deletion of an event that clears a warning or error which is not itself deleted.
        let surviving: Vec<&Occurrence> = occurrences
            .iter()
            .zip(&verdicts)
            .filter_map(|(occurrence, verdict)| occurrence.as_ref().filter(|_| *verdict != Self::Expired))
            .filter(|occurrence| occurrence.needs_attention())
            .collect();
        for (verdict, occurrence) in verdicts.iter_mut().zip(&occurrences) {
            if *verdict == Self::Expired
                && occurrence
                    .as_ref()
                    .is_some_and(|clearer| surviving.iter().any(|kept| kept.is_cleared_by(clearer)))
            {
                *verdict = Self::Kept;
            }
        }
        verdicts
    }

    /// Judges an event note from its frontmatter at `now`. `kind_days` gives a kind's own window, when it declares
    /// one. An event is expired when it is strictly older than its window, so one exactly at the edge is kept. A
    /// severity this build does not know counts as attention: a sweep never deletes what it does not understand.
    pub fn of(
        frontmatter: &Map<String, Value>,
        policy: RetentionPolicy,
        kind_days: impl Fn(&str) -> Option<u32>,
        now: OffsetDateTime,
    ) -> Self {
        let Some(at) = Self::time_of(frontmatter) else {
            return Self::Unreadable;
        };
        let severity = frontmatter
            .get("severity")
            .and_then(Value::as_str)
            .map(Severity::try_from);
        let is_attention = !matches!(severity, Some(Ok(Severity::Info)));
        if is_attention && policy.keep_attention {
            return Self::Kept;
        }
        let days = frontmatter
            .get("kind")
            .and_then(Value::as_str)
            .and_then(kind_days)
            .unwrap_or(policy.default_days);
        if now - at > Duration::days(i64::from(days)) {
            Self::Expired
        } else {
            Self::Kept
        }
    }
}

/// The UTC day a vault's events were last swept, kept in its state directory so that of several processes only the
/// first to find the day new sweeps. Stored as an ISO 8601 date.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct SweepDay {
    last_swept: Date,
}

impl From<SweepDay> for String {
    fn from(value: SweepDay) -> Self {
        // A date always formats; the empty text is read back as unreadable and the vault is simply swept again.
        value.last_swept.format(&Iso8601::DATE).unwrap_or_default()
    }
}

impl TryFrom<String> for SweepDay {
    type Error = time::error::Parse;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Date::parse(&value, &Iso8601::DATE).map(Self::from)
    }
}

impl From<Date> for SweepDay {
    fn from(last_swept: Date) -> Self {
        Self { last_swept }
    }
}

impl SweepDay {
    /// The day the sweep last completed.
    #[must_use]
    pub const fn date(self) -> Date {
        self.last_swept
    }

    /// Whether a sweep is owed on `today`: the last one was on an earlier day.
    #[must_use]
    pub fn is_due(self, today: Date) -> bool {
        self.last_swept < today
    }

    /// The UTC date of `instant`.
    #[must_use]
    pub fn utc_date(instant: OffsetDateTime) -> Date {
        instant.to_offset(UtcOffset::UTC).date()
    }

    /// Reads the record from `path`; `None` when the vault was never swept.
    ///
    /// # Errors
    ///
    /// Returns [`EventsError::TableUnreadable`] when the file cannot be read; the caller sweeps as if never swept.
    pub fn load(path: &std::path::Path) -> Result<Option<Self>, EventsError> {
        read_json(path)
    }

    /// Writes the record to `path` atomically.
    ///
    /// # Errors
    ///
    /// Returns [`EventsError::TableUnwritable`] when the file cannot be written.
    pub fn store(&self, path: &std::path::Path) -> Result<(), EventsError> {
        write_json(path, self)
    }
}

#[cfg(test)]
#[path = "retention_test.rs"]
mod tests;
