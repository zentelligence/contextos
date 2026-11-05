//! Loading the Signals screen's regions: the queries each makes and what it does with the answer. The rules for what
//! needs attention are [`crate::signals_attention`]; how a region looks is [`crate::signals_view`].

use std::collections::HashMap;

use time::OffsetDateTime;

use crate::inbox_outbox::encode_query_value;
use crate::signals_attention::{open_attention, task_indicator};
use crate::signals_event::{DECISION_COLUMNS, Severity, SignalEvent};
use crate::signals_query::{
    Clearers, EventsApi, FeedFilters, ReadError, about_records_filter, attention_filter, attention_in_base_filter,
    background_filter, clearing_filter, feed_filter,
};
use crate::signals_view::{
    ATTENTION_LIMIT, AttentionView, BackgroundView, EventLine, FEED_PAGE_SIZE, FeedView, KindCatalogue,
};
use crate::urls::VaultUrls;

/// The most warning, error, and clearing events one read of what is open takes, across as many queries as that needs.
/// The retention sweep bounds how many there are; this only stops a vault that has none from being read without end.
const ATTENTION_CEILING: usize = 50_000;
/// How many events the background panel reads.
const BACKGROUND_EVENTS: usize = 300;

/// What the feed is asked for.
#[derive(Clone, Debug, Default)]
pub struct FeedRequest {
    pub filters: FeedFilters,
    /// 1-based.
    pub page: usize,
}

/// The query string that selects `request` at `page`, already encoded.
fn feed_query(filters: &FeedFilters, page: usize) -> String {
    let mut pairs = Vec::new();
    for (name, value) in [
        ("entity", &filters.entity),
        ("kind", &filters.kind),
        ("severity", &filters.severity),
    ] {
        if let Some(value) = value.as_deref().filter(|value| !value.is_empty()) {
            pairs.push(format!("{name}={}", encode_query_value(value)));
        }
    }
    if page > 1 {
        pairs.push(format!("page={page}"));
    }
    pairs.join("&")
}

/// One page of the feed, newest first.
///
/// # Errors
///
/// Returns why the events could not be read.
pub async fn load_feed(
    api: &EventsApi<'_>,
    urls: &VaultUrls,
    labels: &KindCatalogue,
    request: &FeedRequest,
) -> Result<FeedView, ReadError> {
    let page = request.page.max(1);
    let wanted = page * FEED_PAGE_SIZE + 1;
    let batch = api
        .newest_first(&feed_filter(&request.filters), wanted, &labels.columns())
        .await?;
    let start = (page - 1) * FEED_PAGE_SIZE;
    let has_older = batch.events.len() > page * FEED_PAGE_SIZE;
    let shown: Vec<&SignalEvent> = batch.events.iter().skip(start).take(FEED_PAGE_SIZE).collect();
    let mut kinds = labels.kinds();
    for event in shown.iter().copied() {
        if !kinds.contains(&event.kind) {
            kinds.push(event.kind.clone());
        }
    }
    let selected_kind = request.filters.kind.clone().unwrap_or_default();
    if !selected_kind.is_empty() && !kinds.contains(&selected_kind) {
        kinds.push(selected_kind.clone());
    }
    kinds.sort();
    let selected_severity = request.filters.severity.clone().unwrap_or_default();
    Ok(FeedView {
        urls: urls.clone(),
        lines: shown.iter().map(|event| EventLine::new(urls, event, labels)).collect(),
        kinds,
        filtered: !selected_kind.is_empty() || !selected_severity.is_empty(),
        selected_kind,
        selected_severity,
        entity: request.filters.entity.clone().filter(|entity| !entity.is_empty()),
        skipped: batch.skipped,
        page,
        has_older,
        older_query: feed_query(&request.filters, page + 1),
        newer_query: feed_query(&request.filters, page.saturating_sub(1).max(1)),
        newest_cursor: batch.events.first().map(|event| event.cursor().to_string()),
        refresh_href: urls.signals_region("feed", &feed_query(&request.filters, page)),
    })
}

/// The events that still need attention, newest first, with the fields in `columns` read.
///
/// Every warning and error is read, page by page, and then the events that may have cleared them: only those about the
/// warnings' own subjects, codes, and jobs, and only from the oldest warning on. `now` only narrows the queries; the
/// answer does not depend on it.
///
/// # Errors
///
/// Returns why the events could not be read.
pub async fn open_events(
    api: &EventsApi<'_>,
    entity: Option<&str>,
    columns: &[String],
    now: OffsetDateTime,
) -> Result<Vec<SignalEvent>, ReadError> {
    let found = api
        .oldest_since(&attention_filter(entity), None, now, ATTENTION_CEILING, columns)
        .await?;
    if found.truncated {
        tracing::warn!(
            "more warnings and errors exist than one read takes, so the oldest are judged and the newest left out"
        );
    }
    let mut candidates = found.events;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let oldest = candidates.first().map(|event| event.at);
    let mut pool = match clearing_filter(&clearers_of(&candidates)) {
        Some(filter) => {
            api.oldest_since(&filter, oldest, now, ATTENTION_CEILING, &decision_columns())
                .await?
                .events
        }
        None => Vec::new(),
    };
    pool.extend(candidates.iter().cloned());
    candidates.reverse();
    Ok(open_attention(&candidates, &pool).into_iter().cloned().collect())
}

