//! Reading events through MCP: `base_query` with an inline definition, so a vault without `memory/events.base` still
//! works. Filters are built here and nowhere else; each is a pure function of its inputs, tested without a server.

use contextos_events::{SCAFFOLD_KINDS, ServerKind};
use serde::Deserialize;
use serde_json::{Map, Value};
use time::{Date, Month, OffsetDateTime, UtcOffset};

use crate::mcp_client::{McpCallError, McpClient};
use crate::signals_event::{Cursor, EVENT_NOTES_FILTER, EventRow, SignalEvent};
use crate::signals_view::KindCatalogue;

/// How many events one query may return at most.
pub const MAX_ROWS: usize = 2_000;
/// The longest run of days a query narrows by day before it scans whole month folders instead.
const MAX_NARROWED_DAYS: i64 = 40;

/// Why events could not be read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadError {
    /// The server could not be reached.
    Unreachable,
    /// The server refused the query, or answered with something unreadable.
    Refused,
}

impl From<&McpCallError> for ReadError {
    fn from(error: &McpCallError) -> Self {
        match error {
            McpCallError::Unreachable { .. } => Self::Unreachable,
            McpCallError::Refused { .. } => Self::Refused,
        }
    }
}

/// The events one query found.
#[derive(Clone, Debug, Default)]
pub struct EventBatch {
    pub events: Vec<SignalEvent>,
    /// Rows that were not readable events.
    pub skipped: usize,
    /// Where each of those rows sits, so a reader that pages through the timeline can step past them.
    pub unreadable: Vec<UnreadableRow>,
    /// Whether the server found more matches than it returned.
    pub truncated: bool,
}

/// A row that was not a readable event: its note path and when it claims to have happened, each only when readable.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct UnreadableRow {
    pub path: Option<String>,
    pub at: Option<OffsetDateTime>,
}

/// What `base_query` answers with.
#[derive(Deserialize)]
struct QueryAnswer {
    content: String,
    #[serde(default)]
    truncated: bool,
}

/// A string as a double-quoted literal in a base filter.
#[must_use]
pub fn quoted(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_owned())
}

/// The filters of the Signals feed.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FeedFilters {
    pub kind: Option<String>,
    pub severity: Option<String>,
    pub entity: Option<String>,
}

/// `property == "value"` for each filter that is set, ready to join with `&&`.
fn equals_terms(filters: &FeedFilters) -> Vec<String> {
    [
        ("kind", &filters.kind),
        ("severity", &filters.severity),
        ("entity", &filters.entity),
    ]
    .into_iter()
    .filter_map(|(property, value)| {
        value
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(|value| format!("{property} == {}", quoted(value)))
    })
    .collect()
}

/// The filter for the feed: every event, narrowed by the filters that are set.
#[must_use]
pub fn feed_filter(filters: &FeedFilters) -> String {
    join_and(EVENT_NOTES_FILTER, equals_terms(filters))
}

fn join_and(base: &str, terms: Vec<String>) -> String {
    terms
        .into_iter()
        .fold(base.to_owned(), |all, term| format!("{all} && {term}"))
}

/// The filter for warnings and errors, narrowed to `entity` when one is selected.
#[must_use]
pub fn attention_filter(entity: Option<&str>) -> String {
    let mut filter = format!("{EVENT_NOTES_FILTER} && (severity == \"warning\" || severity == \"error\")");
    if let Some(entity) = entity.filter(|entity| !entity.is_empty()) {
        filter = format!("{filter} && entity == {}", quoted(entity));
    }
    filter
}

/// What the events that may clear a set of warnings are looked for by.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Clearers {
    /// The vault paths the warnings are about: any event about one may clear it.
    pub subjects: Vec<String>,
    /// The keys of the records the warnings are about: a later event about one of them clears it. The query narrows by
    /// key alone; which base each belongs to is settled by the clearing rules over what it returns.
    pub keys: Vec<String>,
    /// The codes of the `vault.degraded` warnings: a `vault.recovered` with one clears it.
    pub codes: Vec<String>,
    /// The jobs of the `job.failed` and `job.suspended` events: a `job.recovered` for one clears it.
    pub jobs: Vec<String>,
}

