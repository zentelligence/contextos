//! `GET`/`POST`/`PATCH /{{vault_name}}/insights/{{*path}}`: the
//! Insights screen. `path` is a direct mapping onto `insights_dir`'s own
//! real on-disk structure (`{vault_name}://{insights_dir}/{path}`, per
//! `docs/http-routing-response-contract.md`'s own "a resource's identifier
//! is its path" model) - never a query parameter standing in for it. `GET`
//! on the root lists every real shard; `GET` on a real shard (a single path
//! segment) lists its own notes; `GET` on a real note
//! (`{{shard}}/{{slug}}.md`) renders it read, with its own metadata panel (real frontmatter
//! fields, shown and edited independently of the body) and its real backlinks. `POST` on the
//! root creates a note ([`routes::insights_create::create`], always through `note_create`, its
//! own shard derived from the title's slug - never chosen by the request path). `PATCH` on a
//! note saves either its edited body or its frontmatter (the metadata panel's own submitted
//! form) - the dispatch itself lives in `routes::insights_save`, matching
//! `routes::insights_create`'s own identical "split the action out, this module only ever
//! calls back into it" precedent. A `path` naming neither a real shard nor a real note is a
//! genuine `404`, matching `routes::wiki`'s identical contract.
//!
//! `docs/http-routing-response-contract.md`'s own naming conventions reserve four sub-paths as
//! never valid entity identifiers: `/new`, `/edit`, `/confirm-delete`, and `/rename`. All four are
//! implemented. `/new` (`GET .../new`, always at the bare Insights root - a real shard key is
//! always exactly one of the fixed 36 `a`-`z`/`0`-`9` characters, [`insights::list_shards`], so the
//! three-character `new` can never collide with one) loads the dedicated "New insight" form,
//! pre-filled from [`insights::default_title`] and the domain's own `"proposed"` status default
//! (`insights::create_insight`'s own empty-status handling) - matching `routes::wiki`'s own
//! corrected `/new` contract: the header's own "New" button loads this form (`hx-get`) rather than
//! posting directly, so `routes::insights_create::create`'s own `POST` handler now only ever
//! answers a submission that already came from it - every `CreateInsightError` it can produce
//! re-renders the `/new` form itself, the operator's own submitted title and (where representable
//! in the status `<select>`) status preserved, never the plain shard-browse fragment. Unlike Wiki,
//! there is no folder-depth precondition a shard could fail: any title is always valid to attempt,
//! so `/new` never falls back to an explanatory non-form state the way Wiki's too-shallow-folder
//! case does. `/edit` loads a note's own edit form; `/confirm-delete` names the note a delete would
//! remove before confirming it; `/rename` (its own `PATCH .../rename` sub-path) updates a note's
//! own title and status together.
//!
//! Selecting a note always keeps the shard picker open on that note's own
//! shard - there is no independent "shard open, but a note from elsewhere
//! selected" state.

use std::path::PathBuf;
use std::sync::Arc;

use askama::Template;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};

use crate::config;
use crate::file_view::{PageFile, page_file_exists, page_file_html};
use crate::inbox_outbox::sanitised_relative_path;
use crate::insights::{self, ReadInsightError, STATUSES};
use crate::insights_config::{self, InsightsConfig};
use crate::last_visited;
use crate::mcp_client::{McpCallError, McpClient, McpClientSet};
use crate::metadata_panel::{self, MetadataFieldView};
use crate::note_render::{self, NoteView};
use crate::panel_side::PanelSide;
use crate::rendering::page;
use crate::rendering::shell::{self, ActiveScreen};
use crate::routes::file_response::{self, FileRequest};
use crate::routes::vault::{html_response, not_found, unreachable};
use crate::vault_content;

#[derive(Clone)]
pub struct InsightsRoutesState {
    pub clients: Arc<McpClientSet>,
    pub web_config_path: Arc<PathBuf>,
    /// The operator's local time, for every date this screen writes into a vault.
    pub vault_time: Arc<crate::vault_time::VaultTime>,
}

impl From<super::CommonRoutesState> for InsightsRoutesState {
    fn from(value: super::CommonRoutesState) -> Self {
        Self {
            clients: value.clients,
            web_config_path: value.web_config_path,
            vault_time: value.vault_time,
        }
    }
}

impl InsightsRoutesState {
    pub(super) fn client(&self) -> Result<Arc<McpClient>, Box<Response>> {
        crate::routes::primary_session(&self.clients)
    }
}

