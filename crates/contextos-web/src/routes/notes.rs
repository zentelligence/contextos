//! `GET`/`POST`/`PATCH`/`DELETE /{{vault_name}}/notes/{{*path}}`: the Notes
//! screen. `path` is a note's own `relative_path` (relative to the
//! configured `notes_dir`, `notes::NoteState::relative_path`) - a direct
//! mapping onto `notes_dir`'s own real on-disk structure, matching
//! `routes::wiki`'s identical "the resource's path is its identifier" model
//! (`docs/http-routing-response-contract.md`), never a `?note=` query
//! parameter standing in for it. Unlike Wiki, `path` never names a folder to
//! browse: a note is always a single real `.md` file, so `path` is either
//! absent (the bare root) or names one directly.
//!
//! `GET` on the bare root selects the most recent note, or - only when
//! `notes_dir` has no real note at all yet - creates a brand-new one
//! automatically, so the screen opens straight into an editable note rather
//! than an empty prompt an operator would have to click through ([`get_dispatch`]'s
//! own doc for the full selection rule). `GET` on a real note's own path
//! selects that note; a path naming no real note falls back to the same
//! "most recent" rule the bare root uses, a stale link degrading gracefully
//! rather than 404ing - deliberately unlike Wiki's own "an unresolvable path
//! is a genuine 404" contract, since a renamed or deleted note's own old
//! link is common and recoverable here in a way a genuinely wrong Wiki path
//! is not.
//!
//! `docs/http-routing-response-contract.md`'s own naming conventions reserve four
//! sub-paths as never valid entity identifiers: `/new`, `/edit`, `/confirm-delete`, and
//! (an appropriate addition to that set, resolved directly with the repository owner)
//! `/rename`. Notes implements `/new` (`GET .../new` loads the "New" form, pre-filled from
//! [`notes::new_note_defaults`]; `POST` on the bare root is that form's own submit action,
//! [`notes::create_note_at`] - file path and name are the only thing the operator must
//! actually supply, everything else auto-loads from the configured template) and
//! `/confirm-delete` (dispatched inside [`get_dispatch`] itself, loading that note's own
//! delete confirmation). `/edit` is not implemented, since a note's own body is always
//! inline-editable in the read view itself, unlike Wiki/Insights/Entity-folders' own
//! separate edit mode - this one genuinely has no form to load. `/rename` (its own
//! `PATCH .../rename` sub-path, dispatched inside [`patch_body`] itself) renames the file.
//!
//! `PATCH` on a note's own path saves its edited body or its frontmatter
//! ([`SaveBody`]'s own doc). `DELETE` on a note's own path moves it to
//! trash; the bare root is never a deletable target of its own, and - once
//! `notes_dir` has at least one real note - never a creatable one directly
//! either: every creation after the first goes through `/new`'s own form.

use std::path::PathBuf;
use std::sync::Arc;

use askama::Template;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::config;
use crate::date_structure::DateStructureSide;
use crate::inbox_outbox::sanitised_relative_path;
use crate::journal;
use crate::mcp_client::{McpCallError, McpClient, McpClientSet};
use crate::metadata_panel::{self, MetadataFieldView};
use crate::notes::{self, DeleteNoteError, LoadNoteError, NoteState, RenameNoteError, SaveNoteError};
use crate::notes_config::{self, NotesConfig};
use crate::rendering::page;
use crate::rendering::shell::{self, ActiveScreen};
use crate::rendering::title_action::{self, TitleAction};
use crate::routes::file_response::{self, FileRequest};
use crate::routes::notes_view::{self, TreeEvent};
use crate::routes::vault::{html_response, not_found, unreachable};

#[derive(Clone)]
pub struct NotesRoutesState {
    pub clients: Arc<McpClientSet>,
    pub web_config_path: Arc<PathBuf>,
    /// The operator's local time, for every date this screen writes into a vault.
    pub vault_time: Arc<crate::vault_time::VaultTime>,
}

impl From<super::CommonRoutesState> for NotesRoutesState {
    fn from(value: super::CommonRoutesState) -> Self {
        Self {
            clients: value.clients,
            web_config_path: value.web_config_path,
            vault_time: value.vault_time,
        }
    }
}

