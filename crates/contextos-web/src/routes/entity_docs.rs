//! `GET`/`POST`/`PATCH`/`DELETE /{{vault_name}}/entity/{{*path}}`:
//! the entity-folder screen. `path`'s first segment is a configured entity's
//! own slug and the remainder (if any) is a direct mapping onto that
//! entity's own real top-level directory
//! (`{vault_name}://{entity_slug}/{relative_path}`, per
//! `docs/http-routing-response-contract.md`'s own "a resource's identifier
//! is its path" model) - never a query parameter standing in for it.
//!
//! `GET` on a real folder (an entity slug alone, or with a real subfolder
//! path after it) renders that folder's own browse view (the full accordion
//! tree - see `routes::entity_view` - plus that folder's own contents);
//! `GET` on a real markdown file renders it inline. A real non-markdown file
//! has no rendering of its own here - it redirects to the existing general
//! vault content route instead, its own address, rather than this
//! screen re-implementing every content type's own rendering a second time.
//! `POST` on a folder (any real folder in the tree, including the entity's
//! own root - unlike Wiki, there is no minimum-depth floor here) creates a
//! markdown file inside it (always through `note_create`); `POST` on a
//! folder's own `/execute/new-folder` sub-path creates a new, empty
//! subfolder (`fs_create_directory`). `PATCH` on a markdown file saves its
//! edited body. `DELETE` on a file (markdown or not) or an empty folder
//! moves it to trash - a non-markdown file is exactly as deletable as a
//! markdown one, even though it has no rendering of its own here.
//!
//! A `path` naming neither a real folder nor a real file under a configured
//! entity, or an entity slug that names no configured entity at all, is a
//! genuine `404`.
//!
//! `docs/http-routing-response-contract.md`'s own naming conventions reserve four sub-paths as never
//! valid entity identifiers: `/new`, `/edit`, `/confirm-delete`, and `/rename`. Three of the four are
//! implemented here; `/rename` is not (this screen has no rename action of its own for a file, unlike
//! Wiki's own page-title rename - a genuine, pre-existing gap against the contract, out of scope for
//! the `/new` work that brought the other three into line with each other). `/new`, scoped to
//! whichever real folder it trails within a configured entity's own tree
//! (`GET .../{entity_slug}/{folder-path}/new`, or that entity's own root's own `GET .../{entity_slug}/new`),
//! loads that folder's own dedicated "New file" form, pre-filled from [`entity_docs::default_title`],
//! matching `routes::wiki`'s own corrected `/new` contract exactly - resolving the entity slug first,
//! exactly as every other route here already does (`entity_by_slug`, a real `404` for an unconfigured
//! one). Unlike Wiki, there is no `MIN_CREATE_DEPTH`-style floor here: any real folder in the tree, the
//! entity's own root included, always renders a real, usable form - never a too-shallow explanation in
//! its place. The header's own "New" button loads this form (`hx-get`, [`new_page_href`]) rather than
//! posting directly, so `write::create_dispatch`'s own `POST` handler now only ever answers a
//! submission that already came from this form: every failure it can produce
//! ([`entity_docs::CreateEntityFileError`]'s own three variants alike) re-renders the `/new` form
//! itself, the operator's own submitted title preserved, never the plain folder-browse fragment.
//! `/edit` loads a file's own edit form; `/confirm-delete` names the real file or folder a delete would
//! remove before confirming it.
//!
//! `GET` at the bare root (no entity slug at all) has no resource of its own
//! to show - unlike Wiki/Insights, this screen has no "all entities" mode -
//! so it redirects to the vault's first configured entity's own root URL,
//! matching `routes::vault::get_server_root`'s identical "redirect to
//! somewhere that resolves" contract. A vault with no configured entity at
//! all has nowhere to redirect to, so it renders a plain prompt instead.

use std::path::PathBuf;
use std::sync::Arc;

use askama::Template;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;

use crate::entities::{self, EntityEntry};
use crate::entity_docs::{self, EntityFileLink, EntityFolder, ReadEntityFileError};
use crate::entity_docs_config::{self, EntityDocsConfig};
use crate::file_view::{PageFile, page_file_html};
use crate::inbox_outbox::sanitised_relative_path;
use crate::last_visited;
use crate::mcp_client::{McpCallError, McpClient, McpClientSet};
use crate::metadata_panel::{self, MetadataFieldView};
use crate::note_render::{self, NoteView};
use crate::panel_side::PanelSide;
use crate::routes::entity_view::{self, TreeEvent};
use crate::routes::file_response::{self, FileRequest};
use crate::routes::vault::{html_response, not_found, unreachable};
use crate::vault_content;