pub(super) async fn ensure_vault(client: &McpClient, vault_name: &str) -> Result<(), Box<Response>> {
    match crate::apps::vault_exists(client, vault_name).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(Box::new(not_found())),
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => Err(Box::new(unreachable())),
    }
}

/// A URL path segment sequence, sanitised (no traversal) and rejoined with
/// `/`, matching `routes::wiki::sanitised_path`'s identical contract.
pub(super) fn sanitised_path(raw: &str) -> Option<String> {
    let segments = sanitised_relative_path(raw).ok()?;
    Some(segments.join("/"))
}

/// `path` as exactly `{{shard}}/{{filename}}.md` (a real note's own shape) -
/// `None` for the root, a bare shard, or anything deeper.
pub(super) fn as_note_path(path: &str) -> Option<&str> {
    let mut segments = path.split('/');
    segments.next()?;
    let filename = segments.next()?;
    if segments.next().is_some() {
        return None;
    }
    if !filename.to_ascii_lowercase().ends_with(".md") {
        return None;
    }
    Some(path)
}

/// `path` with its trailing `/{{suffix}}` action segment removed, only when
/// what remains is a real note's own shape ([`as_note_path`]) - the
/// `docs/http-routing-response-contract.md` reserved-sub-path pattern
/// (`/edit`, `/confirm-delete`) extended to this screen's own path-is-
/// identity note addressing, plus `/rename` for the combined title/status
/// action. `None` both when `path` does not end with `/{{suffix}}` at all
/// and when what would remain names a bare shard or anything deeper than
/// one note - a suffix can never collide with a real note's own path
/// either way, since every real note path ends in `.md` and no `suffix`
/// this screen uses does.
pub(super) fn note_action_path<'a>(path: &'a str, suffix: &str) -> Option<&'a str> {
    as_note_path(path.strip_suffix(suffix)?)
}

// ---------------------------------------------------------------------
// View models
// ---------------------------------------------------------------------

struct ShardRowView {
    key: String,
    href: String,
    active: bool,
}

struct NoteRowView {
    label: String,
    status: String,
    href: String,
    active: bool,
}

struct BacklinkView {
    label: String,
    href: Option<String>,
}

struct SelectedNoteView {
    title: String,
    /// Whether the selection is a note, which has a status, properties, and backlinks and can be edited; a base,
    /// canvas, or diagram is shown as it is.
    is_note: bool,
    status: String,
    rendered_html: String,
    backlinks: Vec<BacklinkView>,
    path: String,
    /// The rename-and-restatus form's own status `<select>`, one entry per
    /// [`STATUSES`] with `selected` set on whichever matches this note's
    /// own current status - precomputed here rather than compared in the
    /// template, matching `page_shell.html`'s own `option.is_current`
    /// precedent.
    status_options: Vec<StatusOptionView>,
    /// The selected note's own real frontmatter fields, for the metadata panel
    /// ([`metadata_panel::metadata_field_views`]) - degrades to an empty list on any read
    /// failure (`insights::note_frontmatter`'s own contract) rather than blocking this note's
    /// own read view.
    metadata_fields: Vec<MetadataFieldView>,
    /// The metadata panel's own conflict-check hash: the same file content hash
    /// [`EditingView::edit_expected_hash`] carries when editing, computed separately here
    /// when it is not (the plain read view has no edit form of its own to have loaded it
    /// already).
    expected_hash: String,
}

struct StatusOptionView {
    value: &'static str,
    selected: bool,
}

/// Present only right after a brand-new note is created: swaps the main
/// body straight into its own edit form instead of the read view an empty
/// note would otherwise show, matching `routes::wiki::EditingView`'s
/// identical reasoning and shape.
struct EditingView {
    edit_body: String,
    edit_expected_hash: String,
}

/// `GET .../new`'s own pre-filled create-note form state, analogous to
/// `routes::wiki::CreatingView` - the one real difference being that an
/// insight note carries a `status` alongside its title, so this also
/// precomputes that field's own `<select>` (via [`status_options`], the same
/// helper the selected-note rename form already uses) rather than a bare
/// parent-folder label, since Insights has no folder concept a new note
/// waits to land in.
struct CreatingView {
    default_title: String,
    status_options: Vec<StatusOptionView>,
}

fn status_options(current: &str) -> Vec<StatusOptionView> {
    STATUSES
        .iter()
        .map(|status| StatusOptionView {
            value: status,
            selected: *status == current,
        })
        .collect()
}

fn shard_href(vault_name: &str, shard: &str) -> String {
    crate::urls::VaultUrls::from(vault_name).insights(shard)
}

fn note_href(vault_name: &str, relative_path: &str) -> String {
    crate::urls::VaultUrls::from(vault_name).insights(relative_path)
}