impl NotesRoutesState {
    fn client(&self) -> Result<Arc<McpClient>, Box<Response>> {
        crate::routes::primary_session(&self.clients)
    }
}

async fn ensure_vault(client: &McpClient, vault_name: &str) -> Result<(), Box<Response>> {
    match crate::apps::vault_exists(client, vault_name).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(Box::new(not_found())),
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => Err(Box::new(unreachable())),
    }
}

/// Validates a raw, axum-captured `{{*path}}` wildcard value as a
/// `/`-separated relative path with no traversal segment, ending in `.md` -
/// a single real note file, never a directory (Notes has no folder-browsing
/// destination of its own, unlike Wiki's `{{*path}}`). `None` for anything
/// malformed, so a hand-edited or stale link degrades to "nothing
/// requested" (falls back to the most recent note, [`get_dispatch`]'s own
/// doc) rather than an unvalidated value ever reaching an MCP tool call.
fn validate_note_path(raw: &str) -> Option<String> {
    let segments = sanitised_relative_path(raw).ok()?;
    // `segments.last()?` already returns `None` when `segments` is empty
    // (`sanitised_relative_path("")` yields no segments), so there is
    // nothing further to check once a last segment is in hand.
    let is_markdown_file = std::path::Path::new(segments.last()?)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"));
    if !is_markdown_file {
        return None;
    }
    Some(segments.join("/"))
}

/// A path this screen may show: a note ([`validate_note_path`]), or a base, canvas, or diagram in the notes folder,
/// which is shown read-only. Only showing accepts the latter; every write goes through [`validate_note_path`], so a
/// page file is never saved, renamed, created, or deleted here.
fn validate_shown_path(raw: &str) -> Option<String> {
    validate_note_path(raw).or_else(|| {
        let segments = sanitised_relative_path(raw).ok()?;
        crate::file_view::PageFile::try_from(*segments.last()?).ok()?;
        Some(segments.join("/"))
    })
}

/// `path` with its trailing `/{{suffix}}` reserved sub-path removed
/// (`docs/http-routing-response-contract.md`'s reserved `/edit`/`/confirm-delete`
/// sub-paths, `routes::wiki::action_path`'s identical technique). `None`
/// when `path` does not end with `suffix` at all.
fn action_path<'a>(path: &'a str, suffix: &str) -> Option<&'a str> {
    path.strip_suffix(suffix)
}

// ---------------------------------------------------------------------
// View models
// ---------------------------------------------------------------------

struct SelectedNoteView {
    weekday: String,
    iso_week: String,
    metadata_fields: Vec<MetadataFieldView>,
    body: String,
    expected_hash: String,
    /// The rename form's own starting value: the file's current basename,
    /// without its `.md` extension.
    rename_value: String,
    /// The note's path within the notes folder, from which its save, rename, and delete URLs are made.
    relative_path: String,
}

/// `state.date`, parsed as a real calendar date and rendered as its own
/// weekday name and ISO week number, `("", "")` when `date` is absent or
/// not a plain `YYYY-MM-DD` value (a hand-authored note, or a differently
/// configured template) - the header simply omits that part rather than
/// showing a guess.
fn weekday_and_iso_week(date: Option<&str>) -> (String, String) {
    date.and_then(journal::parse_date)
        .map(|date| {
            (
                journal::weekday_name(date.weekday()).to_owned(),
                format!("week {}", date.iso_week()),
            )
        })
        .unwrap_or_default()
}

fn selected_note_view(state: &NoteState) -> SelectedNoteView {
    let (weekday, iso_week) = weekday_and_iso_week(state.date.as_deref());
    let current_name = state.relative_path.rsplit('/').next().unwrap_or(&state.relative_path);
    SelectedNoteView {
        weekday,
        iso_week,
        metadata_fields: metadata_panel::metadata_field_views(&state.frontmatter),
        body: state.body.clone(),
        expected_hash: state.expected_hash.clone().unwrap_or_default(),
        rename_value: current_name.strip_suffix(".md").unwrap_or(current_name).to_owned(),
        relative_path: state.relative_path.clone(),
    }
}

/// A base, canvas, or diagram shown on the Notes screen.
struct PageFileView {
    title: String,
    html: String,
}

