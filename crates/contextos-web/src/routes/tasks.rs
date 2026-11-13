//! `GET /{{vault_name}}/tasks/`: the Command Centre Tasks dashboard, in
//! three view modes over `.contextos/tasks.toml`'s configured flat stages
//! and `memory/tasks.base`'s own scope (`file.inFolder("memory/tasks")`):
//! Focus (the default, `routes::tasks_focus`'s own bucketed triage
//! sections), Table (the flat list, unchanged in row content from before
//! this screen had a Focus view), and Hierarchy (the recursive `parent`-
//! tree, unchanged). Rows are fetched with an inline `base_query`
//! `definition` rather than `memory/tasks.base`'s own declared views: this
//! screen's own column needs (`parent`, for the hierarchy) are a
//! `contextos-web` display concern, not a reason to edit a vault's own
//! `.base` file. Selecting a stage tile, switching view mode, or applying
//! the search/assignee filters, re-renders the same already-fetched row
//! set as an HTMX partial swap, never a second round trip.
//!
//! Also owns the vault root Overview dashboard's own Task-scoped widget (a condensed stage strip, embedded by
//! `routes::overview::render_html`), since it shares this module's own `TaskRow`/`fetch_task_rows`/`stage_tiles`. A
//! companion attention indicator is deliberately not built here: the vault's own governing requirement for this
//! indicator says it must be sourced from a proper events mechanism, never derived directly from a task's `state`
//! or `due` fields, and must not render at all until that events mechanism exists. An earlier vault-dashboard
//! widget once derived a "Needs attention" list directly from `state == "waiting"` and overdue `due` dates in
//! violation of that requirement; it has been removed rather than carried forward.
//!
//! A single task's own detail page (read, edit, create, close) lives in
//! `routes::tasks_detail`, which reuses this module's `TaskRow` and
//! `fetch_task_rows` (`pub(super)` there) rather than re-fetching a second
//! way.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::config;
use crate::entities::{self, EntityEntry};
use crate::mcp_client::{McpCallError, McpClient, McpClientSet};
use crate::rendering::page;
use crate::rendering::shell::{self, ActiveScreen};
use crate::rendering::title_action::{self, TitleAction};
use crate::routes::vault::{html_response, not_found, unreachable};
use crate::tasks_config::{self, TasksConfig};

pub use super::tasks_query::{CompletedOn, TasksQuery};
pub(super) use super::tasks_query::{DisplayMode, DoneVisibility, SortKey, StageOverride, ViewMode, tasks_href};
use super::tasks_query::{display_href, done_href};
use super::tasks_strip::{StageGroup, completed_on, effective_hide_done, is_done_row, stage_groups};

use super::tasks_fields::SelectOption;
use super::tasks_focus::{self, FocusData};
use super::tasks_rows::{RowContext, TaskRowView, flat_rows, hierarchy_rows};

#[derive(Clone)]
pub struct TasksRoutesState {
    pub clients: Arc<McpClientSet>,
    pub web_config_path: Arc<PathBuf>,
    /// The operator's local time, for every date this screen writes into a vault.
    pub vault_time: Arc<crate::vault_time::VaultTime>,
}

impl From<super::CommonRoutesState> for TasksRoutesState {
    fn from(value: super::CommonRoutesState) -> Self {
        Self {
            clients: value.clients,
            web_config_path: value.web_config_path,
            vault_time: value.vault_time,
        }
    }
}

impl TasksRoutesState {
    pub(super) fn client(&self) -> Result<Arc<McpClient>, Box<Response>> {
        crate::routes::primary_session(&self.clients)
    }
}