#[derive(Clone)]
pub struct EntityDocsRoutesState {
    pub clients: Arc<McpClientSet>,
    pub web_config_path: Arc<PathBuf>,
    /// The operator's local time, for every date this screen writes into a vault.
    pub vault_time: Arc<crate::vault_time::VaultTime>,
}

impl From<super::CommonRoutesState> for EntityDocsRoutesState {
    fn from(value: super::CommonRoutesState) -> Self {
        Self {
            clients: value.clients,
            web_config_path: value.web_config_path,
            vault_time: value.vault_time,
        }
    }
}

impl EntityDocsRoutesState {
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

/// Splits a URL `path` into its entity slug (first segment) and the
/// remainder, rejoined with `/` (possibly empty - the entity's own root).
/// `None` for a malformed path (traversal, an empty segment) or one with no
/// segments at all; the caller only reaches this once it already knows
/// `raw` names at least one segment, so `None` here always means malformed.
fn split_entity_path(raw: &str) -> Option<(String, String)> {
    let segments = sanitised_relative_path(raw).ok()?;
    let (first, rest) = segments.split_first()?;
    Some(((*first).to_owned(), rest.join("/")))
}

fn entity_by_slug<'a>(entities: &'a [EntityEntry], slug: &str) -> Option<&'a EntityEntry> {
    entities.iter().find(|entity| entity.slug == slug)
}

/// Whether `folder` (a real, already-resolved folder) lists a file at
/// exactly `relative_path`.
fn file_at<'a>(folder: &'a EntityFolder, relative_path: &str) -> Option<&'a EntityFileLink> {
    folder.files.iter().find(|file| file.relative_path == relative_path)
}

/// `relative_path` (within the entity's own tree) with its trailing
/// `/{{suffix}}` reserved sub-path removed
/// (`docs/http-routing-response-contract.md`'s reserved `/edit`/
/// `/confirm-delete` sub-paths), matching `routes::wiki::action_path`'s
/// identical contract. `None` when `relative_path` does not end with
/// `suffix` at all.
fn action_path<'a>(relative_path: &'a str, suffix: &str) -> Option<&'a str> {
    relative_path.strip_suffix(suffix)
}

/// A path ending in `.md` - `None` for anything else, including a directory
/// path or a real non-markdown file, matching `routes::wiki::as_page_path`'s
/// identical contract: only a markdown file has an edit form of its own.
fn as_markdown_path(relative_path: &str) -> Option<&str> {
    relative_path
        .rsplit('/')
        .next()
        .filter(|filename| filename.to_ascii_lowercase().ends_with(".md"))
        .map(|_| relative_path)
}

/// `relative_path` with its trailing `/execute/new-folder` command sub-path
/// removed (`docs/http-routing-response-contract.md`'s own
/// `/{entities}/execute/{command}` pattern), matching
/// `routes::wiki::new_folder_action_path`'s identical contract, including
/// its own entity-root special case: `NEW_FOLDER_AT_ROOT` with no leading
/// `/`, since the entity's own root has no path segment of its own once the
/// slug is split off.
const NEW_FOLDER_SUFFIX: &str = "/execute/new-folder";
const NEW_FOLDER_AT_ROOT: &str = "execute/new-folder";

fn new_folder_action_path(relative_path: &str) -> Option<&str> {
    if let Some(stripped) = relative_path.strip_suffix(NEW_FOLDER_SUFFIX) {
        return Some(stripped);
    }
    (relative_path == NEW_FOLDER_AT_ROOT).then_some("")
}

/// The header row's own "New folder" form target for `relative_dir` inside
/// `entity_slug`'s own tree, matching [`dir_href`]'s identical root/non-root
/// shape.
fn new_folder_href(vault_name: &str, entity_slug: &str, relative_dir: &str) -> String {
    crate::urls::VaultUrls::from(vault_name).entity_action(entity_slug, relative_dir, NEW_FOLDER_AT_ROOT)
}

/// The header row's own "New" button target for `relative_dir` inside
/// `entity_slug`'s own tree: `GET .../new`'s own reserved sub-path, never an
/// immediate `POST` - matching `routes::wiki::new_page_href`'s identical
/// "the button loads the create form first" contract.
fn new_page_href(vault_name: &str, entity_slug: &str, relative_dir: &str) -> String {
    crate::urls::VaultUrls::from(vault_name).entity_action(entity_slug, relative_dir, "new")
}

// ---------------------------------------------------------------------
// Resolving a path against the real tree - shared by delete and its own
// confirmation
// ---------------------------------------------------------------------

enum TargetKind {
    Folder,
    File,
}

