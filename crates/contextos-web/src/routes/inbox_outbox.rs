//! `GET /{{vault_name}}/inbox/{{*path}}`, `GET /{{vault_name}}/outbox/{{*path}}`, the Inbox
//! upload action, and the Inbox "Ingest" action.
//!
//! Both screens browse a real, arbitrarily nested directory tree one level
//! at a time (`inbox_outbox::list_directory`), never the generic vault
//! content route's own `index.md`-only directory rendering
//! (`routes::vault::render_directory`): a row needs each item's own size
//! and modified time, and a folder needs to be navigable, neither of which
//! a rendered `index.md` carries. `path` names the folder being browsed
//! beneath the screen's own root directory
//! (`inbox_outbox::resolve_inbox_location`/`resolve_outbox_directory`),
//! matching `routes::wiki`'s own "the resource's path is its identifier"
//! model (`docs/http-routing-response-contract.md`) - never a `?path=`
//! query parameter standing in for it, and never repeating the root
//! directory's own name: the Inbox directory is `/{{vault_name}}/inbox/`, its
//! subfolders `/{{vault_name}}/inbox/{{sub}}`, and the processed tree, whose
//! configured directory name is the one reserved first segment, is
//! `/{{vault_name}}/inbox/processed/{{sub}}`. The bare root (no `path` segment
//! at all) is the Inbox screen's own merged view of both `inbox_dir` and
//! `processed_dir` at once, or the Outbox screen's own `outbox_dir` root;
//! every other path browses a subtree of exactly one of those trees, and a
//! path naming a folder that does not exist is "not found".
//!
//! Upload and Ingest are `POST` actions scoped to whichever folder is
//! currently browsed, expressed through the contract's own
//! `/{{entities}}/execute/{{command}}` pattern
//! (`POST /{{vault_name}}/inbox/{{*path}}/execute/upload`,
//! `POST /{{vault_name}}/inbox/{{*path}}/execute/ingest}`, and their own bare-root
//! equivalents, `POST /{{vault_name}}/inbox/execute/upload`/`.../execute/ingest`), dispatched
//! from inside the one `{{*path}}` wildcard POST route rather than a separate literal
//! `app.rs` route entry, matching `routes::wiki`'s own `/execute/new-folder` shape
//! ([`inbox_action`]'s own doc). The URL's own `path` names only the folder an action
//! re-renders after completing, never what Ingest actually acts on: a single item's or a
//! whole folder's own full vault-relative path is a distinct target, always carried in the
//! request body ([`IngestBody`]'s own doc), since a folder-level "ingest everything here" and
//! a file-level "ingest this one item inside the folder I'm browsing" name two different
//! things even when browsing the same folder.

use std::path::PathBuf;
use std::sync::Arc;

