//! `job.toml`'s `schedule` field: a standard cron
//! expression, its next fire time computed against an injected
//! [`Clock`](crate::clock::Clock) rather than the system clock read
//! directly, so scheduling decisions stay deterministic under test.
//!
//! A schedule is wall-clock time in the operator's zone: `0 7 * * *` runs at
//! 07:00 where the operator is, all year, never at 07:00 UTC. The zone is the
//! clock's ([`Clock::time_zone`](crate::clock::Clock::time_zone)), an IANA zone,
//! so daylight saving moves the instant and not the local time. Across a
//! transition `croner` applies the Open Cron Pattern Specification: a fixed
//! time the clock skips runs at the first time after the skip that day, a
//! fixed time the clock repeats runs once, at its first occurrence, and an
//! interval or wildcard schedule skips the missing hour and runs in both
//! passes of a repeated one.
//!
//! `croner`'s pattern search is generic over any date/time type implementing
//! its own `CronDateTime` trait; rather than pull in `chrono` purely to
//! satisfy that trait, [`CronInstant`] implements it for `time::OffsetDateTime`
//! in an [`OperatorTimeZone`], which resolves a local time to the instants it
//! names.

use std::cmp::Ordering;
use std::str::FromStr;

use contextos_core::{LocalResolution, OperatorTimeZone};
use croner::Cron;
use croner::errors::CronError;
use croner::time::{CivilDate, CivilDateTime, CivilTime, CronDateTime, Resolution, Weekday as CronerWeekday};
use thiserror::Error;
use time::{Duration, Month, OffsetDateTime};

/// A parsed and validated `schedule` cron expression.
#[derive(Debug, Clone)]
pub struct CronSchedule(Cron);

impl CronSchedule {
    /// Parses `expression` as a cron pattern.
    ///
    /// # Errors
    ///
    /// Returns [`CronScheduleError`] when `expression` is not a valid cron
    /// pattern.
    pub fn parse(expression: &str) -> Result<Self, CronScheduleError> {
        Cron::from_str(expression)
            .map(Self)
            .map_err(|source| CronScheduleError::InvalidPattern {
                expression: expression.to_owned(),
                source,
            })
    }

    /// The next instant strictly after `after` that this schedule fires on the wall clock of `zone`, shown at
    /// that zone's offset, or `None` when `croner` cannot find one within its own search bounds (an unsatisfiable
    /// pattern, which never fires).
    #[must_use]
    pub fn next_fire_after(&self, after: OffsetDateTime, zone: &OperatorTimeZone) -> Option<OffsetDateTime> {
        self.0
            .find_next_occurrence(
                &CronInstant {
                    instant: zone.localise(after),
                    zone,
                },
                false,
            )
            .ok()
            .map(|found| found.instant)
    }
}

#[derive(Debug, Error)]
pub enum CronScheduleError {
    #[error("schedule {expression:?} is not a valid cron expression: {source}")]
    InvalidPattern { expression: String, source: CronError },
}

/// A cron search instant: an instant shown at the offset `zone` had then, so its civil fields are the operator's
/// wall clock.
#[derive(Clone, Copy, Debug)]
struct CronInstant<'a> {
    instant: OffsetDateTime,
    zone: &'a OperatorTimeZone,
}

impl CronDateTime for CronInstant<'_> {
    fn to_civil(&self) -> CivilDateTime {
        CivilDateTime::new(
            CivilDate::from_parts_unchecked(
                self.instant.year(),
                u32::from(u8::from(self.instant.month())),
                u32::from(self.instant.day()),
            ),
            CivilTime::from_parts_unchecked(
                u32::from(self.instant.hour()),
                u32::from(self.instant.minute()),
                u32::from(self.instant.second()),
            ),
        )
    }

    fn civil_weekday(&self) -> CronerWeekday {
        CronerWeekday::from_days_from_sunday(u32::from(self.instant.weekday().number_days_from_sunday()))
    }

    fn resolve_civil(&self, civil: CivilDateTime) -> Result<Resolution<Self>, CronError> {
        let month = Month::try_from(u8::try_from(civil.month()).map_err(|_source| CronError::InvalidDate)?)
            .map_err(|_source| CronError::InvalidDate)?;
        let day = u8::try_from(civil.day()).map_err(|_source| CronError::InvalidDate)?;
        let date =
            time::Date::from_calendar_date(civil.year(), month, day).map_err(|_source| CronError::InvalidDate)?;
        let hour = u8::try_from(civil.hour()).map_err(|_source| CronError::InvalidTime)?;
        let minute = u8::try_from(civil.minute()).map_err(|_source| CronError::InvalidTime)?;
        let second = u8::try_from(civil.second()).map_err(|_source| CronError::InvalidTime)?;
        let local = date
            .with_hms(hour, minute, second)
            .map_err(|_source| CronError::InvalidTime)?;
        let at = |instant: OffsetDateTime| CronInstant {
            instant,
            zone: self.zone,
        };
        Ok(match self.zone.resolve_local(local) {
            LocalResolution::Single(instant) => Resolution::Single(at(instant)),
            LocalResolution::Repeated(earlier, later) => Resolution::Ambiguous(at(earlier), at(later)),
            LocalResolution::Gap => Resolution::Gap,
        })
    }

    fn checked_add_seconds(&self, seconds: i64) -> Option<Self> {
        self.instant
            .checked_add(Duration::seconds(seconds))
            .map(|instant| CronInstant {
                instant: self.zone.localise(instant),
                zone: self.zone,
            })
    }

    fn cmp_instant(&self, other: &Self) -> Ordering {
        self.instant.cmp(&other.instant)
    }
}

#[cfg(test)]
#[path = "schedule_test.rs"]
mod tests;
