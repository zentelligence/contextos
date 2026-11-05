//! The task creation screen (`GET /{{vault_name}}/tasks/new`, the identical
//! field-by-field view [`routes::tasks_detail`] renders for editing, against
//! a not-yet-created note) and its own `POST /{{vault_name}}/tasks/` route
//! ([`create`]). Shares `routes::tasks_detail_fields`'s field schema and
//! `routes::tasks_detail`'s [`TaskDetailQuery`]/`malformed_create_request`
//! rather than a second, smaller implementation of either - creating a task
//! is genuinely the same field-by-field view as editing one, not a form
//! with its own reduced field set.

use std::collections::HashMap;
use std::sync::Arc;

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::config;
use crate::entities;
use crate::mcp_client::McpCallError;
use crate::rendering::escape_html;
use crate::rendering::page;
use crate::rendering::shell::{self, ActiveScreen};
use crate::routes::vault::{html_response, unreachable};
use crate::task_folder::{TaskFolder, known_task_folders};
use crate::tasks_config;
use crate::team;

use super::tasks::{TasksRoutesState, ensure_vault, fetch_task_rows};
use super::tasks_detail::{TaskDetailQuery, malformed_create_request};
use super::tasks_detail_fields::{FieldContext, FieldView, build_fields, raw_fields_to_patch};
use super::tasks_fields::{FieldWidget, SelectOption};
use super::tasks_key_prefixes::{TaskTypeChoice, available_task_types, default_task_type, load_declared_prefixes};
use crate::tasks_config::TasksConfig;

/// Shown when the submitted `task_type` is not one this vault's tasks base offers.
const TASK_TYPE_UNAVAILABLE: &str = "The chosen task type is not available for this vault's tasks base.";

/// The not-yet-created task's own placeholder id segment, for the link
/// search widget's URL only (`routes::tasks_links::search`'s own
/// self-exclusion is then simply a no-op, since no real task ever carries
/// this literal id) - never rendered, never submitted.
const NEW_TASK_ID_PLACEHOLDER: &str = "new";

#[derive(Template)]
#[template(path = "task_create.html")]
struct TaskCreateTemplate {
    /// Every URL the template links to within the vault.
    urls: crate::urls::VaultUrls,
    id: String,
    title_field: FieldView,
    identity_fields: Vec<FieldView>,
    date_fields: Vec<FieldView>,
    remaining_fields: Vec<FieldView>,
    link_fields: Vec<super::tasks_links::LinkFieldView>,
    /// Where a new task of each offered type starts when its creator leaves the state to its type.
    start_note: Option<String>,
    /// The folders that already hold a task, suggested by the folder input.
    known_folders: Vec<String>,
    /// The folder input's starting value: the top level, which an empty folder names.
    folder: String,
}

/// The `state` option that leaves a new task's state to its type's initial state.
const TYPE_DEFAULT_STATE_LABEL: &str = "Default for its type";