/// One `memory/tasks/**/*.md` row, as this screen needs it: every field the
/// stage strip, the three view modes, and the Focus view's own card
/// rendering each read, fetched in one `base_query` call rather than once
/// per concern. `pub(super)` so `routes::tasks_detail` and
/// `routes::tasks_focus` can each read it without a second fetch
/// implementation or a duplicated field set.
#[derive(Clone, Debug, Default)]
pub(super) struct TaskRow {
    pub(super) path: String,
    pub(super) id: String,
    pub(super) title: String,
    pub(super) state: String,
    pub(super) task_type: String,
    pub(super) entity: String,
    pub(super) priority: Option<String>,
    pub(super) due: Option<String>,
    pub(super) assignee: Option<String>,
    /// The lower-cased target of `parent`'s own wikilink (a stored value
    /// like `[[some-id]]` becomes `"some-id"`), for case-insensitive
    /// matching against another row's own `id`; `None` when `parent` is
    /// unset or not wikilink-shaped.
    pub(super) parent_id: Option<String>,
    /// The date the task was closed as completed, as written: a `YYYY-MM-DD` date, or a timestamp beginning with one.
    pub(super) completed: Option<String>,
    /// When the note last changed, as written.
    pub(super) updated: Option<String>,
    /// The severity of the most recent event about this task when it is a warning or an error, which later events
    /// clear; read from the vault's events when the list is rendered, never stored on the task.
    pub(super) attention: Option<crate::signals_event::Severity>,
}

impl TaskRow {
    /// Where this row's own detail page lives: `/tasks/{id}` when it has a
    /// real `id`, or a direct link to its raw note when it does not (an
    /// unlikely but possible malformed note, never a dead link either way).
    pub(super) fn detail_href(&self, vault_name: &str) -> String {
        let urls = crate::urls::VaultUrls::from(vault_name);
        if self.id.is_empty() {
            urls.file(&self.path)
        } else {
            urls.task(&self.id)
        }
    }
}

#[derive(Debug, Deserialize)]
struct BaseQueryResult {
    content: String,
}