/// Whether `relative_path`'s own freshly loaded tree names is a real folder
/// or a real file (markdown or not - delete never requires a file to be
/// markdown, matching `entity_docs::delete_file`'s identical contract) or a
/// real folder, plus that thing's own display label - owned, so the caller
/// never has to hold a borrow of `root` across an `.await`. `None` for
/// anything `relative_path` names neither.
fn resolve_target(root: &EntityFolder, relative_path: &str) -> Option<(TargetKind, String)> {
    if let Some(folder) = entity_docs::find_folder(root, relative_path) {
        return Some((TargetKind::Folder, folder.label.clone()));
    }
    let (parent_dir, _) = relative_path.rsplit_once('/').unwrap_or(("", relative_path));
    let parent = entity_docs::find_folder(root, parent_dir)?;
    let found = file_at(parent, relative_path)?;
    Some((TargetKind::File, found.label.clone()))
}

/// Whether `relative_path` names a real markdown file in `root` - a real non-markdown file has no
/// read view of its own here (this module's own top-of-file doc), so it is deliberately excluded:
/// a last-visited entry could only ever point at one in the first place if the cookie were
/// tampered with, since [`last_visited::classify`] only ever records a path ending in `.md`.
fn is_real_markdown_file(root: &EntityFolder, relative_path: &str) -> bool {
    let (parent_dir, _) = relative_path.rsplit_once('/').unwrap_or(("", relative_path));
    entity_docs::find_folder(root, parent_dir)
        .and_then(|parent| file_at(parent, relative_path))
        .is_some_and(|file| file.is_markdown)
}

// ---------------------------------------------------------------------
// View models
// ---------------------------------------------------------------------

struct BrowsingFolderView {
    label: String,
    /// The folder's path within its screen's folder.
    relative_path: String,
}

struct SelectedFileView {
    label: String,
    rendered_html: String,
    path: String,
    /// Whether the file is a note, which can be edited and given properties; a base, canvas, or diagram is shown as
    /// it is.
    is_note: bool,
    /// The selected file's own real frontmatter fields, for the metadata panel - degrades to
    /// an empty list on any read failure (`entity_docs::file_frontmatter`'s own contract)
    /// rather than blocking this file's own read view.
    metadata_fields: Vec<MetadataFieldView>,
    /// The metadata panel's own conflict-check hash: the same hash [`EditingView`] carries
    /// when editing, computed separately here when it is not.
    expected_hash: String,
}

/// Present only right after a brand-new markdown file is created: swaps
/// the main body straight into its own edit form instead of the read view
/// an empty file would otherwise show, matching
/// `routes::wiki::EditingView`'s identical reasoning and shape.
struct EditingView {
    edit_body: String,
    edit_expected_hash: String,
}

fn dir_href(vault_name: &str, entity_slug: &str, relative_dir: &str) -> String {
    crate::urls::VaultUrls::from(vault_name).entity_file(entity_slug, relative_dir)
}

fn file_href(vault_name: &str, entity_slug: &str, relative_path: &str) -> String {
    crate::urls::VaultUrls::from(vault_name).entity_file(entity_slug, relative_path)
}

/// The direct, general vault content route for a non-markdown file: this
/// screen never re-implements `.base`/`.canvas`/binary rendering a second
/// time, it just links straight at the route that already handles them.
fn general_route_href(vault_name: &str, entity_slug: &str, relative_path: &str) -> String {
    crate::urls::VaultUrls::from(vault_name).file(&format!("{entity_slug}/{relative_path}"))
}

#[derive(Template)]
#[template(path = "entity_docs.html")]
struct EntityDocsTemplate {
    /// Every URL the template links to within the vault.
    urls: crate::urls::VaultUrls,
    entity_slug: String,
    folder_side_is_left: bool,
    /// Which side the metadata panel stacks above the folder-list pane, independent of
    /// [`Self::folder_side_is_left`]. Only meaningful when [`Self::selected`] is `Some`.
    metadata_panel_side_is_left: bool,
    /// The one CSS grid modifier class this render's layout needs ([`entity_layout_class`]).
    layout_class: &'static str,
    has_entities: bool,
    entity_name: String,
    tree_events: Vec<TreeEvent>,
    current_dir_href: String,
    new_folder_href: String,
    new_page_href: String,
    browsing_folder: Option<BrowsingFolderView>,
    selected: Option<SelectedFileView>,
    editing: Option<EditingView>,
    /// The dedicated "New file" form's own pre-filled state, when this
    /// response is `GET .../new`'s own ([`write::render_new_form_fragment`]),
    /// mutually exclusive with `selected` and `browsing_folder`, never more
    /// than one at once, matching `routes::wiki::WikiTemplate::creating`'s
    /// identical contract. Defined in `write` (`entity_docs_write.rs`), not
    /// here, purely to keep this file under this workspace's own
    /// 1000-line/50kB cap - see that module's own top-of-file doc.
    creating: Option<write::CreatingView>,
    error: Option<String>,
}