#[derive(Template)]
#[template(path = "notes.html")]
struct NotesTemplate {
    /// A base, canvas, or diagram in the notes folder, shown read-only in place of a note.
    page_file: Option<PageFileView>,
    /// Every URL the template links to within the vault.
    urls: crate::urls::VaultUrls,
    /// Whether the sidebar is split into a dated and a regular pane at all
    /// ([`NotesConfig::date_structure_active`]).
    date_structure_active: bool,
    /// Whether the dated pane renders on the left (`true`) or the right
    /// (`false`), when [`Self::date_structure_active`] is on
    /// ([`NotesConfig::date_structure_side`]).
    date_structure_side_is_left: bool,
    /// Which side the metadata panel stacks above the folder pane already
    /// rendering on that side, independent of
    /// [`Self::date_structure_side_is_left`]
    /// ([`crate::notes_config::NotesConfig::metadata_panel_side`]'s own
    /// doc).
    metadata_panel_side_is_left: bool,
    /// The regular pane's own collapsible tree, flattened for rendering
    /// ([`notes_view::flatten`]): every note when the split is off,
    /// otherwise every note whose own path does *not* match the configured
    /// `date_format`, alphabetically ascending
    /// ([`notes_view::build_split_view`]'s own doc).
    regular_tree_events: Vec<notes_view::TreeEvent>,
    /// The dated pane's own collapsible tree, flattened the same way -
    /// empty when the split is off (`date_structure_active` is `false`),
    /// in which case the template never renders this pane at all.
    dated_tree_events: Vec<notes_view::TreeEvent>,
    selected: Option<SelectedNoteView>,
    /// The "New" form's own pre-filled state, when this response is `GET .../new`'s own
    /// ([`render_new_form_fragment`]) - mutually exclusive with `selected`, never both at
    /// once.
    creating: Option<CreatingView>,
    error: Option<String>,
}

/// `GET /{{vault_name}}/notes/new`'s own pre-filled create form state
/// ([`notes::new_note_defaults`]).
struct CreatingView {
    default_path: String,
    default_body: String,
}

/// `GET /{{vault_name}}/notes/{{*path}}/confirm-delete`'s own fragment: `docs/http-routing-response-contract.md`'s
/// `GET /{entities}/{id}/confirm-delete` pattern, `{id}` here being this note's own real
/// path. Swaps into the same `#notes-main-body` container the rename and body forms live
/// in (`notes.html`'s own wrapper around that pair), in place of them - the header above it
/// (an icon plus the Delete action) stays, so the operator can still cancel back to it.
#[derive(Template)]
#[template(path = "notes_confirm_delete.html")]
struct NotesConfirmDeleteTemplate {
    /// Every URL the template links to within the vault.
    urls: crate::urls::VaultUrls,
    title: String,
    filename: String,
    relative_path: String,
}

/// Builds [`NotesConfirmDeleteTemplate`] from a loaded note: just enough of
/// it to let the operator recognise the note before confirming (`title`
/// and its own basename), plus the same percent-encoded path segment every
/// other action on this note already carries.
fn notes_confirm_delete_template(vault_name: &str, state: &NoteState) -> NotesConfirmDeleteTemplate {
    let filename = state
        .relative_path
        .rsplit('/')
        .next()
        .unwrap_or(&state.relative_path)
        .to_owned();
    NotesConfirmDeleteTemplate {
        urls: crate::urls::VaultUrls::from(vault_name),
        title: state.title.clone(),
        filename,
        relative_path: state.relative_path.clone(),
    }
}

fn load_error_message(error: &LoadNoteError) -> String {
    match error {
        LoadNoteError::NotFound => "This note could not be found.".to_owned(),
        LoadNoteError::Malformed => "This note's own frontmatter could not be read.".to_owned(),
        LoadNoteError::Write(_) => "The vault could not be reached.".to_owned(),
    }
}

