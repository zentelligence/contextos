//! `GET`/`POST`/`PATCH /{{vault_name}}/profile/...`: the Profile screen.
//! Unlike Wiki/Insights/Entity-folders (each their own single-tree screen),
//! Profile always shows all three of its own groups - Identity
//! (`memory/identity/`), Operating (`memory/operating/`), Entities
//! (`memory/entities/`) - in one sidebar regardless of which item is
//! selected, so every route here renders the full three-group fragment,
//! differing only in which item (if any) is selected.
//!
//! Every path segment maps directly onto a real file: `identity/{filename}`
//! and `operating/{filename}` onto their own fixed root, `entities/{slug}`
//! onto either a configured entity's own `{{slug}}.md` profile page or a
//! real file matching no configured entity. `GET` renders that item read;
//! its own reserved `/edit` sub-path swaps the main body into an edit form
//! instead, matching `routes::wiki`'s identical reserved-sub-path contract.
//! `POST` on a group's own root creates a new file in it (Identity/Operating
//! only - Entities has no free-text "New", only per-entity provisioning),
//! landing directly in that new file's own edit form; `PATCH` saves an
//! existing item's edited body; `POST entities/{{slug}}` provisions a
//! configured entity's own missing profile page and/or top-level folder. A
//! path naming neither a real item nor (for Entities) a configured slug is
//! a genuine `404`.

use std::path::PathBuf;
use std::sync::Arc;

use askama::Template;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::config;
use crate::mcp_client::{McpCallError, McpClient, McpClientSet};
use crate::note_render::{self, NoteView};
use crate::profile_config::{self, ENTITIES_DIR, IDENTITY_DIR, OPERATING_DIR, ProfileConfig};
use crate::profile_docs::{self, CreateProfileFileError, ProfileFileLink, ReadProfileFileError, SaveFileError};
use crate::profile_entities::{self, ConfiguredEntityRow, EntitiesView, ProvisionEntityError};
use crate::rendering::page;
use crate::rendering::shell::{self, ActiveScreen};
use crate::routes::vault::{html_response, not_found, unreachable};

#[derive(Clone)]
pub struct ProfileRoutesState {
    pub clients: Arc<McpClientSet>,
    pub web_config_path: Arc<PathBuf>,
    /// The operator's local time, for every date this screen writes into a vault.
    pub vault_time: Arc<crate::vault_time::VaultTime>,
}

impl From<super::CommonRoutesState> for ProfileRoutesState {
    fn from(value: super::CommonRoutesState) -> Self {
        Self {
            clients: value.clients,
            web_config_path: value.web_config_path,
            vault_time: value.vault_time,
        }
    }
}

