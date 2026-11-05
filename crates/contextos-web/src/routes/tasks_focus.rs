//! The Tasks screen's own Focus view (the default landing view,
//! `tasks.rs`'s own `ViewMode::Focus`): the design mock's four fixed
//! triage sections (`tasks_config::Bucket`), each populated by scanning
//! whichever real, vault-configured stages of each task type's own
//! lifecycle currently bucket to it, never a hardcoded stage id. A
//! vault's lifecycles can (and, in `TasksConfig::default`, do) put more
//! than one stage in the same bucket, so every section and every "browse
//! all"/"view all"/overflow link this module builds targets the whole set
//! of that bucket's own stages at once, as `type:state` keys comma-joined
//! in the shared `?stage=` query parameter `tasks::flat_rows` and this
//! module's own drill-down both already understand as "any of these
//! stages".
//!
//! Kept apart from `routes::tasks` itself so that module's own Table/
//! Hierarchy rendering, row fetch, and route handler stay well clear of
//! the file-size limit.
//!
//! Deliberately builds no aggregate "N blocked" indicator and no
//! per-card "blocked" badge derived from `state`: the vault's own
//! governing requirement for that cross-cutting indicator says it must be
//! sourced from a proper events mechanism, never derived directly from a
//! task's own `state` field, and must not render at all until that events
//! mechanism exists.

use std::collections::HashMap;

use crate::entities::EntityEntry;
use crate::tasks_config::{Bucket, StageKey, TasksConfig};

use super::tasks::{StageOverride, TaskRow, TasksQuery, ViewMode, accent_for, entity_accents, tasks_href};
use super::tasks_strip::{OTHER_TYPES_LABEL, stage_matches};

/// The Focus mock's own working-in-progress cap on the "Now" grid
/// (`TasksScreen.jsx`'s `WIP_LIMIT`).
const WIP_LIMIT: usize = 6;
/// The Backlog and Done side panels each show at most this many rows
/// before "Browse all"/"View all" is the only way to see the rest
/// (`TasksScreen.jsx`'s own `.slice(0, 3)`).
const PANEL_LIMIT: usize = 3;
/// The "Pull next task" button's own title truncation
/// (`TasksScreen.jsx`'s own `t.title.slice(0, 40) + '…'`).
const BUTTON_TITLE_LIMIT: usize = 40;

/// One card in the "Now"/"Needs your call" grids, or in the single-stage
/// drill-down grid.
pub(super) struct TaskCardView {
    pub(super) id: String,
    pub(super) title: String,
    pub(super) state_label: String,
    /// The colour `state`'s chip is shown in ([`TasksConfig::stage_colour`]).
    pub(super) state_colour: &'static str,
    pub(super) href: String,
    pub(super) assignee: String,
    pub(super) due: String,
    /// The `--entity-N` accent (1-6) this row's own `entity` resolves to;
    /// the template applies it as this card's own left border colour.
    pub(super) entity_accent: u8,
    /// `warning` or `error` while the task's most recent event is one ([`TaskRow::attention`]).
    pub(super) attention: Option<&'static str>,
}

/// One compact row in the Backlog/Done side panels: no status chip (the
/// panel itself already names the bucket), a left accent border, and
/// either a priority badge (Backlog) or a checkmark (Done, `done: true`).
pub(super) struct MiniRow {
    pub(super) title: String,
    pub(super) href: String,
    pub(super) entity_accent: u8,
    pub(super) priority: String,
    pub(super) done: bool,
    /// `warning` or `error` while the task's most recent event is one ([`TaskRow::attention`]).
    pub(super) attention: Option<&'static str>,
}

/// The "Now" grid's own overflow past [`WIP_LIMIT`]: how many more rows
/// exist, and where clicking through lands (the single-stage/bucket
/// drill-down for every state bucketed to `now`).
pub(super) struct Overflow {
    pub(super) count: usize,
    pub(super) href: String,
}