/// Whether `relative_path` names a real note in the real shard/note store - the same check
/// `render_insights_fragment` makes inline for its own "select a note" path, extracted here since
/// [`get_dispatch`]'s own resume-last-visited check needs it before that function's own tree load.
async fn note_exists(client: &McpClient, vault_name: &str, config: &InsightsConfig, relative_path: &str) -> bool {
    let Some(shard) = relative_path.split('/').next() else {
        return false;
    };
    if !insights::list_shards().iter().any(|real| real == shard) {
        return false;
    }
    let notes = insights::list_shard_notes(client, vault_name, config, shard).await;
    notes.iter().any(|note| note.relative_path == relative_path)
}

/// A full-navigation `Redirect` for a plain browser `GET`, or the HTMX-native `HX-Redirect` header
/// on a `200` for an `hx-request`, matching `routes::wiki::redirect_response`'s identical local
/// helper - this crate's own per-module convention rather than one shared function.
fn redirect_response(target: &str, is_hx_request: bool) -> Response {
    if is_hx_request {
        (StatusCode::OK, [("HX-Redirect", target.to_owned())], String::new()).into_response()
    } else {
        Redirect::to(target).into_response()
    }
}

#[derive(Template)]
#[template(path = "insights.html")]
struct InsightsTemplate {
    /// Every URL the template links to within the vault.
    urls: crate::urls::VaultUrls,
    shard_side_is_left: bool,
    /// Which side the metadata panel stacks above the shard-picker pane already rendering on
    /// that side, independent of [`Self::shard_side_is_left`]
    /// ([`crate::insights_config::InsightsConfig::metadata_panel_side`]'s own doc). Only
    /// meaningful when [`Self::selected`] is `Some`: the panel never renders without a
    /// selected note's own frontmatter to show.
    metadata_panel_side_is_left: bool,
    /// The one CSS grid modifier class this render's own layout needs
    /// ([`insights_layout_class`]) - `""` for the plain base two-column grid.
    layout_class: &'static str,
    shard_rows: Vec<ShardRowView>,
    note_rows: Vec<NoteRowView>,
    current_shard: String,
    selected: Option<SelectedNoteView>,
    editing: Option<EditingView>,
    /// The dedicated "New insight" form's own pre-filled state, when this response is `GET
    /// .../new`'s own ([`render_new_form_fragment`]) - mutually exclusive with `selected`, never
    /// more than one at once.
    creating: Option<CreatingView>,
    error: Option<String>,
}

/// The one CSS grid modifier class an Insights render's own layout needs, given both
/// independently configurable panel placements (`shard_side_is_left`,
/// `metadata_panel_side_is_left`) - matching `routes::wiki::wiki_layout_class`'s identical
/// four-combination reasoning: Insights has exactly the same two independent side preferences
/// (its own shard-picker nav pane, and the metadata panel), so the same combinatorics apply.
/// `metadata_present` is `false` whenever no note is selected (browsing a shard, the dedicated
/// "New" form, or the plain empty state), since the metadata panel never renders without a
/// selected note's own frontmatter to show - in that case only `shard_side_is_left` matters,
/// exactly as before this config existed. `""` renders the plain two-column base grid (the
/// shard-picker pane, or the shard-picker pane stacked with the metadata panel, on the left);
/// the other three variants each name a real CSS rule in `contextos-web.css`, shared with Wiki.
fn insights_layout_class(
    shard_side_is_left: bool,
    metadata_panel_side_is_left: bool,
    metadata_present: bool,
) -> &'static str {
    if !metadata_present || shard_side_is_left == metadata_panel_side_is_left {
        return if shard_side_is_left {
            ""
        } else {
            "notes-layout-panel-right"
        };
    }
    if shard_side_is_left {
        "notes-layout-metadata-right"
    } else {
        "notes-layout-metadata-left-panel-right"
    }
}

/// `GET /{{vault_name}}/insights/{{*path}}/confirm-delete`'s own
/// fragment (the contract's reserved `/confirm-delete` sub-path), matching
/// `routes::notes::NotesConfirmDeleteTemplate`'s identical shape and
/// interaction pattern, addressed by path rather than a `?note=` query
/// value.
#[derive(Template)]
#[template(path = "insights_confirm_delete.html")]
struct InsightsConfirmDeleteTemplate {
    /// Every URL the template links to within the vault.
    urls: crate::urls::VaultUrls,
    title: String,
    filename: String,
    path: String,
}

