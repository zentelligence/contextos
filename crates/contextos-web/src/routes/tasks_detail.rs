//! A single task's own detail page: `GET /{{vault_name}}/tasks/{{id}}` (the read view, [`detail`]),
//! `GET /{{vault_name}}/tasks/{{id}}/edit` (the edit form, [`edit_form`]), and every mutation that page triggers -
//! all through the one [`patch`] route ([`TaskPatchBody`]'s own doc covers why there is only one), reading the
//! submission as flatly-named fields straight off a plain HTML form (`hx-ext="json-enc"`, no `hx-vals` computation
//! anywhere on either screen) and classifying them via `tasks_detail_fields::raw_fields_to_patch` rather than
//! trusting client-side JavaScript to have pre-built the right JSON patch shape. The one exception is the four link
//! fields' own add/remove chip picker (`static/task-forms.js`), the one piece of this screen with no plain-HTML
//! equivalent; even that script only ever adds or removes a chip's own already-server-shaped hidden input, never
//! building JSON or deciding what a value means.
//!
//! The task creation screen (`GET /{{vault_name}}/tasks/new`, the identical
//! field-by-field view against a not-yet-created note, and its own
//! `create` route) lives in `routes::tasks_create`, which shares this
//! module's [`TaskDetailQuery`] and [`malformed_create_request`] plus
//! `routes::tasks_detail_fields`'s field schema rather than a second
//! implementation of either.
//!
//! Shares `routes::tasks`'s own `TaskRow`/`fetch_task_rows`/`ensure_vault`
//! (`pub(super)` there) rather than a second row-fetch implementation. The
//! `parent`/`depends_on`/`blocks`/`related` link fields' own chip widget and
//! live search endpoint live in `routes::tasks_links`, kept apart from this
//! module's own generic field handling.

use std::collections::HashMap;
use std::sync::Arc;

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::config;
use crate::mcp_client::{McpCallError, McpClient};
use crate::rendering::page;
use crate::rendering::shell::{self, ActiveScreen};
use crate::routes::file_response::{self, FileRequest, Raw};
use crate::routes::vault::{html_response, not_found, unreachable};

use crate::entities;
use crate::rendering::escape_html;
use crate::task_folder::{TaskFolder, TaskNoteLocation, known_task_folders};
use crate::tasks_config::{self, Closes, StageEntry, TasksConfig};
use crate::team;

use super::tasks::{TaskRow, TasksRoutesState, ensure_vault, fetch_task_rows};
use super::tasks_detail_fields::{FieldContext, FieldView, build_fields, extract_body, raw_fields_to_patch};
use super::tasks_fields::FieldWidget;

// ---------------------------------------------------------------------
// GET /{{vault_name}}/tasks/{{id}}
// ---------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct TaskDetailQuery {
    #[serde(default)]
    pub entity: Option<String>,
}

/// The note's body text: everything from `body_start_line` onward, per
/// `tasks_detail_fields::extract_body`'s own doc.
#[derive(Debug, Deserialize)]
struct FrontmatterReadResult {
    frontmatter: Map<String, Value>,
    body_start_line: u64,
    content_hash: String,
}

async fn read_frontmatter(client: &McpClient, vault_path: &str) -> Result<Option<FrontmatterReadResult>, McpCallError> {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(vault_path.to_owned()));
    let result = client.call_tool("frontmatter_read".to_owned(), args).await?;
    if result.is_error == Some(true) {
        return Ok(None);
    }
    Ok(result.into_typed::<FrontmatterReadResult>().ok())
}

