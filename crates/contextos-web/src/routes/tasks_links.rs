//! The task-to-task link widget for four of `routes::tasks_detail`'s own
//! frontmatter fields: `parent` (single-valued), and `depends_on`, `blocks`,
//! `related` (each a list). All four store real Obsidian wikilinks in
//! frontmatter (`[[{lowercase-id}|{UPPERCASE-ID}]]`, confirmed against a
//! real task note in this vault), never a bare id, so this module owns both
//! directions of that format: parsing a note's current wikilinks into
//! resolved, removable chips for display ([`build_link_fields`]), and a
//! live search endpoint ([`search`]) an operator uses to find and click an
//! existing task rather than typing an id freehand. Reuses
//! `routes::tasks`'s own `wikilink_target` and `matches_search` rather than
//! a second implementation of either.
//!
//! `waiting_on` looks similar but is a plain free-text field (who or what
//! is being waited on, not a task link) and is deliberately untouched by
//! this module; it stays a generic field in `routes::tasks_detail`'s own
//! `FIELD_ORDER`.
//!
//! The actual `[[{lowercase-id}|{UPPERCASE-ID}]]` string this widget's own
//! chips serialise back into on save is built client-side, in
//! `task_detail.html`'s own `hx-vals` script, from whichever chips are
//! present in the DOM at submit time; the save itself travels through the
//! existing generic `routes::vault_mutations::mutate` frontmatter PATCH
//! route unchanged, since a wikilink-valued field is, to that route, just
//! another string or string-array frontmatter value.

use std::collections::HashMap;

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::mcp_client::McpCallError;
use crate::routes::vault::{error_response, html_response, unreachable};

use super::tasks::{TaskRow, TasksRoutesState, ensure_vault, fetch_task_rows, matches_search, wikilink_written_target};
use crate::mcp_client::McpClient;
use contextos_markdown::{LinkDestination, LinkTarget};

use crate::screen_routes::{ScreenRoutes, expanded_frontmatter};
use crate::vault_links::{TASKS_DIR, WikiLinkValue};

/// One of the four link fields this widget owns, in the order they render.
struct LinkFieldSpec {
    key: &'static str,
    label: &'static str,
    /// `true` for `parent` (a note has at most one); `false` for
    /// `depends_on`/`blocks`/`related` (a list).
    single: bool,
}

const LINK_FIELD_SPECS: &[LinkFieldSpec] = &[
    LinkFieldSpec {
        key: "parent",
        label: "Parent",
        single: true,
    },
    LinkFieldSpec {
        key: "depends_on",
        label: "Depends on",
        single: false,
    },
    LinkFieldSpec {
        key: "blocks",
        label: "Blocks",
        single: false,
    },
    LinkFieldSpec {
        key: "related",
        label: "Related",
        single: false,
    },
];

/// Every frontmatter key this widget owns, so `routes::tasks_detail`'s own
/// generic field builder can exclude them (both from its own known-field
/// list and from its "render any other frontmatter key generically"
/// fallback) rather than rendering the identical field twice.
pub(super) fn link_field_keys() -> impl Iterator<Item = &'static str> {
    LINK_FIELD_SPECS.iter().map(|spec| spec.key)
}

/// Every link field's own key and arity (`true` for `parent`, the one
/// single-valued field; `false` for the three list-valued fields), for
/// `routes::tasks_detail`'s own raw-submission-to-patch classification -
/// the one other place besides rendering that needs to know a link field's
/// arity, alongside its key.
pub(super) fn link_field_specs() -> impl Iterator<Item = (&'static str, bool)> {
    LINK_FIELD_SPECS.iter().map(|spec| (spec.key, spec.single))
}

/// One resolved, removable chip in a link field's own chip list: `target` is
/// the lower-cased wikilink target this chip round-trips back into
/// `[[{target}|{target.to_ascii_uppercase()}]]` on save; `display_id` is
/// what an operator actually reads (the matching row's own `id`, which may
/// differ in case or form from a bare uppercase of `target`, when resolved;
/// otherwise `target` itself, upper-cased, as the best available label);
/// `title` is `Some` only when `target` resolved to a real, currently known
/// task row.
pub(super) struct LinkChip {
    pub(super) target: String,
    pub(super) display_id: String,
    pub(super) title: Option<String>,
    /// The route of a note this chip's link names that is not a task row of the tasks base (a requirement, say),
    /// found by the shared link resolver; `None` for a task row and for a link that resolves to nothing.
    pub(super) href: Option<String>,
    /// The link names a note that exists but that no screen shows, so there is nowhere to send the chip.
    pub(super) unrouted: bool,
}

impl LinkChip {
    /// Where the read view's chip goes: the note the shared resolver found, else the task screen for its target;
    /// nowhere for a note no screen shows.
    pub(super) fn read_href(&self, vault_name: &str) -> Option<String> {
        if self.unrouted {
            return None;
        }
        Some(
            self.href
                .clone()
                .unwrap_or_else(|| crate::urls::VaultUrls::from(&*vault_name).task(&self.target)),
        )
    }
}

