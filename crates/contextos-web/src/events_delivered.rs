//! Which events a reader or a stream has already delivered, when several events can share a second and their paths do
//! not sort in the order they were written.
//!
//! A cursor alone cannot say whether an event is new: one written later in the same second as the newest delivered event
//! may have a lower path. So the newest second is read again inclusively and the paths already delivered in it, and in
//! any later second, are remembered; nothing older than the newest second needs remembering, because every read starts
//! at that second.

use std::collections::HashMap;

use time::OffsetDateTime;

use crate::signals_event::Cursor;

/// The start of the whole second `at` falls in.
fn whole_second(at: OffsetDateTime) -> OffsetDateTime {
    at.replace_nanosecond(0).unwrap_or(at)
}

/// The events delivered at or after the second of the newest one, by path.
#[derive(Clone, Debug, Default)]
pub struct Delivered {
    /// The start of the newest delivered event's second; every read starts here.
    floor: Option<OffsetDateTime>,
    /// The paths delivered in that second or later, with when each happened.
    paths: HashMap<String, OffsetDateTime>,
}

impl From<OffsetDateTime> for Delivered {
    /// Nothing delivered, reading from the start of the whole second `at` falls in.
    fn from(at: OffsetDateTime) -> Self {
        Self {
            floor: Some(whole_second(at)),
            paths: HashMap::new(),
        }
    }
}

impl Delivered {
    /// Where the next read starts: the start of the newest delivered event's second, or `None` before any delivery.
    #[must_use]
    pub const fn read_from(&self) -> Option<OffsetDateTime> {
        self.floor
    }

    /// Whether the event at `cursor` has not been delivered. One older than the second reads start from was delivered
    /// or is too old to deliver, so it is not new.
    #[must_use]
    pub fn is_new(&self, cursor: &Cursor) -> bool {
        self.floor.is_none_or(|floor| cursor.at() >= floor) && !self.paths.contains_key(cursor.path())
    }

    /// Remembers that the event at `cursor` was delivered, moving the second reads start from forward when it is the
    /// newest, and forgetting what is now older than that second.
    pub fn record(&mut self, cursor: &Cursor) {
        let second = whole_second(cursor.at());
        if self.floor.is_none_or(|floor| second > floor) {
            self.floor = Some(second);
        }
        self.paths.insert(cursor.path().to_owned(), cursor.at());
        if let Some(floor) = self.floor {
            self.paths.retain(|_, at| *at >= floor);
        }
    }
}

#[cfg(test)]
#[path = "events_delivered_test.rs"]
mod tests;