#[derive(Template)]
#[template(path = "task_detail.html")]
struct TaskDetailTemplate {
    /// Every URL the template links to within the vault.
    urls: crate::urls::VaultUrls,
    vault_name: String,
    id: String,
    title: String,
    expected_hash: String,
    /// `true` for `GET .../edit`'s own response; `false` for every other response this fragment ever renders
    /// (`GET`'s own default, and every mutation's post-save re-render), matching `routes::wiki`'s identical
    /// "editing is its own reserved sub-path, the read view is the default" contract. Gates which half of the
    /// template renders: the existing editable form (`title_field`/`identity_fields`/etc., `body`), or the plain
    /// read view (`body_html`) with an "Edit" button.
    editing: bool,
    /// One button per state of the task's own type that closes it, or none once the task is closed, rather than
    /// offering an action that would just restamp the same state and date a second time.
    close_actions: Vec<CloseAction>,
    title_field: FieldView,
    identity_fields: Vec<FieldView>,
    date_fields: Vec<FieldView>,
    remaining_fields: Vec<FieldView>,
    link_fields: Vec<super::tasks_links::LinkFieldView>,
    body: String,
    /// The body rendered to HTML ([`crate::note_render::render_note`]), read only when `!editing`: the edit
    /// form's own textarea always uses raw `body` instead, matching `routes::wiki`'s identical split between its
    /// own `rendered_html` (read view) and `edit_body` (edit form).
    body_html: String,
    /// The folder the note is filed in relative to the tasks folder, as the read view shows it: the path a task's
    /// folder is typed and created as, empty for the top level.
    folder_path: String,
    /// The folder relative to the tasks folder, the move form's starting value; `None` hides the move form for a
    /// note whose folder the form could not name (a hidden folder written outside this screen).
    folder: Option<String>,
    /// The folders that already hold a task, suggested by the move form.
    known_folders: Vec<String>,
}

/// A button that closes the task by moving it into one of its type's closing states.
struct CloseAction {
    state: String,
    label: String,
}

/// Whether a task of `task_type` in `state` is closed: its type declares the state as one that closes a task.
fn is_closed(config: &TasksConfig, task_type: &str, state: &str) -> bool {
    config
        .closing(task_type, state)
        .is_some_and(|closes| closes != Closes::Never)
}

/// Whether a task was already closed: in a closing state of its type, or in a state its type does not declare whose
/// name closes a task in the shipped lifecycles (a decision left `done` before decisions had their own states). Such a
/// task keeps its close date when it moves to its type's closing state, and is not recorded as closed a second time.
fn was_closed(config: &TasksConfig, task_type: &str, state: &str) -> bool {
    match config.closing(task_type, state) {
        Some(closes) => closes != Closes::Never,
        None => Closes::shipped_for(state) != Closes::Never,
    }
}