/// Where a new task of each offered type starts, in one sentence, for the create form.
fn start_note(config: &TasksConfig, available: &[TaskTypeChoice]) -> Option<String> {
    let starts: Vec<String> = available
        .iter()
        .filter_map(|choice| {
            let state = config.initial_state(&choice.value)?;
            Some(format!(
                "a new {} starts {}",
                choice.label.to_lowercase(),
                config.label_for_state(&choice.value, state)
            ))
        })
        .collect();
    let joined = match starts.as_slice() {
        [] => return None,
        [only] => only.clone(),
        [first, second] => format!("{first} and {second}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    };
    let mut letters = joined.chars();
    let capitalised: String = letters
        .next()
        .into_iter()
        .flat_map(char::to_uppercase)
        .chain(letters)
        .collect();
    Some(format!("{capitalised}, unless you choose its state."))
}

/// Stamps the date a new task created straight into one of its type's closing states closes with (`completed` or
/// `cancelled`), as closing it on its page would, unless its creator gave one; reports whether it was created closed.
fn stamp_created_close(
    config: &TasksConfig,
    task_type: &str,
    today: time::Date,
    frontmatter: &mut Map<String, Value>,
) -> bool {
    let state = frontmatter.get("state").and_then(Value::as_str).unwrap_or_default();
    let Some(date_field) = config
        .closing(task_type, state)
        .and_then(crate::tasks_config::Closes::stamp)
    else {
        return false;
    };
    let given = frontmatter
        .get(date_field)
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty());
    if !given {
        frontmatter.insert(date_field.to_owned(), Value::String(crate::journal::format_date(today)));
    }
    true
}

/// Fills in a new task's state when its creator left it to the type: the type's initial state. A chosen state must be
/// one the type declares.
///
/// # Errors
///
/// Returns the refusal to show when the chosen state is not one of the type's.
fn apply_type_default_state(
    config: &TasksConfig,
    task_type: &str,
    frontmatter: &mut Map<String, Value>,
) -> Result<(), String> {
    let chosen = frontmatter
        .get("state")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let state = match chosen {
        Some(state) if config.stage_of(task_type, &state).is_some() => state,
        Some(state) => {
            let type_label = config
                .task_type(task_type)
                .map_or(task_type, |entry| entry.display_label());
            let choices: Vec<&str> = config
                .lifecycle(task_type)
                .iter()
                .map(crate::tasks_config::StageEntry::display_label)
                .collect();
            return Err(format!(
                "`{state}` is not a state of a {type_label}. Choose one of {}, or leave it to its type.",
                choices.join(", ")
            ));
        }
        None => match config.initial_state(task_type) {
            Some(state) => state.to_owned(),
            None => return Ok(()),
        },
    };
    frontmatter.insert("state".to_owned(), Value::String(state));
    Ok(())
}

/// `GET /{{vault_name}}/tasks/new`: the task creation screen. Shares
/// `tasks_detail_fields::build_fields` and `task_field_widget.html`'s own
/// widget rendering with `routes::tasks_detail`'s own detail template. The
/// `task_type` control is special: `task_type` determines which id series
/// `base_record_create` assigns the id from, so it offers only the types
/// whose prefix `memory/tasks.md` declares, and no control at all when that
/// leaves a single choice (a task is then simply an `action`, as before).
pub async fn new_task_form(
    State(state): State<TasksRoutesState>,
    Path(vault_name): Path<String>,
    Query(query): Query<TaskDetailQuery>,
    headers: HeaderMap,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let (configured_entities, configured_team, tasks_config, declared_prefixes, rows) = tokio::join!(
        entities::load_entities(&client, &vault_name),
        team::load_team(&client, &vault_name),
        tasks_config::load_tasks_config(&client, &vault_name),
        load_declared_prefixes(&client, &vault_name),
        fetch_task_rows(&client, &vault_name),
    );
    // Suggestions only: a list that cannot be read leaves the folder input free text, as it always is.
    let known_folders = rows
        .map(|rows| known_task_folders(rows.iter().map(|row| row.path.as_str())))
        .unwrap_or_default();
    let field_context = FieldContext {
        entities: &configured_entities,
        team: &configured_team,
        tasks_config: &tasks_config,
    };
    let available = available_task_types(&tasks_config.task_type, &declared_prefixes);
    // The State choices lead with the default type's own states, then every other type's.
    let mut seed = Map::new();
    if let Some(choice) = default_task_type(&available) {
        seed.insert("task_type".to_owned(), Value::String(choice.value.clone()));
    }
    let mut field_groups = build_fields(&seed, &field_context);
    offer_task_types(&mut field_groups.identity, &available);
    offer_type_default_state(&mut field_groups.identity);

    let fragment_template = TaskCreateTemplate {
        urls: crate::urls::VaultUrls::from(&*vault_name),
        id: NEW_TASK_ID_PLACEHOLDER.to_owned(),
        title_field: field_groups.title,
        identity_fields: field_groups.identity,
        date_fields: field_groups.dates,
        remaining_fields: field_groups.remaining,
        link_fields: super::tasks_links::build_link_fields(&Map::new(), &HashMap::new()),
        start_note: start_note(&tasks_config, &available),
        known_folders,
        folder: String::new(),
    };
    let fragment = fragment_template
        .render()
        .unwrap_or_else(|_| "<p>The new task form could not be rendered.</p>".to_owned());
    if headers.contains_key("hx-request") {
        html_response(fragment)
    } else {
        let web_config_path = Arc::clone(&state.web_config_path);
        let entity_base_path = crate::urls::VaultUrls::from(&*vault_name).tasks_new();
        let known_mcp_servers = state.clients.names();
        let (mut nav, appearance, mcp_status) = tokio::join!(
            Box::pin(shell::build_nav(
                &client,
                ActiveScreen::Vault,
                Some(&vault_name),
                Some("tasks/new"),
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
        html_response(page::render_page(&nav, "New task", &fragment))
    }
    .into_response()
}

/// Restricts the identity fields' `task_type` control to the `available`
/// types, defaulting to `action` (or the first available type), or removes
/// it when there is nothing to choose between: the one available type then
/// applies without a control (an `action` unless the sidecar declares only
/// another type's prefix).
fn offer_task_types(identity: &mut Vec<FieldView>, available: &[TaskTypeChoice]) {
    if available.len() < 2 {
        identity.retain(|field| field.key != "task_type");
        return;
    }
    let default = default_task_type(available).map(|choice| choice.value.clone());
    for field in identity.iter_mut().filter(|field| field.key == "task_type") {
        field.widget = FieldWidget::Select(
            available
                .iter()
                .map(|choice| SelectOption {
                    value: choice.value.clone(),
                    label: choice.label.clone(),
                    selected: Some(&choice.value) == default.as_ref(),
                })
                .collect(),
        );
        default.clone().unwrap_or_default().clone_into(&mut field.value);
    }
}

/// Leads the `state` control with a selected "Default for its type" option, so a new task starts in its type's initial
/// state unless its creator picks one.
fn offer_type_default_state(identity: &mut [FieldView]) {
    for field in identity.iter_mut().filter(|field| field.key == "state") {
        if let FieldWidget::Select(options) = &mut field.widget {
            for option in options.iter_mut() {
                option.selected = false;
            }
            options.insert(
                0,
                SelectOption {
                    value: String::new(),
                    label: TYPE_DEFAULT_STATE_LABEL.to_owned(),
                    selected: true,
                },
            );
        }
        field.value.clear();
    }
}

#[derive(Debug, Deserialize)]
struct CreateTaskResult {
    path: String,
    #[serde(default)]
    key: Option<String>,
}

/// A refused `base_record_create` call's own explanation, such as a tasks base whose filters cannot take a record in
/// a sub-folder.
#[derive(Debug, Default, Deserialize)]
struct CreateTaskRefusal {
    #[serde(default)]
    message: String,
}

fn create_validation_failed(detail: &str) -> Response {
    html_response(format!("<p class=\"hint\">{}</p>", escape_html(detail)))
}

/// A new task's type, as the tasks sidecar offers it, and the configuration its state was settled against.
struct ResolvedType {
    choice: TaskTypeChoice,
    config: TasksConfig,
}

/// Settles a new task's type (one the tasks sidecar offers, else its default) and state (one its type declares, else
/// its type's initial state) in `frontmatter`.
///
/// # Errors
///
/// Returns the refusal to answer with when the configuration cannot be read, since a new task's state is chosen from,
/// or checked against, its type's states; when the type is not offered; or when the state is not one of the type's.
async fn type_and_state(
    client: &crate::mcp_client::McpClient,
    vault_name: &str,
    frontmatter: &mut Map<String, Value>,
) -> Result<ResolvedType, Box<Response>> {
    let (loaded, declared_prefixes) = tokio::join!(
        tasks_config::load_tasks_config_for_write(client, vault_name),
        load_declared_prefixes(client, vault_name),
    );
    let config = loaded.map_err(|reason| Box::new(create_validation_failed(&format!("Not created: {reason}."))))?;
    let available = available_task_types(&config.task_type, &declared_prefixes);
    let requested = frontmatter
        .get("task_type")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    let chosen = match requested {
        Some(requested) => available.iter().find(|choice| choice.value == requested),
        None => default_task_type(&available),
    };
    let Some(choice) = chosen.cloned() else {
        return Err(Box::new(create_validation_failed(TASK_TYPE_UNAVAILABLE)));
    };
    frontmatter.insert("task_type".to_owned(), Value::String(choice.value.clone()));
    apply_type_default_state(&config, &choice.value, frontmatter)
        .map_err(|refusal| Box::new(create_validation_failed(&refusal)))?;
    Ok(ResolvedType { choice, config })
}

/// The `base_record_create` arguments for a new task: its fields and body, the id series of its type, and the folder
/// beneath the tasks folder it is filed in, omitted for the top level.
fn create_arguments(
    vault_name: &str,
    frontmatter: Map<String, Value>,
    body_content: String,
    prefix: Option<&String>,
    folder: &TaskFolder,
) -> Map<String, Value> {
    let mut args = Map::new();
    args.insert(
        "path".to_owned(),
        Value::String(format!("{vault_name}://{}", crate::vault_links::TASKS_BASE)),
    );
    args.insert("frontmatter".to_owned(), Value::Object(frontmatter));
    args.insert("content".to_owned(), Value::String(body_content));
    if let Some(prefix) = prefix {
        args.insert("prefix".to_owned(), Value::String(prefix.clone()));
    }
    // Relative to the tasks base's own records folder.
    if !folder.as_str().is_empty() {
        args.insert("folder".to_owned(), Value::String(folder.as_str().to_owned()));
    }
    args
}

/// Sends the reader to the new task's own page, by its key.
///
/// `HX-Redirect`, not a real 3xx: this create form is always submitted via `hx-post`, and a genuine redirect response
/// would have htmx's own fetch follow it and swap the redirected GET's full page into this form's own fragment
/// target. `HX-Redirect` instead tells htmx to perform a real `window.location` navigation, landing the reader on the
/// new task's own complete page.
fn created_redirect(vault_name: &str, created: CreateTaskResult) -> Response {
    let urls = crate::urls::VaultUrls::from(vault_name);
    let target = created
        .key
        .map_or_else(|| urls.file(&created.path), |key| urls.task(&key));
    (StatusCode::OK, [("HX-Redirect", target)], String::new()).into_response()
}

/// `POST /{{vault_name}}/tasks/`: creates a note via `base_record_create`
/// against `memory/tasks.base` (its own sidecar allocates the next
/// sequential `id`), redirecting to the new task's own detail page on
/// success. The submitted body is [`new_task_form`]'s own flatly-named
/// fields plus `body` (the note's initial content, written atomically in
/// the same `base_record_create` call rather than a separate follow-up
/// write) - the identical shape and classification `routes::tasks_detail`'s
/// own `patch_frontmatter` uses, via the same
/// `tasks_detail_fields::raw_fields_to_patch`, since creating and editing
/// read the same fields the same way.
///
/// `task_type` chooses the id series: it must be one of the types whose prefix
/// `memory/tasks.md` declares, and the matching prefix is passed to
/// `base_record_create` so a decision gets a `D-` id and an action an `A-` id
/// from their own counters. Omitting it creates an `action`. A type the
/// tasks base does not offer is rejected, never silently replaced.
///
/// `folder` files the note beneath the tasks folder (`memory/tasks/`), passed to `base_record_create` as a folder
/// relative to the base's own records folder; empty or omitted files it at the top level. A folder that would leave
/// the tasks folder, or that is not text, is refused before any id is reserved, and a base whose filters take no
/// sub-folder refuses one with the server's own reason.
pub async fn create(
    State(state): State<TasksRoutesState>,
    Path(vault_name): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let mut raw: Map<String, Value> = if body.is_empty() {
        Map::new()
    } else {
        match serde_json::from_slice(&body) {
            Ok(parsed) => parsed,
            Err(_) => return malformed_create_request(),
        }
    };
    let body_content = raw
        .remove("body")
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default();

    // The shared field decoding turns every submitted value into text (an
    // array becomes its elements joined), so a `task_type` that is not
    // already a string must be refused here, not defaulted or coerced.
    if raw
        .get("task_type")
        .is_some_and(|value| !matches!(value, Value::String(_) | Value::Null))
    {
        return create_validation_failed(TASK_TYPE_UNAVAILABLE);
    }
    // The folder chooses where the note is filed, never a frontmatter field, so it is settled before the fields are
    // read and refused rather than repaired when it would leave the tasks folder.
    let folder = match raw.remove("folder").as_ref().map(TaskFolder::try_from) {
        None => TaskFolder::default(),
        Some(Ok(folder)) => folder,
        Some(Err(reason)) => return create_validation_failed(&format!("Not created: the folder {reason}.")),
    };
    let mut frontmatter = raw_fields_to_patch(raw, None);
    // A brand-new note simply omits an unset optional key; it never carries
    // a literal `null` the way an edit's clearing patch does.
    frontmatter.retain(|_, value| !value.is_null());
    let title = frontmatter
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Untitled task")
        .to_owned();
    frontmatter.insert("title".to_owned(), Value::String(title.clone()));
    let chosen = match type_and_state(&client, &vault_name, &mut frontmatter).await {
        Ok(resolved) => resolved,
        Err(response) => return *response,
    };
    let tasks_config = &chosen.config;
    let today = state.vault_time.today().await;
    let created_closed = stamp_created_close(tasks_config, &chosen.choice.value, today, &mut frontmatter);
    // Unlike `title` and `state`, `assignee` has no product default: a task left unassigned on creation simply
    // carries no `assignee` key, the same as any other unset optional field, rather than falling back to any
    // particular configured team member.
    if frontmatter
        .get("assignee")
        .and_then(Value::as_str)
        .is_some_and(str::is_empty)
    {
        frontmatter.remove("assignee");
    }

    let args = create_arguments(
        &vault_name,
        frontmatter,
        body_content,
        chosen.choice.prefix.as_ref(),
        &folder,
    );
    match client.call_tool("base_record_create".to_owned(), args).await {
        Ok(result) if result.is_error != Some(true) => match result.into_typed::<CreateTaskResult>() {
            Ok(created) => {
                // Listed in today's Journal entry under the section configured
                // for new tasks, if any, best-effort: see
                // `journal::append_task_event_best_effort` for why a failure
                // here never surfaces as this action's result. A task created
                // already closed is not listed as new.
                if !created_closed {
                    crate::journal::append_task_event_best_effort(
                        &client,
                        &vault_name,
                        crate::journal_config::TaskEvent::Opened,
                        &format!(
                            "{title} ({})",
                            super::tasks::task_reference(&created.path, created.key.as_deref())
                        ),
                        today,
                    )
                    .await;
                }
                created_redirect(&vault_name, created)
            }
            Err(_) => create_validation_failed("The task was created but its response could not be read."),
        },
        Ok(refused) => {
            let refusal = refused.into_typed::<CreateTaskRefusal>().unwrap_or_default();
            if refusal.message.is_empty() {
                create_validation_failed("The task could not be created.")
            } else {
                create_validation_failed(&format!("Not created: {}", refusal.message))
            }
        }
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => unreachable(),
    }
}