/// `GET .../new`'s own pre-filled state ([`notes::new_note_defaults`]), degrading to an
/// empty form plus an actionable error when the configured template itself fails to
/// render - the same "the write pipeline is the contract; a downstream failure never
/// blocks the operator outright" spirit as everywhere else in this crate, applied to a
/// read this time.
async fn load_new_form_state(
    client: &McpClient,
    vault_name: &str,
    config: &NotesConfig,
    now: time::OffsetDateTime,
) -> (CreatingView, Option<String>) {
    match notes::new_note_defaults(client, vault_name, config, now).await {
        Ok((default_path, default_body)) => (
            CreatingView {
                default_path,
                default_body,
            },
            None,
        ),
        Err(error) => (
            CreatingView {
                default_path: String::new(),
                default_body: String::new(),
            },
            Some(format!("The create form could not be prepared: {error}")),
        ),
    }
}

// ---------------------------------------------------------------------
// GET /{{vault_name}}/notes/{{*path}}
// ---------------------------------------------------------------------

/// Shared by [`get_root`] (`raw_path` empty) and [`get_path`] (`raw_path` the captured
/// `{{*path}}`, `routes::wiki::get_dispatch`'s identical split). `raw_path`'s own reserved
/// `/confirm-delete` sub-path and its own reserved `new` (bare, the whole path - the
/// contract's `GET .../new` pattern) are dispatched here first, before any note-selection
/// logic runs at all ([`confirm_delete_dispatch`], [`render_new_form_fragment`]).
///
/// Otherwise: selects the most recent note, or - only when `notes_dir` has no real note at
/// all yet - creates a brand-new one automatically (through [`notes::create_note`],
/// `note_create`, bypassing the "New" form entirely: there is nobody to show a form to
/// yet) so the screen opens straight into an editable note rather than an empty prompt an
/// operator would have to click through; that auto-create itself failing (an unresolvable
/// template, an unreachable session) is the one case the screen's own empty-state prompt
/// is ever actually shown for. `raw_path` naming a real note selects that one instead;
/// naming no real note (absent, malformed, a stale link to a renamed or deleted note)
/// falls back to the same "most recent" selection, never a 404 - see this module's own doc
/// for why that deliberately differs from Wiki.
async fn get_dispatch(
    state: NotesRoutesState,
    vault_name: String,
    raw_path: String,
    file: FileRequest,
    headers: HeaderMap,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let config = notes_config::load_notes_config(&client, &vault_name).await;
    // Only a path that stays inside the notes folder is asked of the vault; any other is left to the screen, which
    // refuses it.
    if let Some(shown) = crate::inbox_outbox::sanitised_relative_path(&raw_path)
        .ok()
        .filter(|segments| !segments.is_empty())
        .map(|segments| segments.join("/"))
        && let Some(response) =
            file_response::before_screen(&client, &vault_name, &format!("{}/{shown}", config.notes_dir), &file).await
    {
        return response;
    }

    if let Some(target) = action_path(&raw_path, "/confirm-delete") {
        return Box::pin(confirm_delete_dispatch(&client, &vault_name, &config, target)).await;
    }

    let (fragment, shown) = if raw_path == "new" {
        let (creating, prepare_error) =
            load_new_form_state(&client, &vault_name, &config, state.vault_time.instant().await).await;
        let form = Box::pin(render_new_form_fragment(
            &client,
            &vault_name,
            &config,
            creating,
            prepare_error,
        ))
        .await;
        (form, None)
    } else {
        let requested = validate_shown_path(&raw_path);
        let existing = notes::list_relative_paths(&client, &vault_name, &config).await;
        let (selected_note, create_error) = if existing.is_empty() {
            match notes::create_note(&client, &vault_name, &config, state.vault_time.instant().await).await {
                Ok(relative_path) => (Some(relative_path), None),
                Err(error) => (
                    None,
                    Some(format!("A first note could not be created automatically: {error}")),
                ),
            }
        } else {
            (requested, None)
        };
        Box::pin(render_notes_selection(
            &client,
            &vault_name,
            &config,
            selected_note.as_deref(),
            create_error,
        ))
        .await
    };
    if headers.contains_key("hx-request") {
        return html_response(fragment);
    }
    let web_config_path = Arc::clone(&state.web_config_path);
    let entity_base_path = crate::urls::VaultUrls::from(&*vault_name).notes("");
    let known_mcp_servers = state.clients.names();
    let (mut nav, appearance, mcp_status) = tokio::join!(
        Box::pin(shell::build_nav(
            &client,
            ActiveScreen::Vault,
            Some(&vault_name),
            Some("notes"),
            None,
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
    // A page whose address names the file it shows is that file's page; the screen's own address, and an address
    // naming a file the screen could not show, keep the Notes title bar.
    if let Some(shown) = shown.filter(|shown| *shown == raw_path) {
        let path = notes::note_path(&config, &shown);
        let page_href = crate::urls::VaultUrls::from(&*vault_name).notes(&shown);
        shell::show_file(
            &mut nav,
            &client,
            &vault_name,
            (&crate::view_favourites::NOTES).into(),
            (&path, &page_href),
        )
        .await;
    }
    nav.set_title_actions(title_action::render(&TitleAction::NewNote(&vault_name)));
    html_response(page::render_page(&nav, "Notes", &fragment)).into_response()
}

pub async fn get_root(
    State(state): State<NotesRoutesState>,
    Path(vault_name): Path<String>,
    headers: HeaderMap,
) -> Response {
    get_dispatch(state, vault_name, String::new(), FileRequest::default(), headers).await
}

pub async fn get_path(
    State(state): State<NotesRoutesState>,
    Path((vault_name, path)): Path<(String, String)>,
    file: FileRequest,
    headers: HeaderMap,
) -> Response {
    get_dispatch(state, vault_name, path, file, headers).await
}

// ---------------------------------------------------------------------
// POST /{{vault_name}}/notes/ - the explicit "New" action
// ---------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct CreateBody {
    /// The "New" form's own submitted path and body ([`notes::create_note_at`]'s own
    /// doc): both required for a real save, matching the form's own `required` path input
    /// and the repository owner's own "file path and name are the minimum to save"
    /// resolution.
    #[serde(default)]
    path: String,
    #[serde(default)]
    content: String,
}

/// `POST /{{vault_name}}/notes/`: the "New" form's own submit action
/// ([`notes::create_note_at`], `note_create` - never a direct write). An empty or
/// malformed `path` re-renders the form with its own submitted values preserved
/// (`render_new_form_fragment`) and an actionable error, never a raw `404` - this is the
/// HTMX surface's own "form error" contract (`docs/http-routing-response-contract.md`:
/// "re-render the form fragment with inline validation messages"), not a stable rejection
/// the way an action addressing an *existing* note's own malformed path is.
pub async fn create_root(
    State(state): State<NotesRoutesState>,
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
    let Ok(parsed) = serde_json::from_slice::<CreateBody>(&body) else {
        return malformed_body();
    };
    let config = notes_config::load_notes_config(&client, &vault_name).await;

    let Some(relative_path) = validate_note_path(&parsed.path) else {
        let creating = CreatingView {
            default_path: parsed.path,
            default_body: parsed.content,
        };
        let fragment = Box::pin(render_new_form_fragment(
            &client,
            &vault_name,
            &config,
            creating,
            Some("This note needs a file name ending in .md.".to_owned()),
        ))
        .await;
        return html_response(fragment);
    };

    match notes::create_note_at(
        &client,
        &vault_name,
        &config,
        state.vault_time.instant().await,
        &relative_path,
        &parsed.content,
    )
    .await
    {
        Ok(()) => {
            let target = crate::urls::VaultUrls::from(&*vault_name).notes(&relative_path);
            // `HX-Redirect`, not a real 3xx: matches `routes::tasks_create::create`'s
            // identical "this form is always submitted via `hx-post`" reasoning.
            (StatusCode::OK, [("HX-Redirect", target)], String::new()).into_response()
        }
        Err(error) => {
            let creating = CreatingView {
                default_path: relative_path,
                default_body: parsed.content,
            };
            let fragment = Box::pin(render_new_form_fragment(
                &client,
                &vault_name,
                &config,
                creating,
                Some(format!("The note could not be created: {error}")),
            ))
            .await;
            html_response(fragment)
        }
    }
}

// ---------------------------------------------------------------------
// PATCH /{{vault_name}}/notes/{{*path}} - editing an existing note, and its own
// "/rename" sub-path for the rename action
// ---------------------------------------------------------------------

/// `PATCH /{{vault_name}}/notes/{{*path}}`'s own request body: structurally two distinct
/// shapes on the one endpoint, matching `routes::vault_mutations::MdMutationBody`'s
/// identical "content-replace or frontmatter-patch, distinguished by which field is
/// present" contract - this screen's one partial-update action on a note, rather than a
/// second named PATCH sub-path per field group. `Content` replaces the body text verbatim,
/// keeping frontmatter untouched ([`notes::save_note_body`]); `Metadata` is the metadata
/// panel's own submitted form: one plain-text value per editable frontmatter field it
/// rendered, keyed by that field's own real frontmatter name (`base_view.html`'s identical
/// `data-field` convention) - never a raw JSON value, since every field this panel renders
/// as editable is a string or a string array shown as one comma-separated text input
/// ([`metadata_panel::metadata_field_views`]), so every submitted value is plain text;
/// [`metadata_panel::metadata_patch`] turns it back into the right JSON shape.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum SaveBody {
    Content {
        content: String,
        #[serde(default)]
        expected_hash: Option<String>,
    },
    Metadata {
        patch: std::collections::BTreeMap<String, String>,
        #[serde(default)]
        expected_hash: Option<String>,
    },
}

fn malformed_body() -> Response {
    html_response("<p class=\"hint error\">Malformed request.</p>".to_owned())
}

/// `PATCH /{{vault_name}}/notes/{{*path}}`: saves either the selected note's own body or
/// its frontmatter ([`SaveBody`]'s own doc), never both in one request - or, when `path`
/// ends in `/rename`, dispatches [`patch_rename_dispatch`] instead, matching
/// `routes::wiki::patch_body`'s identical suffix-dispatch shape.
pub async fn patch_body(
    State(state): State<NotesRoutesState>,
    Path((vault_name, raw_path)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let config = notes_config::load_notes_config(&client, &vault_name).await;

    if let Some(target) = action_path(&raw_path, "/rename").and_then(validate_note_path) {
        return Box::pin(patch_rename_dispatch(&client, &vault_name, &config, &target, &body)).await;
    }
    let Some(relative_path) = validate_note_path(&raw_path) else {
        return not_found();
    };
    let Ok(parsed) = serde_json::from_slice::<SaveBody>(&body) else {
        return malformed_body();
    };

    match parsed {
        SaveBody::Content { content, expected_hash } => {
            patch_body_content(&client, &vault_name, &config, &relative_path, &content, expected_hash).await
        }
        SaveBody::Metadata { patch, expected_hash } => {
            patch_body_metadata(&client, &vault_name, &config, &relative_path, patch, expected_hash).await
        }
    }
}

async fn patch_body_content(
    client: &McpClient,
    vault_name: &str,
    config: &NotesConfig,
    relative_path: &str,
    content: &str,
    expected_hash: Option<String>,
) -> Response {
    match notes::save_note_body(client, vault_name, config, relative_path, content, expected_hash).await {
        Ok(true) => html_response(
            Box::pin(render_notes_fragment(
                client,
                vault_name,
                config,
                Some(relative_path),
                None,
            ))
            .await,
        ),
        Ok(false) => {
            let fragment = Box::pin(render_notes_fragment(
                client,
                vault_name,
                config,
                Some(relative_path),
                Some("This note changed since it was last loaded; reload and try again.".to_owned()),
            ))
            .await;
            html_response(fragment)
        }
        Err(SaveNoteError::Load(LoadNoteError::NotFound)) => not_found(),
        Err(SaveNoteError::Load(LoadNoteError::Malformed)) => {
            let fragment = Box::pin(render_notes_fragment(
                client,
                vault_name,
                config,
                Some(relative_path),
                Some("This note's own frontmatter could not be read.".to_owned()),
            ))
            .await;
            html_response(fragment)
        }
        Err(
            SaveNoteError::Load(LoadNoteError::Write(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }))
            | SaveNoteError::Write(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }),
        ) => unreachable(),
    }
}