fn load_error_message(error: &ReadInsightError) -> String {
    match error {
        ReadInsightError::NotFound => "This note could not be found.".to_owned(),
        ReadInsightError::Write(_) => "The vault could not be reached.".to_owned(),
    }
}

fn shard_rows(vault_name: &str, shards: &[String], current_shard: &str) -> Vec<ShardRowView> {
    shards
        .iter()
        .map(|shard| ShardRowView {
            active: shard == current_shard,
            href: shard_href(vault_name, shard),
            key: shard.clone(),
        })
        .collect()
}

fn note_rows(vault_name: &str, notes: &[insights::InsightSummary], active_path: Option<&str>) -> Vec<NoteRowView> {
    notes
        .iter()
        .map(|note| NoteRowView {
            active: active_path == Some(note.relative_path.as_str()),
            href: note_href(vault_name, &note.relative_path),
            status: note.status.clone().unwrap_or_default(),
            label: note.title.clone(),
        })
        .collect()
}

/// Renders the shard-picker-plus-note-list fragment (`shard` empty at the
/// root) with no note selected.
fn render_shard_browse(
    vault_name: &str,
    shard_side_is_left: bool,
    metadata_panel_side_is_left: bool,
    shards: &[String],
    shard: &str,
    notes: &[insights::InsightSummary],
    error: Option<String>,
) -> String {
    InsightsTemplate {
        urls: crate::urls::VaultUrls::from(vault_name),
        shard_side_is_left,
        metadata_panel_side_is_left,
        layout_class: insights_layout_class(shard_side_is_left, metadata_panel_side_is_left, false),
        shard_rows: shard_rows(vault_name, shards, shard),
        note_rows: note_rows(vault_name, notes, None),
        current_shard: shard.to_owned(),
        selected: None,
        editing: None,
        creating: None,
        error,
    }
    .render()
    .unwrap_or_else(|_| "<p>The Insights screen could not be rendered.</p>".to_owned())
}

/// The sidebar's own shard picker and current shard's note list, plus both independent panel
/// side preferences, threaded as one unit through the render functions below -
/// `render_note_selected` otherwise needs every one of these five plus its other, unrelated
/// parameters, past this workspace's own argument-count lint, matching
/// `routes::wiki::PageRenderContext`'s identical reasoning.
struct ShardContext<'a> {
    shard_side_is_left: bool,
    metadata_panel_side_is_left: bool,
    shards: &'a [String],
    shard: &'a str,
    notes: &'a [insights::InsightSummary],
}

/// A base, canvas, or diagram in a shard, shown through the shared file view as it is: it has no status,
/// properties, or backlinks of its own here.
async fn page_file_selection(
    client: &McpClient,
    vault_name: &str,
    config: &InsightsConfig,
    kind: PageFile,
    relative_path: &str,
) -> (Option<SelectedNoteView>, Option<String>) {
    let full_path = insights::note_path(config, relative_path);
    match page_file_html(client, vault_name, kind, &full_path, None).await {
        Ok(rendered_html) => (
            Some(SelectedNoteView {
                title: relative_path.rsplit('/').next().unwrap_or(relative_path).to_owned(),
                is_note: false,
                status: String::new(),
                rendered_html,
                backlinks: Vec::new(),
                path: relative_path.to_owned(),
                status_options: Vec::new(),
                metadata_fields: Vec::new(),
                expected_hash: String::new(),
            }),
            None,
        ),
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => {
            (None, Some("The vault could not be reached.".to_owned()))
        }
    }
}

