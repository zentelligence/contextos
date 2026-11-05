use std::collections::BTreeMap;
use std::sync::Mutex;

use time::Duration;
use time::format_description::well_known::Rfc3339;
use time::macros::datetime;

use super::*;
use crate::signals_event::{Severity, SignalEvent};
use crate::signals_query::{MAX_ROWS, UnreadableRow};

const NOW: OffsetDateTime = datetime!(2026-10-28 12:00:00 UTC);

struct Stored {
    path: String,
    at: String,
    readable: bool,
}

/// A stand-in for the server: it applies the lower bound (`at.containsAny`) and the path exclusions of a filter,
/// orders by `at` then path, and returns at most `limit` rows, saying whether more matched.
#[derive(Default)]
struct FakeServer {
    rows: Vec<Stored>,
    filters: Mutex<Vec<String>>,
}

impl FakeServer {
    fn event(&mut self, path: &str, at: OffsetDateTime) {
        self.push(path, at.format(&Rfc3339).unwrap_or_default(), true);
    }

    fn unreadable(&mut self, path: &str, at: &str) {
        self.push(path, at.to_owned(), false);
    }

    fn push(&mut self, path: &str, at: String, readable: bool) {
        self.rows.push(Stored {
            path: path.to_owned(),
            at,
            readable,
        });
    }

    fn filters(&self) -> Vec<String> {
        self.filters.lock().map(|filters| filters.clone()).unwrap_or_default()
    }
}

/// The quoted items between `open` and the next `)` in `filter`.
fn quoted_list(filter: &str, open: &str) -> Option<Vec<String>> {
    let start = filter.find(open)? + open.len();
    let end = start + filter[start..].find(')')?;
    Some(
        filter[start..end]
            .split(", ")
            .map(|item| item.trim_matches('"').to_owned())
            .collect(),
    )
}

