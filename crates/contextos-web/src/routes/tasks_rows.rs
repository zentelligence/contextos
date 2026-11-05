//! The task list's row views: one row's presentation, the flat and hierarchical orderings, and the sort.

use std::collections::{HashMap, HashSet};

use super::tasks::{SortKey, TaskRow, accent_for};
use super::tasks_strip::{is_done_row, stage_matches};
use crate::tasks_config::TasksConfig;

pub(super) struct TaskRowView {
    pub(super) id: String,
    pub(super) title: String,
    pub(super) state: String,
    pub(super) task_type: String,
    pub(super) entity: String,
    pub(super) priority: String,
    pub(super) due: String,
    pub(super) assignee: String,
    pub(super) href: String,
    pub(super) depth: usize,
    pub(super) depth_label: Option<String>,
    /// The `--entity-N` accent this row's own `entity` resolves to ([`entity_accents`]'s own doc): unused by the
    /// plain `<table>` rendering, but required by `task_card.html` whenever the Table or Hierarchy view's own
    /// display toggle ([`DisplayMode`]) selects `Cards`, so every row carries it regardless of which layout the
    /// current request actually renders.
    pub(super) entity_accent: u8,
    /// The configured display label for `state` ([`TasksConfig::label_for_state`]), the same `card.state_label`
    /// `task_card.html` already reads for the Focus view's own cards.
    pub(super) state_label: String,
    /// The colour `state`'s chip is shown in ([`TasksConfig::stage_colour`]).
    pub(super) state_colour: &'static str,
    /// `warning` or `error` while the task's most recent event is one ([`TaskRow::attention`]).
    pub(super) attention: Option<&'static str>,
}

/// Bundles the two pieces of vault-configured context every row-rendering function below needs, so
/// `row_view`/`flat_rows`/`hierarchy_rows`/`walk` each take one context argument rather than two separate ones
/// (kept under this workspace's own argument-count lint bound, matching `routes::wiki`'s identical
/// `PageRenderContext` precedent).
pub(super) struct RowContext<'a> {
    pub(super) config: &'a TasksConfig,
    pub(super) accents: &'a HashMap<String, u8>,
}

pub(super) fn row_view(
    vault_name: &str,
    row: &TaskRow,
    depth: usize,
    depth_label: Option<String>,
    context: &RowContext<'_>,
) -> TaskRowView {
    TaskRowView {
        id: row.id.clone(),
        title: row.title.clone(),
        state: row.state.clone(),
        task_type: row.task_type.clone(),
        entity: row.entity.clone(),
        priority: row.priority.clone().unwrap_or_default(),
        due: row.due.clone().unwrap_or_default(),
        assignee: row.assignee.clone().unwrap_or_default(),
        href: row.detail_href(vault_name),
        depth,
        depth_label,
        entity_accent: accent_for(context.accents, row),
        state_label: context.config.label_for_state(&row.task_type, &row.state).to_owned(),
        state_colour: context.config.stage_colour(&row.task_type, &row.state).slug(),
        attention: row.attention.map(crate::signals_event::Severity::as_str),
    }
}

/// Sorts `rows` in place per `sort`, ascending, a row whose own sort field is blank always sorting last regardless of
/// key, since an absent value is not meaningfully "least" for any of these fields. [`SortKey::Id`], the default,
/// orders ids by their series and then by number, so `A-2` comes before `A-10`. [`SortKey::Priority`] ranks most urgent first by the priority vocabulary `config` declares, an
/// undeclared value after every declared one, matching the Focus view's own backlog ordering.
pub(super) fn sort_rows(rows: &mut [TaskRowView], sort: SortKey, config: &TasksConfig) {
    fn blank_last(value: &str) -> (bool, String) {
        (value.is_empty(), value.to_ascii_lowercase())
    }
    match sort {
        SortKey::Id => rows.sort_by_key(|row| id_order(&row.id)),
        SortKey::Title => rows.sort_by_key(|row| blank_last(&row.title)),
        SortKey::Due => rows.sort_by_key(|row| blank_last(&row.due)),
        SortKey::Assignee => rows.sort_by_key(|row| blank_last(&row.assignee)),
        SortKey::Priority => rows.sort_by_key(|row| config.priority_sort_key(&row.priority)),
    }
}