fn string_field(row: &Map<String, Value>, key: &str) -> String {
    row.get(key).and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn optional_field(row: &Map<String, Value>, key: &str) -> Option<String> {
    row.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Extracts a bare `[[target]]` or `[[target|alias]]` wikilink's own target
/// text, lower-cased and stripped of a trailing `.md`, for matching against
/// a row's own `id` (frontmatter spells ids upper-case; a `parent` link
/// commonly spells the identical target lower-case). `None` for any value
/// that is not wikilink-shaped. `pub(super)` so `routes::tasks_links` shares
/// this exact parsing rather than a second wikilink parser.
pub(super) fn wikilink_target(raw: &str) -> Option<String> {
    wikilink_written_target(raw).map(|target| target.to_ascii_lowercase())
}

/// [`wikilink_target`] without the lower-casing: the target as it was written, for resolving a path on a
/// case-sensitive filesystem and for writing the link back unchanged.
pub(super) fn wikilink_written_target(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let inner = trimmed.strip_prefix("[[")?.strip_suffix("]]")?;
    let target = inner.split('|').next().unwrap_or(inner).trim();
    if target.is_empty() {
        None
    } else {
        Some(target.trim_end_matches(".md").to_owned())
    }
}

/// The reverse of [`wikilink_target`]: renders a bare task id (lower- or
/// upper-case, exactly as the link-chip widget's own hidden field submits
/// it) as the `[[{lowercase-id}|{UPPERCASE-ID}]]` wikilink form every link
/// field stores in frontmatter (confirmed against a real task note; see
/// `routes::tasks_links`'s own module doc). `pub(super)` so
/// `routes::tasks_detail` shares this exact reverse-formatting rather than a
/// second implementation of it.
pub(super) fn wikilink_from_target(target: &str) -> String {
    let target = target.trim();
    // A path (a link to a note outside the tasks base, kept as written) is never case-folded, which a
    // case-sensitive filesystem would read as a different note.
    match target.rsplit_once('/') {
        Some((_, last)) => format!("[[{target}|{last}]]"),
        None => format!("[[{}|{}]]", target.to_ascii_lowercase(), target.to_ascii_uppercase()),
    }
}

/// The wikilink a task is referenced by from anywhere else in the vault, such as a Journal loop entry: its key, as
/// `[[{lowercase-key}|{KEY}]]`, the form every link field stores, so the reference survives the note being moved to
/// another folder. A note whose file is not named for its key is linked by its file name instead, still never by its
/// folder, and one with no key by its file name alone.
pub(super) fn task_reference(path: &str, key: Option<&str>) -> String {
    let file_name = path.rsplit('/').next().unwrap_or(path);
    let stem = file_name.strip_suffix(".md").unwrap_or(file_name);
    match key.map(str::trim).filter(|key| !key.is_empty()) {
        Some(key) if stem.eq_ignore_ascii_case(key) => wikilink_from_target(key),
        Some(key) => format!("[[{stem}|{}]]", key.to_ascii_uppercase()),
        None => format!("[[{stem}]]"),
    }
}

fn task_row(row: &Map<String, Value>) -> TaskRow {
    TaskRow {
        path: string_field(row, "file.path"),
        id: string_field(row, "id"),
        title: string_field(row, "title"),
        state: string_field(row, "state"),
        task_type: string_field(row, "task_type"),
        entity: string_field(row, "entity"),
        priority: optional_field(row, "priority"),
        due: optional_field(row, "due"),
        assignee: optional_field(row, "assignee"),
        parent_id: optional_field(row, "parent").and_then(|raw| wikilink_target(&raw)),
        completed: optional_field(row, "completed"),
        updated: optional_field(row, "updated"),
        attention: None,
    }
}

/// Fetches every `memory/tasks/**/*.md` row via an inline `base_query`
/// definition (never `memory/tasks.base`'s own path, so this screen's own
/// column needs never require editing a vault's `.base` file). `pub(super)`
/// for `routes::tasks_detail`'s own id-lookup.
pub(super) async fn fetch_task_rows(client: &McpClient, vault_name: &str) -> Result<Vec<TaskRow>, McpCallError> {
    let mut args = Map::new();
    args.insert("vault".to_owned(), Value::String(format!("{vault_name}://.")));
    args.insert("format".to_owned(), Value::String("json".to_owned()));
    args.insert(
        "definition".to_owned(),
        json!({
            "filters": crate::tasks_config::TASK_NOTES_FILTER,
            // An inline definition is one view with its keys at the top level: `order` names every column this
            // screen reads. A `views` list is the shape of a `.base` file, which an inline definition refuses.
            "order": [
                "file.path", "id", "title", "state", "task_type", "entity",
                "priority", "due", "assignee", "parent", "completed", "updated"
            ],
        }),
    );
    let result = client.call_tool("base_query".to_owned(), args).await?;
    if result.is_error == Some(true) {
        return Ok(Vec::new());
    }
    let Ok(parsed) = result.into_typed::<BaseQueryResult>() else {
        return Ok(Vec::new());
    };
    let rows: Vec<Map<String, Value>> = serde_json::from_str(&parsed.content).unwrap_or_default();
    Ok(rows.iter().map(task_row).collect())
}

fn matches_entity(row: &TaskRow, entity: Option<&str>) -> bool {
    entity.is_none_or(|entity| row.entity.eq_ignore_ascii_case(entity))
}

/// Case-insensitive substring match against a row's own `title` or `id`,
/// matching `TasksScreen.jsx`'s own `t.title.toLowerCase().includes(q) ||
/// t.taskKey.toLowerCase().includes(q)`. `None` or an empty `q` matches
/// every row. `pub(super)` so `routes::tasks_links`'s own link-picker search
/// reuses this exact match rule rather than a second implementation of it.
pub(super) fn matches_search(row: &TaskRow, q: Option<&str>) -> bool {
    match q {
        None | Some("") => true,
        Some(q) => {
            let needle = q.to_ascii_lowercase();
            row.title.to_ascii_lowercase().contains(&needle) || row.id.to_ascii_lowercase().contains(&needle)
        }
    }
}

/// Exact, case-insensitive `assignee` match. `None` or an empty `assignee`
/// (the "All agents" default) matches every row, including a row with no
/// `assignee` of its own.
fn matches_assignee(row: &TaskRow, assignee: Option<&str>) -> bool {
    match assignee {
        None | Some("") => true,
        Some(assignee) => row
            .assignee
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case(assignee)),
    }
}

/// Exact, case-insensitive `task_type` match. `None` or an empty `task_type` (the "All types" default) matches
/// every row.
fn matches_task_type(row: &TaskRow, task_type: Option<&str>) -> bool {
    match task_type {
        None | Some("") => true,
        Some(task_type) => row.task_type.eq_ignore_ascii_case(task_type),
    }
}

/// Exact, case-insensitive `priority` match. `None` or an empty `priority` (the "All priorities" default) matches
/// every row, including a row with no `priority` of its own.
fn matches_priority(row: &TaskRow, priority: Option<&str>) -> bool {
    match priority {
        None | Some("") => true,
        Some(priority) => row
            .priority
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case(priority)),
    }
}