/// The metadata panel's own save: applies `submitted`'s edits to the
/// selected note's frontmatter, its body left untouched
/// ([`notes::save_note_metadata`]). Re-reads the note first (never trusting
/// a client-supplied shape, [`metadata_panel::metadata_patch`]'s own doc), so this is two
/// MCP calls rather than one, matching [`patch_body_content`]'s identical
/// "read fresh, then write" shape rather than [`notes::save_note_metadata`]'s
/// own leaner single-call contract for a caller that already knows the
/// shape it wants.
async fn patch_body_metadata(
    client: &McpClient,
    vault_name: &str,
    config: &NotesConfig,
    relative_path: &str,
    submitted: std::collections::BTreeMap<String, String>,
    expected_hash: Option<String>,
) -> Response {
    let current = match notes::load_note(client, vault_name, config, relative_path).await {
        Ok(state) => state,
        Err(LoadNoteError::Write(McpCallError::Unreachable { .. } | McpCallError::Refused { .. })) => {
            return unreachable();
        }
        Err(LoadNoteError::NotFound) => return not_found(),
        Err(LoadNoteError::Malformed) => {
            let fragment = Box::pin(render_notes_fragment(
                client,
                vault_name,
                config,
                Some(relative_path),
                Some("This note's own frontmatter could not be read.".to_owned()),
            ))
            .await;
            return html_response(fragment);
        }
    };
    let patch = metadata_panel::metadata_patch(&current.frontmatter, submitted);

    match notes::save_note_metadata(client, vault_name, config, relative_path, patch, expected_hash).await {
        Ok(true) => html_response(
            Box::pin(render_notes_fragment(
                client,
                vault_name,
                config,
                Some(relative_path),
                None,
            ))
            .await,
        ),
        Ok(false) => {
            let fragment = Box::pin(render_notes_fragment(
                client,
                vault_name,
                config,
                Some(relative_path),
                Some("This note changed since it was last loaded; reload and try again.".to_owned()),
            ))
            .await;
            html_response(fragment)
        }
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => unreachable(),
    }
}

