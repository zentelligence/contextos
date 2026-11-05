//! The time source behind every instant the server reads: the system clock when serving, or one a caller injects so
//! that anything that depends on today's date can be made deterministic.

use std::fmt;
use std::sync::Arc;

use contextos_core::Clock;
use time::OffsetDateTime;

/// A shareable source of the current instant.
#[derive(Clone)]
pub struct ServerClock {
    now: Arc<dyn Fn() -> OffsetDateTime + Send + Sync>,
}

impl Default for ServerClock {
    /// The system clock.
    fn default() -> Self {
        Self::from(OffsetDateTime::now_utc)
    }
}

impl fmt::Debug for ServerClock {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ServerClock")
    }
}

impl<F> From<F> for ServerClock
where
    F: Fn() -> OffsetDateTime + Send + Sync + 'static,
{
    fn from(now: F) -> Self {
        Self { now: Arc::new(now) }
    }
}

impl Clock for ServerClock {
    fn now(&self) -> OffsetDateTime {
        (self.now)()
    }
}

#[cfg(test)]
#[path = "server_clock_test.rs"]
mod tests;
