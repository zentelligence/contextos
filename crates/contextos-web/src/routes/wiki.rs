//! `GET`/`POST`/`PATCH`/`DELETE /{{vault_name}}/wiki/{{*path}}`:
//! the Wiki screen. `path` is a direct mapping onto `wiki_dir`'s own real
//! on-disk structure (`{vault_name}://{wiki_dir}/{path}`, per
//! `docs/http-routing-response-contract.md`'s own "a resource's identifier
//! is its path" model) - never a query parameter standing in for it. `GET`
//! on a real folder renders that folder's own browse view (the full
//! accordion tree - see `routes::wiki_view` - plus that folder's own
//! contents); `GET` on a real page renders that page read. `POST` on a
//! folder creates a page inside it (always through `note_create`); `POST`
//! on a folder's own `/execute/new-folder` sub-path creates a new, empty
//! subfolder (`fs_create_directory`), matching
//! `docs/http-routing-response-contract.md`'s own "execute a command on a
//! collection" pattern. `PATCH` on a page saves either its edited body or its
//! frontmatter (the metadata panel's own submitted form), or (its own
//! `/rename` sub-path) updates its title - the dispatch itself lives in
//! `routes::wiki_save`, matching `routes::wiki_create`'s own identical "split
//! the action out, this module only ever calls back into it" precedent.
//! `DELETE` on a page or an empty
//! folder moves it to trash. A `path` that resolves to neither a real
//! folder nor a real page is a genuine `404`, matching the generic vault
//! content route's own identical contract - never a fragment rendered with
//! an inline "not found" message.
//!
//! `docs/http-routing-response-contract.md`'s own naming conventions reserve four sub-paths as never
//! valid entity identifiers: `/new`, `/edit`, `/confirm-delete`, and `/rename`. All four are implemented.
//! `/new`, scoped to whichever real folder it trails (`GET .../{folder}/new`, or the bare root's own
//! `GET .../new`), loads that folder's own dedicated "New page" form, pre-filled from [`wiki::default_title`],
//! matching `routes::notes`'s own corrected `/new` contract exactly (`routes::notes`'s own top-of-file
//! doc: a titleless immediate-`POST` button was a genuine gap against the contract's own "load create
//! form" pattern, not a deliberate design, and was fixed there first). The header's own "New" button
//! loads this form (`hx-get`, [`new_page_href`]) rather than posting directly, so
//! `routes::wiki_create::create_dispatch`'s own `POST` handler now only ever answers a submission that
//! already came from this form: every failure it can produce re-renders the `/new` form itself, the
//! operator's own submitted title preserved, never the plain folder-browse fragment. The one real
//! difference a folder-scoped tree structure adds over Notes' own flat one: a folder shallower than
//! [`MIN_CREATE_DEPTH`] (the root included, which structurally can never satisfy it) renders that
//! folder's own browse fragment with [`CreateWikiPageError::TooShallow`]'s own explanatory message
//! instead of the form, since such a form could never succeed there ([`render_new_form_fragment`]'s own
//! doc): the header button itself never offers "New" from a folder this shallow in the first place
//! (`WikiTemplate::can_create`). Like every other reserved sub-path here, a real folder actually named
//! `new` (or `edit`, `confirm-delete`, `rename`) is shadowed by its own reserved meaning rather than
//! ever being reachable as a folder in its own right, an accepted, pre-existing limitation of
//! path-is-identity addressing, not one `/new` introduces. `/edit` loads a page's own edit form;
//! `/confirm-delete` names the real page or folder a delete would remove before confirming it;
//! `/rename` (its own `PATCH .../rename` sub-path) updates a page's own title.
//!
//! Selecting a page always shows its own parent folder's siblings alongside
//! it - there is no independent "folder open, but a page from elsewhere
//! selected" state.

use std::path::PathBuf;
use std::sync::Arc;

use askama::Template;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};

use crate::config;
use crate::file_view::{PageFile, page_file_html};
use crate::inbox_outbox::sanitised_relative_path;
use crate::last_visited;
use crate::mcp_client::{McpCallError, McpClient, McpClientSet};
use crate::metadata_panel::{self, MetadataFieldView};
use crate::note_render::{self, NoteView};
use crate::panel_side::PanelSide;
use crate::rendering::page;
use crate::rendering::shell::{self, ActiveScreen};
use crate::routes::file_response::{self, FileRequest};
use crate::routes::vault::{html_response, not_found, unreachable};
use crate::routes::wiki_view::{self, TreeEvent};
use crate::urls::VaultUrls;
use crate::vault_content;
use crate::wiki::{self, CreateWikiPageError, MIN_CREATE_DEPTH, ReadWikiPageError, WikiFolder};
use crate::wiki_config::{self, WikiConfig};