impl ProfileRoutesState {
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

fn malformed_body() -> Response {
    html_response("<p class=\"hint error\">Malformed request.</p>".to_owned())
}

// ---------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------

/// Which item (if any) the main pane shows. `Auto` is resolved to a
/// concrete selection (or `None`) once the group listings are loaded -
/// every bare group/screen root uses it, so hitting `/profile/`,
/// `/profile/identity/`, `/profile/operating/`, or `/profile/entities/`
/// all fall back to the same "first available document" default, since the
/// sidebar shows every group regardless of which root was requested.
#[derive(Clone)]
enum Selection {
    Auto,
    None,
    Identity(String),
    Operating(String),
    Entity(String),
}

// ---------------------------------------------------------------------
// Loading every group at once
// ---------------------------------------------------------------------

struct LoadedGroups {
    identity: Vec<ProfileFileLink>,
    operating: Vec<ProfileFileLink>,
    entities: EntitiesView,
}

async fn load_groups(client: &McpClient, vault_name: &str) -> LoadedGroups {
    let (identity, operating, entities) = tokio::join!(
        profile_docs::list_files(client, vault_name, IDENTITY_DIR),
        profile_docs::list_files(client, vault_name, OPERATING_DIR),
        profile_entities::load_view(client, vault_name),
    );
    LoadedGroups {
        identity,
        operating,
        entities,
    }
}

impl LoadedGroups {
    /// The first real document across every group, in Identity, Operating,
    /// Entities order - `Selection::None` when the vault has none at all
    /// yet.
    fn default_selection(&self) -> Selection {
        if let Some(first) = self.identity.first() {
            return Selection::Identity(first.relative_path.clone());
        }
        if let Some(first) = self.operating.first() {
            return Selection::Operating(first.relative_path.clone());
        }
        if let Some(first) = self.entities.configured.iter().find(|row| row.file_exists) {
            return Selection::Entity(first.slug.clone());
        }
        if let Some(first) = self.entities.orphans.first() {
            return Selection::Entity(first.label.clone());
        }
        Selection::None
    }
}

// ---------------------------------------------------------------------
// View models
// ---------------------------------------------------------------------

struct FileRowView {
    label: String,
    href: String,
    active: bool,
}

struct EntityRowView {
    label: String,
    href: String,
    active: bool,
    needs_provisioning: bool,
}

struct SelectedView {
    label: String,
    rendered_html: String,
    save_href: String,
    edit_href: String,
}

struct ProvisionView {
    label: String,
    provision_href: String,
}

/// Present only right after a brand-new Identity/Operating document is
/// created: swaps the main body straight into its own edit form instead of
/// the read view an empty document would otherwise show, matching
/// `routes::wiki::EditingView`'s identical reasoning and shape.
struct EditingView {
    edit_body: String,
    edit_expected_hash: String,
}

fn identity_href(vault_name: &str, relative_path: &str) -> String {
    crate::urls::VaultUrls::from(vault_name).profile_identity(relative_path)
}

fn operating_href(vault_name: &str, relative_path: &str) -> String {
    crate::urls::VaultUrls::from(vault_name).profile_operating(relative_path)
}

fn entity_href(vault_name: &str, slug: &str) -> String {
    crate::urls::VaultUrls::from(vault_name).profile_entity(slug)
}

fn identity_rows(vault_name: &str, groups: &LoadedGroups, selection: &Selection) -> Vec<FileRowView> {
    groups
        .identity
        .iter()
        .map(|file| FileRowView {
            label: file.label.clone(),
            href: identity_href(vault_name, &file.relative_path),
            active: matches!(selection, Selection::Identity(relative_path) if relative_path == &file.relative_path),
        })
        .collect()
}

fn operating_rows(vault_name: &str, groups: &LoadedGroups, selection: &Selection) -> Vec<FileRowView> {
    groups
        .operating
        .iter()
        .map(|file| FileRowView {
            label: file.label.clone(),
            href: operating_href(vault_name, &file.relative_path),
            active: matches!(selection, Selection::Operating(relative_path) if relative_path == &file.relative_path),
        })
        .collect()
}

fn entity_rows(vault_name: &str, configured: &[ConfiguredEntityRow], selection: &Selection) -> Vec<EntityRowView> {
    configured
        .iter()
        .map(|row| EntityRowView {
            label: row.name.clone(),
            href: entity_href(vault_name, &row.slug),
            active: matches!(selection, Selection::Entity(slug) if slug == &row.slug),
            needs_provisioning: row.needs_provisioning(),
        })
        .collect()
}

fn entity_orphan_rows(vault_name: &str, orphans: &[ProfileFileLink], selection: &Selection) -> Vec<FileRowView> {
    orphans
        .iter()
        .map(|file| FileRowView {
            label: file.label.clone(),
            href: entity_href(vault_name, &file.label),
            active: matches!(selection, Selection::Entity(slug) if slug == &file.label),
        })
        .collect()
}

#[derive(Template)]
#[template(path = "profile.html")]
struct ProfileTemplate {
    side_is_left: bool,
    identity_rows: Vec<FileRowView>,
    identity_new_href: String,
    operating_rows: Vec<FileRowView>,
    operating_new_href: String,
    entity_rows: Vec<EntityRowView>,
    entity_orphan_rows: Vec<FileRowView>,
    selected: Option<SelectedView>,
    editing: Option<EditingView>,
    provisioning: Option<ProvisionView>,
    error: Option<String>,
}

fn load_error_message(error: &ReadProfileFileError) -> String {
    match error {
        ReadProfileFileError::NotFound => "This file could not be found.".to_owned(),
        ReadProfileFileError::Write(_) => "The vault could not be reached.".to_owned(),
    }
}

/// Reads and renders one real file's own content, matching
/// `wiki::render_page_selected`'s identical shape. The edit form's own
/// body/hash is loaded separately, on demand, by [`render_edit_fragment`]
/// when the operator actually opens the item's own reserved `/edit`
/// sub-path - the read view no longer needs it up front, matching
/// `routes::wiki::render_page_selected`'s identical precedent.
async fn render_selected_file(
    client: &McpClient,
    vault_name: &str,
    dir: &str,
    relative_path: &str,
    save_href: String,
) -> (Option<SelectedView>, Option<String>) {
    match profile_docs::read_file(client, vault_name, dir, relative_path).await {
        Ok(raw) => {
            let rendered = note_render::render_note(
                client,
                vault_name,
                NoteView {
                    path: &profile_docs::file_path(dir, relative_path),
                    raw: Some(&raw),
                    template: false,
                },
            )
            .await;
            let label = relative_path.strip_suffix(".md").unwrap_or(relative_path).to_owned();
            let edit_href = format!("{save_href}/edit");
            (
                Some(SelectedView {
                    label,
                    rendered_html: rendered.html,
                    save_href,
                    edit_href,
                }),
                None,
            )
        }
        Err(error) => (None, Some(load_error_message(&error))),
    }
}

/// Resolves `selection` against `groups` and renders the whole
/// three-group fragment: every group's own sidebar rows plus whichever
/// main-pane state the selection names (a rendered file, a provisioning
/// prompt for a configured-but-missing entity, or nothing). `None` when
/// `selection` names neither a real file nor a configured slug nor a real
/// orphan file - the caller turns that into a real `404`.
async fn render_fragment(
    client: &McpClient,
    vault_name: &str,
    config: &ProfileConfig,
    selection: Selection,
    error: Option<String>,
) -> Option<String> {
    let groups = Box::pin(load_groups(client, vault_name)).await;
    let selection = match selection {
        Selection::Auto => groups.default_selection(),
        other => other,
    };
    let side_is_left = config.side == crate::panel_side::PanelSide::Left;

    let (selected, provisioning, load_error) = match &selection {
        Selection::Auto => unreachable!("Auto is resolved above"),
        Selection::None => (None, None, None),
        Selection::Identity(relative_path) => {
            if !groups.identity.iter().any(|file| &file.relative_path == relative_path) {
                return None;
            }
            let href = identity_href(vault_name, relative_path);
            let (selected, err) = Box::pin(render_selected_file(
                client,
                vault_name,
                IDENTITY_DIR,
                relative_path,
                href,
            ))
            .await;
            (selected, None, err)
        }
        Selection::Operating(relative_path) => {
            if !groups.operating.iter().any(|file| &file.relative_path == relative_path) {
                return None;
            }
            let href = operating_href(vault_name, relative_path);
            let (selected, err) = Box::pin(render_selected_file(
                client,
                vault_name,
                OPERATING_DIR,
                relative_path,
                href,
            ))
            .await;
            (selected, None, err)
        }
        Selection::Entity(slug) => {
            let configured = groups.entities.configured.iter().find(|row| &row.slug == slug);
            let is_orphan = groups.entities.orphans.iter().any(|file| &file.label == slug);
            if configured.is_none() && !is_orphan {
                return None;
            }
            if let Some(row) = configured.filter(|row| !row.file_exists) {
                (
                    None,
                    Some(ProvisionView {
                        label: row.name.clone(),
                        provision_href: entity_href(vault_name, slug),
                    }),
                    None,
                )
            } else {
                let relative_path = format!("{slug}.md");
                let href = entity_href(vault_name, slug);
                let (selected, err) = Box::pin(render_selected_file(
                    client,
                    vault_name,
                    ENTITIES_DIR,
                    &relative_path,
                    href,
                ))
                .await;
                (selected, None, err)
            }
        }
    };

    Some(
        ProfileTemplate {
            side_is_left,
            identity_rows: identity_rows(vault_name, &groups, &selection),
            identity_new_href: crate::urls::VaultUrls::from(vault_name).profile_identity(""),
            operating_rows: operating_rows(vault_name, &groups, &selection),
            operating_new_href: crate::urls::VaultUrls::from(vault_name).profile_operating(""),
            entity_rows: entity_rows(vault_name, &groups.entities.configured, &selection),
            entity_orphan_rows: entity_orphan_rows(vault_name, &groups.entities.orphans, &selection),
            selected,
            editing: None,
            provisioning,
            error: error.or(load_error),
        }
        .render()
        .unwrap_or_else(|_| "<p>The Profile screen could not be rendered.</p>".to_owned()),
    )
}

/// After a brand-new Identity/Operating document is created, renders the
/// full three-group fragment with that document's own edit form already
/// populated in place of the read view - matching
/// `routes::wiki::render_created_page`'s identical reasoning: landing on a
/// freshly created empty document in the read view first, only to make the
/// operator separately click "Edit", is worse than opening it ready to type
/// in directly.
async fn render_created_document(
    client: &McpClient,
    vault_name: &str,
    config: &ProfileConfig,
    dir: &'static str,
    selection: Selection,
    save_href: String,
    relative_path: &str,
) -> String {
    let groups = Box::pin(load_groups(client, vault_name)).await;
    let side_is_left = config.side == crate::panel_side::PanelSide::Left;
    let label = relative_path.strip_suffix(".md").unwrap_or(relative_path).to_owned();
    let edit_href = format!("{save_href}/edit");
    let (edit_body, edit_expected_hash) =
        match profile_docs::load_file_body(client, vault_name, dir, relative_path).await {
            Ok((body, hash)) => (body, hash.unwrap_or_default()),
            Err(_) => (String::new(), String::new()),
        };

    ProfileTemplate {
        side_is_left,
        identity_rows: identity_rows(vault_name, &groups, &selection),
        identity_new_href: crate::urls::VaultUrls::from(vault_name).profile_identity(""),
        operating_rows: operating_rows(vault_name, &groups, &selection),
        operating_new_href: crate::urls::VaultUrls::from(vault_name).profile_operating(""),
        entity_rows: entity_rows(vault_name, &groups.entities.configured, &selection),
        entity_orphan_rows: entity_orphan_rows(vault_name, &groups.entities.orphans, &selection),
        selected: Some(SelectedView {
            label,
            rendered_html: String::new(),
            save_href,
            edit_href,
        }),
        editing: Some(EditingView {
            edit_body,
            edit_expected_hash,
        }),
        provisioning: None,
        error: None,
    }
    .render()
    .unwrap_or_else(|_| "<p>The Profile screen could not be rendered.</p>".to_owned())
}

/// `GET` on an Identity/Operating/Entities item's own reserved `/edit`
/// sub-path (`docs/http-routing-response-contract.md`'s reserved-sub-path
/// pattern): the whole three-group screen, with `editing` populated in
/// place of the read view - never just a bare
/// `#profile-main-body` fragment alone. The header's own Edit button lives
/// outside that div, as a sibling within the same `.notes-main-header`; a
/// response that swapped only the body left that button stale, rendering
/// it alongside the edit form's own Save/Cancel pair rather than in place
/// of it. Matches `routes::wiki::render_edit_response`'s identical
/// contract, and `render_created_document`'s identical "editing populated,
/// in the full template" shape for the one other case that already needed
/// it. `href` doubles as both this fragment's own `PATCH` save target and
/// its `GET` cancel target, since every Profile item's own URL
/// already answers both methods.
async fn render_edit_fragment(
    client: &McpClient,
    vault_name: &str,
    config: &ProfileConfig,
    dir: &'static str,
    selection: Selection,
    relative_path: &str,
    href: String,
) -> Response {
    let groups = Box::pin(load_groups(client, vault_name)).await;
    let side_is_left = config.side == crate::panel_side::PanelSide::Left;
    let label = relative_path.strip_suffix(".md").unwrap_or(relative_path).to_owned();
    let edit_href = format!("{href}/edit");
    match profile_docs::load_file_body(client, vault_name, dir, relative_path).await {
        Ok((edit_body, edit_expected_hash)) => html_response(
            ProfileTemplate {
                side_is_left,
                identity_rows: identity_rows(vault_name, &groups, &selection),
                identity_new_href: crate::urls::VaultUrls::from(vault_name).profile_identity(""),
                operating_rows: operating_rows(vault_name, &groups, &selection),
                operating_new_href: crate::urls::VaultUrls::from(vault_name).profile_operating(""),
                entity_rows: entity_rows(vault_name, &groups.entities.configured, &selection),
                entity_orphan_rows: entity_orphan_rows(vault_name, &groups.entities.orphans, &selection),
                selected: Some(SelectedView {
                    label,
                    rendered_html: String::new(),
                    save_href: href,
                    edit_href,
                }),
                editing: Some(EditingView {
                    edit_body,
                    edit_expected_hash: edit_expected_hash.unwrap_or_default(),
                }),
                provisioning: None,
                error: None,
            }
            .render()
            .unwrap_or_else(|_| "<p>The Profile screen could not be rendered.</p>".to_owned()),
        ),
        Err(ReadProfileFileError::NotFound) => not_found(),
        Err(ReadProfileFileError::Write(McpCallError::Unreachable { .. } | McpCallError::Refused { .. })) => {
            unreachable()
        }
    }
}

pub async fn get_identity_edit(
    State(state): State<ProfileRoutesState>,
    Path((vault_name, filename)): Path<(String, String)>,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let config = profile_config::load_profile_config(&client, &vault_name).await;
    let href = identity_href(&vault_name, &filename);
    render_edit_fragment(
        &client,
        &vault_name,
        &config,
        IDENTITY_DIR,
        Selection::Identity(filename.clone()),
        &filename,
        href,
    )
    .await
}

pub async fn get_operating_edit(
    State(state): State<ProfileRoutesState>,
    Path((vault_name, filename)): Path<(String, String)>,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let config = profile_config::load_profile_config(&client, &vault_name).await;
    let href = operating_href(&vault_name, &filename);
    render_edit_fragment(
        &client,
        &vault_name,
        &config,
        OPERATING_DIR,
        Selection::Operating(filename.clone()),
        &filename,
        href,
    )
    .await
}

pub async fn get_entity_edit(
    State(state): State<ProfileRoutesState>,
    Path((vault_name, slug)): Path<(String, String)>,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let config = profile_config::load_profile_config(&client, &vault_name).await;
    let relative_path = format!("{slug}.md");
    let href = entity_href(&vault_name, &slug);
    render_edit_fragment(
        &client,
        &vault_name,
        &config,
        ENTITIES_DIR,
        Selection::Entity(slug.clone()),
        &relative_path,
        href,
    )
    .await
}

// ---------------------------------------------------------------------
// GET
// ---------------------------------------------------------------------

async fn get_dispatch(
    state: ProfileRoutesState,
    vault_name: String,
    selection: Selection,
    headers: HeaderMap,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let config = profile_config::load_profile_config(&client, &vault_name).await;
    let Some(fragment) = Box::pin(render_fragment(&client, &vault_name, &config, selection, None)).await else {
        return not_found();
    };
    if headers.contains_key("hx-request") {
        return html_response(fragment);
    }
    let web_config_path = Arc::clone(&state.web_config_path);
    let entity_base_path = crate::urls::VaultUrls::from(&*vault_name).profile();
    let known_mcp_servers = state.clients.names();
    let (mut nav, appearance, mcp_status) = tokio::join!(
        Box::pin(shell::build_nav(
            &client,
            ActiveScreen::Vault,
            Some(&vault_name),
            Some("profile"),
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
    html_response(page::render_page(&nav, "Profile", &fragment)).into_response()
}

pub async fn get_root(
    State(state): State<ProfileRoutesState>,
    Path(vault_name): Path<String>,
    headers: HeaderMap,
) -> Response {
    get_dispatch(state, vault_name, Selection::Auto, headers).await
}

pub async fn get_identity_file(
    State(state): State<ProfileRoutesState>,
    Path((vault_name, filename)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    get_dispatch(state, vault_name, Selection::Identity(filename), headers).await
}

pub async fn get_operating_file(
    State(state): State<ProfileRoutesState>,
    Path((vault_name, filename)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    get_dispatch(state, vault_name, Selection::Operating(filename), headers).await
}

pub async fn get_entity(
    State(state): State<ProfileRoutesState>,
    Path((vault_name, slug)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    get_dispatch(state, vault_name, Selection::Entity(slug), headers).await
}

// ---------------------------------------------------------------------
// POST - creating a new Identity/Operating document
// ---------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct CreateBody {
    #[serde(default)]
    title: String,
}

fn create_error_message(error: &CreateProfileFileError) -> String {
    match error {
        CreateProfileFileError::EmptyTitle => "A file needs a title.".to_owned(),
        CreateProfileFileError::Write(_) => "The vault could not be reached.".to_owned(),
        CreateProfileFileError::Rejected => {
            "The file could not be created (a file with that name may already exist).".to_owned()
        }
    }
}

async fn create_group_dispatch(
    state: ProfileRoutesState,
    vault_name: String,
    dir: &'static str,
    make_href: fn(&str, &str) -> String,
    make_selection: fn(String) -> Selection,
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
    let config = profile_config::load_profile_config(&client, &vault_name).await;
    match profile_docs::create_file(&client, &vault_name, dir, &parsed.title).await {
        Ok(relative_path) => {
            let target = make_href(&vault_name, &relative_path);
            let selection = make_selection(relative_path.clone());
            let fragment = Box::pin(render_created_document(
                &client,
                &vault_name,
                &config,
                dir,
                selection,
                target.clone(),
                &relative_path,
            ))
            .await;
            let mut response = html_response(fragment);
            if let Ok(value) = HeaderValue::from_str(&target) {
                response.headers_mut().insert("HX-Push-Url", value);
            }
            response
        }
        Err(error) => {
            let message = create_error_message(&error);
            let fragment = Box::pin(render_fragment(
                &client,
                &vault_name,
                &config,
                Selection::Auto,
                Some(message),
            ))
            .await
            .unwrap_or_else(|| "<p>The Profile screen could not be rendered.</p>".to_owned());
            html_response(fragment)
        }
    }
}

pub async fn create_identity(
    State(state): State<ProfileRoutesState>,
    Path(vault_name): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    create_group_dispatch(
        state,
        vault_name,
        IDENTITY_DIR,
        identity_href,
        Selection::Identity,
        body,
    )
    .await
}

pub async fn create_operating(
    State(state): State<ProfileRoutesState>,
    Path(vault_name): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    create_group_dispatch(
        state,
        vault_name,
        OPERATING_DIR,
        operating_href,
        Selection::Operating,
        body,
    )
    .await
}

// ---------------------------------------------------------------------
// PATCH - editing an existing Identity/Operating/Entities document
// ---------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct SaveBody {
    #[serde(default)]
    content: String,
    #[serde(default)]
    expected_hash: Option<String>,
}

async fn patch_group_dispatch(
    state: ProfileRoutesState,
    vault_name: String,
    dir: &'static str,
    relative_path: String,
    selection: Selection,
    body: axum::body::Bytes,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let Ok(parsed) = serde_json::from_slice::<SaveBody>(&body) else {
        return malformed_body();
    };
    let config = profile_config::load_profile_config(&client, &vault_name).await;
    match profile_docs::save_file_body(
        &client,
        &vault_name,
        dir,
        &relative_path,
        &parsed.content,
        parsed.expected_hash,
    )
    .await
    {
        Ok(true) => html_response(
            Box::pin(render_fragment(&client, &vault_name, &config, selection, None))
                .await
                .unwrap_or_else(|| "<p>The Profile screen could not be rendered.</p>".to_owned()),
        ),
        Ok(false) => html_response(
            Box::pin(render_fragment(
                &client,
                &vault_name,
                &config,
                selection,
                Some("This file changed since it was last loaded; reload and try again.".to_owned()),
            ))
            .await
            .unwrap_or_else(|| "<p>The Profile screen could not be rendered.</p>".to_owned()),
        ),
        Err(SaveFileError::Load(ReadProfileFileError::NotFound)) => not_found(),
        Err(
            SaveFileError::Load(ReadProfileFileError::Write(
                McpCallError::Unreachable { .. } | McpCallError::Refused { .. },
            ))
            | SaveFileError::Write(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }),
        ) => unreachable(),
    }
}

pub async fn patch_identity(
    State(state): State<ProfileRoutesState>,
    Path((vault_name, filename)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> Response {
    let selection = Selection::Identity(filename.clone());
    patch_group_dispatch(state, vault_name, IDENTITY_DIR, filename, selection, body).await
}

pub async fn patch_operating(
    State(state): State<ProfileRoutesState>,
    Path((vault_name, filename)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> Response {
    let selection = Selection::Operating(filename.clone());
    patch_group_dispatch(state, vault_name, OPERATING_DIR, filename, selection, body).await
}

pub async fn patch_entity(
    State(state): State<ProfileRoutesState>,
    Path((vault_name, slug)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> Response {
    let relative_path = format!("{slug}.md");
    let selection = Selection::Entity(slug);
    patch_group_dispatch(state, vault_name, ENTITIES_DIR, relative_path, selection, body).await
}

// ---------------------------------------------------------------------
// POST /{{vault_name}}/profile/entities/{{slug}} - provisioning
// ---------------------------------------------------------------------

fn provision_error_message(error: &ProvisionEntityError) -> String {
    match error {
        ProvisionEntityError::NotConfigured => "This entity is not configured.".to_owned(),
        ProvisionEntityError::Template(_) => "The configured entity-profile template could not be rendered.".to_owned(),
        ProvisionEntityError::Write(_) => "The vault could not be reached.".to_owned(),
        ProvisionEntityError::FolderRejected => "The entity's own folder could not be created.".to_owned(),
        ProvisionEntityError::FileRejected => {
            "The entity's own profile page could not be created (it may already exist).".to_owned()
        }
    }
}

pub async fn post_provision_entity(
    State(state): State<ProfileRoutesState>,
    Path((vault_name, slug)): Path<(String, String)>,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let config = profile_config::load_profile_config(&client, &vault_name).await;
    match profile_entities::provision_entity(&client, &vault_name, &config, &slug, state.vault_time.instant().await)
        .await
    {
        Ok(()) => html_response(
            Box::pin(render_fragment(
                &client,
                &vault_name,
                &config,
                Selection::Entity(slug),
                None,
            ))
            .await
            .unwrap_or_else(|| "<p>The Profile screen could not be rendered.</p>".to_owned()),
        ),
        Err(ProvisionEntityError::NotConfigured) => not_found(),
        Err(ProvisionEntityError::Write(McpCallError::Unreachable { .. } | McpCallError::Refused { .. })) => {
            unreachable()
        }
        Err(error) => {
            let message = provision_error_message(&error);
            html_response(
                Box::pin(render_fragment(
                    &client,
                    &vault_name,
                    &config,
                    Selection::Entity(slug),
                    Some(message),
                ))
                .await
                .unwrap_or_else(|| "<p>The Profile screen could not be rendered.</p>".to_owned()),
            )
        }
    }
}

#[cfg(test)]
#[path = "profile_test.rs"]
mod tests;