/// Renders the selected-note fragment: `context` supplies the sidebar's
/// own shard picker and sibling listing; `relative_path` is the selected
/// note itself.
async fn render_note_selected(
    client: &McpClient,
    vault_name: &str,
    config: &InsightsConfig,
    context: ShardContext<'_>,
    relative_path: &str,
    error: Option<String>,
) -> String {
    let ShardContext {
        shard_side_is_left,
        metadata_panel_side_is_left,
        shards,
        shard,
        notes,
    } = context;
    let (selected, load_error) = if let Ok(kind) = PageFile::try_from(relative_path) {
        page_file_selection(client, vault_name, config, kind, relative_path).await
    } else {
        match insights::read_note(client, vault_name, config, relative_path).await {
            Ok(raw) => {
                // The edit form's own body/hash is loaded separately, on demand,
                // by `render_edit_response` when the operator actually opens
                // `.../edit` - the read view no longer needs it up front now
                // that editing is its own reserved sub-path rather than an
                // always-rendered inline form.
                let full_path = insights::note_path(config, relative_path);
                let (rendered, backlinks, frontmatter, hash) = tokio::join!(
                    note_render::render_note(
                        client,
                        vault_name,
                        NoteView {
                            path: &full_path,
                            raw: Some(&raw),
                            template: false,
                        }
                    ),
                    insights::load_backlinks(client, vault_name, config, relative_path),
                    insights::note_frontmatter(client, vault_name, config, relative_path),
                    vault_content::file_hash(client, vault_name, &full_path),
                );
                let status = notes
                    .iter()
                    .find(|note| note.relative_path == relative_path)
                    .and_then(|note| note.status.clone())
                    .unwrap_or_default();
                let title = relative_path
                    .rsplit('/')
                    .next()
                    .unwrap_or(relative_path)
                    .strip_suffix(".md")
                    .unwrap_or(relative_path)
                    .to_owned();
                (
                    Some(SelectedNoteView {
                        status_options: status_options(&status),
                        title,
                        is_note: true,
                        status,
                        rendered_html: rendered.html,
                        backlinks: backlinks
                            .into_iter()
                            .map(|backlink| BacklinkView {
                                href: backlink
                                    .relative_path
                                    .as_deref()
                                    .map(|relative_path| note_href(vault_name, relative_path)),
                                label: backlink.title,
                            })
                            .collect(),
                        path: (relative_path).to_owned(),
                        metadata_fields: metadata_panel::link_field_views(
                            &frontmatter,
                            &rendered.frontmatter,
                            &rendered.routes,
                        ),
                        expected_hash: hash.unwrap_or_default(),
                    }),
                    None,
                )
            }
            Err(error) => (None, Some(load_error_message(&error))),
        }
    };

    InsightsTemplate {
        urls: crate::urls::VaultUrls::from(vault_name),
        shard_side_is_left,
        metadata_panel_side_is_left,
        layout_class: insights_layout_class(shard_side_is_left, metadata_panel_side_is_left, selected.is_some()),
        shard_rows: shard_rows(vault_name, shards, shard),
        note_rows: note_rows(vault_name, notes, Some(relative_path)),
        current_shard: shard.to_owned(),
        selected,
        editing: None,
        creating: None,
        error: error.or(load_error),
    }
    .render()
    .unwrap_or_else(|_| "<p>The Insights screen could not be rendered.</p>".to_owned())
}

/// After a brand-new note is created, renders the same fragment
/// [`render_note_selected`] would for a normal `GET`, but with its own edit
/// form already populated in place of the read view - matching
/// `routes::wiki::render_created_page`'s identical reasoning. `None` when
/// `relative_path`'s own shard cannot be resolved - it always can,
/// immediately after `insights::create_insight` succeeded against that
/// same path, but the caller still turns that into a real `404` rather
/// than an `unwrap`.
pub(super) async fn render_created_note(
    client: &McpClient,
    vault_name: &str,
    config: &InsightsConfig,
    relative_path: &str,
) -> Option<String> {
    let shard_side_is_left = config.shard_side == PanelSide::Left;
    let metadata_panel_side_is_left = config.metadata_panel_side == PanelSide::Left;
    let shards = insights::list_shards();
    let shard = relative_path.split('/').next()?;
    let notes = insights::list_shard_notes(client, vault_name, config, shard).await;
    let note = notes.iter().find(|note| note.relative_path == relative_path)?;
    let status = note.status.clone().unwrap_or_default();
    let title = relative_path
        .rsplit('/')
        .next()
        .unwrap_or(relative_path)
        .strip_suffix(".md")
        .unwrap_or(relative_path)
        .to_owned();
    let (frontmatter, (edit_body, edit_expected_hash)) = tokio::join!(
        insights::note_frontmatter(client, vault_name, config, relative_path),
        async {
            match insights::load_note_body(client, vault_name, config, relative_path).await {
                Ok((body, hash)) => (body, hash.unwrap_or_default()),
                Err(_) => (String::new(), String::new()),
            }
        },
    );

    Some(
        InsightsTemplate {
            urls: crate::urls::VaultUrls::from(vault_name),
            shard_side_is_left,
            metadata_panel_side_is_left,
            layout_class: insights_layout_class(shard_side_is_left, metadata_panel_side_is_left, true),
            shard_rows: shard_rows(vault_name, &shards, shard),
            note_rows: note_rows(vault_name, &notes, Some(relative_path)),
            current_shard: shard.to_owned(),
            selected: Some(SelectedNoteView {
                status_options: status_options(&status),
                title,
                is_note: true,
                status,
                rendered_html: String::new(),
                backlinks: Vec::new(),
                path: (relative_path).to_owned(),
                metadata_fields: metadata_panel::linked_field_views(
                    client,
                    vault_name,
                    &insights::note_path(config, relative_path),
                    &frontmatter,
                )
                .await,
                expected_hash: edit_expected_hash.clone(),
            }),
            editing: Some(EditingView {
                edit_body,
                edit_expected_hash,
            }),
            creating: None,
            error: None,
        }
        .render()
        .unwrap_or_else(|_| "<p>The Insights screen could not be rendered.</p>".to_owned()),
    )
}