/// The text `map` holds for `key`, or empty.
fn text_of(map: Option<&Map<String, Value>>, key: &str) -> String {
    map.and_then(|map| map.get(key))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// The type and state a save leaves the task in, and whether that differs from its current type and state. Types are
/// compared without regard to ASCII case, as everywhere else.
fn resulting_stage(patch: &Map<String, Value>, current: Option<&Map<String, Value>>) -> (String, String, bool) {
    let (current_type, current_state) = (text_of(current, "task_type"), text_of(current, "state"));
    let task_type = patch
        .get("task_type")
        .and_then(Value::as_str)
        .map_or_else(|| current_type.clone(), str::to_owned);
    let state = patch
        .get("state")
        .and_then(Value::as_str)
        .map_or_else(|| current_state.clone(), str::to_owned);
    let changed = !task_type.eq_ignore_ascii_case(&current_type) || state != current_state;
    (task_type, state, changed)
}

/// The close buttons a task of `task_type` in `state` is offered: one per closing state of its type, unless it is
/// already closed.
fn close_actions(config: &TasksConfig, task_type: &str, state: &str) -> Vec<CloseAction> {
    if is_closed(config, task_type, state) {
        return Vec::new();
    }
    config
        .lifecycle(task_type)
        .iter()
        .filter(|stage| stage.closing() != Closes::Never)
        .map(|stage| CloseAction {
            state: stage.state.clone(),
            label: format!("Mark as {}", stage.display_label()),
        })
        .collect()
}

/// `a`, `a or b`, or `a, b, or c`.
fn either_of(labels: &[&str]) -> String {
    match labels {
        [] => String::new(),
        [only] => (*only).to_owned(),
        [first, second] => format!("{first} or {second}"),
        [rest @ .., last] => format!("{}, or {last}", rest.join(", ")),
    }
}

/// Refuses a save that would leave the task in a state its type does not declare. Checked only when the save changes
/// the task's type or state, so a task already holding a state its type does not declare (written by hand, or before
/// each type had its own states) can still have its other fields edited.
fn check_state_for_type(
    config: &TasksConfig,
    patch: &Map<String, Value>,
    current: Option<&Map<String, Value>>,
) -> Result<(), String> {
    let (task_type, state, changed) = resulting_stage(patch, current);
    if !changed || config.stage_of(&task_type, &state).is_some() {
        return Ok(());
    }
    let type_label = config
        .task_type(&task_type)
        .map_or(task_type.as_str(), |entry| entry.display_label());
    let choices: Vec<&str> = config
        .lifecycle(&task_type)
        .iter()
        .map(StageEntry::display_label)
        .collect();
    Err(format!(
        "Not saved: `{state}` is not a state of a {type_label}. Choose {}.",
        either_of(&choices)
    ))
}

/// Re-fetches `id`'s current state and renders it as the `.task-detail`
/// fragment [`TaskDetailTemplate`] produces: the shared "the write
/// pipeline's own completed write is the contract; show what's on disk
/// now" response every mutation on this page - [`detail`] itself and every
/// successful [`patch`] - renders after a successful write, rather than
/// several copies of the same row lookup and field assembly. `Err` only for
/// a genuinely unreachable MCP transport or an `id` that no longer resolves
/// (the note was deleted out from under this request between the write and
/// this re-read).
async fn render_detail_fragment(
    client: &McpClient,
    vault_name: &str,
    id: &str,
    editing: bool,
) -> Result<(TaskRow, String), Box<Response>> {
    let rows = match fetch_task_rows(client, vault_name).await {
        Ok(rows) => rows,
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => return Err(Box::new(unreachable())),
    };
    let needle = id.to_ascii_lowercase();
    let Some(row) = rows.iter().find(|row| row.id.to_ascii_lowercase() == needle).cloned() else {
        return Err(Box::new(not_found()));
    };
    let vault_path = format!("{vault_name}://{}", row.path);
    let (frontmatter_read_result, attached, configured_entities, configured_team, tasks_config) = tokio::join!(
        read_frontmatter(client, &vault_path),
        crate::routes::vault::fetch_attached(client, vault_path.clone()),
        entities::load_entities(client, vault_name),
        team::load_team(client, vault_name),
        tasks_config::load_tasks_config(client, vault_name),
    );
    let Ok(Some(read)) = frontmatter_read_result else {
        return Err(Box::new(match frontmatter_read_result {
            Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => unreachable(),
            _ => not_found(),
        }));
    };
    let full_text = match attached {
        Ok(crate::routes::vault::Attached::Found {
            body: crate::routes::vault::AttachedBody::Text(text),
            ..
        }) => text,
        _ => String::new(),
    };
    let rows_by_id: HashMap<String, &TaskRow> = rows
        .iter()
        .filter(|row| !row.id.is_empty())
        .map(|row| (row.id.to_ascii_lowercase(), row))
        .collect();
    let field_context = FieldContext {
        entities: &configured_entities,
        team: &configured_team,
        tasks_config: &tasks_config,
    };
    let field_groups = build_fields(&read.frontmatter, &field_context);
    let body = extract_body(&full_text, read.body_start_line);
    // The read view's own rendered HTML: skipped on the edit form (whose textarea always uses raw `body` instead)
    // so a request that is only ever going to show the raw text never pays for a wikilink/embed/Mermaid resolution
    // round trip it does not need.
    let body_html = if editing {
        String::new()
    } else {
        crate::note_render::render_note(
            client,
            vault_name,
            crate::note_render::NoteView {
                path: &row.path,
                raw: Some(&body),
                template: false,
            },
        )
        .await
        .html
    };
    let location = TaskNoteLocation::try_from(row.path.as_str()).ok();
    let folder_path = location.as_ref().map_or_else(
        || folder_beneath_tasks(&row.path).to_owned(),
        |location| location.folder.as_str().to_owned(),
    );
    let known_folders = if editing {
        known_task_folders(rows.iter().map(|row| row.path.as_str()))
    } else {
        Vec::new()
    };
    let mut link_fields = super::tasks_links::build_link_fields(&read.frontmatter, &rows_by_id);
    super::tasks_links::resolve_dangling_chips(client, vault_name, &row.path, &mut link_fields).await;
    let fragment_template = TaskDetailTemplate {
        urls: crate::urls::VaultUrls::from(vault_name),
        vault_name: vault_name.to_owned(),
        id: row.id.clone(),
        title: row.title.clone(),
        expected_hash: read.content_hash,
        editing,
        close_actions: close_actions(&tasks_config, &row.task_type, &row.state),
        title_field: field_groups.title,
        identity_fields: field_groups.identity,
        date_fields: field_groups.dates,
        remaining_fields: field_groups.remaining,
        link_fields,
        body,
        body_html,
        folder_path,
        folder: location.map(|location| location.folder.as_str().to_owned()),
        known_folders,
    };
    let fragment = fragment_template
        .render()
        .unwrap_or_else(|_| "<p>The task detail page could not be rendered.</p>".to_owned());
    Ok((row, fragment))
}

/// The vault path of the task note whose id is `id`, matched as the task list matches ids, without regard to case.
async fn task_note_path(client: &McpClient, vault_name: &str, id: &str) -> Result<Option<String>, McpCallError> {
    let needle = id.to_ascii_lowercase();
    Ok(fetch_task_rows(client, vault_name)
        .await?
        .into_iter()
        .find(|row| row.id.to_ascii_lowercase() == needle)
        .map(|row| row.path))
}

/// Shared by [`detail`] and [`edit_form`]: both are a plain `GET` differing only in `editing`, and both wrap
/// [`render_detail_fragment`]'s own fragment in the full page shell exactly the same way, matching
/// `routes::wiki`'s identical "the read view and `.../edit` share one renderer" contract.
async fn render_detail_response(
    state: TasksRoutesState,
    vault_name: String,
    (id, raw): (String, Raw),
    query: TaskDetailQuery,
    headers: HeaderMap,
    editing: bool,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    if raw.0 {
        return match task_note_path(&client, &vault_name, &id).await {
            Ok(Some(path)) => file_response::stored(&client, &vault_name, &path, raw).await,
            Ok(None) => not_found(),
            Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => unreachable(),
        };
    }
    let (row, fragment) = match Box::pin(render_detail_fragment(&client, &vault_name, &id, editing)).await {
        Ok(result) => result,
        Err(response) => return *response,
    };
    let task_id = row.id;
    if headers.contains_key("hx-request") {
        html_response(fragment)
    } else {
        let web_config_path = Arc::clone(&state.web_config_path);
        let breadcrumb = format!("tasks/{}", task_id.to_ascii_lowercase());
        let entity_base_path = crate::urls::VaultUrls::from(&*vault_name).task(&task_id);
        let known_mcp_servers = state.clients.names();
        let (mut nav, appearance, mcp_status) = tokio::join!(
            Box::pin(shell::build_nav(
                &client,
                ActiveScreen::Vault,
                Some(&vault_name),
                Some(&breadcrumb),
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
        if !editing {
            shell::show_file(
                &mut nav,
                &client,
                &vault_name,
                (&crate::view_favourites::TASKS).into(),
                (&row.path, &entity_base_path),
            )
            .await;
        }
        html_response(page::render_page(&nav, &task_id, &fragment))
    }
    .into_response()
}

/// `GET /{{vault_name}}/tasks/{{id}}`: the read view, matching `routes::wiki`'s own "the plain `GET` is always the
/// read view" contract.
pub async fn detail(
    State(state): State<TasksRoutesState>,
    Path((vault_name, id)): Path<(String, String)>,
    Query(query): Query<TaskDetailQuery>,
    file: FileRequest,
    headers: HeaderMap,
) -> Response {
    Box::pin(render_detail_response(
        state,
        vault_name,
        (id, file.raw),
        query,
        headers,
        false,
    ))
    .await
}

/// `GET /{{vault_name}}/tasks/{{id}}/edit`: the edit form, `docs/http-routing-response-contract.md`'s reserved
/// `/edit` sub-path, matching `routes::wiki`'s identical "editing is its own reserved sub-path" contract - no task
/// ever has the literal id `edit`, so this can never shadow a real task.
pub async fn edit_form(
    State(state): State<TasksRoutesState>,
    Path((vault_name, id)): Path<(String, String)>,
    Query(query): Query<TaskDetailQuery>,
    headers: HeaderMap,
) -> Response {
    Box::pin(render_detail_response(
        state,
        vault_name,
        (id, Raw::default()),
        query,
        headers,
        true,
    ))
    .await
}

/// Renders the current fragment after a successful mutation, or a genuine
/// tool failure (a conflict, most often) as a small in-place error message
/// rather than losing the whole page to a raw tool-error payload.
fn mutation_error(detail: &str) -> Response {
    html_response(format!("<p class=\"hint\">{detail}</p>"))
}

/// A submitted body this module (and `routes::tasks_create`'s own `create`)
/// could not even parse as JSON.
pub(super) fn malformed_create_request() -> Response {
    html_response("<p class=\"hint\">Malformed request.</p>".to_owned())
}

/// `PATCH /{{vault_name}}/tasks/{{id}}`'s own request body: structurally three distinct shapes on the one endpoint,
/// matching `routes::wiki_save::SaveBody`'s identical "content-replace or field patch, distinguished by which
/// fields are present" contract. `docs/http-routing-response-contract.md` names exactly one PATCH endpoint per
/// entity (`/{{entities}}/{{id}}` - "submit partial update"), never a field-scoped sub-path of its own; the body
/// editor, the move form, the sidebar's frontmatter form, and the "Mark done"/"Cancel task" buttons (a `state` patch
/// like any other) all save through this one route rather than a separate `/body`, `/move`, `/complete`, or
/// `/cancel`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum TaskPatchBody {
    /// The body editor's own whole-text replace (`fs_edit_file`'s exact-old-text-match contract), distinguished
    /// from [`Self::Fields`] by requiring both `old` and `new`, neither of which is a real task frontmatter key.
    Body {
        old: String,
        new: String,
        #[serde(default)]
        expected_hash: Option<String>,
    },
    /// The move form's own submission: `folder` alone, the folder beneath the tasks folder to file the note in. Any
    /// other field alongside it makes the submission [`Self::Fields`] instead, so a vault-specific `folder`
    /// frontmatter field saved from the sidebar form is never mistaken for a move.
    Move(TaskMoveBody),
    /// Everything else: the sidebar form's flatly-named fields, or a close action's own minimal `{"state": ...}`
    /// submission, both read the identical way (`raw_fields_to_patch`'s own doc covers the shape).
    Fields(Map<String, Value>),
}

/// [`TaskPatchBody::Move`]'s own body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskMoveBody {
    /// Text, or `null` for the top level; anything else is refused by [`TaskFolder`]'s own conversion.
    folder: Value,
}

/// The stable `fs_move_file` failure code for a destination that already exists.
const DESTINATION_EXISTS: &str = "io/destination-exists";

/// A refused `fs_move_file` call's own stable code and explanation.
#[derive(Debug, Default, Deserialize)]
struct MoveRefusal {
    #[serde(default)]
    code: String,
    #[serde(default)]
    message: String,
}

/// `base_read`'s reading of where the tasks base's records may sit.
#[derive(Debug, Deserialize)]
struct TasksBaseScope {
    #[serde(default)]
    records_folder: Option<String>,
    #[serde(default)]
    records_include_subfolders: Option<bool>,
}

/// Whether the tasks base takes records only directly in the tasks folder (`file.folder ==`), so a task moved into a
/// sub-folder would leave it, as `base_record_create` refuses on create. A base that cannot be read confines nothing
/// here: the Tasks screen lists every note beneath the tasks folder whatever the base says.
async fn base_takes_no_sub_folders(client: &McpClient, vault_name: &str) -> Result<bool, McpCallError> {
    let mut args = Map::new();
    args.insert(
        "path".to_owned(),
        Value::String(format!("{vault_name}://{}", crate::vault_links::TASKS_BASE)),
    );
    let result = client.call_tool("base_read".to_owned(), args).await?;
    if result.is_error == Some(true) {
        return Ok(false);
    }
    Ok(result.into_typed::<TasksBaseScope>().is_ok_and(|scope| {
        scope.records_folder.as_deref() == Some(crate::vault_links::TASKS_DIR)
            && scope.records_include_subfolders == Some(false)
    }))
}

/// The folder `note_path` is filed in relative to the tasks folder, for a note whose folder [`TaskNoteLocation`] cannot
/// name (one written outside this screen): empty at the top level, and the whole vault-relative folder for a note
/// outside the tasks folder, which a tasks base row never is.
fn folder_beneath_tasks(note_path: &str) -> &str {
    let parent = note_path.rsplit_once('/').map_or("", |(parent, _)| parent);
    if parent == crate::vault_links::TASKS_DIR {
        return "";
    }
    parent
        .strip_prefix(crate::vault_links::TASKS_DIR)
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(parent)
}

/// [`TaskPatchBody::Move`]'s own save: files the note in `folder` beneath the tasks folder under its own file name,
/// through `fs_move_file`, which never replaces a note already there. Its content is untouched, so the move needs no
/// conflict check of its own, and its id-named file keeps every link that names it by name resolving. A move to the
/// folder it is already in changes nothing. `Ok(())`/`Err` matches [`save_body`]'s identical contract.
async fn move_note(client: &McpClient, vault_name: &str, row: &TaskRow, folder: &Value) -> Result<(), Box<Response>> {
    let refused = |reason: String| Box::new(mutation_error(&escape_html(&format!("Not moved: {reason}."))));
    let folder = TaskFolder::try_from(folder).map_err(|reason| refused(format!("the folder {reason}")))?;
    let location = TaskNoteLocation::try_from(row.path.as_str()).map_err(|reason| refused(reason.to_string()))?;
    if location.folder == folder {
        return Ok(());
    }
    if !folder.as_str().is_empty() {
        match base_takes_no_sub_folders(client, vault_name).await {
            Ok(false) => {}
            Ok(true) => {
                return Err(refused(format!(
                    "the tasks base takes only the notes directly in {}, so a task in a sub-folder would leave it",
                    crate::vault_links::TASKS_DIR
                )));
            }
            Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => {
                return Err(Box::new(unreachable()));
            }
        }
    }
    let mut args = Map::new();
    args.insert(
        "source".to_owned(),
        Value::String(format!("{vault_name}://{}", row.path)),
    );
    args.insert(
        "destination".to_owned(),
        Value::String(format!("{vault_name}://{}", location.moved_to(&folder))),
    );
    match client.call_tool("fs_move_file".to_owned(), args).await {
        Ok(result) if result.is_error != Some(true) => Ok(()),
        Ok(result) => {
            let refusal = result.into_typed::<MoveRefusal>().unwrap_or_default();
            Err(refused(if refusal.code == DESTINATION_EXISTS {
                match folder.as_str() {
                    "" => format!("a note named {} is already filed at the top level", location.file_name),
                    named => format!("a note named {} is already filed in {named}", location.file_name),
                }
            } else if refusal.message.is_empty() {
                "the note could not be moved".to_owned()
            } else {
                refusal.message.trim_end_matches('.').to_owned()
            }))
        }
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => Err(Box::new(unreachable())),
    }
}

/// [`TaskPatchBody::Fields`]'s own close-action side effects: when `patch` moves the task into a state its type
/// declares as closing, stamps the date that state closes with (`completed` or `cancelled`) with this server's
/// operator's local date (`today`, never the reader's browser) unless the patch carries a date of its own (the edit
/// form submits the field blank when it has none) or the task was already closed with that date set, and reports
/// whether this save is a genuine transition into a closed state (the prior state was not already closed, see
/// [`was_closed`]) so the caller lists a closed task in the Journal exactly once per close, never one per ordinary
/// edit of an already-closed task's other fields.
fn stamp_close_dates(
    config: &TasksConfig,
    patch: &mut Map<String, Value>,
    current_frontmatter: Option<&Map<String, Value>>,
    today: time::Date,
) -> bool {
    if !patch.contains_key("state") {
        return false;
    }
    let (task_type, state, _) = resulting_stage(patch, current_frontmatter);
    let Some(date_field) = config.closing(&task_type, &state).and_then(Closes::stamp) else {
        return false;
    };
    let already_closed = was_closed(
        config,
        &text_of(current_frontmatter, "task_type"),
        &text_of(current_frontmatter, "state"),
    );
    let given = !text_of(Some(patch), date_field).is_empty();
    let kept =
        already_closed && !patch.contains_key(date_field) && !text_of(current_frontmatter, date_field).is_empty();
    if !given && !kept {
        patch.insert(date_field.to_owned(), Value::String(crate::journal::format_date(today)));
    }
    !already_closed
}

/// [`TaskPatchBody::Body`]'s own save. `Ok(())` tells [`patch`] to re-fetch and re-render the fragment; `Err`
/// carries the final response to return as-is (a conflict/failure message, or a genuine transport error).
async fn save_body(
    client: &McpClient,
    vault_path: &str,
    old: String,
    new: String,
    expected_hash: Option<String>,
) -> Result<(), Box<Response>> {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(vault_path.to_owned()));
    let mut one_edit = Map::new();
    one_edit.insert("old_text".to_owned(), Value::String(old));
    one_edit.insert("new_text".to_owned(), Value::String(new));
    args.insert("edits".to_owned(), Value::Array(vec![Value::Object(one_edit)]));
    if let Some(hash) = expected_hash {
        args.insert("expected_hash".to_owned(), Value::String(hash));
    }
    match client.call_tool("fs_edit_file".to_owned(), args).await {
        Ok(result) if result.is_error != Some(true) => Ok(()),
        Ok(_) => Err(Box::new(mutation_error(
            "The body could not be saved; it may have changed since this page loaded.",
        ))),
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => Err(Box::new(unreachable())),
    }
}

/// [`TaskPatchBody::Fields`]'s own save: builds the merge patch, applies [`stamp_close_dates`]'s own close-action
/// side effects, writes it, and lists the closed task in the Journal (best-effort, see
/// `journal::append_task_event_best_effort`) exactly when this save is a genuine transition into a closed state. `Ok(())`/`Err` matches [`save_body`]'s identical
/// contract.
async fn save_fields(
    client: &McpClient,
    vault_name: &str,
    row: &TaskRow,
    vault_path: &str,
    mut raw: Map<String, Value>,
    today: time::Date,
) -> Result<(), Box<Response>> {
    let expected_hash = raw
        .remove("expected_hash")
        .and_then(|value| value.as_str().map(str::to_owned));
    let (current, loaded) = tokio::join!(
        read_frontmatter(client, vault_path),
        tasks_config::load_tasks_config_for_write(client, vault_name),
    );
    let current_frontmatter = match &current {
        Ok(Some(read)) => Some(&read.frontmatter),
        Ok(None) => None,
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => return Err(Box::new(unreachable())),
    };
    let mut patch = raw_fields_to_patch(raw, current_frontmatter);
    // A configuration that cannot be read cannot check a change of type or state, so that change waits for it; any
    // other edit goes ahead, and closes nothing it could not check.
    let tasks_config = match loaded {
        Ok(config) => config,
        Err(reason) if resulting_stage(&patch, current_frontmatter).2 => {
            return Err(Box::new(mutation_error(&escape_html(&format!("Not saved: {reason}.")))));
        }
        Err(_) => TasksConfig::default(),
    };
    if let Err(refusal) = check_state_for_type(&tasks_config, &patch, current_frontmatter) {
        return Err(Box::new(mutation_error(&escape_html(&refusal))));
    }
    let just_closed = stamp_close_dates(&tasks_config, &mut patch, current_frontmatter, today);

    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(vault_path.to_owned()));
    args.insert("patch".to_owned(), Value::Object(patch));
    if let Some(hash) = expected_hash {
        args.insert("expected_hash".to_owned(), Value::String(hash));
    }
    match client.call_tool("frontmatter_update".to_owned(), args).await {
        Ok(result) if result.is_error != Some(true) => {
            if just_closed {
                crate::journal::append_task_event_best_effort(
                    client,
                    vault_name,
                    crate::journal_config::TaskEvent::Closed,
                    &format!(
                        "{} ({})",
                        row.title,
                        super::tasks::task_reference(&row.path, Some(&row.id))
                    ),
                    today,
                )
                .await;
            }
            Ok(())
        }
        Ok(_) => Err(Box::new(mutation_error(
            "The task could not be updated; it may have changed since this page loaded.",
        ))),
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => Err(Box::new(unreachable())),
    }
}