/// The order of an id: blank ids last, then by series (the text before the last `-`, without regard to case), then by
/// the number after it, numerically, an id with no number after its series ordering by its text after those that
/// have one.
fn id_order(id: &str) -> (bool, String, Option<u64>, String) {
    let lowered = id.to_ascii_lowercase();
    let (series, number) = match lowered.rsplit_once('-') {
        Some((series, rest)) => (series.to_owned(), rest.parse::<u64>().ok()),
        None => (lowered.clone(), None),
    };
    (id.is_empty(), series, number, lowered)
}

pub(super) fn flat_rows(
    vault_name: &str,
    rows: &[TaskRow],
    stage: Option<&str>,
    hide_done: bool,
    sort: SortKey,
    context: &RowContext<'_>,
) -> Vec<TaskRowView> {
    let mut views: Vec<TaskRowView> = rows
        .iter()
        .filter(|row| stage.is_none_or(|stage| stage_matches(context.config, row, stage)))
        .filter(|row| !hide_done || !is_done_row(context.config, row))
        .map(|row| row_view(vault_name, row, 0, None, context))
        .collect();
    sort_rows(&mut views, sort, context.config);
    views
}

/// Flattens `rows`' own `parent` graph into pre-order (a parent immediately followed by its own children,
/// recursively), each entry carrying its own tree depth and `context.config`'s own configured label for that depth,
/// when any. Cycle-safe: a row already visited on the current walk is never re-entered, so a corrupted `parent`
/// cycle in the underlying notes degrades to "this branch stops here", never an infinite loop or a stack overflow. A
/// row whose own `parent_id` does not match any row currently in scope (a parent outside the entity filter, an
/// operator's own hide-done toggle excluding it, or simply absent) is treated as a root, not dropped: this is what
/// makes pre-filtering `rows` to exclude done tasks before this call safe, unlike the single-state `?stage=` filter
/// `TasksQuery::stage`'s own doc explains this view deliberately never applies. Never sorted by anything but tree
/// pre-order: `TasksQuery::sort`
/// is a Table-view-only concern, since reordering a tree's own siblings by an unrelated field would not change which
/// row is whose parent, only obscure it.
pub(super) fn hierarchy_rows(vault_name: &str, rows: &[TaskRow], context: &RowContext<'_>) -> Vec<TaskRowView> {
    let mut by_id: HashMap<String, &TaskRow> = HashMap::new();
    for row in rows {
        if !row.id.is_empty() {
            by_id.insert(row.id.to_ascii_lowercase(), row);
        }
    }
    let lookup = super::tasks_links::RowLookup::new(rows);
    let mut children: HashMap<String, Vec<&TaskRow>> = HashMap::new();
    let mut roots: Vec<&TaskRow> = Vec::new();
    for row in rows {
        let parent_key = row
            .parent_id
            .as_deref()
            .and_then(|parent| lookup.find(parent))
            .map(|parent| parent.id.to_ascii_lowercase())
            .filter(|key| by_id.contains_key(key));
        match parent_key {
            Some(key) => children.entry(key).or_default().push(row),
            None => roots.push(row),
        }
    }
    let mut output = Vec::new();
    let mut visited = HashSet::new();
    for root in roots {
        walk(vault_name, root, 0, &children, context, &mut visited, &mut output);
    }
    output
}

fn walk<'a>(
    vault_name: &str,
    row: &'a TaskRow,
    depth: usize,
    children: &HashMap<String, Vec<&'a TaskRow>>,
    context: &RowContext<'_>,
    visited: &mut HashSet<String>,
    output: &mut Vec<TaskRowView>,
) {
    let key = row.id.to_ascii_lowercase();
    if !key.is_empty() && !visited.insert(key.clone()) {
        return;
    }
    output.push(row_view(
        vault_name,
        row,
        depth,
        context.config.hierarchy.label_for_depth(depth).map(str::to_owned),
        context,
    ));
    if let Some(kids) = children.get(&key) {
        for kid in kids {
            walk(vault_name, kid, depth + 1, children, context, visited, output);
        }
    }
}