/// The filter for the events that may clear the attention `clearers` describes, or `None` when nothing could.
#[must_use]
pub fn clearing_filter(clearers: &Clearers) -> Option<String> {
    let names = |values: &[String]| values.iter().map(|value| quoted(value)).collect::<Vec<_>>().join(", ");
    let mut terms = Vec::new();
    if !clearers.subjects.is_empty() {
        // A note linked by name is found by the path the server stamped beside the link.
        terms.push(format!(
            "(subject.containsAny({paths}) || subject_path.containsAny({paths}))",
            paths = names(&clearers.subjects)
        ));
    }
    if !clearers.keys.is_empty() {
        terms.push(format!("subject_id.containsAny({})", names(&clearers.keys)));
    }
    if !clearers.codes.is_empty() {
        terms.push(format!(
            "(kind == \"vault.recovered\" && code.containsAny({}))",
            names(&clearers.codes)
        ));
    }
    if !clearers.jobs.is_empty() {
        terms.push(format!(
            "(kind == \"job.recovered\" && job.containsAny({}))",
            names(&clearers.jobs)
        ));
    }
    (!terms.is_empty()).then(|| format!("{EVENT_NOTES_FILTER} && ({})", terms.join(" || ")))
}

/// The filter for warnings and errors about the records of the base `base` (a vault-relative `.base` path).
#[must_use]
pub fn attention_in_base_filter(base: &str) -> String {
    format!(
        "{EVENT_NOTES_FILTER} && (severity == \"warning\" || severity == \"error\") && subject_base == {}",
        quoted(base)
    )
}

/// The filter for events about the records of `base` whose key is one of `keys`.
#[must_use]
pub fn about_records_filter(base: &str, keys: &[String]) -> String {
    let names = keys.iter().map(|key| quoted(key)).collect::<Vec<_>>().join(", ");
    format!(
        "{EVENT_NOTES_FILTER} && subject_base == {} && subject_id.containsAny({names})",
        quoted(base)
    )
}

/// The filter for events a source records rather than the server firing them: every kind but the server's own, each
/// excluded by exact name.
#[must_use]
pub fn recorded_filter() -> String {
    let server = ServerKind::ALL
        .iter()
        .map(|kind| format!("kind != {}", quoted(kind.as_str())))
        .collect::<Vec<_>>();
    format!("{EVENT_NOTES_FILTER} && {}", server.join(" && "))
}

/// The filter for events of kinds the product does not ship, narrowed to `entity` when one is selected. Each shipped kind
/// is excluded by exact name, so a recorded kind that merely contains or begins like one is background activity.
#[must_use]
pub fn background_filter(entity: Option<&str>) -> String {
    let shipped = ServerKind::ALL
        .iter()
        .map(|kind| kind.as_str())
        .chain(SCAFFOLD_KINDS)
        .map(|kind| format!("kind != {}", quoted(kind)))
        .collect::<Vec<_>>();
    let mut filter = join_and(EVENT_NOTES_FILTER, shipped);
    if let Some(entity) = entity.filter(|entity| !entity.is_empty()) {
        filter = format!("{filter} && entity == {}", quoted(entity));
    }
    filter
}

/// The filter for events of one kind among `kinds`, newest first, for a reader that wants the last of each.
#[must_use]
pub fn kinds_filter(kinds: &[&str]) -> String {
    let names = kinds.iter().map(|kind| quoted(kind)).collect::<Vec<_>>().join(", ");
    format!("{EVENT_NOTES_FILTER} && kind.containsAny({names})")
}

/// The month folders that may hold events from the day `first` on at `now`: from that month to the current one, and
/// the previous month on the first day of a month when there is no `first`. More than three months are not narrowed.
fn months_between(first: Option<Date>, now: OffsetDateTime) -> Option<Vec<(i32, Month)>> {
    let now = now.to_offset(UtcOffset::UTC);
    let first = first.unwrap_or_else(|| {
        if now.day() == 1 {
            now.date()
                .replace_day(1)
                .ok()
                .and_then(time::Date::previous_day)
                .unwrap_or(now.date())
        } else {
            now.date()
        }
    });
    let mut months = Vec::new();
    let (mut year, mut month) = (first.year(), first.month());
    loop {
        months.push((year, month));
        if (year, month) >= (now.year(), now.month()) || months.len() > 3 {
            break;
        }
        month = month.next();
        if month == Month::January {
            year += 1;
        }
    }
    (months.len() <= 3).then_some(months)
}