/// Everything `templates/tasks_focus.html` renders. Either `drill_label`
/// is `Some` (a `?stage=` selection is active: one card grid, no
/// sections), or every other field describes the four bucketed sections;
/// never both, matching `TasksScreen.jsx`'s own `stageFilter ? ... : ...`
/// branch.
pub(super) struct FocusData {
    pub(super) drill_label: Option<String>,
    pub(super) drill_cards: Vec<TaskCardView>,
    pub(super) clear_filter_href: String,

    pub(super) now_cards: Vec<TaskCardView>,
    pub(super) now_total: usize,
    pub(super) now_overflow: Option<Overflow>,

    pub(super) needs_call_cards: Vec<TaskCardView>,
    pub(super) needs_call_total: usize,

    pub(super) backlog_rows: Vec<MiniRow>,
    pub(super) backlog_total: usize,
    pub(super) backlog_browse_href: String,
    pub(super) pull_next_href: Option<String>,
    pub(super) pull_next_title: String,

    pub(super) done_rows: Vec<MiniRow>,
    pub(super) done_total: usize,
    pub(super) done_browse_href: String,
}

fn card_view(vault_name: &str, row: &TaskRow, accents: &HashMap<String, u8>, config: &TasksConfig) -> TaskCardView {
    TaskCardView {
        id: row.id.clone(),
        title: row.title.clone(),
        state_label: config.label_for_state(&row.task_type, &row.state).to_owned(),
        state_colour: config.stage_colour(&row.task_type, &row.state).slug(),
        href: row.detail_href(vault_name),
        assignee: row.assignee.clone().unwrap_or_default(),
        due: row.due.clone().unwrap_or_default(),
        entity_accent: accent_for(accents, row),
        attention: row.attention.map(crate::signals_event::Severity::as_str),
    }
}

fn mini_row(vault_name: &str, row: &TaskRow, accents: &HashMap<String, u8>, done: bool) -> MiniRow {
    MiniRow {
        title: row.title.clone(),
        href: row.detail_href(vault_name),
        entity_accent: accent_for(accents, row),
        priority: row.priority.clone().unwrap_or_default(),
        done,
        attention: row.attention.map(crate::signals_event::Severity::as_str),
    }
}

/// Truncates `title` to [`BUTTON_TITLE_LIMIT`] characters with a trailing
/// ellipsis, matching the "Pull next task" button's own mock precedent of
/// keeping a long title from pushing the button wide. Character-counted,
/// not byte-counted, so a truncation never lands inside a multi-byte
/// UTF-8 sequence.
fn truncated_for_button(title: &str) -> String {
    let char_count = title.chars().count();
    if char_count <= BUTTON_TITLE_LIMIT {
        title.to_owned()
    } else {
        let mut truncated: String = title.chars().take(BUTTON_TITLE_LIMIT).collect();
        truncated.push('…');
        truncated
    }
}

/// The `?stage=` value naming every stage in `keys`, comma-joined (the
/// convention `tasks::flat_rows`'s own `stage_matches` and this module's
/// own drill-down both read as "any of these stages"). Targets the full
/// Table view, not a Focus-drill: a "Browse all"/"View all"/overflow link
/// exists precisely because a bucket's own capped preview
/// (`PANEL_LIMIT`/`WIP_LIMIT`) is not the whole picture, so it lands
/// somewhere that already offers sorting, the card/list display toggle,
/// and a route to the Hierarchy view too, rather than the single
/// card-grid-only drill `build_drill_data` renders for an ordinary
/// stage-tile click. A bucket with no configured states at all (a vault
/// that never assigned one) yields a link that simply clears any existing
/// stage filter instead, since a comma-joined empty value would otherwise
/// ask for a stage matching nothing.
fn bucket_href(vault_name: &str, query: &TasksQuery, keys: &[StageKey]) -> String {
    if keys.is_empty() {
        tasks_href(vault_name, ViewMode::Table, query, StageOverride::Clear)
    } else {
        let joined = keys.iter().map(ToString::to_string).collect::<Vec<_>>().join(",");
        tasks_href(vault_name, ViewMode::Table, query, StageOverride::Set(joined.as_str()))
    }
}