use askama::Template;
use axum::extract::{FromRequest, Multipart, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::config;
use crate::inbox_outbox::{self, InboxLocation, format_modified, format_size};
use crate::inbox_outbox_config::{self, InboxOutboxConfig};
use crate::mcp_client::{McpCallError, McpClient, McpClientSet};
use crate::rendering::page;
use crate::rendering::shell::{self, ActiveScreen};
use crate::rendering::title_action::{self, TitleAction};
use crate::routes::file_response::{self, FileRequest};
use crate::routes::vault::{html_response, not_found, unreachable};
use crate::team::{self, TeamMemberKind};

#[derive(Clone)]
pub struct InboxOutboxRoutesState {
    pub clients: Arc<McpClientSet>,
    pub web_config_path: Arc<PathBuf>,
}

impl From<super::CommonRoutesState> for InboxOutboxRoutesState {
    fn from(value: super::CommonRoutesState) -> Self {
        Self {
            clients: value.clients,
            web_config_path: value.web_config_path,
        }
    }
}

impl InboxOutboxRoutesState {
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

/// Whether `curator_agent` names an agent slug `.contextos/team.toml` currently configures. `team::load_team`
/// already degrades an absent file, or an unreachable session, to an empty list, so this never distinguishes "no
/// file" from "file exists but lacks this slug": both correctly fail the check, since the created task would be
/// assigned to nobody real either way.
async fn curator_agent_is_registered(client: &McpClient, vault_name: &str, curator_agent: &str) -> bool {
    team::load_team(client, vault_name)
        .await
        .iter()
        .any(|agent| agent.kind == TeamMemberKind::Agent && agent.slug == curator_agent)
}

fn unregistered_curator_error(curator_agent: &str) -> String {
    format!(
        "The configured curator agent \"{curator_agent}\" is not registered as an agent in .contextos/team.toml. \
         Add it there before ingesting."
    )
}

// ---------------------------------------------------------------------
// Row view models, shared by Inbox and Outbox
// ---------------------------------------------------------------------

struct ItemRow {
    name: String,
    /// This file's own address on the screen with `?raw=1`, which answers with the file as it is stored.
    download_href: String,
    size_label: String,
    /// Best-effort MIME type by extension (`inbox_outbox::mime_type_for_name`), or the generic
    /// fallback the resource listing surface already uses for anything unrecognised, so a binary
    /// file's row never renders as if it were an untyped file the way a bare name/size/modified
    /// listing did before binary uploads were accepted.
    type_label: String,
    modified_label: String,
    /// `Some("pending")`/`Some("processed")` for an Inbox row (its own
    /// status badge, matching the design mock's per-item `STATUS_TONE`
    /// badge exactly rather than two headed sections), `None` for an
    /// Outbox row (no status concept there).
    status: Option<&'static str>,
    /// This file's own full vault-relative path, when it can be ingested
    /// (a pending Inbox row only, rendered as the template's own
    /// `{% if let Some(path) = item.ingest_path %}`): the exact value the
    /// "Ingest" action posts back, so ingest never has to re-derive a
    /// file's location from whatever directory happens to be current.
    ingest_path: Option<String>,
}

struct FolderRow {
    name: String,
    /// Navigates into this folder: this folder's own full vault-relative
    /// path, addressed directly as this screen's own real path segment.
    href: String,
    status: Option<&'static str>,
    /// This folder's own full vault-relative path, when the whole folder
    /// can be ingested in one action (a pending Inbox folder only,
    /// rendered the same `{% if let %}` way as [`ItemRow::ingest_path`]).
    ingest_path: Option<String>,
}

/// `dir` and `url_dir` as for [`folder_row`]. The download is the item's own address on the screen, which answers
/// with the file as it is stored.
fn file_row(
    vault_name: &str,
    breadcrumb: &str,
    (dir, url_dir): (&str, &str),
    status: Option<&'static str>,
    item: inbox_outbox::DirectoryItem,
) -> ItemRow {
    let path = format!("{dir}/{}", item.name);
    let url_path = if url_dir.is_empty() {
        item.name.clone()
    } else {
        format!("{url_dir}/{}", item.name)
    };
    let type_label =
        inbox_outbox::mime_type_for_name(&item.name).unwrap_or_else(|| "application/octet-stream".to_owned());
    ItemRow {
        download_href: crate::urls::VaultUrls::download(
            &crate::urls::VaultUrls::from(vault_name).tray(breadcrumb, &url_path),
        ),
        modified_label: format_modified(item.modified.as_deref()),
        size_label: item.size.map_or_else(|| "-".to_owned(), format_size),
        type_label,
        ingest_path: (status == Some("pending")).then(|| path.clone()),
        name: item.name,
        status,
    }
}

/// `dir` is the folder's parent as a real vault-relative directory (what an ingest names), `url_dir` the same
/// parent as the screen addresses it (empty at the screen's own root): the two differ by the screen's root
/// directory, which the address never repeats.
fn folder_row(
    vault_name: &str,
    breadcrumb: &str,
    dir: &str,
    url_dir: &str,
    status: Option<&'static str>,
    folder: inbox_outbox::DirectoryFolder,
) -> FolderRow {
    let path = format!("{dir}/{}", folder.name);
    let url_path = if url_dir.is_empty() {
        folder.name.clone()
    } else {
        format!("{url_dir}/{}", folder.name)
    };
    let href = crate::urls::VaultUrls::from(vault_name).tray(breadcrumb, &url_path);
    FolderRow {
        ingest_path: (status == Some("pending")).then(|| path.clone()),
        name: folder.name,
        href,
        status,
    }
}

/// Most-recently-modified first, matching the design mock's own upload
/// handler (`setItems([newItem, ...items])`, prepending the newest); an
/// item with no `modified` sorts last, by name, rather than an arbitrary
/// position.
fn compare_by_recency(a: &inbox_outbox::DirectoryItem, b: &inbox_outbox::DirectoryItem) -> std::cmp::Ordering {
    match (&a.modified, &b.modified) {
        (Some(a_modified), Some(b_modified)) => b_modified.cmp(a_modified),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.name.cmp(&b.name),
    }
}

// ---------------------------------------------------------------------
// GET /{{vault_name}}/inbox/{{*path}}
// ---------------------------------------------------------------------

#[derive(Template)]
#[template(path = "inbox.html")]
struct InboxTemplate {
    ancestors: Vec<inbox_outbox::AncestorRow>,
    folders: Vec<FolderRow>,
    items: Vec<ItemRow>,
    /// `POST` target for every "Ingest"/"Ingest folder" form on this
    /// fragment: the browsed folder alone, never the item or folder being
    /// ingested itself, which each form's own `hx-vals` still carries
    /// separately (`item.ingest_path`/`folder.ingest_path`).
    ingest_action_href: String,
    error: Option<String>,
    notice: Option<String>,
}

/// The `/execute/{{command}}` action target for whichever folder `raw_path`
/// (the folder's address beneath the Inbox root) names (absent/empty for the
/// merged root): the bare-root form (`/{{vault_name}}/inbox/{{at_root}}`) when
/// there is no path segment to address, or the browsed folder's own address with `suffix` appended,
/// matching [`action_target`]'s own inverse (strips exactly what this
/// builds).
fn inbox_action_href(vault_name: &str, raw_path: Option<&str>, suffix: &str, at_root: &str) -> String {
    crate::urls::VaultUrls::from(vault_name).tray_command(
        "inbox",
        raw_path.filter(|path| !path.trim().is_empty()),
        suffix,
        at_root,
    )
}

fn render_inbox(
    vault_name: &str,
    ancestors: Vec<inbox_outbox::AncestorRow>,
    folders: Vec<FolderRow>,
    items: Vec<ItemRow>,
    raw_path: Option<&str>,
    error: Option<String>,
    notice: Option<String>,
) -> String {
    InboxTemplate {
        ancestors,
        folders,
        items,
        ingest_action_href: inbox_action_href(vault_name, raw_path, INGEST_SUFFIX, INGEST_AT_ROOT),
        error,
        notice,
    }
    .render()
    .unwrap_or_else(|_| "<p>Inbox could not be rendered.</p>".to_owned())
}

/// The merged root view: every top-level folder and file from both
/// `inbox_dir` and `processed_dir` at once, each carrying its own status,
/// matching the mock's single list (never two headed sections).
async fn inbox_merged_root(
    client: &McpClient,
    vault_name: &str,
    config: &InboxOutboxConfig,
) -> Result<(Vec<FolderRow>, Vec<ItemRow>), McpCallError> {
    let (inbox_listing, processed_listing) = tokio::try_join!(
        inbox_outbox::list_directory(client, vault_name, &config.inbox_dir),
        inbox_outbox::list_directory(client, vault_name, &config.processed_dir),
    )?;
    let mut folders: Vec<FolderRow> = inbox_listing
        .folders
        .into_iter()
        .map(|folder| folder_row(vault_name, "inbox", &config.inbox_dir, "", Some("pending"), folder))
        .chain(processed_listing.folders.into_iter().map(|folder| {
            folder_row(
                vault_name,
                "inbox",
                &config.processed_dir,
                &config.processed_dir,
                Some("processed"),
                folder,
            )
        }))
        .collect();
    folders.sort_by(|a, b| a.name.cmp(&b.name));

    // A pending item sits at the screen's own root; a processed one under the processed directory's name.
    let mut files: Vec<(inbox_outbox::DirectoryItem, (&str, &str), &'static str)> = inbox_listing
        .files
        .into_iter()
        .map(|item| (item, (config.inbox_dir.as_str(), ""), "pending"))
        .chain(processed_listing.files.into_iter().map(|item| {
            let processed = config.processed_dir.as_str();
            (item, (processed, processed), "processed")
        }))
        .collect();
    files.sort_by(|(a, _, _), (b, _, _)| compare_by_recency(a, b));
    let items = files
        .into_iter()
        .map(|(item, dirs, status)| file_row(vault_name, "inbox", dirs, Some(status), item))
        .collect();
    Ok((folders, items))
}

/// The processed directory's name is the reserved first segment of the processed tree's address, so a pending
/// folder that shares it cannot be opened from this screen. Said aloud, with the way out, instead of leaving the
/// operator with a folder whose link opens a different tree.
fn shadowed_pending_folder_warning(config: &InboxOutboxConfig, folders: &[FolderRow]) -> Option<String> {
    folders
        .iter()
        .any(|folder| folder.status == Some("pending") && folder.name == config.processed_dir)
        .then(|| {
            format!(
                "The pending folder \"{name}\" shares its name with the processed directory, so it cannot be opened \
                 here. Rename it to browse its contents.",
                name = config.processed_dir
            )
        })
}

/// A real subtree view: `directory`'s own immediate folders and files,
/// every row sharing `status` (a browsed subfolder is always entirely
/// within one tree, never a mix of pending and processed).
async fn inbox_tree_view(
    client: &McpClient,
    vault_name: &str,
    directory: &str,
    url_dir: &str,
    status: &'static str,
    is_root: bool,
) -> Result<Option<(Vec<FolderRow>, Vec<ItemRow>)>, McpCallError> {
    // A tree's own root may not exist yet (nothing has been processed) and still renders, empty; a named
    // subfolder that does not exist is simply not there.
    let listing = match inbox_outbox::list_existing_directory(client, vault_name, directory).await? {
        Some(listing) => listing,
        None if is_root => inbox_outbox::DirectoryListing::default(),
        None => return Ok(None),
    };
    let folders = listing
        .folders
        .into_iter()
        .map(|folder| folder_row(vault_name, "inbox", directory, url_dir, Some(status), folder))
        .collect();
    let mut files = listing.files;
    files.sort_by(compare_by_recency);
    let items = files
        .into_iter()
        .map(|item| file_row(vault_name, "inbox", (directory, url_dir), Some(status), item))
        .collect();
    Ok(Some((folders, items)))
}

/// An Inbox screen ready to serve: the content fragment, and the `POST` target of the upload control the title bar
/// shows for it (`None` where uploading is refused, inside the processed tree).
struct InboxScreen {
    fragment: String,
    upload_action_href: Option<String>,
}

/// Resolves `raw_path`, builds whichever view it names (the merged root or
/// a real subtree), and renders the Inbox screen. The one place both the
/// GET route and every mutating action (upload, ingest) go to re-render
/// the location they were called from.
async fn inbox_screen(
    client: &McpClient,
    vault_name: &str,
    config: &InboxOutboxConfig,
    raw_path: Option<&str>,
    error: Option<String>,
    notice: Option<String>,
) -> Result<InboxScreen, Box<Response>> {
    // A malformed `path` falls back to the merged root's own real content
    // (never an empty page) with the error surfaced alongside it: a stale
    // or hand-edited link should not look like the Inbox itself is empty.
    let (location, error) = match inbox_outbox::resolve_inbox_location(config, raw_path) {
        Ok(location) => (location, error),
        Err(reason) => (InboxLocation::MergedRoot, Some(format!("path {reason}."))),
    };
    let directory = location.directory();
    let url_segments = location.url_segments();
    let url_path = location.url_path();
    let unreachable_response = || Box::new(unreachable());
    match &location {
        InboxLocation::MergedRoot => {
            let (folders, items) = inbox_merged_root(client, vault_name, config)
                .await
                .map_err(|(McpCallError::Unreachable { .. } | McpCallError::Refused { .. })| unreachable_response())?;
            let error = error.or_else(|| shadowed_pending_folder_warning(config, &folders));
            Ok(InboxScreen {
                fragment: render_inbox(vault_name, Vec::new(), folders, items, None, error, notice),
                upload_action_href: Some(inbox_action_href(vault_name, None, UPLOAD_SUFFIX, UPLOAD_AT_ROOT)),
            })
        }
        InboxLocation::Tree {
            status, sub_segments, ..
        } => {
            let directory = directory.unwrap_or_else(|| config.inbox_dir.clone());
            let is_processed_root = *status == "processed" && sub_segments.is_empty();
            let Some((folders, items)) =
                inbox_tree_view(client, vault_name, &directory, &url_path, status, is_processed_root)
                    .await
                    .map_err(|(McpCallError::Unreachable { .. } | McpCallError::Refused { .. })| {
                        unreachable_response()
                    })?
            else {
                return Err(Box::new(not_found()));
            };
            let ancestors = inbox_outbox::ancestor_rows(vault_name, "inbox", "Inbox", &url_segments);
            let upload_action_href = (*status == "pending")
                .then(|| inbox_action_href(vault_name, Some(&url_path), UPLOAD_SUFFIX, UPLOAD_AT_ROOT));
            Ok(InboxScreen {
                fragment: render_inbox(vault_name, ancestors, folders, items, Some(&url_path), error, notice),
                upload_action_href,
            })
        }
    }
}

pub(super) async fn inbox_response(
    client: &McpClient,
    vault_name: &str,
    config: &InboxOutboxConfig,
    raw_path: Option<&str>,
    error: Option<String>,
    notice: Option<String>,
) -> Response {
    match inbox_screen(client, vault_name, config, raw_path, error, notice).await {
        Ok(screen) => html_response(screen.fragment),
        Err(response) => *response,
    }
}

async fn inbox_get_dispatch(
    state: InboxOutboxRoutesState,
    vault_name: String,
    raw_path: Option<String>,
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
    let config = inbox_outbox_config::load_inbox_outbox_config(&client, &vault_name).await;
    let screen = match inbox_screen(&client, &vault_name, &config, raw_path.as_deref(), None, None).await {
        Ok(screen) => screen,
        // A path that names no folder may name a file in it, which the screen answers with the file itself.
        Err(response) if response.status() == StatusCode::NOT_FOUND => {
            return match inbox_outbox::resolve_inbox_location(&config, raw_path.as_deref())
                .ok()
                .and_then(|location| location.directory())
            {
                Some(path) => file_response::stored(&client, &vault_name, &path, file.raw).await,
                None => *response,
            };
        }
        Err(response) => return *response,
    };
    let title_actions = screen
        .upload_action_href
        .as_deref()
        .map(|href| title_action::render(&TitleAction::UploadToInbox(href)))
        .unwrap_or_default();
    let body = ScreenBody {
        fragment: screen.fragment,
        title_actions,
    };
    full_page_or_fragment(&state, &client, &vault_name, "inbox", "Inbox", body, headers).await
}

/// `GET /{{vault_name}}/inbox`/`GET /{{vault_name}}/inbox/`: the merged root
/// view - axum's own `{{*path}}` wildcard cannot match zero path segments, so
/// this is registered as its own bare route, matching `build_router`'s own
/// "collection root" comment.
pub async fn inbox_get(
    State(state): State<InboxOutboxRoutesState>,
    Path(vault_name): Path<String>,
    headers: HeaderMap,
) -> Response {
    inbox_get_dispatch(state, vault_name, None, FileRequest::default(), headers).await
}

/// `GET /{{vault_name}}/inbox/{{*path}}`: browses the real subtree `path`
/// names, or answers with the file it names.
pub async fn inbox_get_path(
    State(state): State<InboxOutboxRoutesState>,
    Path((vault_name, path)): Path<(String, String)>,
    file: FileRequest,
    headers: HeaderMap,
) -> Response {
    inbox_get_dispatch(state, vault_name, Some(path), file, headers).await
}

// ---------------------------------------------------------------------
// GET /{{vault_name}}/outbox/{{*path}}
// ---------------------------------------------------------------------

#[derive(Template)]
#[template(path = "outbox.html")]
struct OutboxTemplate {
    ancestors: Vec<inbox_outbox::AncestorRow>,
    folders: Vec<FolderRow>,
    items: Vec<ItemRow>,
    error: Option<String>,
}

async fn outbox_get_dispatch(
    state: InboxOutboxRoutesState,
    vault_name: String,
    raw_path: Option<String>,
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
    let config = inbox_outbox_config::load_inbox_outbox_config(&client, &vault_name).await;
    let (directory, url_segments, error) =
        match inbox_outbox::resolve_outbox_directory(&config.outbox_dir, raw_path.as_deref()) {
            Ok((directory, url_segments)) => (directory, url_segments, None),
            Err(reason) => (config.outbox_dir.clone(), Vec::new(), Some(format!("path {reason}."))),
        };
    // The Outbox directory itself may not exist yet and still renders, empty; a named subfolder that does not
    // exist is not found.
    let listing = match inbox_outbox::list_existing_directory(&client, &vault_name, &directory).await {
        Ok(Some(listing)) => listing,
        Ok(None) if url_segments.is_empty() => inbox_outbox::DirectoryListing::default(),
        // A path that names no folder may name a file in it, which the screen answers with the file itself.
        Ok(None) => return file_response::stored(&client, &vault_name, &directory, file.raw).await,
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => return unreachable(),
    };
    let folders = listing
        .folders
        .into_iter()
        .map(|folder| folder_row(&vault_name, "outbox", &directory, &url_segments.join("/"), None, folder))
        .collect();
    let mut files = listing.files;
    files.sort_by(compare_by_recency);
    let items = files
        .into_iter()
        .map(|item| file_row(&vault_name, "outbox", (&directory, &url_segments.join("/")), None, item))
        .collect();
    let ancestors = inbox_outbox::ancestor_rows(&vault_name, "outbox", "Outbox", &url_segments);
    let fragment = OutboxTemplate {
        ancestors,
        folders,
        items,
        error,
    }
    .render()
    .unwrap_or_else(|_| "<p>Outbox could not be rendered.</p>".to_owned());
    let body = ScreenBody {
        fragment,
        title_actions: String::new(),
    };
    full_page_or_fragment(&state, &client, &vault_name, "outbox", "Outbox", body, headers).await
}

/// `GET /{{vault_name}}/outbox`/`GET /{{vault_name}}/outbox/`: `outbox_dir`'s
/// own root, registered bare for the identical reason [`inbox_get`] is.
pub async fn outbox_get(
    State(state): State<InboxOutboxRoutesState>,
    Path(vault_name): Path<String>,
    headers: HeaderMap,
) -> Response {
    outbox_get_dispatch(state, vault_name, None, FileRequest::default(), headers).await
}

/// `GET /{{vault_name}}/outbox/{{*path}}`: browses the real subtree `path`
/// names, or answers with the file it names.
pub async fn outbox_get_path(
    State(state): State<InboxOutboxRoutesState>,
    Path((vault_name, path)): Path<(String, String)>,
    file: FileRequest,
    headers: HeaderMap,
) -> Response {
    outbox_get_dispatch(state, vault_name, Some(path), file, headers).await
}

/// What a full-page Inbox or Outbox response wraps in the shell: the content fragment, and the action controls the
/// title bar shows beside the star (empty for none). An HTMX request swaps in the fragment alone.
struct ScreenBody {
    fragment: String,
    title_actions: String,
}

/// Shared by [`inbox_get`] and [`outbox_get`]: an `hx-request` swaps in
/// `fragment` alone, any other request wraps it in the full page shell,
/// matching `routes::vault_settings::get`'s identical split.
async fn full_page_or_fragment(
    state: &InboxOutboxRoutesState,
    client: &McpClient,
    vault_name: &str,
    breadcrumb: &str,
    page_title: &str,
    body: ScreenBody,
    headers: HeaderMap,
) -> Response {
    if headers.contains_key("hx-request") {
        return html_response(body.fragment);
    }
    let web_config_path = Arc::clone(&state.web_config_path);
    let entity_base_path = crate::urls::VaultUrls::from(vault_name).tray(breadcrumb, "");
    let known_mcp_servers = state.clients.names();
    let (mut nav, appearance, mcp_status) = tokio::join!(
        Box::pin(shell::build_nav(
            client,
            ActiveScreen::Vault,
            Some(vault_name),
            Some(breadcrumb),
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
    nav.set_title_actions(body.title_actions);
    html_response(page::render_page(&nav, page_title, &body.fragment)).into_response()
}

// ---------------------------------------------------------------------
// POST /{{vault_name}}/inbox/{{*path}}/execute/upload
// ---------------------------------------------------------------------

/// `path` with its trailing `/execute/{{command}}` action sub-path removed
/// (`docs/http-routing-response-contract.md`'s own `/{{entities}}/execute/{{command}}`
/// pattern, matching `routes::wiki::new_folder_action_path`'s identical
/// shape): "Upload"/"Ingest" are commands executed on the currently browsed
/// folder, never the identity of a real page or folder of their own.
/// Handles the Inbox root specially, the same way `new_folder_action_path`
/// handles the Wiki root: its own bare merged-root view has no leading path
/// segment at all, so its own command path is `at_root` with no leading
/// `/`. Operates on the raw, not-yet-validated wildcard capture - actual
/// confinement validation happens afterwards, inside
/// `inbox_outbox::resolve_inbox_location`, so a malformed target still
/// degrades to that function's own graceful "fall back to the merged root
/// with an error" contract rather than a bare `404` here.
fn action_target<'a>(raw_path: &'a str, suffix: &str, at_root: &str) -> Option<&'a str> {
    if let Some(stripped) = raw_path.strip_suffix(suffix) {
        return Some(stripped);
    }
    (raw_path == at_root).then_some("")
}

const UPLOAD_SUFFIX: &str = "/execute/upload";
const UPLOAD_AT_ROOT: &str = "execute/upload";
const INGEST_SUFFIX: &str = "/execute/ingest";
const INGEST_AT_ROOT: &str = "execute/ingest";

fn malformed_request_response() -> Response {
    html_response("<p class=\"hint error\">The request was malformed.</p>".to_owned())
}

/// `POST /{{vault_name}}/inbox/{{*path}}`: dispatches to [`upload_dispatch`] or
/// [`ingest_dispatch`] by `path`'s own trailing `/execute/upload`/`/execute/ingest`
/// action sub-path ([`action_target`]) - the two share this one route table
/// entry since both are a `POST` against the identical
/// `/{{vault_name}}/inbox/{{*path}}` wildcard, matching
/// `routes::wiki::create_dispatch`'s own identical suffix-dispatch shape.
/// Upload and Ingest need different request bodies (`multipart/form-data`
/// versus a JSON object), so this reads the generic, not-yet-typed
/// [`axum::extract::Request`] itself and only then extracts the concrete
/// body type the matched action actually needs, rather than committing to
/// one extractor before knowing which action `path` names.
pub async fn inbox_action(
    State(state): State<InboxOutboxRoutesState>,
    Path((vault_name, raw_path)): Path<(String, String)>,
    request: axum::extract::Request,
) -> Response {
    if let Some(target) = action_target(&raw_path, UPLOAD_SUFFIX, UPLOAD_AT_ROOT) {
        let target = target.to_owned();
        let Ok(multipart) = Multipart::from_request(request, &state).await else {
            return malformed_request_response();
        };
        return super::inbox_outbox_upload::upload_dispatch(&state, &vault_name, Some(&target), multipart).await;
    }
    if let Some(target) = action_target(&raw_path, INGEST_SUFFIX, INGEST_AT_ROOT) {
        let target = target.to_owned();
        let Ok(body) = axum::body::Bytes::from_request(request, &state).await else {
            return malformed_request_response();
        };
        return ingest_dispatch(&state, &vault_name, Some(&target), &body).await;
    }
    not_found()
}

// ---------------------------------------------------------------------
// POST /{{vault_name}}/inbox/{{*path}}/execute/ingest
// ---------------------------------------------------------------------

/// Exactly one of `file_path`/`folder_path` names what to ingest, each a
/// full vault-relative path (not a bare name): the row that posted this
/// already knows its own full location, so ingest never has to re-derive
/// it from whatever directory happens to be current.
#[derive(Debug, Deserialize)]
struct IngestBody {
    #[serde(default)]
    file_path: Option<String>,
    #[serde(default)]
    folder_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateTaskResult {
    path: String,
    #[serde(default)]
    key: Option<String>,
}

/// `body` with each named path normalised and required to lie inside the pending Inbox: an ingest creates a task
/// for the curator agent naming that path, so a request may never name anything else in the vault.
fn confined_to_pending_inbox(config: &InboxOutboxConfig, body: IngestBody) -> Result<IngestBody, String> {
    let confine = |raw: Option<String>| -> Result<Option<String>, String> {
        match raw.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
            None => Ok(None),
            Some(path) => inbox_outbox::pending_item_path(config, path)
                .map(Some)
                .map_err(|reason| format!("Only a pending Inbox item can be ingested: the path {reason}.")),
        }
    };
    Ok(IngestBody {
        file_path: confine(body.file_path)?,
        folder_path: confine(body.folder_path)?,
    })
}

fn ingest_task_fields(body: &IngestBody) -> Result<(String, String), String> {
    let file_path = body
        .file_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let folder_path = body
        .folder_path
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match (file_path, folder_path) {
        (Some(file_path), None) => {
            let name = file_path.rsplit('/').next().unwrap_or(file_path);
            Ok((
                format!("Ingest {name}"),
                format!("Ingest `{file_path}` into the knowledge substrate.\n\nSource: [[{file_path}|{name}]]\n"),
            ))
        }
        (None, Some(folder_path)) => {
            let name = folder_path.rsplit('/').next().unwrap_or(folder_path);
            Ok((
                format!("Ingest {name}/"),
                format!(
                    "Ingest every file under `{folder_path}/` into the knowledge substrate.\n\n\
                     Source: [[{folder_path}|{name}]]\n"
                ),
            ))
        }
        _ => Err("No Inbox item or folder was named to ingest.".to_owned()),
    }
}

/// `POST /{{vault_name}}/inbox/{{*path}}/execute/ingest`: creates one
/// `task_type: "action"` record via `base_record_create` against
/// `memory/tasks.base`, assigned to the configured curator agent, naming
/// either a single file or a whole folder - always [`IngestBody`]'s own
/// `file_path`/`folder_path`, never `raw_path` (the browsed folder this
/// re-renders on completion, a distinct target even when the ingested item
/// sits inside that same folder). Never touches the source file or folder
/// itself (moving it into the processed directory is the curator agent's
/// own ingestion skill's job, run outside this web server entirely); a
/// whole-folder ingest creates one task referencing the folder, never one
/// task per file inside it.
async fn ingest_dispatch(
    state: &InboxOutboxRoutesState,
    vault_name: &str,
    raw_path: Option<&str>,
    body: &[u8],
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, vault_name).await {
        return *response;
    }
    let config = inbox_outbox_config::load_inbox_outbox_config(&client, vault_name).await;
    let Ok(parsed) = serde_json::from_slice::<IngestBody>(body) else {
        return inbox_response(
            &client,
            vault_name,
            &config,
            raw_path,
            Some("Malformed request.".to_owned()),
            None,
        )
        .await;
    };
    if !curator_agent_is_registered(&client, vault_name, &config.curator_agent).await {
        return inbox_response(
            &client,
            vault_name,
            &config,
            raw_path,
            Some(unregistered_curator_error(&config.curator_agent)),
            None,
        )
        .await;
    }
    let parsed = match confined_to_pending_inbox(&config, parsed) {
        Ok(parsed) => parsed,
        Err(reason) => return inbox_response(&client, vault_name, &config, raw_path, Some(reason), None).await,
    };
    let (title, content) = match ingest_task_fields(&parsed) {
        Ok(fields) => fields,
        Err(reason) => return inbox_response(&client, vault_name, &config, raw_path, Some(reason), None).await,
    };

    let mut frontmatter = Map::new();
    frontmatter.insert("title".to_owned(), Value::String(title));
    // `base_record_create`'s own note construction defaults an unset `type`
    // to the generic `"note"`, not the task schema's `"task"`; every real
    // task file (this one included) must set it explicitly.
    frontmatter.insert("type".to_owned(), Value::String("task".to_owned()));
    frontmatter.insert("task_type".to_owned(), Value::String("action".to_owned()));
    frontmatter.insert("state".to_owned(), Value::String("next".to_owned()));
    frontmatter.insert("assignee".to_owned(), Value::String(config.curator_agent.clone()));

    let mut args = Map::new();
    args.insert(
        "path".to_owned(),
        Value::String(format!("{vault_name}://memory/tasks.base")),
    );
    args.insert("frontmatter".to_owned(), Value::Object(frontmatter));
    args.insert("content".to_owned(), Value::String(content));
    match client.call_tool("base_record_create".to_owned(), args).await {
        Ok(result) if result.is_error != Some(true) => {
            let notice = match result.into_typed::<CreateTaskResult>() {
                Ok(created) => format!(
                    "Created task {} for {}.",
                    created.key.unwrap_or(created.path),
                    config.curator_agent
                ),
                Err(_) => format!("Created a task for {}.", config.curator_agent),
            };
            inbox_response(&client, vault_name, &config, raw_path, None, Some(notice)).await
        }
        Ok(_) => {
            inbox_response(
                &client,
                vault_name,
                &config,
                raw_path,
                Some("The task could not be created.".to_owned()),
                None,
            )
            .await
        }
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => unreachable(),
    }
}

#[cfg(test)]
#[path = "inbox_outbox_test.rs"]
mod tests;