/// `GET .../new`'s own dedicated create-note form fragment, and `routes::insights_create::create`'s
/// own `POST`-error re-render target for every `CreateInsightError` - sharing this one function
/// keeps both paths' own rendering identical, matching `routes::wiki::render_new_form_fragment`'s
/// identical precedent. Always renders the shard-picker sidebar with nothing selected (`shard: ""`)
/// since a new insight's own shard is only known once its title's slug exists - there is no parent
/// folder to scope this form to the way Wiki's own version is scoped to `parent_dir`, and therefore
/// no gate this form could ever fail to satisfy: a real "note create form" always exists.
///
/// `default_title` and `status` seed the form's own fields - the operator's just-submitted (and
/// rejected) values on a `POST`-error re-render, or [`insights::default_title`]/`"proposed"` on a
/// fresh `GET`. `status` is shown via [`status_options`]'s own `selected` matching; a submitted
/// value outside [`STATUSES`] (a malformed or tampered request - the `<select>` itself only ever
/// offers the three real values) simply leaves no option marked `selected`, the closest this
/// control shape can come to preserving it verbatim.
pub(super) fn render_new_form_fragment(
    vault_name: &str,
    config: &InsightsConfig,
    default_title: String,
    status: &str,
    error: Option<String>,
) -> String {
    let shard_side_is_left = config.shard_side == PanelSide::Left;
    let metadata_panel_side_is_left = config.metadata_panel_side == PanelSide::Left;
    let shards = insights::list_shards();
    InsightsTemplate {
        urls: crate::urls::VaultUrls::from(vault_name),
        shard_side_is_left,
        metadata_panel_side_is_left,
        layout_class: insights_layout_class(shard_side_is_left, metadata_panel_side_is_left, false),
        shard_rows: shard_rows(vault_name, &shards, ""),
        note_rows: Vec::new(),
        current_shard: String::new(),
        selected: None,
        editing: None,
        creating: Some(CreatingView {
            default_title,
            status_options: status_options(status),
        }),
        error,
    }
    .render()
    .unwrap_or_else(|_| "<p>The Insights screen could not be rendered.</p>".to_owned())
}

/// Resolves `path` against the real shard/note store and renders whichever
/// fragment applies: the root shard picker, a shard's own note list, a
/// selected note, or `None` when `path` names none of those - the caller
/// turns that into a real `404`.
pub(super) async fn render_insights_fragment(
    client: &McpClient,
    vault_name: &str,
    config: &InsightsConfig,
    path: &str,
    error: Option<String>,
) -> Option<String> {
    let shard_side_is_left = config.shard_side == PanelSide::Left;
    let metadata_panel_side_is_left = config.metadata_panel_side == PanelSide::Left;
    let shards = insights::list_shards();

    if path.is_empty() {
        return Some(render_shard_browse(
            vault_name,
            shard_side_is_left,
            metadata_panel_side_is_left,
            &shards,
            "",
            &[],
            error,
        ));
    }
    let mut segments = path.splitn(2, '/');
    let shard = segments.next().unwrap_or_default();
    if !shards.iter().any(|real| real == shard) {
        return None;
    }
    let notes = insights::list_shard_notes(client, vault_name, config, shard).await;

    if segments.next().is_none() {
        return Some(render_shard_browse(
            vault_name,
            shard_side_is_left,
            metadata_panel_side_is_left,
            &shards,
            shard,
            &notes,
            error,
        ));
    }
    if PageFile::try_from(path).is_ok() {
        // The shard's own listing holds notes only, so a base, canvas, or diagram is confirmed directly.
        if !page_file_exists(client, vault_name, &insights::note_path(config, path)).await {
            return None;
        }
    } else {
        notes.iter().find(|note| note.relative_path == path)?;
    }
    let context = ShardContext {
        shard_side_is_left,
        metadata_panel_side_is_left,
        shards: &shards,
        shard,
        notes: &notes,
    };
    Some(Box::pin(render_note_selected(client, vault_name, config, context, path, error)).await)
}

// ---------------------------------------------------------------------
// GET /{{vault_name}}/insights/{{*path}}
// ---------------------------------------------------------------------