#[derive(Clone)]
pub struct WikiRoutesState {
    pub clients: Arc<McpClientSet>,
    pub web_config_path: Arc<PathBuf>,
    /// The operator's local time, for every date this screen writes into a vault.
    pub vault_time: Arc<crate::vault_time::VaultTime>,
}

impl From<super::CommonRoutesState> for WikiRoutesState {
    fn from(value: super::CommonRoutesState) -> Self {
        Self {
            clients: value.clients,
            web_config_path: value.web_config_path,
            vault_time: value.vault_time,
        }
    }
}

impl WikiRoutesState {
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
/// `/`, matching `routes::vault`'s own path handling - `path` here is the
/// resource's own identifier, so this is validation, not query-parameter
/// parsing.
pub(super) fn sanitised_path(raw: &str) -> Option<String> {
    let segments = sanitised_relative_path(raw).ok()?;
    Some(segments.join("/"))
}

/// `path` with its trailing `/{{suffix}}` reserved sub-path removed -
/// `docs/http-routing-response-contract.md`'s reserved `/edit`/`/confirm-delete`
/// sub-paths, extended to Wiki's own path-is-identity addressing (a folder
/// or a page alike, unlike `as_page_path`, which a caller applies
/// separately when the remainder must specifically be a page). `None` when
/// `path` does not end with `suffix` at all.
pub(super) fn action_path<'a>(path: &'a str, suffix: &str) -> Option<&'a str> {
    path.strip_suffix(suffix)
}

/// A path ending in `.md` - `None` for anything else, including a directory
/// path.
pub(super) fn as_page_path(path: &str) -> Option<&str> {
    path.rsplit('/')
        .next()
        .filter(|filename| filename.to_ascii_lowercase().ends_with(".md"))
        .map(|_| path)
}

/// `path` with its trailing `/execute/new-folder` command sub-path removed
/// (`docs/http-routing-response-contract.md`'s own `/{entities}/execute/{command}`
/// pattern - "New folder" is a command executed on the currently browsed
/// folder as a collection, not the identity of a real page or folder of its
/// own). Handles the Wiki root specially: its own `current_dir_href` has no
/// leading path segment at all, so the root's own command path is
/// `execute/new-folder` with no leading `/`.
const NEW_FOLDER_SUFFIX: &str = "/execute/new-folder";
const NEW_FOLDER_AT_ROOT: &str = "execute/new-folder";

pub(super) fn new_folder_action_path(path: &str) -> Option<&str> {
    if let Some(stripped) = path.strip_suffix(NEW_FOLDER_SUFFIX) {
        return Some(stripped);
    }
    (path == NEW_FOLDER_AT_ROOT).then_some("")
}

/// The header row's own "New folder" form target for `relative_dir`,
/// matching [`dir_href`]'s identical root/non-root shape.
fn new_folder_href(vault_name: &str, relative_dir: &str) -> String {
    VaultUrls::from(vault_name).wiki_action(relative_dir, NEW_FOLDER_AT_ROOT)
}

/// The header row's own "New" button target for `relative_dir`: `GET .../new`'s own reserved
/// sub-path, never an immediate `POST` - matching `routes::notes`'s own corrected "the button
/// loads the create form first" contract (`GET .../new`'s own top-of-file doc), and [`dir_href`]'s
/// identical root/non-root shape.
fn new_page_href(vault_name: &str, relative_dir: &str) -> String {
    VaultUrls::from(vault_name).wiki_action(relative_dir, "new")
}

// ---------------------------------------------------------------------
// Resolving a path against the real tree - shared by delete and its own
// confirmation
// ---------------------------------------------------------------------

pub(super) enum TargetKind {
    Folder,
    Page,
}

/// Whether `path`'s own freshly loaded tree names is a real folder or a
/// real page, plus that thing's own display label - owned, so the caller
/// never has to hold a borrow of `root` across an `.await`. `None` for
/// anything `path` names neither.
pub(super) fn resolve_target(root: &WikiFolder, path: &str) -> Option<(TargetKind, String)> {
    if let Some(folder) = wiki::find_folder(root, path) {
        return Some((TargetKind::Folder, folder.label.clone()));
    }
    let (parent_dir, _) = path.rsplit_once('/').unwrap_or(("", path));
    let parent = wiki::find_folder(root, parent_dir)?;
    let found = page_at(parent, path)?;
    Some((TargetKind::Page, found.label.clone()))
}

