//! Injected time source. Every cron-schedule evaluation and run timeout in
//! this crate reads "now" through this trait, never `OffsetDateTime::
//! now_utc()` directly outside [`SystemClock`], so scheduling and timeout
//! decisions stay deterministic under test (`testing.md`: "never depend on
//! ... wall-clock sleeps").

use time::OffsetDateTime;

pub trait Clock: Send + Sync {
    fn now(&self) -> OffsetDateTime;

    /// The zone this clock's readings are shown in, and the wall clock a schedule runs on: UTC for a clock with no
    /// operator zone of its own.
    fn time_zone(&self) -> contextos_core::OperatorTimeZone {
        contextos_core::OperatorTimeZone::utc()
    }

    /// `instant` (one this clock did not report, such as a cron fire time) shown in [`Self::time_zone`], at the offset
    /// that zone had then. The instant itself is unchanged.
    fn localise(&self, instant: OffsetDateTime) -> OffsetDateTime {
        self.time_zone().localise(instant)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}