/// Every distinct `assignee` present in `rows`, case-preserved on first
/// appearance, in first-appearance order: the assignee filter's own
/// `<select>` options, computed from the entity-scoped row set before
/// search/assignee filtering narrows it further, so choosing one assignee
/// never makes every other one disappear from the list.
fn distinct_assignees(rows: &[TaskRow]) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut assignees = Vec::new();
    for row in rows {
        if let Some(assignee) = &row.assignee
            && seen.insert(assignee.to_ascii_lowercase())
        {
            assignees.push(assignee.clone());
        }
    }
    assignees
}

/// Resolves the `--entity-N` accent (1-6) for every distinct `entity` value in `rows`, in first-appearance order:
/// `configured`'s own explicit accent when `.contextos/entities.toml` names this entity, otherwise the identical
/// by-position fallback [`shell::entity_for`] already assigns the vault switcher's own entries with no configured
/// accent of their own, so a task's entity accent and a vault's entity accent never disagree about what "no explicit
/// entry" means. Shared by every view this screen renders a card from (Focus's own sections, and the Table/Hierarchy
/// views' [`DisplayMode::Cards`] layout).
pub(super) fn entity_accents(configured: &[EntityEntry], rows: &[TaskRow]) -> HashMap<String, u8> {
    let mut accents = HashMap::new();
    let mut position = 0usize;
    for row in rows {
        let key = row.entity.to_ascii_lowercase();
        if key.is_empty() || accents.contains_key(&key) {
            continue;
        }
        let accent = configured
            .iter()
            .find(|entity| entity.slug.eq_ignore_ascii_case(&row.entity))
            .map_or_else(|| shell::entity_for(position), |entity| entity.accent);
        accents.insert(key, accent);
        position += 1;
    }
    accents
}

pub(super) fn accent_for(accents: &HashMap<String, u8>, row: &TaskRow) -> u8 {
    accents.get(&row.entity.to_ascii_lowercase()).copied().unwrap_or(1)
}

/// A fixed-vocabulary filter `<select>`'s own options: a leading "match everything" blank option, then one option
/// per `(value, label)` pair, `selected` set for whichever one equals `current`. Shared by the Type and Priority
/// filters below - each a closed vocabulary `.contextos/tasks.toml` declares - and the Sort filter, unlike the
/// Assignee filter's own open vocabulary drawn from the rows themselves ([`distinct_assignees`]).
fn fixed_filter_options<'a>(
    blank_label: &str,
    values: impl IntoIterator<Item = (&'a str, &'a str)>,
    current: Option<&str>,
) -> Vec<SelectOption> {
    let current = current.filter(|value| !value.is_empty());
    let mut options = vec![SelectOption {
        value: String::new(),
        label: blank_label.to_owned(),
        selected: current.is_none(),
    }];
    options.extend(values.into_iter().map(|(value, label)| SelectOption {
        value: value.to_owned(),
        label: label.to_owned(),
        selected: current == Some(value),
    }));
    options
}

/// The Type filter's options: every task type `.contextos/tasks.toml` declares.
fn task_type_filter_options(config: &TasksConfig, current: Option<&str>) -> Vec<SelectOption> {
    fixed_filter_options(
        "All types",
        config
            .task_type
            .iter()
            .map(|entry| (entry.value.as_str(), entry.display_label())),
        current,
    )
}

/// The Priority filter's options: every priority `.contextos/tasks.toml` declares, most urgent first.
fn priority_filter_options(config: &TasksConfig, current: Option<&str>) -> Vec<SelectOption> {
    fixed_filter_options(
        "All priorities",
        config
            .priority
            .iter()
            .map(|entry| (entry.value.as_str(), entry.display_label())),
        current,
    )
}

/// The Table view's own Sort filter options - not vault-configured, so its vocabulary lives here rather than in
/// `tasks_fields`, unlike Type/Priority.
const SORT_VALUES: &[(&str, &str)] = &[
    ("id", "ID (A\u{2013}Z)"),
    ("title", "Title (A\u{2013}Z)"),
    ("due", "Due date"),
    ("priority", "Priority"),
    ("assignee", "Assignee"),
];

fn sort_options(current: SortKey) -> Vec<SelectOption> {
    SORT_VALUES
        .iter()
        .map(|(value, label)| SelectOption {
            value: (*value).to_owned(),
            label: (*label).to_owned(),
            selected: *value == current.as_param(),
        })
        .collect()
}