/// `GET /{{vault_name}}/insights/{{*path}}/edit`: the whole
/// screen, its own shard picker included, with `editing` populated in
/// place of the read view - never just a bare `#insights-main-body`
/// fragment alone. The header's own Rename/Edit/Delete cluster lives
/// outside that div, as a sibling within the same
/// `.notes-main-header`; a response that swapped only the body left that
/// cluster stale, rendering it alongside the edit form's own Save/Cancel
/// pair rather than in place of it. Matches `routes::wiki::render_edit_
/// response`'s identical contract, and `render_created_note`'s identical
/// "editing populated, in the full template" shape for the one other case
/// that already needed it.
async fn render_edit_response(
    client: &McpClient,
    vault_name: &str,
    config: &InsightsConfig,
    note_path: &str,
) -> Response {
    let shard_side_is_left = config.shard_side == PanelSide::Left;
    let metadata_panel_side_is_left = config.metadata_panel_side == PanelSide::Left;
    let shards = insights::list_shards();
    let shard = note_path.split('/').next().unwrap_or("");
    let notes = insights::list_shard_notes(client, vault_name, config, shard).await;
    let status = notes
        .iter()
        .find(|note| note.relative_path == note_path)
        .and_then(|note| note.status.clone())
        .unwrap_or_default();
    let title = note_path
        .rsplit('/')
        .next()
        .unwrap_or(note_path)
        .strip_suffix(".md")
        .unwrap_or(note_path)
        .to_owned();
    let (frontmatter, loaded_body) = tokio::join!(
        insights::note_frontmatter(client, vault_name, config, note_path),
        insights::load_note_body(client, vault_name, config, note_path),
    );
    match loaded_body {
        Ok((edit_body, edit_expected_hash)) => {
            let hash = edit_expected_hash.unwrap_or_default();
            html_response(
                InsightsTemplate {
                    urls: crate::urls::VaultUrls::from(vault_name),
                    shard_side_is_left,
                    metadata_panel_side_is_left,
                    layout_class: insights_layout_class(shard_side_is_left, metadata_panel_side_is_left, true),
                    shard_rows: shard_rows(vault_name, &shards, shard),
                    note_rows: note_rows(vault_name, &notes, Some(note_path)),
                    current_shard: shard.to_owned(),
                    selected: Some(SelectedNoteView {
                        status_options: status_options(&status),
                        title,
                        is_note: true,
                        status,
                        rendered_html: String::new(),
                        backlinks: Vec::new(),
                        path: (note_path).to_owned(),
                        metadata_fields: metadata_panel::linked_field_views(
                            client,
                            vault_name,
                            &insights::note_path(config, note_path),
                            &frontmatter,
                        )
                        .await,
                        expected_hash: hash.clone(),
                    }),
                    editing: Some(EditingView {
                        edit_body,
                        edit_expected_hash: hash,
                    }),
                    creating: None,
                    error: None,
                }
                .render()
                .unwrap_or_else(|_| "<p>The Insights screen could not be rendered.</p>".to_owned()),
            )
        }
        Err(ReadInsightError::NotFound) => not_found(),
        Err(ReadInsightError::Write(McpCallError::Unreachable { .. } | McpCallError::Refused { .. })) => unreachable(),
    }
}

/// `GET /{{vault_name}}/insights/{{*path}}/confirm-delete`: names
/// the note before confirming ([`insights::note_display_title`]), matching
/// `routes::notes::confirm_delete`'s identical contract - a note that fails
/// to load for any reason other than the transport itself being down falls
/// back to the shard's own browse fragment with an error message, never a
/// bare error string or a raw `404`.
async fn render_confirm_delete_response(
    client: &McpClient,
    vault_name: &str,
    config: &InsightsConfig,
    note_path: &str,
) -> Response {
    match insights::note_display_title(client, vault_name, config, note_path).await {
        Ok(title) => html_response(
            InsightsConfirmDeleteTemplate {
                urls: crate::urls::VaultUrls::from(vault_name),
                title,
                filename: note_path.rsplit('/').next().unwrap_or(note_path).to_owned(),
                path: (note_path).to_owned(),
            }
            .render()
            .unwrap_or_else(|_| "<p>The delete confirmation could not be rendered.</p>".to_owned()),
        ),
        Err(ReadInsightError::Write(McpCallError::Unreachable { .. } | McpCallError::Refused { .. })) => unreachable(),
        Err(ReadInsightError::NotFound) => {
            let shard = note_path.split('/').next().unwrap_or("");
            let fragment = Box::pin(render_insights_fragment(
                client,
                vault_name,
                config,
                shard,
                Some("This note could not be found.".to_owned()),
            ))
            .await
            .unwrap_or_else(|| "<p>The Insights screen could not be rendered.</p>".to_owned());
            html_response(fragment)
        }
    }
}