/// What may clear `candidates`: their subjects, the codes of the degraded vault ones, and the jobs of the job ones.
fn clearers_of(candidates: &[SignalEvent]) -> Clearers {
    let mut clearers = Clearers::default();
    for event in candidates {
        match event.subject_identity() {
            Some(contextos_events::SubjectIdentity::Record { .. }) => clearers.keys.extend(event.subject_id.clone()),
            Some(contextos_events::SubjectIdentity::Note { path }) => clearers.subjects.push(path),
            None => {}
        }
        match event.kind.as_str() {
            "vault.degraded" => clearers.codes.extend(event.field("code").map(str::to_owned)),
            "job.failed" | "job.suspended" => clearers.jobs.extend(event.field("job").map(str::to_owned)),
            _ => {}
        }
    }
    for list in [
        &mut clearers.subjects,
        &mut clearers.keys,
        &mut clearers.codes,
        &mut clearers.jobs,
    ] {
        list.sort();
        list.dedup();
    }
    clearers
}

/// The columns a reader that only decides what is open reads.
#[must_use]
pub fn decision_columns() -> Vec<String> {
    DECISION_COLUMNS.iter().map(|column| (*column).to_owned()).collect()
}

/// The tasks whose most recent event is a warning or an error, by the task's key upper-cased, with that event's
/// severity. A task is matched by its key within the tasks base, never by its path, so moving it keeps its indicator. A
/// later event of any kind about a task clears it, except `task.moved`. Empty when events cannot be read, so a screen
/// that shows the indicator never fails over it.
pub async fn task_attention(api: &EventsApi<'_>, tasks_base: &str, now: OffsetDateTime) -> HashMap<String, Severity> {
    let Ok(candidates) = api
        .oldest_since(
            &attention_in_base_filter(tasks_base),
            None,
            now,
            ATTENTION_CEILING,
            &decision_columns(),
        )
        .await
    else {
        return HashMap::new();
    };
    let mut keys: Vec<String> = candidates
        .events
        .iter()
        .filter_map(|event| event.subject_id.clone())
        .collect();
    keys.sort();
    keys.dedup();
    let Some(oldest) = candidates.events.first().map(|event| event.at) else {
        return HashMap::new();
    };
    if keys.is_empty() {
        return HashMap::new();
    }
    let Ok(about) = api
        .oldest_since(
            &about_records_filter(tasks_base, &keys),
            Some(oldest),
            now,
            ATTENTION_CEILING,
            &decision_columns(),
        )
        .await
    else {
        return HashMap::new();
    };
    let mut by_task: HashMap<String, Vec<SignalEvent>> = HashMap::new();
    for event in about.events {
        if let Some(contextos_events::SubjectIdentity::Record { base, key }) = event.subject_identity()
            && base == tasks_base
        {
            by_task.entry(key).or_default().push(event);
        }
    }
    by_task
        .into_iter()
        .filter_map(|(task, events)| task_indicator(&events).map(|severity| (task, severity)))
        .collect()
}

/// The attention region.
///
/// # Errors
///
/// Returns why the events could not be read.
pub async fn load_attention(
    api: &EventsApi<'_>,
    urls: &VaultUrls,
    labels: &KindCatalogue,
    entity: Option<&str>,
    now: OffsetDateTime,
) -> Result<AttentionView, ReadError> {
    let lookup = api.client.open_counts().lookup(api.vault_name);
    let mut open = open_events(api, entity, &labels.columns(), now).await?;
    if entity.is_none_or(str::is_empty) {
        // The panel has just worked out what the sidebar count needs, so a page that shows both reads the events once.
        api.client
            .open_counts()
            .store(api.vault_name, lookup.generation, open.len());
    }
    let more = open.len().saturating_sub(ATTENTION_LIMIT);
    open.truncate(ATTENTION_LIMIT);
    let refresh_query = entity
        .filter(|entity| !entity.is_empty())
        .map(|entity| format!("entity={}", encode_query_value(entity)))
        .unwrap_or_default();
    Ok(AttentionView {
        urls: urls.clone(),
        refresh_href: urls.signals_region("attention", &refresh_query),
        lines: open
            .iter()
            .take(ATTENTION_LIMIT)
            .map(|event| EventLine::new(urls, event, labels).without_id())
            .collect(),
        entity: entity.map(str::to_owned),
        more,
        unreadable: false,
    })
}

/// The background activity region.
///
/// # Errors
///
/// Returns why the events could not be read.
pub async fn load_background(
    api: &EventsApi<'_>,
    urls: &VaultUrls,
    labels: &KindCatalogue,
    entity: Option<&str>,
) -> Result<BackgroundView, ReadError> {
    let batch = api
        .newest_first(&background_filter(entity), BACKGROUND_EVENTS, &labels.columns())
        .await?;
    Ok(BackgroundView::new(
        urls.clone(),
        &batch.events,
        labels,
        entity.map(str::to_owned),
    ))
}

#[cfg(test)]
#[path = "signals_load_test.rs"]
mod tests;