/// One link field, as `task_link_fields.html` renders it: a label, whether
/// it accepts one chip or several, and the chips this note's own current
/// frontmatter value resolves to.
pub(super) struct LinkFieldView {
    pub(super) key: &'static str,
    pub(super) label: &'static str,
    pub(super) single: bool,
    pub(super) chips: Vec<LinkChip>,
}

/// Every wikilink target a field's raw frontmatter value carries: a bare
/// string for a single-valued field, or a string array for a list field.
/// Anything else (absent, `null`, or a value not shaped as this field's own
/// arity expects) yields no targets, never an error: a malformed link field
/// degrades to "no chips shown", the same as a genuinely unset field, rather
/// than failing the whole detail page over one bad value.
fn raw_targets(frontmatter: &Map<String, Value>, spec: &LinkFieldSpec) -> Vec<String> {
    let Some(value) = frontmatter.get(spec.key) else {
        return Vec::new();
    };
    if spec.single {
        value.as_str().and_then(wikilink_written_target).into_iter().collect()
    } else {
        value
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .filter_map(wikilink_written_target)
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Finds the task row a wikilink target names. A link inside the tasks base is written as the bare key, or, for a
/// base keyed by file path, as a path within the base's folder; a link from anywhere else may be the full vault path.
/// Matching is case-insensitive, like the lower-cased targets [`wikilink_target`] yields.
pub(super) struct RowLookup<'a> {
    by_id: HashMap<String, &'a TaskRow>,
    by_path: HashMap<String, &'a TaskRow>,
}

impl<'a> RowLookup<'a> {
    pub(super) fn new(rows: impl IntoIterator<Item = &'a TaskRow>) -> Self {
        let mut by_id = HashMap::new();
        let mut by_path = HashMap::new();
        for row in rows {
            if !row.id.is_empty() {
                by_id.insert(row.id.to_ascii_lowercase(), row);
            }
            by_path.insert(row.path.to_ascii_lowercase(), row);
        }
        Self { by_id, by_path }
    }

    pub(super) fn find(&self, target: &str) -> Option<&'a TaskRow> {
        let target = target.to_ascii_lowercase();
        self.by_id.get(&target).copied().or_else(|| {
            task_paths(&target)
                .iter()
                .find_map(|path| self.by_path.get(path).copied())
        })
    }
}

/// The task note paths a link's `target` may name: a path within the tasks base's folder, then a vault path. A leading
/// `/` makes it a vault path outright. `.md` is assumed when the last segment has no extension.
fn task_paths(target: &str) -> Vec<String> {
    let with_extension = |path: &str| {
        let last = path.rsplit('/').next().unwrap_or(path);
        if last.contains('.') {
            path.to_owned()
        } else {
            format!("{path}.md")
        }
    };
    match target.strip_prefix('/') {
        Some(rooted) => vec![with_extension(rooted)],
        None => vec![
            format!("{TASKS_DIR}/{}", with_extension(target)),
            with_extension(target),
        ],
    }
}

/// Resolves one wikilink `target` against `lookup` into a display chip: a genuinely dangling reference (no row it
/// names, most likely because the referenced task was since deleted) still renders, labelled only with its own raw
/// target and no title, rather than being silently dropped - the operator needs to see it to fix it. A resolved
/// chip's `target` is the row's own lower-cased id whatever form the link was written in, so it round-trips through
/// the picker's save as the bare-key link.
fn resolve_chip(target: String, lookup: &RowLookup<'_>) -> LinkChip {
    match lookup.find(&target) {
        Some(row) => LinkChip {
            display_id: row.id.clone(),
            title: Some(row.title.clone()),
            target: row.id.to_ascii_lowercase(),
            href: None,
            unrouted: false,
        },
        None => LinkChip {
            display_id: target.to_ascii_uppercase(),
            title: None,
            target,
            href: None,
            unrouted: false,
        },
    }
}

/// Gives every chip that names no task row a second chance by the server's own resolution of its link, so a link to a
/// note outside the tasks base (a requirement, or a record of another base) opens that note rather than showing as not
/// found. The note's frontmatter is read once with every link expanded; each chip's expanded target is the item in the
/// same place of the same field. The chip keeps the target as written, so saving the field writes the same link back.
pub(super) async fn resolve_dangling_chips(
    client: &McpClient,
    vault_name: &str,
    from_note: &str,
    fields: &mut [LinkFieldView],
) {
    if fields
        .iter()
        .all(|field| field.chips.iter().all(|chip| chip.title.is_some()))
    {
        return;
    }
    let Some(expanded) = expanded_frontmatter(client, vault_name, from_note).await else {
        return;
    };
    let targets: Vec<Vec<LinkTarget>> = fields.iter().map(|field| expanded_targets(&expanded, field)).collect();
    let paths: Vec<&str> = targets
        .iter()
        .flatten()
        .filter_map(|target| match target {
            LinkTarget::Vault { path, .. } => Some(path.as_str()),
            _ => None,
        })
        .collect();
    let routes = ScreenRoutes::load(client, vault_name, paths).await;
    for (field, field_targets) in fields.iter_mut().zip(&targets) {
        if field_targets.len() != field.chips.len() {
            continue;
        }
        for (chip, target) in field.chips.iter_mut().zip(field_targets) {
            let LinkTarget::Vault { path, .. } = target else {
                continue;
            };
            if chip.title.is_some() {
                continue;
            }
            chip.display_id = String::from(&WikiLinkValue {
                target: chip.target.clone(),
                display: None,
            });
            let route = routes.route(path);
            chip.unrouted = route.is_none();
            chip.href = route;
        }
    }
}