#[derive(Debug, Default, Deserialize)]
struct RenameBody {
    #[serde(default)]
    new_name: String,
}

/// `PATCH /{{vault_name}}/notes/{{*path}}/rename`: renames the selected note's own file
/// ([`notes::rename_note`]), redirecting to its new own path on success so the URL an
/// operator bookmarks or shares always names the file's current name. Called from
/// [`patch_body`] once it strips the reserved `/rename` sub-path off the wildcard
/// (`routes::wiki::patch_rename`'s identical shape), never a route handler of its own.
async fn patch_rename_dispatch(
    client: &McpClient,
    vault_name: &str,
    config: &NotesConfig,
    relative_path: &str,
    body: &axum::body::Bytes,
) -> Response {
    let Ok(parsed) = serde_json::from_slice::<RenameBody>(body) else {
        return malformed_body();
    };

    match notes::rename_note(client, vault_name, config, relative_path, &parsed.new_name).await {
        Ok(new_relative_path) => {
            let target = crate::urls::VaultUrls::from(vault_name).notes(&new_relative_path);
            (StatusCode::OK, [("HX-Redirect", target)], String::new()).into_response()
        }
        Err(RenameNoteError::InvalidName(reason)) => {
            let fragment = Box::pin(render_notes_fragment(
                client,
                vault_name,
                config,
                Some(relative_path),
                Some(format!("File name {reason}.")),
            ))
            .await;
            html_response(fragment)
        }
        Err(RenameNoteError::Rejected) => {
            let fragment = Box::pin(render_notes_fragment(
                client,
                vault_name,
                config,
                Some(relative_path),
                Some("This note could not be renamed (a file with that name may already exist).".to_owned()),
            ))
            .await;
            html_response(fragment)
        }
        Err(RenameNoteError::Write(McpCallError::Unreachable { .. } | McpCallError::Refused { .. })) => unreachable(),
    }
}