async fn get_dispatch(
    state: InsightsRoutesState,
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
    let Some(path) = sanitised_path(&raw_path) else {
        return not_found();
    };
    let config = insights_config::load_insights_config(&client, &vault_name).await;
    if !path.is_empty()
        && let Some(response) =
            file_response::before_screen(&client, &vault_name, &format!("{}/{path}", config.insights_dir), &file).await
    {
        return response;
    }

    // The Insights sidebar link's own bare root: resume the operator's last-visited real note,
    // matching `routes::wiki::get_dispatch`'s identical contract and reasoning - re-validated
    // against the real shard/note store here, never trusted blindly.
    if path.is_empty() {
        let last_visited_state = last_visited::read_from_headers(&headers);
        if let Some(target) = last_visited::insights_last_visited(&last_visited_state, &vault_name)
            && note_exists(&client, &vault_name, &config, target).await
        {
            return redirect_response(&note_href(&vault_name, target), headers.contains_key("hx-request"));
        }
    }

    if let Some(note_path) = note_action_path(&path, "/edit") {
        return Box::pin(render_edit_response(&client, &vault_name, &config, note_path)).await;
    }
    if let Some(note_path) = note_action_path(&path, "/confirm-delete") {
        return Box::pin(render_confirm_delete_response(&client, &vault_name, &config, note_path)).await;
    }

    // `path == "new"` is the bare Insights root's own reserved form - unlike Wiki's per-folder
    // scoping, an insight note's own shard is always derived from its title's slug, never from
    // where the operator happened to be browsing, so this form is never scoped to anything else.
    // Real shard keys are always exactly one character ([`insights::list_shards`]), so the
    // three-character `new` can never be mistaken for one.
    let fragment = if path == "new" {
        let default_title = insights::default_title(state.vault_time.instant().await);
        // `"proposed"` mirrors `insights::create_insight`'s own domain-level empty-status
        // default - the fresh form's own seed, never a routing-layer synthesised fallback (that
        // distinction is `title`'s own, not `status`'s: see `routes::insights_create::CreateBody`).
        Some(render_new_form_fragment(
            &vault_name,
            &config,
            default_title,
            "proposed",
            None,
        ))
    } else {
        Box::pin(render_insights_fragment(&client, &vault_name, &config, &path, None)).await
    };
    let Some(fragment) = fragment else {
        return not_found();
    };
    if headers.contains_key("hx-request") {
        return html_response(fragment);
    }
    let web_config_path = Arc::clone(&state.web_config_path);
    let breadcrumb_suffix = format!("insights/{path}");
    let entity_base_path = crate::urls::VaultUrls::from(&*vault_name).insights(&path);
    let known_mcp_servers = state.clients.names();
    let (mut nav, appearance, mcp_status) = tokio::join!(
        Box::pin(shell::build_nav(
            &client,
            ActiveScreen::Vault,
            Some(&vault_name),
            Some(&breadcrumb_suffix),
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
    if contextos_markdown::FileKind::from(path.as_str()).is_page() {
        let shown = format!("{}/{path}", config.insights_dir);
        shell::show_file(
            &mut nav,
            &client,
            &vault_name,
            (&crate::view_favourites::INSIGHTS).into(),
            (&shown, &entity_base_path),
        )
        .await;
    }
    html_response(page::render_page(&nav, "Insights", &fragment)).into_response()
}

pub async fn get_root(
    State(state): State<InsightsRoutesState>,
    Path(vault_name): Path<String>,
    headers: HeaderMap,
) -> Response {
    get_dispatch(state, vault_name, String::new(), FileRequest::default(), headers).await
}

pub async fn get_path(
    State(state): State<InsightsRoutesState>,
    Path((vault_name, path)): Path<(String, String)>,
    file: FileRequest,
    headers: HeaderMap,
) -> Response {
    get_dispatch(state, vault_name, path, file, headers).await
}

// ---------------------------------------------------------------------
// PATCH /{{vault_name}}/insights/{{*path}} - `routes::insights_save`
// ---------------------------------------------------------------------

// ---------------------------------------------------------------------
// DELETE /{{vault_name}}/insights/{{*path}} - `routes::insights_delete`
// ---------------------------------------------------------------------

#[cfg(test)]
#[path = "insights_test.rs"]
mod tests;