fn excluded(filter: &str) -> Vec<String> {
    filter
        .split(" && file.path != \"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next().map(str::to_owned))
        .collect()
}

impl OldestPages for FakeServer {
    fn page(
        &self,
        filter: &str,
        limit: usize,
        _columns: &[String],
    ) -> impl Future<Output = Result<EventBatch, ReadError>> {
        std::future::ready(Ok(self.respond(filter, limit)))
    }
}

impl FakeServer {
    fn respond(&self, filter: &str, limit: usize) -> EventBatch {
        if let Ok(mut filters) = self.filters.lock() {
            filters.push(filter.to_owned());
        }
        let prefixes = quoted_list(filter, "at.containsAny(");
        let excluded = excluded(filter);
        let mut matching: Vec<&Stored> = self
            .rows
            .iter()
            .filter(|row| {
                prefixes
                    .as_ref()
                    .is_none_or(|prefixes| prefixes.iter().any(|prefix| row.at.starts_with(prefix.as_str())))
            })
            .filter(|row| !excluded.contains(&row.path))
            .collect();
        matching.sort_by(|a, b| (&a.at, &a.path).cmp(&(&b.at, &b.path)));
        let truncated = matching.len() > limit;
        let mut batch = EventBatch {
            truncated,
            ..EventBatch::default()
        };
        for row in matching.into_iter().take(limit) {
            let at = OffsetDateTime::parse(&row.at, &Rfc3339).ok();
            if let (true, Some(at)) = (row.readable, at) {
                batch.events.push(signal(&row.path, at));
            } else {
                batch.skipped += 1;
                batch.unreadable.push(UnreadableRow {
                    path: Some(row.path.clone()),
                    at,
                });
            }
        }
        batch
    }
}

fn signal(path: &str, at: OffsetDateTime) -> SignalEvent {
    SignalEvent {
        path: path.to_owned(),
        kind: "task.stuck".to_owned(),
        severity: Severity::Warning,
        at,
        entity: "vault".to_owned(),
        subject: None,
        subject_id: None,
        subject_path: None,
        subject_base: None,
        actor: None,
        fields: BTreeMap::new(),
    }
}

/// `count` events one second apart from `first`, named in time order.
fn spread(server: &mut FakeServer, first: OffsetDateTime, count: usize) {
    for index in 0..count {
        let offset = i64::try_from(index).unwrap_or(0);
        server.event(&format!("e{index:05}.md"), first + Duration::seconds(offset));
    }
}

async fn read(
    server: &FakeServer,
    page_size: usize,
    from: Option<OffsetDateTime>,
    ceiling: usize,
) -> Result<EventBatch, ReadError> {
    oldest_since(server, page_size, "base", from, NOW, ceiling, &[]).await
}

fn most_excluded(server: &FakeServer) -> usize {
    server
        .filters()
        .iter()
        .map(|filter| excluded(filter).len())
        .max()
        .unwrap_or(0)
}

#[tokio::test]
async fn more_open_warnings_than_one_page_holds_are_all_counted_without_re_excluding_the_ones_before()
-> Result<(), ReadError> {
    let mut server = FakeServer::default();
    spread(&mut server, datetime!(2026-10-28 00:00:00 UTC), MAX_ROWS + 500);

    let found = read(&server, MAX_ROWS, None, 10_000).await?;

    assert_eq!(found.events.len(), MAX_ROWS + 500);
    assert!(!found.truncated);
    assert!(server.filters().len() >= 2, "one page cannot hold them all");
    assert!(
        most_excluded(&server) <= 1,
        "only the cursor's own second may be excluded, not every path seen so far"
    );
    Ok(())
}

#[tokio::test]
async fn more_events_in_one_second_than_one_page_holds_do_not_hide_a_later_event() -> Result<(), ReadError> {
    let second = datetime!(2026-10-28 09:00:00 UTC);
    let mut server = FakeServer::default();
    for index in 0..(MAX_ROWS + 100) {
        server.event(&format!("same{index:05}.md"), second);
    }
    server.event("later.md", second + Duration::hours(1));

    let found = read(&server, MAX_ROWS, Some(second), 10_000).await?;

    assert_eq!(found.events.len(), MAX_ROWS + 101);
    assert_eq!(found.events.last().map(|event| event.path.as_str()), Some("later.md"));
    assert!(!found.truncated);
    Ok(())
}

#[tokio::test]
async fn an_unbounded_first_page_continues_into_a_bounded_second_page() -> Result<(), ReadError> {
    let mut server = FakeServer::default();
    spread(&mut server, datetime!(2026-10-28 00:00:00 UTC), MAX_ROWS + 10);

    let found = read(&server, MAX_ROWS, None, 10_000).await?;

    let filters = server.filters();
    assert_eq!(found.events.len(), MAX_ROWS + 10);
    assert!(!filters[0].contains("at.containsAny"), "{}", filters[0]);
    assert!(filters[1].contains("at.containsAny"), "{}", filters[1]);
    Ok(())
}

#[tokio::test]
async fn a_read_stops_at_the_ceiling_with_the_oldest_events_and_says_it_was_cut_short() -> Result<(), ReadError> {
    let mut server = FakeServer::default();
    spread(&mut server, datetime!(2026-10-28 00:00:00 UTC), MAX_ROWS + 500);

    let beyond_one_page = read(&server, MAX_ROWS, None, MAX_ROWS + 200).await?;
    assert_eq!(beyond_one_page.events.len(), MAX_ROWS + 200);
    assert!(beyond_one_page.truncated);
    assert_eq!(
        beyond_one_page.events.last().map(|event| event.path.as_str()),
        Some(format!("e{:05}.md", MAX_ROWS + 199).as_str())
    );

    let within_one_page = read(&server, MAX_ROWS, None, 100).await?;
    assert_eq!(within_one_page.events.len(), 100);
    assert!(within_one_page.truncated);
    Ok(())
}

#[tokio::test]
async fn a_read_from_a_cursor_older_than_the_day_window_is_still_bounded_below() -> Result<(), ReadError> {
    let from = NOW - Duration::days(60);
    let mut server = FakeServer::default();
    spread(&mut server, from, MAX_ROWS + 500);

    let found = read(&server, MAX_ROWS, Some(from), 10_000).await?;

    assert_eq!(found.events.len(), MAX_ROWS + 500);
    for filter in server.filters() {
        assert!(filter.contains("at.containsAny"), "{filter}");
    }
    assert!(
        most_excluded(&server) <= 1,
        "a quiet vault read after a long gap must not exclude every path it has seen"
    );
    Ok(())
}

#[tokio::test]
async fn a_page_of_nothing_but_unreadable_rows_is_stepped_past_and_each_row_counted_once() -> Result<(), ReadError> {
    let mut server = FakeServer::default();
    for index in 0..7 {
        server.unreadable(&format!("u{index}.md"), &format!("2026-10-28T08:00:0{index}Z"));
    }
    server.unreadable("no-time.md", "2026-10-28T08:30:00 bogus");
    server.event("good1.md", datetime!(2026-10-28 09:00:00 UTC));
    server.event("good2.md", datetime!(2026-10-28 09:00:01 UTC));

    let found = read(&server, 3, None, 100).await?;

    assert_eq!(
        found.events.len(),
        2,
        "the events after the unreadable rows are reached"
    );
    assert_eq!(found.skipped, 8);
    assert!(!found.truncated);
    Ok(())
}

#[tokio::test]
async fn an_unreadable_row_in_the_cursors_second_is_not_read_and_counted_again() -> Result<(), ReadError> {
    let second = datetime!(2026-10-28 09:00:00 UTC);
    let mut server = FakeServer::default();
    server.event("a.md", second);
    server.unreadable("b.md", "2026-10-28T09:00:00Z");
    server.event("c.md", second);
    server.event("d.md", second + Duration::minutes(5));

    let found = read(&server, 2, Some(second), 100).await?;

    assert_eq!(found.events.len(), 3);
    assert_eq!(found.skipped, 1, "the unreadable row is counted once");
    Ok(())
}