/// `GET /{{vault_name}}/notes/{{*path}}/confirm-delete`: loads the selected note only far
/// enough to name it back to the operator, then renders [`NotesConfirmDeleteTemplate`]
/// ([`notes_confirm_delete_template`]). A malformed `path` is the same stable rejection
/// every other action on this screen gives it ([`validate_note_path`]); a note that fails
/// to load for any other reason (not found, unparsable, or the transport itself down)
/// falls back to the full screen fragment with its own error message, matching every other
/// handler's own error slot - never a bare error string. Called from [`get_dispatch`] once
/// it strips the reserved `/confirm-delete` sub-path off the wildcard
/// (`routes::wiki::get_dispatch`'s identical shape), never a route handler of its own.
async fn confirm_delete_dispatch(
    client: &McpClient,
    vault_name: &str,
    config: &NotesConfig,
    raw_path: &str,
) -> Response {
    let Some(relative_path) = validate_note_path(raw_path) else {
        return not_found();
    };
    match notes::load_note(client, vault_name, config, &relative_path).await {
        Ok(state) => html_response(
            notes_confirm_delete_template(vault_name, &state)
                .render()
                .unwrap_or_else(|_| "<p>The delete confirmation could not be rendered.</p>".to_owned()),
        ),
        Err(LoadNoteError::Write(McpCallError::Unreachable { .. } | McpCallError::Refused { .. })) => unreachable(),
        Err(error) => {
            let fragment = Box::pin(render_notes_fragment(
                client,
                vault_name,
                config,
                Some(&relative_path),
                Some(load_error_message(&error)),
            ))
            .await;
            html_response(fragment)
        }
    }
}