// ---------------------------------------------------------------------
// View models
// ---------------------------------------------------------------------

struct BrowsingFolderView {
    label: String,
    /// The folder's path within its screen's folder.
    relative_path: String,
}

struct SelectedPageView {
    title: String,
    rendered_html: String,
    path: String,
    /// Whether the page is a note, which can be edited, renamed, and given properties; a base, canvas, or diagram is
    /// shown as it is.
    is_note: bool,
    /// The selected page's own real frontmatter fields, for the metadata panel
    /// ([`metadata_panel::metadata_field_views`]) - degrades to an empty list on any read
    /// failure (`wiki::page_title_and_frontmatter`'s own contract) rather than blocking this
    /// page's own read view.
    metadata_fields: Vec<MetadataFieldView>,
    /// The metadata panel's own conflict-check hash: the same file content hash
    /// [`EditingView::edit_expected_hash`] carries when editing, computed separately here
    /// when it is not (the plain read view has no edit form of its own to have loaded it
    /// already).
    expected_hash: String,
}

/// Present only right after a brand-new page is created: swaps the main
/// body straight into its own edit form instead of the read view an empty
/// page would otherwise show, so the operator lands somewhere they can
/// immediately start typing rather than on a blank rendered page they must
/// then separately choose to edit.
struct EditingView {
    edit_body: String,
    edit_expected_hash: String,
}

/// `GET .../new`'s own pre-filled create-page form state, analogous to
/// `routes::notes::CreatingView` - the one real difference being that a Wiki page always needs a
/// real parent folder to land in (never a bare operator-typed file path the way Notes' own form
/// asks for one), so this carries that folder's own display label rather than a file path.
/// `default_title` is [`wiki::default_title`]'s own seed, matching what the header "New" button's
/// own titleless submission already falls back to when no title is supplied.
struct CreatingView {
    parent_label: String,
    default_title: String,
}

pub(super) fn dir_href(vault_name: &str, relative_dir: &str) -> String {
    VaultUrls::from(vault_name).wiki(relative_dir)
}

#[derive(Template)]
#[template(path = "wiki.html")]
struct WikiTemplate {
    /// Every URL the template links to within the vault.
    urls: crate::urls::VaultUrls,
    tree_side_is_left: bool,
    /// Which side the metadata panel stacks above the folder-tree pane already rendering on
    /// that side, independent of [`Self::tree_side_is_left`]
    /// ([`crate::wiki_config::WikiConfig::metadata_panel_side`]'s own doc). Only meaningful
    /// when [`Self::selected`] is `Some`: the panel never renders without a selected page's
    /// own frontmatter to show.
    metadata_panel_side_is_left: bool,
    /// The one CSS grid modifier class this render's own layout needs
    /// ([`wiki_layout_class`]) - `""` for the plain base two-column grid.
    layout_class: &'static str,
    tree_events: Vec<TreeEvent>,
    can_create: bool,
    current_dir_href: String,
    new_folder_href: String,
    new_page_href: String,
    browsing_folder: Option<BrowsingFolderView>,
    selected: Option<SelectedPageView>,
    editing: Option<EditingView>,
    /// The dedicated "New page" form's own pre-filled state, when this response is `GET .../new`'s
    /// own ([`render_new_form_fragment`]) - mutually exclusive with `selected` and `browsing_folder`,
    /// never more than one at once.
    creating: Option<CreatingView>,
    error: Option<String>,
}

/// The one CSS grid modifier class a Wiki render's own layout needs, given both
/// independently configurable panel placements (`tree_side_is_left`,
/// `metadata_panel_side_is_left`). `metadata_present` is `false` whenever no page is selected
/// (browsing a folder, the dedicated "New" form, or the plain empty state), since the
/// metadata panel never renders without a selected page's own frontmatter to show - in that
/// case only `tree_side_is_left` matters, exactly as before this config existed. `""` renders
/// the plain two-column base grid (the tree pane, or the tree pane stacked with the metadata
/// panel, on the left); the other three variants each name a real CSS rule in
/// `contextos-web.css`.
fn wiki_layout_class(
    tree_side_is_left: bool,
    metadata_panel_side_is_left: bool,
    metadata_present: bool,
) -> &'static str {
    if !metadata_present || tree_side_is_left == metadata_panel_side_is_left {
        return if tree_side_is_left {
            ""
        } else {
            "notes-layout-panel-right"
        };
    }
    if tree_side_is_left {
        "notes-layout-metadata-right"
    } else {
        "notes-layout-metadata-left-panel-right"
    }
}