fn empty_screen(
    folder_side_is_left: bool,
    metadata_panel_side_is_left: bool,
    error: Option<String>,
) -> EntityDocsTemplate {
    EntityDocsTemplate {
        urls: crate::urls::VaultUrls::from(""),
        entity_slug: String::new(),
        folder_side_is_left,
        metadata_panel_side_is_left,
        layout_class: entity_layout_class(folder_side_is_left, metadata_panel_side_is_left, false),
        has_entities: false,
        entity_name: String::new(),
        tree_events: Vec::new(),
        current_dir_href: String::new(),
        new_folder_href: String::new(),
        new_page_href: String::new(),
        browsing_folder: None,
        selected: None,
        editing: None,
        creating: None,
        error,
    }
}

fn load_error_message(error: &ReadEntityFileError) -> String {
    match error {
        ReadEntityFileError::NotFound => "This file could not be found.".to_owned(),
        ReadEntityFileError::Write(_) => "The vault could not be reached.".to_owned(),
    }
}

/// The sidebar's own accordion tree and side, plus the selected file's own
/// containing folder and the entity itself, threaded as one unit through
/// [`render_file_selected`]: otherwise that function needs every one of
/// these four plus its other, unrelated parameters, past this workspace's
/// own argument-count lint, matching `routes::wiki::PageRenderContext`'s
/// identical reasoning.
struct FileRenderContext<'a> {
    config: &'a EntityDocsConfig,
    tree_events: Vec<TreeEvent>,
    entity: &'a EntityEntry,
    parent: &'a EntityFolder,
}

/// The one CSS grid modifier class an entity-folder render's layout needs, given both
/// independently configurable panel placements, matching `routes::wiki::wiki_layout_class`'s
/// identical four-combination reasoning. `metadata_present` is `false` whenever no file is
/// selected, since the metadata panel never renders without one - in that case only
/// `folder_side_is_left` matters.
fn entity_layout_class(
    folder_side_is_left: bool,
    metadata_panel_side_is_left: bool,
    metadata_present: bool,
) -> &'static str {
    if !metadata_present || folder_side_is_left == metadata_panel_side_is_left {
        return if folder_side_is_left {
            ""
        } else {
            "notes-layout-panel-right"
        };
    }
    if folder_side_is_left {
        "notes-layout-metadata-right"
    } else {
        "notes-layout-metadata-left-panel-right"
    }
}

fn render_folder(
    vault_name: &str,
    config: &EntityDocsConfig,
    entity: &EntityEntry,
    root: &EntityFolder,
    folder: &EntityFolder,
    error: Option<String>,
) -> String {
    let folder_side_is_left = config.folder_side == PanelSide::Left;
    let metadata_panel_side_is_left = config.metadata_panel_side == PanelSide::Left;
    let tree_events = entity_view::flatten(vault_name, &entity.slug, root, &folder.relative_path, None);
    EntityDocsTemplate {
        urls: crate::urls::VaultUrls::from(vault_name),
        entity_slug: entity.slug.clone(),
        folder_side_is_left,
        metadata_panel_side_is_left,
        layout_class: entity_layout_class(folder_side_is_left, metadata_panel_side_is_left, false),
        has_entities: true,
        entity_name: entity.name.clone(),
        tree_events,
        current_dir_href: dir_href(vault_name, &entity.slug, &folder.relative_path),
        new_folder_href: new_folder_href(vault_name, &entity.slug, &folder.relative_path),
        new_page_href: new_page_href(vault_name, &entity.slug, &folder.relative_path),
        browsing_folder: (!folder.relative_path.is_empty()).then(|| BrowsingFolderView {
            label: folder.label.clone(),
            relative_path: folder.relative_path.clone(),
        }),
        selected: None,
        editing: None,
        creating: None,
        error,
    }
    .render()
    .unwrap_or_else(|_| "<p>The entity folder screen could not be rendered.</p>".to_owned())
}