// ---------------------------------------------------------------------
// DELETE /{{vault_name}}/notes/{{*path}}
// ---------------------------------------------------------------------

/// `DELETE /{{vault_name}}/notes/{{*path}}`: moves the named note to trash
/// ([`notes::delete_note`]).
///
/// On success, re-renders the whole `.notes-screen` fragment with no note requested (the
/// deleted note is gone from the freshly-loaded list either way, so
/// [`render_notes_fragment`]'s own fallback selects whichever note is now most recent, or
/// shows the screen's own empty state when none remain) - the same `hx-target="closest
/// .notes-screen" hx-swap="outerHTML"` convention every other mutation on this screen
/// already uses, deliberately not the contract's alternative empty `204`-response option:
/// this screen always shows a list alongside a selected note's own detail pane as one
/// piece of state, unlike a plain list-of-rows screen a `204` row removal suits.
///
/// A malformed `path` is the same stable rejection every other action on this screen gives
/// it. A note that no longer exists, or that `fs_delete_file` otherwise refuses to delete,
/// re-renders the full screen fragment with an error message in the same `error` slot
/// every other handler already uses - never a raw `404`, matching the HTMX surface's own
/// error-fragment rule.
pub async fn delete_path(
    State(state): State<NotesRoutesState>,
    Path((vault_name, raw_path)): Path<(String, String)>,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let Some(relative_path) = validate_note_path(&raw_path) else {
        return not_found();
    };

    let config = notes_config::load_notes_config(&client, &vault_name).await;
    match notes::delete_note(&client, &vault_name, &config, &relative_path).await {
        Ok(()) => html_response(Box::pin(render_notes_fragment(&client, &vault_name, &config, None, None)).await),
        Err(DeleteNoteError::NotFound) => {
            let fragment = Box::pin(render_notes_fragment(
                &client,
                &vault_name,
                &config,
                Some(&relative_path),
                Some("This note could not be found.".to_owned()),
            ))
            .await;
            html_response(fragment)
        }
        Err(DeleteNoteError::Rejected) => {
            let fragment = Box::pin(render_notes_fragment(
                &client,
                &vault_name,
                &config,
                Some(&relative_path),
                Some("This note could not be deleted.".to_owned()),
            ))
            .await;
            html_response(fragment)
        }
        Err(DeleteNoteError::Write(McpCallError::Unreachable { .. } | McpCallError::Refused { .. })) => unreachable(),
    }
}

/// `DELETE /{{vault_name}}/notes`/`DELETE /{{vault_name}}/notes/`: the Notes root's own
/// bare routes carry no `{{*path}}` wildcard segment (axum's own wildcard cannot match zero
/// path segments, `build_router`'s own "collection root" comment), so a `DELETE` against
/// the root is never dispatched to [`delete_path`] at all - this always answers a real
/// `404`, matching `routes::wiki::delete_root`'s identical "the root itself is never a
/// deletable target" contract.
pub async fn delete_root(State(state): State<NotesRoutesState>, Path(vault_name): Path<String>) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    not_found()
}

#[path = "notes_fragments.rs"]
mod fragments;
use fragments::{render_new_form_fragment, render_notes_fragment, render_notes_selection};

#[cfg(test)]
#[path = "notes_test.rs"]
mod tests;