/// `PATCH /{{vault_name}}/tasks/{{id}}`: [`TaskPatchBody`]'s own dispatch, matching `routes::wiki_save`'s identical
/// precedent. The body editor and the sidebar/close-action forms each submit independently (three forms, matching
/// the existing UI - `TaskPatchBody`'s own doc covers why), so this always re-fetches and re-renders the fragment
/// after either save succeeds rather than trying to reconcile which fields a partial in-memory view still has
/// correct.
pub async fn patch(
    State(state): State<TasksRoutesState>,
    Path((vault_name, id)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let rows = match fetch_task_rows(&client, &vault_name).await {
        Ok(rows) => rows,
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => return unreachable(),
    };
    let needle = id.to_ascii_lowercase();
    let Some(row) = rows.iter().find(|row| row.id.to_ascii_lowercase() == needle) else {
        return not_found();
    };
    let vault_path = format!("{vault_name}://{}", row.path);
    let Ok(parsed) = serde_json::from_slice::<TaskPatchBody>(&body) else {
        return malformed_create_request();
    };
    let saved = match parsed {
        TaskPatchBody::Body {
            old,
            new,
            expected_hash,
        } => Box::pin(save_body(&client, &vault_path, old, new, expected_hash)).await,
        TaskPatchBody::Move(TaskMoveBody { folder }) => Box::pin(move_note(&client, &vault_name, row, &folder)).await,
        TaskPatchBody::Fields(raw) => {
            let today = state.vault_time.today().await;
            Box::pin(save_fields(&client, &vault_name, row, &vault_path, raw, today)).await
        }
    };
    if let Err(response) = saved {
        return *response;
    }
    match Box::pin(render_detail_fragment(&client, &vault_name, &id, false)).await {
        Ok((_, fragment)) => html_response(fragment),
        Err(response) => *response,
    }
}

#[cfg(test)]
#[path = "tasks_detail_test.rs"]
mod tests;