/// The longest stretch of years before today that a read lists by year. A cursor older than that is read from the start
/// of the oldest listed year, so a forged one cannot make the filter list thousands of years.
const MAX_LISTED_YEARS: i32 = 100;

/// The prefixes of `at` that together cover every instant from the start of the second `from` falls in to the end of
/// the day after `now`'s: the rest of that minute by second, the rest of that hour by minute, the rest of that day by
/// hour, then each later day. When more than [`MAX_NARROWED_DAYS`] days lie between, the later days of `from`'s month
/// are listed by day and the months after it by month (the year after it by year), so there is always a lower bound
/// and the filter stays short. An event's `at` is an RFC 3339 time in UTC, so a prefix is a bound the query itself
/// applies, and a read of the newest events is never filled with older ones.
fn at_prefixes(from: OffsetDateTime, now: OffsetDateTime) -> Vec<String> {
    let today = now.to_offset(UtcOffset::UTC).date();
    // One day past today as well, for a host whose clock runs a little ahead of this one.
    let last = today.next_day().unwrap_or(today);
    let oldest_year = last.year().saturating_sub(MAX_LISTED_YEARS);
    let from = from.to_offset(UtcOffset::UTC);
    let from = if from.year() < oldest_year {
        Date::from_calendar_date(oldest_year, Month::January, 1).map_or(from, |date| date.midnight().assume_utc())
    } else {
        from
    };
    let day = from.date();
    let (hour, minute) = (from.hour(), from.minute());
    let mut prefixes = Vec::new();
    prefixes.extend((from.second()..60).map(|second| format!("{day}T{hour:02}:{minute:02}:{second:02}")));
    prefixes.extend((minute + 1..60).map(|minute| format!("{day}T{hour:02}:{minute:02}")));
    prefixes.extend((hour + 1..24).map(|hour| format!("{day}T{hour:02}")));
    if day < last {
        if (last - day).whole_days() <= MAX_NARROWED_DAYS {
            prefixes.extend(days_from(day, last).into_iter().skip(1).map(|later| later.to_string()));
        } else {
            prefixes.extend(wider_prefixes(day, last));
        }
    }
    prefixes
}

/// The prefixes that cover the days after `day` up to `last`, when they are too many to list by day: the rest of
/// `day`'s month by day, then each later month of its year by month, then each later year by year, ending with the
/// whole of `last`'s month (and year, when it is a later one).
fn wider_prefixes(day: Date, last: Date) -> Vec<String> {
    let month_end = Date::from_calendar_date(
        day.year(),
        day.month(),
        time::util::days_in_month(day.month(), day.year()),
    )
    .unwrap_or(day);
    let mut prefixes: Vec<String> = days_from(day, month_end.min(last))
        .into_iter()
        .skip(1)
        .map(|later| later.to_string())
        .collect();
    let month_number = |month: Month| u8::from(month);
    let mut year = day.year();
    let mut first_month = month_number(day.month()) + 1;
    while year <= last.year() {
        let whole_year = year > day.year() && year < last.year();
        if whole_year {
            prefixes.push(format!("{year:04}"));
        } else {
            let final_month = if year == last.year() {
                month_number(last.month())
            } else {
                12
            };
            prefixes.extend((first_month..=final_month).map(|month| format!("{year:04}-{month:02}")));
        }
        year += 1;
        first_month = 1;
    }
    prefixes
}

/// The clause that keeps a query to the month folders from the one `first` falls in (the current month, and the previous
/// on the first day of a month, when there is no `first`) to the current one; none when that is more than three months.
fn month_folders(first: Option<Date>, now: OffsetDateTime) -> Option<String> {
    let months = months_between(first, now)?;
    let folders = months
        .iter()
        .map(|(year, month)| {
            format!(
                "file.inFolder({})",
                quoted(&format!("memory/events/{year:04}/{:02}", u8::from(*month)))
            )
        })
        .collect::<Vec<_>>()
        .join(" || ");
    Some(format!("({folders})"))
}