/// `GET /{{vault_name}}/wiki/{{*path}}/confirm-delete`'s own
/// fragment (the contract's reserved `/confirm-delete` sub-path), matching
/// `routes::notes::NotesConfirmDeleteTemplate`'s identical shape - `kind`
/// names whichever real thing `path` resolved to ("page" or "folder"), so
/// the one confirmation prompt covers both without needing two templates.
#[derive(Template)]
#[template(path = "wiki_confirm_delete.html")]
struct WikiConfirmDeleteTemplate {
    /// Every URL the template links to within the vault.
    urls: crate::urls::VaultUrls,
    kind: &'static str,
    label: String,
    path: String,
}

fn load_error_message(error: &ReadWikiPageError) -> String {
    match error {
        ReadWikiPageError::NotFound => "This page could not be found.".to_owned(),
        ReadWikiPageError::Write(_) => "The vault could not be reached.".to_owned(),
    }
}

/// Whether `folder` (a real, already-resolved folder) lists a page at
/// exactly `relative_path`.
fn page_at<'a>(folder: &'a WikiFolder, relative_path: &str) -> Option<&'a wiki::WikiPageLink> {
    folder.pages.iter().find(|page| page.relative_path == relative_path)
}

/// Renders the folder-browse fragment for `folder` (its own accordion tree,
/// and whether "New" is offered here) with no page selected.
fn render_folder(
    vault_name: &str,
    tree_side_is_left: bool,
    metadata_panel_side_is_left: bool,
    tree_events: Vec<TreeEvent>,
    folder: &WikiFolder,
    error: Option<String>,
) -> String {
    WikiTemplate {
        urls: crate::urls::VaultUrls::from(vault_name),
        tree_side_is_left,
        metadata_panel_side_is_left,
        layout_class: wiki_layout_class(tree_side_is_left, metadata_panel_side_is_left, false),
        tree_events,
        can_create: wiki::depth_of(&folder.relative_path) >= MIN_CREATE_DEPTH,
        current_dir_href: dir_href(vault_name, &folder.relative_path),
        new_folder_href: new_folder_href(vault_name, &folder.relative_path),
        new_page_href: new_page_href(vault_name, &folder.relative_path),
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
    .unwrap_or_else(|_| "<p>The Wiki screen could not be rendered.</p>".to_owned())
}

/// The sidebar's own accordion tree and side, plus the selected page's own
/// containing folder, threaded as one unit through [`render_page_selected`]:
/// otherwise that function needs every one of these three plus its other,
/// unrelated parameters, past this workspace's own argument-count lint,
/// matching `routes::insights::ShardContext`'s identical reasoning.
struct PageRenderContext<'a> {
    tree_side_is_left: bool,
    metadata_panel_side_is_left: bool,
    tree_events: Vec<TreeEvent>,
    parent: &'a WikiFolder,
}