/// The expanded targets of `field`'s links in `expanded`, in the order its chips were built from.
fn expanded_targets(expanded: &Map<String, Value>, field: &LinkFieldView) -> Vec<LinkTarget> {
    let items: Vec<&str> = match expanded.get(field.key) {
        Some(Value::String(text)) if field.single => vec![text.as_str()],
        Some(Value::Array(items)) if !field.single => items.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    items
        .into_iter()
        .filter_map(|item| WikiLinkValue::try_from(item).ok())
        .map(|link| {
            LinkTarget::from(LinkDestination {
                text: &link.target,
                percent_encoded: false,
            })
        })
        .collect()
}

/// Builds every link field's own current chip list from `frontmatter`'s raw
/// values, resolving each wikilink target against `rows_by_id` for display.
/// `pub(super)` for `routes::tasks_detail`'s own detail-page assembly.
pub(super) fn build_link_fields(
    frontmatter: &Map<String, Value>,
    rows_by_id: &HashMap<String, &TaskRow>,
) -> Vec<LinkFieldView> {
    let lookup = RowLookup::new(rows_by_id.values().copied());
    LINK_FIELD_SPECS
        .iter()
        .map(|spec| LinkFieldView {
            key: spec.key,
            label: spec.label,
            single: spec.single,
            chips: raw_targets(frontmatter, spec)
                .into_iter()
                .map(|target| resolve_chip(target, &lookup))
                .collect(),
        })
        .collect()
}

// ---------------------------------------------------------------------
// GET /{{vault_name}}/tasks/{{id}}/links/search
// ---------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct TaskLinkSearchQuery {
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub field: Option<String>,
}

/// The live search box's own result cap: generous enough to browse a
/// vault's real task volume without either flooding the dropdown or reading
/// every row's own content into the response for no benefit.
const MAX_LINK_SEARCH_RESULTS: usize = 20;

struct LinkSearchResult {
    target: String,
    display_id: String,
    title: String,
}

#[derive(Template)]
#[template(path = "task_link_search_results.html")]
struct LinkSearchResultsTemplate {
    field: String,
    results: Vec<LinkSearchResult>,
}

fn unknown_link_field() -> Response {
    error_response(StatusCode::BAD_REQUEST, "tasks/unknown-link-field")
}

/// `GET /{{vault_name}}/tasks/{{id}}/links/search`: a small HTML fragment of
/// clickable results for the live search-and-pick widget on one of the four
/// link fields, matched case-insensitively against a row's own `title` or
/// `id` (`routes::tasks`'s own `matches_search`), excluding the current
/// task itself - a task must never link to itself - and every note outside
/// `memory/tasks/**` (`fetch_task_rows`'s own scope already excludes those).
/// `field` must name one of the four known link fields: an unrecognised or
/// missing `field` is rejected outright rather than rendering a fragment
/// the client-side picker would not know what to do with.
pub async fn search(
    State(state): State<TasksRoutesState>,
    Path((vault_name, id)): Path<(String, String)>,
    Query(query): Query<TaskLinkSearchQuery>,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let Some(field) = query.field.filter(|field| link_field_keys().any(|key| key == field)) else {
        return unknown_link_field();
    };
    let rows = match fetch_task_rows(&client, &vault_name).await {
        Ok(rows) => rows,
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => return unreachable(),
    };
    let self_id = id.to_ascii_lowercase();
    let q = query.q.as_deref().map(str::trim).filter(|value| !value.is_empty());
    let results: Vec<LinkSearchResult> = rows
        .iter()
        .filter(|row| !row.id.is_empty() && row.id.to_ascii_lowercase() != self_id)
        .filter(|row| matches_search(row, q))
        .take(MAX_LINK_SEARCH_RESULTS)
        .map(|row| LinkSearchResult {
            target: row.id.to_ascii_lowercase(),
            display_id: row.id.clone(),
            title: row.title.clone(),
        })
        .collect();
    let template = LinkSearchResultsTemplate { field, results };
    html_response(
        template
            .render()
            .unwrap_or_else(|_| "<p class=\"hint\">The search results could not be rendered.</p>".to_owned()),
    )
    .into_response()
}

#[cfg(test)]
#[path = "tasks_links_test.rs"]
mod tests;