#[derive(Template)]
#[template(path = "tasks_list.html")]
struct TasksListTemplate {
    stage_groups: Vec<StageGroup>,
    is_focus: bool,
    is_table: bool,
    is_hierarchy: bool,
    /// The Table/Hierarchy views' own row layout ([`TasksListTemplate::is_focus`] gates its every use in the
    /// template, since the Focus view ignores it entirely): an enum, not a fourth bool, to stay under this
    /// workspace's own `clippy::struct_excessive_bools` bound.
    display_mode: DisplayMode,
    focus_href: String,
    table_href: String,
    hierarchy_href: String,
    display_table_href: String,
    display_cards_href: String,
    /// Whether the Table/Hierarchy views are currently excluding done-state rows ([`effective_hide_done`]): drives
    /// the toggle's own label/pressed state. Always [`DoneVisibility::Shown`] for the Focus view, which ignores the
    /// toggle entirely. [`DoneVisibility`] rather than a fifth bool ([`display_mode`](Self::display_mode)'s own
    /// doc covers this struct's existing `clippy::struct_excessive_bools` bound).
    hide_done: DoneVisibility,
    done_toggle_href: String,
    search_value: String,
    assignee_value: String,
    assignee_options: Vec<String>,
    task_type_options: Vec<SelectOption>,
    priority_options: Vec<SelectOption>,
    sort_options: Vec<SelectOption>,
    entity_hidden: Option<String>,
    stage_hidden: Option<String>,
    completed_hidden: Option<&'static str>,
    view_hidden: String,
    rows: Vec<TaskRowView>,
    total: usize,
    focus: Option<FocusData>,
    /// This list's own address, which it reloads itself from when the live stream says attention may have changed.
    refresh_href: String,
    attention: AttentionTile,
}

/// The stage strip's Attention tile: how many of the listed tasks have an open warning or error, from the vault's
/// events, opening the Signals screen where they are listed.
struct AttentionTile {
    count: usize,
    href: String,
}

impl AttentionTile {
    fn new(vault_name: &str, rows: &[TaskRow]) -> Self {
        Self {
            count: rows.iter().filter(|row| row.attention.is_some()).count(),
            href: crate::urls::VaultUrls::from(vault_name).signals(),
        }
    }
}

/// What the three views are drawn from.
struct ViewInputs<'a> {
    vault_name: &'a str,
    config: &'a TasksConfig,
    /// The rows the query selects and the Done today selection keeps.
    listed: &'a [TaskRow],
    configured_entities: &'a [EntityEntry],
    query: &'a TasksQuery,
    hide_done: bool,
    sort: SortKey,
}

/// The rows of `view`, and the Focus view's own data when it is that view.
fn rows_for_view(
    view: ViewMode,
    inputs: &ViewInputs<'_>,
    row_context: &RowContext<'_>,
) -> (Vec<TaskRowView>, Option<FocusData>) {
    let ViewInputs {
        vault_name,
        config,
        listed,
        configured_entities,
        query,
        hide_done,
        sort,
    } = *inputs;
    match view {
        ViewMode::Hierarchy => {
            let scoped: Vec<TaskRow> = listed
                .iter()
                .filter(|row| !hide_done || !is_done_row(config, row))
                .cloned()
                .collect();
            (hierarchy_rows(vault_name, &scoped, row_context), None)
        }
        ViewMode::Table => (
            flat_rows(vault_name, listed, query.stage.as_deref(), hide_done, sort, row_context),
            None,
        ),
        ViewMode::Focus => (
            Vec::new(),
            Some(tasks_focus::build_focus_data(
                vault_name,
                config,
                listed,
                configured_entities,
                query,
            )),
        ),
    }
}

