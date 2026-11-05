//! The operator's local time: the time zone people working in a vault read their notes in, and a clock that reports
//! the current instant at that zone's offset.
//!
//! Every time the server or the web writes into a vault (a note's `created` and `updated` stamps, a new note's
//! dated file name, the day an operation log entry is filed under, the journal's "today") is the operator's local
//! time, never UTC: an operator reads and edits those values, and a UTC date is the wrong day for much of theirs.
//! The zone is an IANA zone (`Australia/Sydney`), never a fixed offset, so each instant takes the offset its zone
//! actually had then, daylight saving included. It is the one named in configuration, or else the host's own.
//!
//! An instant keeps its identity whatever offset it is shown at, so comparing, ordering, and measuring elapsed time
//! are unaffected: only the rendering changes.

use std::fmt;

use jiff::Timestamp;
use jiff::tz::{AmbiguousOffset, Offset, TimeZone};
use thiserror::Error;
use time::{OffsetDateTime, PrimitiveDateTime, UtcOffset};

use crate::pipeline::{Clock, SystemClock};

/// Why a configured time zone was refused.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum TimeZoneError {
    /// Not written as an IANA time zone name: empty, padded, a fixed offset such as `+10:00`, or holding characters
    /// or path segments no zone name has.
    #[error("{name:?} is not an IANA time zone name such as \"Australia/Sydney\"")]
    Malformed { name: String },
    /// Well formed, but the time zone database has no zone of that name.
    #[error("the time zone database has no zone named {name:?}")]
    Unknown { name: String },
    /// A reported zone gave neither an IANA name nor POSIX rules that can be read.
    #[error("the reported time zone has neither a known IANA name nor readable POSIX rules")]
    Unreported,
}

impl TimeZoneError {
    /// The stable machine-readable code for this failure.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Malformed { .. } => "config/time-zone-malformed",
            Self::Unknown { .. } => "config/time-zone-unknown",
            Self::Unreported => "config/time-zone-unreported",
        }
    }
}

/// An IANA time zone named in configuration, validated against the time zone database.
#[derive(Clone)]
pub struct ConfiguredTimeZone {
    name: String,
    zone: TimeZone,
}

impl ConfiguredTimeZone {
    /// The zone's IANA name as the database spells it.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Debug for ConfiguredTimeZone {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfiguredTimeZone")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl PartialEq for ConfiguredTimeZone {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for ConfiguredTimeZone {}

/// Whether `name` is written as an IANA zone name: `/`-separated segments of ASCII letters, digits, `_`, `-`, and
/// `+`, none empty and none a `.` or `..` path step. A fixed offset (`+10:00`) and anything padded are not.
fn is_zone_name(name: &str) -> bool {
    !name.is_empty()
        && name.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '+'))
        })
}

impl TryFrom<&str> for ConfiguredTimeZone {
    type Error = TimeZoneError;

    fn try_from(name: &str) -> Result<Self, Self::Error> {
        if !is_zone_name(name) {
            return Err(TimeZoneError::Malformed { name: name.to_owned() });
        }
        let zone = TimeZone::get(name).map_err(|_| TimeZoneError::Unknown { name: name.to_owned() })?;
        Ok(Self {
            name: zone.iana_name().unwrap_or(name).to_owned(),
            zone,
        })
    }
}

/// Where the operator's time zone came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeZoneSource {
    /// Named in configuration.
    Configured,
    /// The host's own zone, because none was configured.
    System,
    /// None was configured and the host's own could not be determined, so UTC stands in: worth an operator's
    /// attention, since dates then fall on UTC's day rather than theirs.
    Undetected,
}

impl TimeZoneSource {
    /// A stable lower-case label for reports.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Configured => "configured",
            Self::System => "system",
            Self::Undetected => "undetected",
        }
    }
}

/// The time zone the operator works in: the configured one, or else the host's own.
#[derive(Clone)]
pub struct OperatorTimeZone {
    zone: TimeZone,
    name: Option<String>,
    /// The POSIX `TZ` rules the zone was built from, kept for a zone with no IANA name so another process can build
    /// the same zone.
    rules: Option<String>,
    source: TimeZoneSource,
}

