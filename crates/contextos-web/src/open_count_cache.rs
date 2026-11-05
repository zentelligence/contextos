//! A short memory of how many events need attention in each vault, so rendering a page does not read every event note
//! for a number that changes only when an event arrives.
//!
//! An entry lives for [`OPEN_COUNT_TTL`], and the event reader forgets a vault's entry the moment it sees an event that
//! may change the count ([`OpenCountCache::invalidate`]). A count worked out while an invalidation happened is not
//! stored, so a stale answer never outlives the event that made it stale.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use tokio::time::Instant;

/// How long a stored count is used before the vault is read again, for events that arrive without the reader seeing
/// them (no stream is open).
pub const OPEN_COUNT_TTL: Duration = Duration::from_secs(10);

#[derive(Default)]
struct Entry {
    /// Bumped by every invalidation.
    generation: u64,
    stored: Option<(Instant, usize)>,
}

/// What a look at the cache found.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Lookup {
    /// The stored count, when there is a fresh one.
    pub count: Option<usize>,
    /// To be handed back with the count worked out after this lookup missed.
    pub generation: u64,
}

/// The open-event count of each vault, by vault name.
#[derive(Default)]
pub struct OpenCountCache {
    entries: Mutex<HashMap<String, Entry>>,
}

impl OpenCountCache {
    /// The fresh count for `vault`, if any.
    pub fn lookup(&self, vault: &str) -> Lookup {
        let Ok(mut entries) = self.entries.lock() else {
            return Lookup {
                count: None,
                generation: 0,
            };
        };
        let entry = entries.entry(vault.to_owned()).or_default();
        let count = entry
            .stored
            .filter(|(at, _)| at.elapsed() < OPEN_COUNT_TTL)
            .map(|(_, count)| count);
        Lookup {
            count,
            generation: entry.generation,
        }
    }

    /// Stores `count` for `vault` unless the vault has been invalidated since the lookup that returned `generation`.
    pub fn store(&self, vault: &str, generation: u64, count: usize) {
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        let entry = entries.entry(vault.to_owned()).or_default();
        if entry.generation == generation {
            entry.stored = Some((Instant::now(), count));
        }
    }

    /// Forgets `vault`'s count, and refuses any count still being worked out.
    pub fn invalidate(&self, vault: &str) {
        if let Ok(mut entries) = self.entries.lock() {
            let entry = entries.entry(vault.to_owned()).or_default();
            entry.generation += 1;
            entry.stored = None;
        }
    }
}

#[cfg(test)]
#[path = "open_count_cache_test.rs"]
mod tests;