fn render_list(
    vault_name: &str,
    config: &TasksConfig,
    all_rows: &[TaskRow],
    configured_entities: &[EntityEntry],
    query: &TasksQuery,
    today: time::Date,
) -> String {
    let entity = query.entity.as_deref();
    let entity_scoped: Vec<TaskRow> = all_rows
        .iter()
        .filter(|row| matches_entity(row, entity))
        .cloned()
        .collect();
    let assignee_options = distinct_assignees(&entity_scoped);

    let q = query.q.as_deref().map(str::trim).filter(|value| !value.is_empty());
    let assignee_filter = query.assignee.as_deref().filter(|value| !value.is_empty());
    let task_type_filter = query.task_type.as_deref().filter(|value| !value.is_empty());
    let priority_filter = query.priority.as_deref().filter(|value| !value.is_empty());
    let filtered: Vec<TaskRow> = entity_scoped
        .into_iter()
        .filter(|row| {
            matches_search(row, q)
                && matches_assignee(row, assignee_filter)
                && matches_task_type(row, task_type_filter)
                && matches_priority(row, priority_filter)
        })
        .collect();

    // The Done today selection narrows what every view lists, never what the strip counts.
    let listed: Cow<'_, [TaskRow]> = match query.completed {
        Some(CompletedOn::Today) => filtered
            .iter()
            .filter(|row| completed_on(config, row, today))
            .cloned()
            .collect(),
        None => Cow::Borrowed(&filtered),
    };
    let view = ViewMode::from_query(query);
    let display = DisplayMode::from_query(query);
    let sort = SortKey::from_query(query);
    let accents = entity_accents(configured_entities, &filtered);
    let row_context = RowContext {
        config,
        accents: &accents,
    };
    let total = filtered.len();
    let hide_done = effective_hide_done(config, query);
    let (rows, focus) = rows_for_view(
        view,
        &ViewInputs {
            vault_name,
            config,
            listed: &listed,
            configured_entities,
            query,
            hide_done,
            sort,
        },
        &row_context,
    );

    let template = TasksListTemplate {
        attention: AttentionTile::new(vault_name, &filtered),
        stage_groups: stage_groups(vault_name, config, &filtered, query, today),
        is_focus: view == ViewMode::Focus,
        is_table: view == ViewMode::Table,
        is_hierarchy: view == ViewMode::Hierarchy,
        display_mode: display,
        focus_href: tasks_href(vault_name, ViewMode::Focus, query, StageOverride::Keep),
        table_href: tasks_href(vault_name, ViewMode::Table, query, StageOverride::Keep),
        hierarchy_href: tasks_href(vault_name, ViewMode::Hierarchy, query, StageOverride::Keep),
        display_table_href: display_href(vault_name, view, query, DisplayMode::Table),
        display_cards_href: display_href(vault_name, view, query, DisplayMode::Cards),
        hide_done: hide_done.into(),
        done_toggle_href: done_href(vault_name, view, query, !hide_done),
        search_value: query.q.clone().unwrap_or_default(),
        assignee_value: query.assignee.clone().unwrap_or_default(),
        assignee_options,
        task_type_options: task_type_filter_options(config, task_type_filter),
        priority_options: priority_filter_options(config, priority_filter),
        sort_options: sort_options(sort),
        entity_hidden: query.entity.clone(),
        stage_hidden: query.stage.clone(),
        completed_hidden: query.completed.map(CompletedOn::as_param),
        view_hidden: view.as_param().unwrap_or("focus").to_owned(),
        rows,
        total,
        focus,
        refresh_href: tasks_href(vault_name, view, query, StageOverride::Keep),
    };
    template
        .render()
        .unwrap_or_else(|_| "<p>The tasks dashboard could not be rendered.</p>".to_owned())
}

/// `pub(super)` so `routes::tasks_detail` shares the identical
/// vault-existence check rather than a second implementation of it.
pub(super) async fn ensure_vault(client: &McpClient, vault_name: &str) -> Result<(), Box<Response>> {
    match crate::apps::vault_exists(client, vault_name).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(Box::new(not_found())),
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => Err(Box::new(unreachable())),
    }
}