/// `base` narrowed to the events from the start of the second `from` falls in and not among `excluded` paths. With a
/// `from`, the query keeps to the month folders from its month on and to `at` prefixes down to the second, so it is
/// always bounded below and `excluded` need only name paths in that second. Without one, no time bound is applied.
#[must_use]
pub fn since_filter(base: &str, from: Option<OffsetDateTime>, now: OffsetDateTime, excluded: &[&str]) -> String {
    let mut filter = base.to_owned();
    if let Some(from) = from {
        if let Some(folders) = month_folders(Some(from.to_offset(UtcOffset::UTC).date()), now) {
            filter = format!("{filter} && {folders}");
        }
        let prefixes = at_prefixes(from, now)
            .iter()
            .map(|prefix| quoted(prefix))
            .collect::<Vec<_>>()
            .join(", ");
        filter = format!("{filter} && at.containsAny({prefixes})");
    }
    for path in excluded {
        filter = format!("{filter} && file.path != {}", quoted(path));
    }
    filter
}

/// The filter for events that may be newer than `cursor` at `now`: from the start of the cursor's second; without a
/// cursor, the current month's folder (and the previous on the first day of a month).
#[must_use]
pub fn after_filter(cursor: Option<&Cursor>, now: OffsetDateTime) -> String {
    match cursor {
        Some(cursor) => since_filter(EVENT_NOTES_FILTER, Some(cursor.at()), now, &[]),
        None => match month_folders(None, now) {
            Some(folders) => format!("{EVENT_NOTES_FILTER} && {folders}"),
            None => EVENT_NOTES_FILTER.to_owned(),
        },
    }
}

/// Every date from `from` to `to`, both included.
fn days_from(from: Date, to: Date) -> Vec<Date> {
    let mut days = vec![from];
    let mut day = from;
    while day < to {
        let Some(next) = day.next_day() else { break };
        days.push(next);
        day = next;
    }
    days
}

/// The kind labels declared in the vault's `registry/events/`, by kind.
pub(crate) const KIND_REGISTRY_FILTER: &str = "file.inFolder(\"registry/events\") && note.type == \"event-kind\"";

/// Which end of the timeline a read starts from.
#[derive(Clone, Copy)]
enum Direction {
    Newest,
    Oldest,
}

/// Event reads against one vault.
pub struct EventsApi<'a> {
    pub client: &'a McpClient,
    pub vault_name: &'a str,
}