/// Renders the selected-page fragment: `context.parent` (the page's own
/// containing folder, already resolved) supplies the accordion tree's own
/// anchor; `relative_path` is the selected page itself.
async fn render_page_selected(
    client: &McpClient,
    vault_name: &str,
    config: &WikiConfig,
    context: PageRenderContext<'_>,
    relative_path: &str,
    error: Option<String>,
) -> String {
    let PageRenderContext {
        tree_side_is_left,
        metadata_panel_side_is_left,
        tree_events,
        parent,
    } = context;
    let (selected, load_error) = if let Ok(kind) = PageFile::try_from(relative_path) {
        // A base, canvas, or diagram is shown through the shared file view, inside this screen.
        let full_path = wiki::page_path(config, relative_path);
        match page_file_html(client, vault_name, kind, &full_path, None).await {
            Ok(rendered_html) => (
                Some(SelectedPageView {
                    title: relative_path.rsplit('/').next().unwrap_or(relative_path).to_owned(),
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
        match wiki::read_page(client, vault_name, config, relative_path).await {
            Ok(raw) => {
                // The edit form's own body/hash is loaded separately, on
                // demand, by `render_edit_response` when the operator actually
                // opens `.../edit` - the read view no longer needs it up front
                // now that editing is its own reserved sub-path rather than an
                // always-rendered inline form.
                let full_path = wiki::page_path(config, relative_path);
                let (rendered, (title, frontmatter), hash) = tokio::join!(
                    note_render::render_note(
                        client,
                        vault_name,
                        NoteView {
                            path: &full_path,
                            raw: Some(&raw),
                            template: false,
                        }
                    ),
                    wiki::page_title_and_frontmatter(client, vault_name, config, relative_path),
                    vault_content::file_hash(client, vault_name, &full_path),
                );
                (
                    Some(SelectedPageView {
                        title,
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

    WikiTemplate {
        urls: crate::urls::VaultUrls::from(vault_name),
        tree_side_is_left,
        metadata_panel_side_is_left,
        layout_class: wiki_layout_class(tree_side_is_left, metadata_panel_side_is_left, selected.is_some()),
        tree_events,
        can_create: wiki::depth_of(&parent.relative_path) >= MIN_CREATE_DEPTH,
        current_dir_href: dir_href(vault_name, &parent.relative_path),
        new_folder_href: new_folder_href(vault_name, &parent.relative_path),
        new_page_href: new_page_href(vault_name, &parent.relative_path),
        browsing_folder: None,
        selected,
        editing: None,
        creating: None,
        error: error.or(load_error),
    }
    .render()
    .unwrap_or_else(|_| "<p>The Wiki screen could not be rendered.</p>".to_owned())
}

/// After a brand-new, empty page is created, renders the same fragment
/// [`render_page_selected`] would for a normal `GET`, but with its own edit
/// form already populated in place of the read view: landing on a freshly
/// created empty page in the read view first, only to make the operator
/// separately click "Edit", is worse than opening it ready to type in
/// directly. `None` when `relative_path`'s own parent folder cannot be
/// found - it always can, immediately after `wiki::create_page` succeeded
/// against that same path, but the caller still turns that into a real
/// `404` rather than an `unwrap`.
pub(super) async fn render_created_page(
    client: &McpClient,
    vault_name: &str,
    config: &WikiConfig,
    relative_path: &str,
) -> Option<String> {
    let root = wiki::load_tree(client, vault_name, config).await;
    let (parent_dir, _) = relative_path.rsplit_once('/').unwrap_or(("", relative_path));
    let parent = wiki::find_folder(&root, parent_dir)?;
    let tree_side_is_left = config.tree_side == PanelSide::Left;
    let metadata_panel_side_is_left = config.metadata_panel_side == PanelSide::Left;
    let tree_events = wiki_view::flatten(vault_name, &root, &parent.relative_path, Some(relative_path));
    let (title, frontmatter) = wiki::page_title_and_frontmatter(client, vault_name, config, relative_path).await;
    let (edit_body, edit_expected_hash) = match wiki::load_page_body(client, vault_name, config, relative_path).await {
        Ok((body, hash)) => (body, hash.unwrap_or_default()),
        Err(_) => (String::new(), String::new()),
    };

    Some(
        WikiTemplate {
            urls: crate::urls::VaultUrls::from(vault_name),
            tree_side_is_left,
            metadata_panel_side_is_left,
            layout_class: wiki_layout_class(tree_side_is_left, metadata_panel_side_is_left, true),
            tree_events,
            can_create: wiki::depth_of(&parent.relative_path) >= MIN_CREATE_DEPTH,
            current_dir_href: dir_href(vault_name, &parent.relative_path),
            new_folder_href: new_folder_href(vault_name, &parent.relative_path),
            new_page_href: new_page_href(vault_name, &parent.relative_path),
            browsing_folder: None,
            selected: Some(SelectedPageView {
                title,
                is_note: true,
                rendered_html: String::new(),
                path: (relative_path).to_owned(),
                metadata_fields: metadata_panel::linked_field_views(
                    client,
                    vault_name,
                    &wiki::page_path(config, relative_path),
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
        .unwrap_or_else(|_| "<p>The Wiki screen could not be rendered.</p>".to_owned()),
    )
}

/// Resolves `path` against the real tree and renders whichever fragment
/// applies: a folder browse, a selected page, or `None` when `path` names
/// neither - the caller turns that into a real `404`.
pub(super) async fn render_wiki_fragment(
    client: &McpClient,
    vault_name: &str,
    config: &WikiConfig,
    path: &str,
    error: Option<String>,
) -> Option<String> {
    let root = wiki::load_tree(client, vault_name, config).await;
    let tree_side_is_left = config.tree_side == PanelSide::Left;
    let metadata_panel_side_is_left = config.metadata_panel_side == PanelSide::Left;

    if let Some(folder) = wiki::find_folder(&root, path) {
        let tree_events = wiki_view::flatten(vault_name, &root, &folder.relative_path, None);
        return Some(render_folder(
            vault_name,
            tree_side_is_left,
            metadata_panel_side_is_left,
            tree_events,
            folder,
            error,
        ));
    }
    let (parent_dir, _) = path.rsplit_once('/').unwrap_or(("", path));
    let parent = wiki::find_folder(&root, parent_dir)?;
    page_at(parent, path)?;
    let tree_events = wiki_view::flatten(vault_name, &root, &parent.relative_path, Some(path));
    let context = PageRenderContext {
        tree_side_is_left,
        metadata_panel_side_is_left,
        tree_events,
        parent,
    };
    Some(Box::pin(render_page_selected(client, vault_name, config, context, path, error)).await)
}

// ---------------------------------------------------------------------
// GET /{{vault_name}}/wiki/{{*path}}
// ---------------------------------------------------------------------

/// `GET /{{vault_name}}/wiki/{{*path}}/edit`: the whole screen,
/// its own sidebar tree included, with `editing` populated in place of the
/// read view - never just a bare `#wiki-main-body` fragment alone. The
/// header's own Rename/Edit/Delete cluster lives outside that div, as a
/// sibling within the same `.notes-main-header`; a
/// response that swapped only the body left that cluster stale, rendering
/// it alongside the edit form's own Save/Cancel pair rather than in place
/// of it. Matches `routes::insights::render_edit_response`'s identical
/// contract, and `render_created_page`'s identical "editing populated, in
/// the full template" shape for the one other case that already needed it.
async fn render_edit_response(
    client: &McpClient,
    vault_name: &str,
    config: &WikiConfig,
    page_path_str: &str,
) -> Response {
    let root = wiki::load_tree(client, vault_name, config).await;
    let tree_side_is_left = config.tree_side == PanelSide::Left;
    let metadata_panel_side_is_left = config.metadata_panel_side == PanelSide::Left;
    let (parent_dir, _) = page_path_str.rsplit_once('/').unwrap_or(("", page_path_str));
    let Some(parent) = wiki::find_folder(&root, parent_dir) else {
        return not_found();
    };
    let tree_events = wiki_view::flatten(vault_name, &root, &parent.relative_path, Some(page_path_str));
    let (title, frontmatter) = wiki::page_title_and_frontmatter(client, vault_name, config, page_path_str).await;
    match wiki::load_page_body(client, vault_name, config, page_path_str).await {
        Ok((edit_body, edit_expected_hash)) => {
            let hash = edit_expected_hash.unwrap_or_default();
            html_response(
                WikiTemplate {
                    urls: crate::urls::VaultUrls::from(vault_name),
                    tree_side_is_left,
                    metadata_panel_side_is_left,
                    layout_class: wiki_layout_class(tree_side_is_left, metadata_panel_side_is_left, true),
                    tree_events,
                    can_create: wiki::depth_of(&parent.relative_path) >= MIN_CREATE_DEPTH,
                    current_dir_href: dir_href(vault_name, &parent.relative_path),
                    new_folder_href: new_folder_href(vault_name, &parent.relative_path),
                    new_page_href: new_page_href(vault_name, &parent.relative_path),
                    browsing_folder: None,
                    selected: Some(SelectedPageView {
                        title,
                        is_note: true,
                        rendered_html: String::new(),
                        path: (page_path_str).to_owned(),
                        metadata_fields: metadata_panel::linked_field_views(
                            client,
                            vault_name,
                            &wiki::page_path(config, page_path_str),
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
                .unwrap_or_else(|_| "<p>The Wiki screen could not be rendered.</p>".to_owned()),
            )
        }
        Err(ReadWikiPageError::NotFound) => not_found(),
        Err(ReadWikiPageError::Write(McpCallError::Unreachable { .. } | McpCallError::Refused { .. })) => unreachable(),
    }
}

/// `GET /{{vault_name}}/wiki/{{*path}}/confirm-delete`: names the
/// real page or folder `path` resolves to before confirming
/// ([`resolve_target`]), matching `routes::notes::confirm_delete`'s
/// identical contract - a `path` naming neither falls back to its own
/// parent folder's own browse fragment with an error message, never a bare
/// error string or a raw `404`.
async fn render_confirm_delete_response(
    client: &McpClient,
    vault_name: &str,
    config: &WikiConfig,
    target: &str,
) -> Response {
    let root = wiki::load_tree(client, vault_name, config).await;
    let Some((kind, label)) = resolve_target(&root, target) else {
        let (parent_dir, _) = target.rsplit_once('/').unwrap_or(("", target));
        let fragment = Box::pin(render_wiki_fragment(
            client,
            vault_name,
            config,
            parent_dir,
            Some("This could not be found.".to_owned()),
        ))
        .await
        .unwrap_or_else(|| "<p>The Wiki screen could not be rendered.</p>".to_owned());
        return html_response(fragment);
    };
    html_response(
        WikiConfirmDeleteTemplate {
            urls: crate::urls::VaultUrls::from(vault_name),
            kind: match kind {
                TargetKind::Folder => "folder",
                TargetKind::Page => "page",
            },
            label,
            path: (target).to_owned(),
        }
        .render()
        .unwrap_or_else(|_| "<p>The delete confirmation could not be rendered.</p>".to_owned()),
    )
}

/// `CreateWikiPageError`'s own operator-facing message: shared by [`render_new_form_fragment`]'s
/// own too-shallow fallback here and `routes::wiki_create::create_dispatch`'s own POST-error path,
/// `pub(super)` for that reason - the message wording is one contract, not two.
pub(super) fn create_error_message(error: &CreateWikiPageError) -> String {
    match error {
        CreateWikiPageError::TooShallow => {
            "A new page needs at least a domain and a topic folder to sit in.".to_owned()
        }
        CreateWikiPageError::EmptyTitle => "A page needs a title.".to_owned(),
        CreateWikiPageError::Write(_) => "The vault could not be reached.".to_owned(),
        CreateWikiPageError::Rejected => {
            "The page could not be created (a page with that name may already exist).".to_owned()
        }
    }
}

/// `GET .../new`'s own dedicated create-page form fragment, and `routes::wiki_create::create_dispatch`'s
/// own POST-error re-render target for every [`CreateWikiPageError`] - sharing this one function keeps
/// both paths' own "can this folder ever take a page" gate identical: `parent_dir` shallower than
/// [`MIN_CREATE_DEPTH`] never renders a form that could not possibly succeed, instead showing that
/// same folder's own browse fragment with [`CreateWikiPageError::TooShallow`]'s own explanatory
/// message ([`create_error_message`]) - the repository owner's own resolution for the bare Wiki
/// root, extended to any shallow folder for the identical structural reason.
///
/// `error`, when present, is shown either way: the too-shallow explanation itself when that gate is
/// what failed, or the create form's own validation or rejection message alongside the operator's
/// own just-submitted (and rejected) title, preserved rather than lost - matching every other
/// form-error path in this crate's own "redisplay what was actually submitted" contract, and
/// `routes::notes::create_root`'s own identical "any failure keeps the operator on their own form"
/// shape.
///
/// `None` only when `parent_dir` itself no longer resolves to a real folder at all (a stale or
/// tampered link) - the caller turns that into a real `404`, matching [`render_wiki_fragment`]'s own
/// "unresolvable path" contract.
pub(super) async fn render_new_form_fragment(
    client: &McpClient,
    vault_name: &str,
    config: &WikiConfig,
    parent_dir: &str,
    default_title: String,
    error: Option<String>,
) -> Option<String> {
    let root = wiki::load_tree(client, vault_name, config).await;
    let folder = wiki::find_folder(&root, parent_dir)?;
    let tree_side_is_left = config.tree_side == PanelSide::Left;
    let metadata_panel_side_is_left = config.metadata_panel_side == PanelSide::Left;
    let tree_events = wiki_view::flatten(vault_name, &root, &folder.relative_path, None);

    if wiki::depth_of(&folder.relative_path) < MIN_CREATE_DEPTH {
        let message = error.unwrap_or_else(|| create_error_message(&CreateWikiPageError::TooShallow));
        return Some(render_folder(
            vault_name,
            tree_side_is_left,
            metadata_panel_side_is_left,
            tree_events,
            folder,
            Some(message),
        ));
    }

    Some(
        WikiTemplate {
            urls: crate::urls::VaultUrls::from(vault_name),
            tree_side_is_left,
            metadata_panel_side_is_left,
            layout_class: wiki_layout_class(tree_side_is_left, metadata_panel_side_is_left, false),
            tree_events,
            can_create: true,
            current_dir_href: dir_href(vault_name, &folder.relative_path),
            new_folder_href: new_folder_href(vault_name, &folder.relative_path),
            new_page_href: new_page_href(vault_name, &folder.relative_path),
            browsing_folder: None,
            selected: None,
            editing: None,
            creating: Some(CreatingView {
                parent_label: folder.label.clone(),
                default_title,
            }),
            error,
        }
        .render()
        .unwrap_or_else(|_| "<p>The Wiki screen could not be rendered.</p>".to_owned()),
    )
}

/// A full-navigation `Redirect` for a plain browser `GET`, or the HTMX-native `HX-Redirect` header
/// on a `200` for an `hx-request`, matching `routes::entity_docs`'s own identical local helper -
/// this crate's own per-module convention rather than one shared function
/// (`routes::vault::redirect_response`'s own doc comment names the same precedent).
fn redirect_response(target: &str, is_hx_request: bool) -> Response {
    if is_hx_request {
        (StatusCode::OK, [("HX-Redirect", target.to_owned())], String::new()).into_response()
    } else {
        Redirect::to(target).into_response()
    }
}

async fn get_dispatch(
    state: WikiRoutesState,
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
    let config = wiki_config::load_wiki_config(&client, &vault_name).await;
    if !path.is_empty()
        && let Some(response) =
            file_response::before_screen(&client, &vault_name, &wiki::page_path(&config, &path), &file).await
    {
        return response;
    }

    // The Wiki sidebar link's own bare root: resume the operator's last-visited real page
    // (`last_visited::wiki_last_visited`'s own cookie) rather than always landing on the
    // domain-grouped browse view, matching `routes::entity_docs`'s identical "resume, or fall
    // back to the ordinary landing view" contract for its own entity-root case. Re-validated
    // against the real tree here, never trusted blindly: a deleted or renamed page since the
    // cookie was written falls straight through to the unchanged landing render below.
    if path.is_empty() {
        let last_visited_state = last_visited::read_from_headers(&headers);
        if let Some(target) = last_visited::wiki_last_visited(&last_visited_state, &vault_name) {
            let root = wiki::load_tree(&client, &vault_name, &config).await;
            if matches!(resolve_target(&root, target), Some((TargetKind::Page, _))) {
                return redirect_response(&dir_href(&vault_name, target), headers.contains_key("hx-request"));
            }
        }
    }

    if let Some(page_path_str) = action_path(&path, "/edit").and_then(as_page_path) {
        return Box::pin(render_edit_response(&client, &vault_name, &config, page_path_str)).await;
    }
    if let Some(target) = action_path(&path, "/confirm-delete") {
        return Box::pin(render_confirm_delete_response(&client, &vault_name, &config, target)).await;
    }

    // `path == "new"` is the bare Wiki root's own reserved form (no leading folder segment to
    // strip a `/new` suffix from at all); `action_path(&path, "/new")` covers every real folder
    // beneath it. Unlike `/edit` and `/confirm-delete` (pure HTMX actions, answered as a bare
    // fragment regardless of how they were requested), `/new` is meant to be a real, bookmarkable
    // navigation target, so it flows into the same full-page-wrap tail every ordinary folder/page
    // `GET` already does below, matching `routes::notes::get_dispatch`'s identical `/new` shape.
    let new_page_parent_dir = if path == "new" {
        Some("")
    } else {
        action_path(&path, "/new")
    };

    let Some(fragment) = (if let Some(parent_dir) = new_page_parent_dir {
        let default_title = wiki::default_title(state.vault_time.instant().await);
        Box::pin(render_new_form_fragment(
            &client,
            &vault_name,
            &config,
            parent_dir,
            default_title,
            None,
        ))
        .await
    } else {
        Box::pin(render_wiki_fragment(&client, &vault_name, &config, &path, None)).await
    }) else {
        return not_found();
    };
    if headers.contains_key("hx-request") {
        return html_response(fragment);
    }
    let web_config_path = Arc::clone(&state.web_config_path);
    let breadcrumb_suffix = format!("wiki/{path}");
    let entity_base_path = VaultUrls::from(vault_name.as_str()).wiki(&path);
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
        let shown = wiki::page_path(&config, &path);
        shell::show_file(
            &mut nav,
            &client,
            &vault_name,
            (&crate::view_favourites::WIKI).into(),
            (&shown, &entity_base_path),
        )
        .await;
    }
    html_response(page::render_page(&nav, "Wiki", &fragment)).into_response()
}

pub async fn get_root(
    State(state): State<WikiRoutesState>,
    Path(vault_name): Path<String>,
    headers: HeaderMap,
) -> Response {
    get_dispatch(state, vault_name, String::new(), FileRequest::default(), headers).await
}

pub async fn get_path(
    State(state): State<WikiRoutesState>,
    Path((vault_name, path)): Path<(String, String)>,
    file: FileRequest,
    headers: HeaderMap,
) -> Response {
    get_dispatch(state, vault_name, path, file, headers).await
}

// ---------------------------------------------------------------------
// PATCH /{{vault_name}}/wiki/{{*path}} - `routes::wiki_save`
// ---------------------------------------------------------------------

// ---------------------------------------------------------------------
// DELETE /{{vault_name}}/wiki/{{*path}} - `routes::wiki_delete`
// ---------------------------------------------------------------------

#[cfg(test)]
#[path = "wiki_test.rs"]
mod tests;