/// The instants a local wall-clock time names in a zone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalResolution {
    /// The time happens once.
    Single(OffsetDateTime),
    /// The time happens twice, because the clock was set back (daylight saving ending); the earlier instant first.
    Repeated(OffsetDateTime, OffsetDateTime),
    /// The time never happens, because the clock was set forward over it (daylight saving beginning).
    Gap,
}

impl OperatorTimeZone {
    /// UTC itself, for a clock with no operator zone of its own.
    #[must_use]
    pub fn utc() -> Self {
        Self {
            zone: TimeZone::UTC,
            name: Some("UTC".to_owned()),
            rules: None,
            source: TimeZoneSource::Configured,
        }
    }

    /// The instants the local wall-clock time `local` names in this zone: one, two across the hour the clock repeats
    /// when daylight saving ends, or none in the hour it skips when daylight saving begins. A time outside the range
    /// the zone database covers names none.
    #[must_use]
    pub fn resolve_local(&self, local: PrimitiveDateTime) -> LocalResolution {
        let Some(civil) = civil_of(local) else {
            return LocalResolution::Gap;
        };
        let at = |offset: Offset| {
            UtcOffset::from_whole_seconds(offset.seconds())
                .ok()
                .map(|offset| local.assume_offset(offset))
        };
        match self.zone.to_ambiguous_timestamp(civil).offset() {
            AmbiguousOffset::Unambiguous { offset } => at(offset).map_or(LocalResolution::Gap, LocalResolution::Single),
            AmbiguousOffset::Fold { before, after } => match (at(before), at(after)) {
                (Some(first), Some(second)) if first <= second => LocalResolution::Repeated(first, second),
                (Some(first), Some(second)) => LocalResolution::Repeated(second, first),
                (Some(only), None) | (None, Some(only)) => LocalResolution::Single(only),
                (None, None) => LocalResolution::Gap,
            },
            AmbiguousOffset::Gap { .. } => LocalResolution::Gap,
        }
    }

    /// The zone's IANA name, when it has one: a host zone given only as rules (a POSIX `TZ` string) has none.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The POSIX `TZ` rules (`AEST-10AEDT,M10.1.0,M4.1.0/3`) of a zone that has no IANA name, so another process
    /// can build the same zone; `None` for a named zone.
    #[must_use]
    pub fn rules(&self) -> Option<&str> {
        self.rules.as_deref()
    }

    /// Where the zone came from.
    #[must_use]
    pub const fn source(&self) -> TimeZoneSource {
        self.source
    }

    /// `instant` at the offset this zone had at that instant, daylight saving included. The instant itself is
    /// unchanged.
    #[must_use]
    pub fn localise(&self, instant: OffsetDateTime) -> OffsetDateTime {
        instant.to_offset(self.offset_at(instant))
    }

    /// The offset from UTC this zone had at `instant`.
    #[must_use]
    pub fn offset_at(&self, instant: OffsetDateTime) -> UtcOffset {
        // Every instant `time` can represent is one jiff can, and every offset a zone can have is within the range
        // `time` represents, so neither conversion can fail for a real zone; UTC stands in should either ever do so,
        // which leaves the instant correct and only its rendering at UTC's offset.
        Timestamp::new(
            instant.unix_timestamp(),
            i32::try_from(instant.nanosecond()).unwrap_or_default(),
        )
        .ok()
        .and_then(|timestamp| UtcOffset::from_whole_seconds(self.zone.to_offset(timestamp).seconds()).ok())
        .unwrap_or(UtcOffset::UTC)
    }
}

/// `local` as jiff's civil date and time, or `None` beyond jiff's range.
fn civil_of(local: PrimitiveDateTime) -> Option<jiff::civil::DateTime> {
    jiff::civil::DateTime::new(
        i16::try_from(local.year()).ok()?,
        i8::try_from(u8::from(local.month())).ok()?,
        i8::try_from(local.day()).ok()?,
        i8::try_from(local.hour()).ok()?,
        i8::try_from(local.minute()).ok()?,
        i8::try_from(local.second()).ok()?,
        i32::try_from(local.nanosecond()).ok()?,
    )
    .ok()
}