impl EventsApi<'_> {
    async fn rows(
        &self,
        filter: &str,
        order: &[String],
        sort: Value,
        limit: usize,
    ) -> Result<(Vec<Value>, bool), ReadError> {
        let mut arguments = Map::new();
        arguments.insert("vault".to_owned(), Value::String(format!("{}://.", self.vault_name)));
        arguments.insert("format".to_owned(), Value::String("json".to_owned()));
        arguments.insert("limit".to_owned(), serde_json::json!(limit.min(MAX_ROWS)));
        arguments.insert(
            "definition".to_owned(),
            serde_json::json!({ "filters": filter, "order": order, "sort": sort }),
        );
        let result = self
            .client
            .call_tool("base_query".to_owned(), arguments)
            .await
            .map_err(|error| ReadError::from(&error))?;
        if result.is_error == Some(true) {
            return Err(ReadError::Refused);
        }
        let answer = result.into_typed::<QueryAnswer>().map_err(|_| ReadError::Refused)?;
        let rows: Vec<Value> = serde_json::from_str(&answer.content).map_err(|_| ReadError::Refused)?;
        Ok((rows, answer.truncated))
    }

    async fn events(
        &self,
        filter: &str,
        limit: usize,
        columns: &[String],
        direction: Direction,
    ) -> Result<EventBatch, ReadError> {
        let way = match direction {
            Direction::Newest => "DESC",
            Direction::Oldest => "ASC",
        };
        let sort = serde_json::json!([
            {"property": "at", "direction": way},
            {"property": "file.path", "direction": way}
        ]);
        let (rows, truncated) = self.rows(filter, columns, sort, limit).await?;
        let mut batch = EventBatch {
            truncated,
            ..EventBatch::default()
        };
        for row in rows {
            let row = serde_json::from_value::<EventRow>(row).ok();
            let place = row.as_ref().map(UnreadableRow::from);
            if let Some(event) = row.and_then(|row| SignalEvent::try_from(row).ok()) {
                batch.events.push(event);
            } else {
                batch.skipped += 1;
                batch.unreadable.push(place.unwrap_or_default());
            }
        }
        match direction {
            Direction::Newest => batch.events.sort_by_key(|event| std::cmp::Reverse(event.cursor())),
            Direction::Oldest => batch.events.sort_by_key(SignalEvent::cursor),
        }
        Ok(batch)
    }

    /// The events matching `filter`, newest first (by `at`, then path), at most `limit`, with the fields in `columns`
    /// read as query columns.
    ///
    /// # Errors
    ///
    /// Returns why the events could not be read.
    pub async fn newest_first(&self, filter: &str, limit: usize, columns: &[String]) -> Result<EventBatch, ReadError> {
        self.events(filter, limit, columns, Direction::Newest).await
    }

    /// The events matching `filter`, oldest first, at most `limit`: when more match, it is the newest that are left for
    /// the next read, so a reader that follows the timeline never skips one.
    ///
    /// # Errors
    ///
    /// Returns why the events could not be read.
    pub async fn oldest_first(&self, filter: &str, limit: usize, columns: &[String]) -> Result<EventBatch, ReadError> {
        self.events(filter, limit, columns, Direction::Oldest).await
    }

    /// The events matching `base` from the start of the second `from` falls in (from the beginning when `None`), oldest
    /// first, read page by page until none remain or `ceiling` have been found, so a reader that follows the timeline
    /// is never held up by more matching events than one query returns, whether they are many earlier in the day or
    /// many in one second. `truncated` says events were left beyond `ceiling`: the newest.
    ///
    /// # Errors
    ///
    /// Returns why the events could not be read.
    pub async fn oldest_since(
        &self,
        base: &str,
        from: Option<OffsetDateTime>,
        now: OffsetDateTime,
        ceiling: usize,
        columns: &[String],
    ) -> Result<EventBatch, ReadError> {
        crate::signals_pager::oldest_since(self, MAX_ROWS, base, from, now, ceiling, columns).await
    }

    /// What the vault's kind registry declares: each kind's label and the fields its definition names. A vault with no
    /// registry declares none. Read once per request or poll, so the fields a feed shows are read as columns of the
    /// event query and never one read per event.
    ///
    /// # Errors
    ///
    /// Returns why the registry could not be read.
    pub async fn kind_catalogue(&self) -> Result<KindCatalogue, ReadError> {
        let columns = ["kind", "label", "fields", "id_field"].map(str::to_owned);
        let (rows, _) = self
            .rows(KIND_REGISTRY_FILTER, &columns, serde_json::json!([]), MAX_ROWS)
            .await?;
        let mut catalogue = KindCatalogue::default();
        for row in rows {
            let text = |name: &str| {
                row.get(name)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
            };
            let (Some(kind), Some(label)) = (text("kind"), text("label")) else {
                continue;
            };
            catalogue.declare(kind, label, declared_fields(&row));
        }
        Ok(catalogue)
    }
}

/// The field names a kind definition row declares: required, then optional, then the server-assigned id field.
fn declared_fields(row: &Value) -> Vec<String> {
    let list = |key: &str| -> Vec<String> {
        row.get("fields")
            .and_then(|fields| fields.get(key))
            .and_then(Value::as_array)
            .map(|names| names.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_default()
    };
    let mut names = list("required");
    names.extend(list("optional"));
    names.extend(row.get("id_field").and_then(Value::as_str).map(str::to_owned));
    names
}

#[cfg(test)]
#[path = "signals_query_test.rs"]
mod tests;