/// `GET /{{vault_name}}/tasks/`.
pub async fn list(
    State(state): State<TasksRoutesState>,
    Path(vault_name): Path<String>,
    Query(query): Query<TasksQuery>,
    headers: HeaderMap,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let events = crate::signals_query::EventsApi {
        client: &client,
        vault_name: &vault_name,
    };
    let (config, rows, configured_entities, today, attention) = tokio::join!(
        tasks_config::load_tasks_config(&client, &vault_name),
        fetch_task_rows(&client, &vault_name),
        entities::load_entities(&client, &vault_name),
        state.vault_time.today(),
        crate::signals_load::task_attention(&events, crate::vault_links::TASKS_BASE, time::OffsetDateTime::now_utc()),
    );
    let mut rows = match rows {
        Ok(rows) => rows,
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => return unreachable(),
    };
    for row in &mut rows {
        row.attention = attention.get(&row.id.to_uppercase()).copied();
    }
    let fragment = render_list(&vault_name, &config, &rows, &configured_entities, &query, today);
    if headers.contains_key("hx-request") {
        html_response(fragment)
    } else {
        let web_config_path = Arc::clone(&state.web_config_path);
        let entity_base_path = crate::urls::VaultUrls::from(&*vault_name).tasks();
        let known_mcp_servers = state.clients.names();
        let (mut nav, appearance, mcp_status) = tokio::join!(
            Box::pin(shell::build_nav(
                &client,
                ActiveScreen::Vault,
                Some(&vault_name),
                Some("tasks"),
                query.entity.as_deref(),
                Some(&entity_base_path),
                &known_mcp_servers,
            )),
            async move {
                tokio::task::spawn_blocking(move || config::current_appearance(&web_config_path))
                    .await
                    .unwrap_or_default()
            },
            shell::mcp_status(&state.clients),
        );
        nav.appearance = appearance;
        nav.sidebar_wide = crate::routes::sidebar_wide_from_headers(&headers);
        nav.mcp_status = mcp_status;
        nav.set_title_actions(title_action::render(&TitleAction::NewTask(&vault_name)));
        html_response(page::render_page(&nav, "Tasks", &fragment))
    }
    .into_response()
}

/// How many open tasks are assigned to the team member `slug`, matched without regard to case; `None` when the tasks
/// cannot be read.
pub(super) async fn open_tasks_assigned_to(client: &McpClient, vault_name: &str, slug: &str) -> Option<usize> {
    let (rows, config) = tokio::join!(
        fetch_task_rows(client, vault_name),
        tasks_config::load_tasks_config(client, vault_name),
    );
    let rows = rows.ok()?;
    Some(
        rows.iter()
            .filter(|row| config.is_open(&row.task_type, &row.state))
            .filter(|row| {
                row.assignee
                    .as_deref()
                    .is_some_and(|assignee| assignee.eq_ignore_ascii_case(slug))
            })
            .count(),
    )
}

// ---------------------------------------------------------------------------
// Vault root Overview dashboard's Task-scoped widget (embedded by `routes::overview::render_html`)
// ---------------------------------------------------------------------------

#[derive(Template)]
#[template(path = "task_dashboard_widgets.html")]
struct TaskDashboardWidgetsTemplate {
    stage_groups: Vec<StageGroup>,
    tasks_href: String,
    attention: AttentionTile,
}

/// Renders the Overview dashboard's own Task-scoped widget: a condensed stage-count strip, scoped to `entity` the
/// same way the Tasks screen itself is. Never fails outright: any MCP failure renders an empty widget rather than
/// failing the whole Overview dashboard over a summary panel. Deliberately carries no "needs attention" list of its
/// own: see this module's own doc comment.
pub(crate) async fn dashboard_widgets_html(
    client: &McpClient,
    vault_name: &str,
    entity: Option<&str>,
    vault_time: &crate::vault_time::VaultTime,
) -> String {
    let events = crate::signals_query::EventsApi { client, vault_name };
    let (config, rows, today, attention) = tokio::join!(
        tasks_config::load_tasks_config(client, vault_name),
        fetch_task_rows(client, vault_name),
        vault_time.today(),
        crate::signals_load::task_attention(&events, crate::vault_links::TASKS_BASE, time::OffsetDateTime::now_utc()),
    );
    let rows: Vec<TaskRow> = rows
        .unwrap_or_default()
        .into_iter()
        .filter(|row| matches_entity(row, entity))
        .map(|mut row| {
            row.attention = attention.get(&row.id.to_uppercase()).copied();
            row
        })
        .collect();
    let query = TasksQuery {
        entity: entity.map(str::to_owned),
        ..TasksQuery::default()
    };
    let template = TaskDashboardWidgetsTemplate {
        stage_groups: stage_groups(vault_name, &config, &rows, &query, today),
        tasks_href: crate::urls::VaultUrls::from(vault_name).tasks(),
        attention: AttentionTile::new(vault_name, &rows),
    };
    template.render().unwrap_or_default()
}

#[cfg(test)]
#[path = "tasks_test.rs"]
mod tests;