impl fmt::Debug for OperatorTimeZone {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OperatorTimeZone")
            .field("name", &self.name)
            .field("rules", &self.rules)
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

impl From<ConfiguredTimeZone> for OperatorTimeZone {
    fn from(configured: ConfiguredTimeZone) -> Self {
        Self {
            zone: configured.zone,
            name: Some(configured.name),
            rules: None,
            source: TimeZoneSource::Configured,
        }
    }
}

/// The configured zone when there is one; otherwise the host's own (its `TZ` setting or system zone, as the
/// platform reports it), or UTC, reported as [`TimeZoneSource::Undetected`], when the host's cannot be determined.
impl From<Option<ConfiguredTimeZone>> for OperatorTimeZone {
    fn from(configured: Option<ConfiguredTimeZone>) -> Self {
        if let Some(configured) = configured {
            return Self::from(configured);
        }
        match TimeZone::try_system() {
            Ok(zone) => {
                let name = zone.iana_name().map(str::to_owned);
                // A host zone with no IANA name came from POSIX rules in `TZ`, which the platform reads first: keep
                // them, so a client of this process can build the very same zone.
                let rules = name
                    .is_none()
                    .then(|| std::env::var("TZ").ok())
                    .flatten()
                    .filter(|rules| TimeZone::posix(rules).is_ok());
                Self {
                    name,
                    rules,
                    zone,
                    source: TimeZoneSource::System,
                }
            }
            Err(_) => Self {
                zone: TimeZone::UTC,
                name: Some("UTC".to_owned()),
                rules: None,
                source: TimeZoneSource::Undetected,
            },
        }
    }
}

/// A zone as another process reports it (`vault_info`'s `time_zone`): its IANA name, or, for a zone without one,
/// its POSIX `TZ` rules.
#[derive(Clone, Copy, Debug)]
pub struct ReportedTimeZone<'a> {
    pub name: Option<&'a str>,
    pub rules: Option<&'a str>,
}

/// The zone another process reported, built the same way it built it: by IANA name, else by POSIX rules.
impl TryFrom<ReportedTimeZone<'_>> for OperatorTimeZone {
    type Error = TimeZoneError;

    fn try_from(reported: ReportedTimeZone<'_>) -> Result<Self, TimeZoneError> {
        if let Some(name) = reported.name {
            return ConfiguredTimeZone::try_from(name).map(Self::from);
        }
        let rules = reported.rules.ok_or(TimeZoneError::Unreported)?;
        let zone = TimeZone::posix(rules).map_err(|_| TimeZoneError::Unreported)?;
        Ok(Self {
            zone,
            name: None,
            rules: Some(rules.to_owned()),
            source: TimeZoneSource::Configured,
        })
    }
}

/// A clock and the zone its readings are shown in, for [`LocalClock`].
#[derive(Clone, Debug)]
pub struct LocalClockInput<C> {
    pub clock: C,
    pub zone: OperatorTimeZone,
}

/// A clock that reports the current instant at the operator's local offset: the clock behind every time written
/// into a vault.
#[derive(Clone, Debug)]
pub struct LocalClock<C = SystemClock> {
    clock: C,
    zone: OperatorTimeZone,
}

impl<C> LocalClock<C> {
    /// The zone this clock reports in.
    #[must_use]
    pub const fn zone(&self) -> &OperatorTimeZone {
        &self.zone
    }
}

impl<C> From<LocalClockInput<C>> for LocalClock<C> {
    fn from(input: LocalClockInput<C>) -> Self {
        Self {
            clock: input.clock,
            zone: input.zone,
        }
    }
}

impl From<OperatorTimeZone> for LocalClock {
    fn from(zone: OperatorTimeZone) -> Self {
        Self {
            clock: SystemClock,
            zone,
        }
    }
}

impl<C: Clock> Clock for LocalClock<C> {
    fn now(&self) -> OffsetDateTime {
        self.zone.localise(self.clock.now())
    }

    fn localise(&self, instant: OffsetDateTime) -> OffsetDateTime {
        self.zone.localise(instant)
    }
}

#[cfg(test)]
#[path = "local_time_test.rs"]
mod tests;