/// What a drill-down into `keys` is called: each stage's label, with its type's when it names one, so `dropped` in two
/// types reads as two different stages.
fn label_for_stages(config: &TasksConfig, keys: &[StageKey]) -> String {
    keys.iter()
        .map(|key| match &key.task_type {
            Some(task_type) => {
                let type_label = if task_type.is_empty() {
                    OTHER_TYPES_LABEL
                } else {
                    config
                        .task_type(task_type)
                        .map_or(task_type.as_str(), |entry| entry.display_label())
                };
                format!("{type_label}: {}", config.label_for_state(task_type, &key.state))
            }
            None => config
                .task_type
                .iter()
                .find_map(|entry| config.stage_of(&entry.value, &key.state))
                .map_or_else(|| key.state.clone(), |stage| stage.display_label().to_owned()),
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

/// Every stage bucketed to `bucket`: each declared type's, then, when `rows` holds a task whose type is not declared,
/// the shared stages' for those tasks, so a "browse all" link reaches every row the section counts.
fn bucket_keys(config: &TasksConfig, rows: &[TaskRow], bucket: Bucket) -> Vec<StageKey> {
    let mut keys = config.stage_keys_in_bucket(bucket);
    if rows.iter().any(|row| !config.is_declared(&row.task_type)) {
        keys.extend(
            config
                .stage
                .iter()
                .filter(|stage| stage.bucket == Some(bucket))
                .map(|stage| StageKey::other(&stage.state)),
        );
    }
    keys
}

/// Whether `row`, in its own type's lifecycle, is bucketed to `bucket`.
fn in_bucket(config: &TasksConfig, row: &TaskRow, bucket: Bucket) -> bool {
    config.bucket_of(&row.task_type, &row.state) == Some(bucket)
}

/// Builds the single-stage/bucket drill-down `FocusData`: every section
/// field empty, since `TasksScreen.jsx`'s own `stageFilter` branch replaces
/// the sectioned view entirely rather than rendering alongside it.
fn build_drill_data(
    vault_name: &str,
    config: &TasksConfig,
    filtered_rows: &[TaskRow],
    accents: &HashMap<String, u8>,
    query: &TasksQuery,
    stage_param: &str,
) -> FocusData {
    let drill_keys: Vec<StageKey> = stage_param.split(',').map(StageKey::from).collect();
    let drill_cards: Vec<TaskCardView> = filtered_rows
        .iter()
        .filter(|row| stage_matches(config, row, stage_param))
        .map(|row| card_view(vault_name, row, accents, config))
        .collect();
    FocusData {
        drill_label: Some(label_for_stages(config, &drill_keys)),
        drill_cards,
        clear_filter_href: tasks_href(vault_name, ViewMode::Focus, query, StageOverride::Clear),
        now_cards: Vec::new(),
        now_total: 0,
        now_overflow: None,
        needs_call_cards: Vec::new(),
        needs_call_total: 0,
        backlog_rows: Vec::new(),
        backlog_total: 0,
        backlog_browse_href: String::new(),
        pull_next_href: None,
        pull_next_title: String::new(),
        done_rows: Vec::new(),
        done_total: 0,
        done_browse_href: String::new(),
    }
}

/// Builds the four bucketed sections' own `FocusData`, when no `?stage=`
/// drill-down is active.
fn build_sectioned_data(
    vault_name: &str,
    config: &TasksConfig,
    filtered_rows: &[TaskRow],
    accents: &HashMap<String, u8>,
    query: &TasksQuery,
) -> FocusData {
    let now_states = bucket_keys(config, filtered_rows, Bucket::Now);
    let backlog_states = bucket_keys(config, filtered_rows, Bucket::Backlog);
    let done_states = bucket_keys(config, filtered_rows, Bucket::Done);

    let now_rows: Vec<&TaskRow> = filtered_rows
        .iter()
        .filter(|row| in_bucket(config, row, Bucket::Now))
        .collect();
    let needs_call_rows: Vec<&TaskRow> = filtered_rows
        .iter()
        .filter(|row| in_bucket(config, row, Bucket::NeedsCall))
        .collect();
    let mut backlog_rows_all: Vec<&TaskRow> = filtered_rows
        .iter()
        .filter(|row| in_bucket(config, row, Bucket::Backlog))
        .collect();
    // Most urgent first, by the priority vocabulary `.contextos/tasks.toml` declares; a stable sort keeps fetch order
    // among equals.
    backlog_rows_all.sort_by_key(|row| config.priority_sort_key(row.priority.as_deref().unwrap_or_default()));
    let done_rows_all: Vec<&TaskRow> = filtered_rows
        .iter()
        .filter(|row| in_bucket(config, row, Bucket::Done))
        .collect();

    let now_total = now_rows.len();
    let now_cards: Vec<TaskCardView> = now_rows
        .iter()
        .take(WIP_LIMIT)
        .copied()
        .map(|row| card_view(vault_name, row, accents, config))
        .collect();
    let now_overflow_count = now_total.saturating_sub(WIP_LIMIT);
    let now_overflow = (now_overflow_count > 0).then(|| Overflow {
        count: now_overflow_count,
        href: bucket_href(vault_name, query, &now_states),
    });

    let needs_call_total = needs_call_rows.len();
    let needs_call_cards: Vec<TaskCardView> = needs_call_rows
        .iter()
        .copied()
        .map(|row| card_view(vault_name, row, accents, config))
        .collect();

    let backlog_total = backlog_rows_all.len();
    let backlog_rows: Vec<MiniRow> = backlog_rows_all
        .iter()
        .take(PANEL_LIMIT)
        .copied()
        .map(|row| mini_row(vault_name, row, accents, false))
        .collect();
    let pull_next = backlog_rows_all.first().copied();
    let pull_next_href = pull_next.map(|row| row.detail_href(vault_name));
    let pull_next_title = pull_next.map_or_else(String::new, |row| truncated_for_button(&row.title));

    let done_total = done_rows_all.len();
    let done_rows: Vec<MiniRow> = done_rows_all
        .iter()
        .take(PANEL_LIMIT)
        .copied()
        .map(|row| mini_row(vault_name, row, accents, true))
        .collect();

    FocusData {
        drill_label: None,
        drill_cards: Vec::new(),
        clear_filter_href: String::new(),
        now_cards,
        now_total,
        now_overflow,
        needs_call_cards,
        needs_call_total,
        backlog_rows,
        backlog_total,
        backlog_browse_href: bucket_href(vault_name, query, &backlog_states),
        pull_next_href,
        pull_next_title,
        done_rows,
        done_total,
        done_browse_href: bucket_href(vault_name, query, &done_states),
    }
}

/// Builds the Focus view's own [`FocusData`]: `filtered_rows` is the
/// row set `routes::tasks::render_list` has already narrowed by entity,
/// search text, and assignee, exactly as `TasksScreen.jsx`'s own `base`
/// does before deriving every section from it.
pub(super) fn build_focus_data(
    vault_name: &str,
    config: &TasksConfig,
    filtered_rows: &[TaskRow],
    configured_entities: &[EntityEntry],
    query: &TasksQuery,
) -> FocusData {
    let accents = entity_accents(configured_entities, filtered_rows);
    match query.stage.as_deref() {
        Some(stage_param) => build_drill_data(vault_name, config, filtered_rows, &accents, query, stage_param),
        None => build_sectioned_data(vault_name, config, filtered_rows, &accents, query),
    }
}

#[cfg(test)]
#[path = "tasks_focus_test.rs"]
mod tests;