/// Renders the selected-file fragment: `context.parent` (the file's own
/// containing folder, already resolved) supplies the accordion tree's own
/// anchor; `relative_path` is the selected file itself.
async fn render_file_selected(
    client: &McpClient,
    vault_name: &str,
    context: FileRenderContext<'_>,
    relative_path: &str,
    error: Option<String>,
) -> String {
    let FileRenderContext {
        config,
        tree_events,
        entity,
        parent,
    } = context;
    let folder_side_is_left = config.folder_side == PanelSide::Left;
    let metadata_panel_side_is_left = config.metadata_panel_side == PanelSide::Left;
    let (selected, load_error) = if let Ok(kind) = PageFile::try_from(relative_path) {
        // A base, canvas, or diagram is shown through the shared file view, inside this screen.
        let full_path = format!("{}/{relative_path}", entity.slug);
        match page_file_html(client, vault_name, kind, &full_path, None).await {
            Ok(rendered_html) => (
                Some(SelectedFileView {
                    label: relative_path.rsplit('/').next().unwrap_or(relative_path).to_owned(),
                    is_note: false,
                    rendered_html,
                    path: relative_path.to_owned(),
                    metadata_fields: Vec::new(),
                    expected_hash: String::new(),
                }),
                None,
            ),
            Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => {
                (None, Some("The vault could not be reached.".to_owned()))
            }
        }
    } else {
        match entity_docs::read_file(client, vault_name, &entity.slug, relative_path).await {
            Ok(raw) => {
                let full_path = format!("{}/{relative_path}", entity.slug);
                let (rendered, frontmatter, hash) = tokio::join!(
                    note_render::render_note(
                        client,
                        vault_name,
                        NoteView {
                            path: &full_path,
                            raw: Some(&raw),
                            template: false,
                        }
                    ),
                    entity_docs::file_frontmatter(client, vault_name, &entity.slug, relative_path),
                    vault_content::file_hash(client, vault_name, &full_path),
                );
                let label = relative_path
                    .rsplit('/')
                    .next()
                    .unwrap_or(relative_path)
                    .strip_suffix(".md")
                    .unwrap_or(relative_path)
                    .to_owned();
                (
                    Some(SelectedFileView {
                        label,
                        is_note: true,
                        rendered_html: rendered.html,
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

    EntityDocsTemplate {
        urls: crate::urls::VaultUrls::from(vault_name),
        entity_slug: entity.slug.clone(),
        folder_side_is_left,
        metadata_panel_side_is_left,
        layout_class: entity_layout_class(folder_side_is_left, metadata_panel_side_is_left, selected.is_some()),
        has_entities: true,
        entity_name: entity.name.clone(),
        tree_events,
        current_dir_href: dir_href(vault_name, &entity.slug, &parent.relative_path),
        new_folder_href: new_folder_href(vault_name, &entity.slug, &parent.relative_path),
        new_page_href: new_page_href(vault_name, &entity.slug, &parent.relative_path),
        browsing_folder: None,
        selected,
        editing: None,
        creating: None,
        error: error.or(load_error),
    }
    .render()
    .unwrap_or_else(|_| "<p>The entity folder screen could not be rendered.</p>".to_owned())
}

/// Resolves `relative_path` against `entity`'s own real tree and renders
/// whichever fragment applies: a folder browse, a selected markdown file, or
/// `None` when `relative_path` names neither - the caller turns that into a
/// real `404`. A real non-markdown file has no read view of its own here
/// (`routes::entity_docs`'s own module doc); this helper - used only for
/// mutation-refresh fragments, never the main `GET` view, which redirects a
/// non-markdown file instead - falls back to rendering that file's own
/// parent folder, since there is nothing else on this screen to show it.
async fn render_entity_fragment(
    client: &McpClient,
    vault_name: &str,
    config: &EntityDocsConfig,
    entity: &EntityEntry,
    relative_path: &str,
    error: Option<String>,
) -> Option<String> {
    let root = entity_docs::load_tree(client, vault_name, &entity.slug).await;
    if let Some(folder) = entity_docs::find_folder(&root, relative_path) {
        return Some(render_folder(vault_name, config, entity, &root, folder, error));
    }
    let (parent_dir, _) = relative_path.rsplit_once('/').unwrap_or(("", relative_path));
    let parent = entity_docs::find_folder(&root, parent_dir)?;
    let file = file_at(parent, relative_path)?;
    if !file.is_markdown {
        return Some(render_folder(vault_name, config, entity, &root, parent, error));
    }
    let tree_events = entity_view::flatten(
        vault_name,
        &entity.slug,
        &root,
        &parent.relative_path,
        Some(relative_path),
    );
    let context = FileRenderContext {
        config,
        tree_events,
        entity,
        parent,
    };
    Some(Box::pin(render_file_selected(client, vault_name, context, relative_path, error)).await)
}

/// After a brand-new markdown file is created, renders the same fragment
/// [`render_file_selected`] would for a normal `GET`, but with its own edit
/// form already populated in place of the read view - matching
/// `routes::wiki::render_created_page`'s identical reasoning: landing on a
/// freshly created empty file in the read view first, only to make the
/// operator separately click "Edit", is worse than opening it ready to type
/// in directly. `None` when `relative_path`'s own parent folder cannot be
/// found - it always can, immediately after `entity_docs::create_file`
/// succeeded against that same path, but the caller still turns that into a
/// real `404` rather than an `unwrap`.
async fn render_created_file(
    client: &McpClient,
    vault_name: &str,
    config: &EntityDocsConfig,
    entity: &EntityEntry,
    relative_path: &str,
) -> Option<String> {
    let folder_side_is_left = config.folder_side == PanelSide::Left;
    let metadata_panel_side_is_left = config.metadata_panel_side == PanelSide::Left;
    let root = entity_docs::load_tree(client, vault_name, &entity.slug).await;
    let (parent_dir, _) = relative_path.rsplit_once('/').unwrap_or(("", relative_path));
    let parent = entity_docs::find_folder(&root, parent_dir)?;
    let tree_events = entity_view::flatten(
        vault_name,
        &entity.slug,
        &root,
        &parent.relative_path,
        Some(relative_path),
    );
    let label = relative_path
        .rsplit('/')
        .next()
        .unwrap_or(relative_path)
        .strip_suffix(".md")
        .unwrap_or(relative_path)
        .to_owned();
    let (body_result, frontmatter) = tokio::join!(
        entity_docs::load_file_body(client, vault_name, &entity.slug, relative_path),
        entity_docs::file_frontmatter(client, vault_name, &entity.slug, relative_path),
    );
    let (edit_body, edit_expected_hash) = match body_result {
        Ok((body, hash)) => (body, hash.unwrap_or_default()),
        Err(_) => (String::new(), String::new()),
    };

    Some(
        EntityDocsTemplate {
            urls: crate::urls::VaultUrls::from(vault_name),
            entity_slug: entity.slug.clone(),
            folder_side_is_left,
            metadata_panel_side_is_left,
            layout_class: entity_layout_class(folder_side_is_left, metadata_panel_side_is_left, true),
            has_entities: true,
            entity_name: entity.name.clone(),
            tree_events,
            current_dir_href: dir_href(vault_name, &entity.slug, &parent.relative_path),
            new_folder_href: new_folder_href(vault_name, &entity.slug, &parent.relative_path),
            new_page_href: new_page_href(vault_name, &entity.slug, &parent.relative_path),
            browsing_folder: None,
            selected: Some(SelectedFileView {
                label,
                is_note: true,
                rendered_html: String::new(),
                path: (relative_path).to_owned(),
                metadata_fields: metadata_panel::linked_field_views(
                    client,
                    vault_name,
                    &format!("{}/{relative_path}", entity.slug),
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
        .unwrap_or_else(|_| "<p>The entity folder screen could not be rendered.</p>".to_owned()),
    )
}

fn redirect_response(target: &str, headers: &HeaderMap) -> Response {
    if headers.contains_key("hx-request") {
        (StatusCode::OK, [("HX-Redirect", target.to_owned())], String::new()).into_response()
    } else {
        Redirect::to(target).into_response()
    }
}

// ---------------------------------------------------------------------
// GET /{{vault_name}}/entity/{{*path}}
// ---------------------------------------------------------------------

/// `GET /{{vault_name}}/entity/{{*path}}/edit`: the whole screen,
/// its own sidebar tree included, with `editing` populated in place of the
/// read view - never just a bare `#entity-docs-main-body` fragment alone.
/// The header's own Edit/Delete
/// cluster lives outside that div, as a sibling within the same
/// `.notes-main-header`; a response that swapped only the body left that
/// cluster stale, rendering it alongside the edit form's own Save/Cancel
/// pair rather than in place of it. Matches `routes::wiki::render_edit_
/// response`'s identical contract, and `render_created_file`'s identical
/// "editing populated, in the full template" shape for the one other case
/// that already needed it.
async fn render_edit_response(
    client: &McpClient,
    vault_name: &str,
    config: &EntityDocsConfig,
    entity: &EntityEntry,
    file_path: &str,
) -> Response {
    let folder_side_is_left = config.folder_side == PanelSide::Left;
    let metadata_panel_side_is_left = config.metadata_panel_side == PanelSide::Left;
    let root = entity_docs::load_tree(client, vault_name, &entity.slug).await;
    let (parent_dir, _) = file_path.rsplit_once('/').unwrap_or(("", file_path));
    let Some(parent) = entity_docs::find_folder(&root, parent_dir) else {
        return not_found();
    };
    let tree_events = entity_view::flatten(vault_name, &entity.slug, &root, &parent.relative_path, Some(file_path));
    let label = file_path
        .rsplit('/')
        .next()
        .unwrap_or(file_path)
        .strip_suffix(".md")
        .unwrap_or(file_path)
        .to_owned();
    let (body_result, frontmatter) = tokio::join!(
        entity_docs::load_file_body(client, vault_name, &entity.slug, file_path),
        entity_docs::file_frontmatter(client, vault_name, &entity.slug, file_path),
    );
    match body_result {
        Ok((edit_body, edit_expected_hash)) => html_response(
            EntityDocsTemplate {
                urls: crate::urls::VaultUrls::from(vault_name),
                entity_slug: entity.slug.clone(),
                folder_side_is_left,
                metadata_panel_side_is_left,
                layout_class: entity_layout_class(folder_side_is_left, metadata_panel_side_is_left, true),
                has_entities: true,
                entity_name: entity.name.clone(),
                tree_events,
                current_dir_href: dir_href(vault_name, &entity.slug, &parent.relative_path),
                new_folder_href: new_folder_href(vault_name, &entity.slug, &parent.relative_path),
                new_page_href: new_page_href(vault_name, &entity.slug, &parent.relative_path),
                browsing_folder: None,
                selected: Some(SelectedFileView {
                    label,
                    is_note: true,
                    rendered_html: String::new(),
                    path: (file_path).to_owned(),
                    metadata_fields: metadata_panel::linked_field_views(
                        client,
                        vault_name,
                        &format!("{}/{file_path}", entity.slug),
                        &frontmatter,
                    )
                    .await,
                    expected_hash: edit_expected_hash.clone().unwrap_or_default(),
                }),
                editing: Some(EditingView {
                    edit_body,
                    edit_expected_hash: edit_expected_hash.unwrap_or_default(),
                }),
                creating: None,
                error: None,
            }
            .render()
            .unwrap_or_else(|_| "<p>The entity folder screen could not be rendered.</p>".to_owned()),
        ),
        Err(ReadEntityFileError::NotFound) => not_found(),
        Err(ReadEntityFileError::Write(McpCallError::Unreachable { .. } | McpCallError::Refused { .. })) => {
            unreachable()
        }
    }
}

async fn get_dispatch(
    state: EntityDocsRoutesState,
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
    let config = entity_docs_config::load_entity_docs_config(&client, &vault_name).await;

    if raw_path.trim().trim_matches('/').is_empty() {
        let configured_entities = entities::load_entities(&client, &vault_name).await;
        if let Some(first) = configured_entities.first() {
            return redirect_response(&dir_href(&vault_name, &first.slug, ""), &headers);
        }
        let folder_side_is_left = config.folder_side == PanelSide::Left;
        let metadata_panel_side_is_left = config.metadata_panel_side == PanelSide::Left;
        let fragment = empty_screen(folder_side_is_left, metadata_panel_side_is_left, None)
            .render()
            .unwrap_or_else(|_| "<p>The entity folder screen could not be rendered.</p>".to_owned());
        return finish_response(
            &state,
            &client,
            &vault_name,
            (fragment, None),
            "entity".to_owned(),
            None,
            headers,
        )
        .await;
    }

    Box::pin(get_dispatch_for_entity(
        state.clone(),
        &client,
        vault_name,
        &config,
        (raw_path, file),
        headers,
    ))
    .await
}

/// The non-empty-`raw_path` half of [`get_dispatch`]: resolves the entity
/// slug and either reserved sub-path (`/edit`, `/confirm-delete`), or
/// otherwise the real folder/file `relative_path` names - split out purely
/// to keep `get_dispatch` itself under this workspace's own line-count
/// lint, not a meaningful behavioural seam of its own.
async fn get_dispatch_for_entity(
    state: EntityDocsRoutesState,
    client: &Arc<McpClient>,
    vault_name: String,
    config: &EntityDocsConfig,
    (raw_path, file): (String, FileRequest),
    headers: HeaderMap,
) -> Response {
    let Some((entity_slug, relative_path)) = split_entity_path(&raw_path) else {
        return not_found();
    };
    let configured_entities = entities::load_entities(client, &vault_name).await;
    let Some(entity) = entity_by_slug(&configured_entities, &entity_slug) else {
        return not_found();
    };
    if let Some(response) = before_entity_screen(client, &vault_name, (entity, &relative_path), &file, &headers).await {
        return response;
    }

    if let Some(file_path) = action_path(&relative_path, "/edit").and_then(as_markdown_path) {
        return Box::pin(render_edit_response(client, &vault_name, config, entity, file_path)).await;
    }
    if let Some(target) = action_path(&relative_path, "/confirm-delete") {
        return Box::pin(write::render_confirm_delete_response(
            client,
            &vault_name,
            config,
            entity,
            target,
        ))
        .await;
    }

    // `relative_path == "new"` is the entity's own bare root's own reserved
    // form (no leading folder segment to strip a `/new` suffix from at
    // all); `action_path(&relative_path, "/new")` covers every real folder
    // beneath it, matching `routes::wiki::get_dispatch`'s identical
    // root/non-root shape. Unlike `/edit` and `/confirm-delete` (pure HTMX
    // actions, answered as a bare fragment regardless of how they were
    // requested), `/new` is meant to be a real, bookmarkable navigation
    // target, so it flows into the same full-page-wrap tail every ordinary
    // folder/file `GET` already goes through below.
    let new_page_parent_dir = if relative_path == "new" {
        Some(String::new())
    } else {
        action_path(&relative_path, "/new").map(str::to_owned)
    };

    let fragment = if let Some(parent_dir) = &new_page_parent_dir {
        let default_title = entity_docs::default_title(state.vault_time.instant().await);
        let Some(fragment) = Box::pin(write::render_new_form_fragment(
            client,
            &vault_name,
            config,
            entity,
            parent_dir,
            default_title,
            None,
        ))
        .await
        else {
            return not_found();
        };
        fragment
    } else {
        let root = entity_docs::load_tree(client, &vault_name, &entity.slug).await;

        if let Some(folder) = entity_docs::find_folder(&root, &relative_path) {
            render_folder(&vault_name, config, entity, &root, folder, None)
        } else {
            let (parent_dir, _) = relative_path.rsplit_once('/').unwrap_or(("", relative_path.as_str()));
            let Some(parent) = entity_docs::find_folder(&root, parent_dir) else {
                return not_found();
            };
            let Some(file) = file_at(parent, &relative_path) else {
                return not_found();
            };
            // A file that is not a page has no view here: its bytes are served from its own vault address.
            if !file.is_markdown && PageFile::try_from(file.relative_path.as_str()).is_err() {
                let target = general_route_href(&vault_name, &entity.slug, &file.relative_path);
                return redirect_response(&target, &headers);
            }
            let tree_events = entity_view::flatten(
                &vault_name,
                &entity.slug,
                &root,
                &parent.relative_path,
                Some(&relative_path),
            );
            let context = FileRenderContext {
                config,
                tree_events,
                entity,
                parent,
            };
            Box::pin(render_file_selected(client, &vault_name, context, &relative_path, None)).await
        }
    };

    let breadcrumb_suffix = if relative_path.is_empty() {
        format!("entity/{entity_slug}")
    } else {
        format!("entity/{entity_slug}/{relative_path}")
    };
    let shown = contextos_markdown::FileKind::from(relative_path.as_str())
        .is_page()
        .then(|| ShownFile {
            entity_name: entity.name.clone(),
            path: format!("{}/{relative_path}", entity.slug),
            href: file_href(&vault_name, &entity.slug, &relative_path),
        });
    finish_response(
        &state,
        client,
        &vault_name,
        (fragment, shown),
        breadcrumb_suffix,
        Some(&entity_slug),
        headers,
    )
    .await
}

pub async fn get_root(
    State(state): State<EntityDocsRoutesState>,
    Path(vault_name): Path<String>,
    headers: HeaderMap,
) -> Response {
    get_dispatch(state, vault_name, String::new(), FileRequest::default(), headers).await
}

pub async fn get_path(
    State(state): State<EntityDocsRoutesState>,
    Path((vault_name, path)): Path<(String, String)>,
    file: FileRequest,
    headers: HeaderMap,
) -> Response {
    get_dispatch(state, vault_name, path, file, headers).await
}

// ---------------------------------------------------------------------
// POST/PATCH/DELETE /{{vault_name}}/entity/{{*path}}: the create,
// save, and delete mutations, split into their own file purely to keep
// this one under the workspace's own 1000-line/50kB cap - see
// `entity_docs_write.rs`'s own module doc.
// ---------------------------------------------------------------------

#[path = "entity_docs_write.rs"]
mod write;

#[path = "entity_docs_page.rs"]
mod page_shell;
use page_shell::{ShownFile, before_entity_screen, finish_response};

#[path = "entity_docs_metadata.rs"]
mod metadata;

pub use metadata::patch_body;
pub use write::{create_path, delete_path};

#[cfg(test)]
#[path = "entity_docs_test.rs"]
mod tests;
