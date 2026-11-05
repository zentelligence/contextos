//! Reading the timeline oldest first, page by page. The paging is generic over where a page comes from, so it is tested
//! against a stand-in for the server without thousands of notes.

use std::collections::HashSet;

use time::OffsetDateTime;

use crate::signals_query::{EventBatch, EventsApi, ReadError, since_filter};

/// Where one page of events, oldest first, comes from.
pub(crate) trait OldestPages {
    /// The events matching `filter`, oldest first, at most `limit`.
    async fn page(&self, filter: &str, limit: usize, columns: &[String]) -> Result<EventBatch, ReadError>;
}

impl OldestPages for EventsApi<'_> {
    async fn page(&self, filter: &str, limit: usize, columns: &[String]) -> Result<EventBatch, ReadError> {
        self.oldest_first(filter, limit, columns).await
    }
}

/// A note the pager has read and must not read again: an event or an unreadable row.
struct Held<'a> {
    path: &'a str,
    /// When it happened, where that is known.
    at: Option<OffsetDateTime>,
}

/// The start of the second `at` falls in.
fn second_of(at: OffsetDateTime) -> OffsetDateTime {
    at.replace_nanosecond(0).unwrap_or(at)
}

/// The paths a query from `position` must leave out because they were read already and the query would find them
/// again: those in the second `position` falls in, and any whose time is unknown. A read that has no position yet
/// leaves out everything it holds.
fn exclusions<'a>(held: &[Held<'a>], position: Option<OffsetDateTime>) -> Vec<&'a str> {
    let second = position.map(second_of);
    held.iter()
        .filter(|row| match (row.at, second) {
            (Some(at), Some(second)) => at >= second,
            _ => true,
        })
        .map(|row| row.path)
        .collect()
}

/// See [`EventsApi::oldest_since`]; `page_size` is the most one query returns.
///
/// Every page after the first starts from the second of the newest row read, so its exclusions only ever cover that
/// second. Unreadable rows count as progress and are read, and counted, once. A page that reads nothing new ends the
/// read as cut short rather than looping.
pub(crate) async fn oldest_since<S: OldestPages>(
    source: &S,
    page_size: usize,
    base: &str,
    from: Option<OffsetDateTime>,
    now: OffsetDateTime,
    ceiling: usize,
    columns: &[String],
) -> Result<EventBatch, ReadError> {
    let mut found = EventBatch::default();
    let mut position = from;
    let mut counted: HashSet<String> = HashSet::new();
    loop {
        let page = ceiling.saturating_sub(found.events.len()).min(page_size);
        if page == 0 {
            found.truncated = true;
            break;
        }
        let held: Vec<Held<'_>> = found
            .events
            .iter()
            .map(|event| Held {
                path: &event.path,
                at: Some(event.at),
            })
            .chain(
                found
                    .unreadable
                    .iter()
                    .filter_map(|row| row.path.as_deref().map(|path| Held { path, at: row.at })),
            )
            .collect();
        let filter = since_filter(base, position, now, &exclusions(&held, position));
        let batch = source.page(&filter, page, columns).await?;
        let more = batch.truncated || batch.events.len() + batch.skipped >= page;
        let mut progressed = !batch.events.is_empty();
        let newest = batch
            .events
            .iter()
            .map(|event| event.at)
            .chain(batch.unreadable.iter().filter_map(|row| row.at))
            .max();
        found.events.extend(batch.events);
        for row in batch.unreadable {
            if row.path.as_ref().is_none_or(|path| counted.insert(path.clone())) {
                progressed |= row.path.is_some();
                found.skipped += 1;
                found.unreadable.push(row);
            }
        }
        found.truncated = more;
        if !more || !progressed {
            break;
        }
        position = position.max(newest);
    }
    found.events.sort_by_key(crate::signals_event::SignalEvent::cursor);
    Ok(found)
}

#[cfg(test)]
#[path = "signals_pager_test.rs"]
mod tests;
