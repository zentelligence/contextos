//! `GET`/`PATCH /{{vault_name}}/settings/`: the vault-scoped settings screen's configuration page, distinct from the
//! vault-independent `/settings/` (`web.toml`, Command Centre chrome). Its sidebar ([`SettingsNav`]) has two groups:
//! the configuration panes, all rendered in this one response and switched client-side (the same `data-pane`
//! mechanism as `templates/settings.html`), and the vault management categories, each its own page
//! (`routes::vault_management`).
//!
//! Every configuration pane edits its own `.contextos/*.toml` file through a real form, never raw text: Tasks through
//! a structured editor over its lists ([`crate::routes::vault_settings_tasks`]); Inbox & Outbox, Journal, Notes, Wiki,
//! Insights, Entity folders, and Profile through a small fixed set of fields each (`curator_agent` in particular is a
//! `<select>` sourced from `.contextos/team.toml`'s own agent entries, so an unregistered agent is structurally
//! unselectable rather than merely rejected after a parse).
//!
//! Journal and Notes ([`crate::routes::vault_settings_dates`]), Wiki, Insights, and Entity folders
//! ([`crate::routes::vault_settings_knowledge`]), Profile ([`crate::routes::vault_settings_profile`]), and Tasks each
//! live in a sibling module; this file keeps Inbox & Outbox and the shared page composition ([`ConfigSections`],
//! [`render_sections`]) every section's own save path re-renders through.

use std::path::PathBuf;
use std::sync::Arc;

use askama::Template;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::config;
use crate::inbox_outbox::sanitised_path_segment;
use crate::inbox_outbox_config::{self, INBOX_OUTBOX_TOML_PATH, InboxOutboxConfig};
use crate::mcp_client::{McpCallError, McpClient, McpClientSet};
use crate::rendering::page;
use crate::rendering::shell::{self, ActiveScreen};
use crate::routes::vault::{html_response, not_found, unreachable};
use crate::routes::vault_settings_dates::{self, JournalSectionState, NotesSectionState};
use crate::routes::vault_settings_knowledge::{self, EntityDocsSectionState, InsightsSectionState, WikiSectionState};
use crate::routes::vault_settings_profile::{self, ProfileSectionState};
use crate::team::{self, TeamMember, TeamMemberKind};
use crate::vault_catalogue;

use super::vault_management::SettingsNav;
use super::vault_settings_tasks::{self, TasksPaneView, TasksSectionState};

#[derive(Clone)]
pub struct VaultSettingsRoutesState {
    pub clients: Arc<McpClientSet>,
    pub web_config_path: Arc<PathBuf>,
}

impl From<super::CommonRoutesState> for VaultSettingsRoutesState {
    fn from(value: super::CommonRoutesState) -> Self {
        Self {
            clients: value.clients,
            web_config_path: value.web_config_path,
        }
    }
}

impl VaultSettingsRoutesState {
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

#[derive(Debug, Deserialize)]
pub(crate) struct FileInfoResult {
    #[serde(default)]
    pub(crate) content_hash: Option<String>,
}

/// The Inbox/Outbox file's own current hash alone: this section's own
/// fields come from [`InboxOutboxConfig`], already loaded separately, never
/// from this file's raw text (there is no textarea to show it in).
async fn inbox_outbox_hash(client: &McpClient, vault_name: &str) -> Option<String> {
    let mut args = Map::new();
    args.insert(
        "path".to_owned(),
        Value::String(format!("{vault_name}://{INBOX_OUTBOX_TOML_PATH}")),
    );
    match client.call_tool("fs_get_file_info".to_owned(), args).await {
        Ok(result) if result.is_error != Some(true) => result
            .into_typed::<FileInfoResult>()
            .ok()
            .and_then(|info| info.content_hash),
        _ => None,
    }
}

/// One `<option>` the `curator_agent` select offers.
pub(crate) struct AgentOption {
    pub(crate) value: String,
    pub(crate) label: String,
    pub(crate) selected: bool,
}

/// Builds `curator_agent`'s own select options from every registered agent,
/// marking `current` selected. When `current` matches no registered agent
/// (a since-removed slug, or the built-in `"karla"` default before any real
/// agent has ever been registered), it is appended as its own extra,
/// selected option rather than silently dropped: the field must never
/// visibly reset to a different value than what `.contextos/inbox-outbox.toml`
/// actually holds. Never empty: `current` alone is always at least one row,
/// so this section's own form never renders an unusable empty `<select>`.
fn agent_options(team: &[TeamMember], current: &str) -> Vec<AgentOption> {
    let mut options: Vec<AgentOption> = team
        .iter()
        .filter(|agent| agent.kind == TeamMemberKind::Agent)
        .map(|agent| AgentOption {
            value: agent.slug.clone(),
            label: agent.name.clone(),
            selected: agent.slug == current,
        })
        .collect();
    if !options.iter().any(|option| option.selected) {
        options.push(AgentOption {
            value: current.to_owned(),
            label: current.to_owned(),
            selected: true,
        });
    }
    options
}

/// The Inbox/Outbox section's own rendered state: real, structured field
/// values, never a raw TOML blob.
pub(crate) struct InboxOutboxSectionState {
    curator_agent_options: Vec<AgentOption>,
    inbox_dir_value: String,
    outbox_dir_value: String,
    processed_dir_value: String,
    expected_hash: Option<String>,
    error: Option<String>,
}

/// Reads the Inbox/Outbox section fresh: the configured file (or its typed
/// default when absent/malformed) plus the vault's currently registered
/// agents, combined into this section's own select options.
pub(crate) async fn read_inbox_outbox_section(client: &McpClient, vault_name: &str) -> InboxOutboxSectionState {
    let (config, agents, expected_hash) = tokio::join!(
        inbox_outbox_config::load_inbox_outbox_config(client, vault_name),
        team::load_team(client, vault_name),
        inbox_outbox_hash(client, vault_name),
    );
    InboxOutboxSectionState {
        curator_agent_options: agent_options(&agents, &config.curator_agent),
        inbox_dir_value: config.inbox_dir,
        outbox_dir_value: config.outbox_dir,
        processed_dir_value: config.processed_dir,
        expected_hash,
        error: None,
    }
}

// Six independent boolean flags is over clippy's own default threshold for
// one struct, but each one drives exactly one unrelated `{% if %}` in its
// own section of the page (Journal's own active/side pair, Notes' own
// active/side pair) - there is no combinatorial state machine here an enum
// would meaningfully replace, just several sections' own flat view-model
// fields happening to share a type.
#[allow(clippy::struct_excessive_bools)]
#[derive(Template)]
#[template(path = "vault_settings.html")]
struct VaultSettingsTemplate {
    /// Every URL the template links to within the vault.
    urls: crate::urls::VaultUrls,
    nav: SettingsNav,
    tasks: TasksPaneView,
    curator_agent_options: Vec<AgentOption>,
    inbox_dir_value: String,
    outbox_dir_value: String,
    processed_dir_value: String,
    inbox_outbox_expected_hash: String,
    inbox_outbox_error: Option<String>,
    journal_dir_value: String,
    template_path_value: String,
    journal_date_format_value: String,
    journal_date_structure_side_is_left: bool,
    journal_task_opened_section_value: String,
    journal_task_closed_section_value: String,
    journal_read_only_sections_value: String,
    journal_expected_hash: String,
    journal_error: Option<String>,
    notes_dir_value: String,
    notes_template_path_value: String,
    notes_date_format_value: String,
    notes_date_structure_active: bool,
    notes_date_structure_side_is_left: bool,
    notes_metadata_panel_side_is_left: bool,
    notes_expected_hash: String,
    notes_error: Option<String>,
    wiki_dir_value: String,
    wiki_tree_side_is_left: bool,
    wiki_metadata_panel_side_is_left: bool,
    wiki_expected_hash: String,
    wiki_error: Option<String>,
    insights_dir_value: String,
    insights_shard_side_is_left: bool,
    insights_metadata_panel_side_is_left: bool,
    insights_expected_hash: String,
    insights_error: Option<String>,
    entity_folder_side_is_left: bool,
    entity_metadata_panel_side_is_left: bool,
    entity_expected_hash: String,
    entity_error: Option<String>,
    profile_side_is_left: bool,
    profile_entity_template_path_value: String,
    profile_expected_hash: String,
    profile_error: Option<String>,
}

/// Every configuration section's rendered state, read together for one response. A rejected save replaces only its
/// own section (keeping what was submitted, plus the reason) before rendering, so every other section still shows
/// what is on disk.
pub(crate) struct ConfigSections {
    pub(crate) tasks: TasksSectionState,
    pub(crate) inbox_outbox: InboxOutboxSectionState,
    pub(crate) journal: JournalSectionState,
    pub(crate) notes: NotesSectionState,
    pub(crate) wiki: WikiSectionState,
    pub(crate) insights: InsightsSectionState,
    pub(crate) entity: EntityDocsSectionState,
    pub(crate) profile: ProfileSectionState,
}

impl ConfigSections {
    /// Reads every section fresh from the vault, concurrently.
    pub(crate) async fn read(client: &McpClient, vault_name: &str) -> Self {
        let (tasks, inbox_outbox, journal, notes, wiki, insights, entity, profile) = tokio::join!(
            vault_settings_tasks::read_tasks_section(client, vault_name),
            read_inbox_outbox_section(client, vault_name),
            vault_settings_dates::read_journal_section(client, vault_name),
            vault_settings_dates::read_notes_section(client, vault_name),
            vault_settings_knowledge::read_wiki_section(client, vault_name),
            vault_settings_knowledge::read_insights_section(client, vault_name),
            vault_settings_knowledge::read_entity_docs_section(client, vault_name),
            vault_settings_profile::read_profile_section(client, vault_name),
        );
        Self {
            tasks,
            inbox_outbox,
            journal,
            notes,
            wiki,
            insights,
            entity,
            profile,
        }
    }
}

fn render(vault_name: &str, sections: ConfigSections, nav: SettingsNav) -> String {
    let ConfigSections {
        tasks,
        inbox_outbox,
        journal,
        notes,
        wiki,
        insights,
        entity,
        profile,
    } = sections;
    VaultSettingsTemplate {
        urls: crate::urls::VaultUrls::from(vault_name),
        nav,
        tasks: TasksPaneView::from(tasks),
        curator_agent_options: inbox_outbox.curator_agent_options,
        inbox_dir_value: inbox_outbox.inbox_dir_value,
        outbox_dir_value: inbox_outbox.outbox_dir_value,
        processed_dir_value: inbox_outbox.processed_dir_value,
        inbox_outbox_expected_hash: inbox_outbox.expected_hash.unwrap_or_default(),
        inbox_outbox_error: inbox_outbox.error,
        journal_dir_value: journal.journal_dir_value,
        template_path_value: journal.template_path_value,
        journal_date_format_value: journal.date_format_value,
        journal_date_structure_side_is_left: journal.date_structure_side_is_left,
        journal_task_opened_section_value: journal.task_opened_section_value,
        journal_task_closed_section_value: journal.task_closed_section_value,
        journal_read_only_sections_value: journal.read_only_sections_value,
        journal_expected_hash: journal.expected_hash.unwrap_or_default(),
        journal_error: journal.error,
        notes_dir_value: notes.notes_dir_value,
        notes_template_path_value: notes.template_path_value,
        notes_date_format_value: notes.date_format_value,
        notes_date_structure_active: notes.date_structure_active,
        notes_date_structure_side_is_left: notes.date_structure_side_is_left,
        notes_metadata_panel_side_is_left: notes.metadata_panel_side_is_left,
        notes_expected_hash: notes.expected_hash.unwrap_or_default(),
        notes_error: notes.error,
        wiki_dir_value: wiki.wiki_dir_value,
        wiki_tree_side_is_left: wiki.tree_side_is_left,
        wiki_metadata_panel_side_is_left: wiki.metadata_panel_side_is_left,
        wiki_expected_hash: wiki.expected_hash.unwrap_or_default(),
        wiki_error: wiki.error,
        insights_dir_value: insights.insights_dir_value,
        insights_shard_side_is_left: insights.shard_side_is_left,
        insights_metadata_panel_side_is_left: insights.metadata_panel_side_is_left,
        insights_expected_hash: insights.expected_hash.unwrap_or_default(),
        insights_error: insights.error,
        entity_folder_side_is_left: entity.folder_side_is_left,
        entity_metadata_panel_side_is_left: entity.metadata_panel_side_is_left,
        entity_expected_hash: entity.expected_hash.unwrap_or_default(),
        entity_error: entity.error,
        profile_side_is_left: profile.side_is_left,
        profile_entity_template_path_value: profile.entity_template_path_value,
        profile_expected_hash: profile.expected_hash.unwrap_or_default(),
        profile_error: profile.error,
    }
    .render()
    .unwrap_or_else(|_| "<p>Vault settings could not be rendered.</p>".to_owned())
}

/// Renders the configuration page with every section read fresh, after `adjust` has had the chance to put a rejected
/// submission (and its reason) back into its own section.
pub(crate) async fn render_sections(
    client: &McpClient,
    vault_name: &str,
    adjust: impl FnOnce(&mut ConfigSections),
) -> String {
    // Both halves join many tool calls; boxed so their combined state lives on the heap, not the caller's stack.
    let (mut sections, catalogue) = tokio::join!(
        Box::pin(ConfigSections::read(client, vault_name)),
        Box::pin(vault_catalogue::load_catalogue(client, vault_name)),
    );
    adjust(&mut sections);
    render(vault_name, sections, SettingsNav::configuration(vault_name, &catalogue))
}

/// Re-reads every section fresh from disk, rendering with no error: the
/// common "just saved, or just loaded the page" case.
pub(crate) async fn render_current(client: &McpClient, vault_name: &str) -> String {
    render_sections(client, vault_name, |_| {}).await
}

/// `GET /{{vault_name}}/settings/`.
pub async fn get(
    State(state): State<VaultSettingsRoutesState>,
    Path(vault_name): Path<String>,
    headers: HeaderMap,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let fragment = Box::pin(render_current(&client, &vault_name)).await;
    if headers.contains_key("hx-request") {
        html_response(fragment)
    } else {
        let web_config_path = Arc::clone(&state.web_config_path);
        let breadcrumb = "settings".to_owned();
        let entity_base_path = crate::urls::VaultUrls::from(&*vault_name).settings();
        let known_mcp_servers = state.clients.names();
        let (mut nav, appearance, mcp_status) = tokio::join!(
            Box::pin(shell::build_nav(
                &client,
                ActiveScreen::Vault,
                Some(&vault_name),
                Some(&breadcrumb),
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
        html_response(page::render_page(&nav, "Vault settings", &fragment))
    }
    .into_response()
}

/// One `PATCH /{{vault_name}}/settings/` submission. `target` selects which
/// section was submitted (`"tasks"`, the default, matching this route's
/// original single-section body shape, `"inbox_outbox"`, `"journal"`, or
/// `"notes"`); an unrecognised value is malformed input, not a silent
/// no-op. The Tasks section still submits raw `content`; every other
/// section submits its own fields directly, since each is a real form now,
/// not a textarea. `template_path`/`date_format`/`date_structure_active`/
/// `date_structure_side` are shared by Journal and Notes (never submitted
/// by the same request, since only one section is ever the `target`),
/// matching `curator_agent`/`inbox_dir`/and so on's identical "one shared
/// field name, meaning depends on `target`" shape.
#[derive(Debug, Deserialize)]
pub(crate) struct SaveSectionBody {
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    curator_agent: Option<String>,
    #[serde(default)]
    inbox_dir: Option<String>,
    #[serde(default)]
    outbox_dir: Option<String>,
    #[serde(default)]
    processed_dir: Option<String>,
    #[serde(default)]
    pub(crate) journal_dir: Option<String>,
    #[serde(default)]
    pub(crate) notes_dir: Option<String>,
    #[serde(default)]
    pub(crate) template_path: Option<String>,
    #[serde(default)]
    pub(crate) date_format: Option<String>,
    #[serde(default)]
    pub(crate) date_structure_active: Option<bool>,
    #[serde(default)]
    pub(crate) date_structure_side: Option<String>,
    #[serde(default)]
    pub(crate) metadata_panel_side: Option<String>,
    #[serde(default)]
    pub(crate) task_opened_section: Option<String>,
    #[serde(default)]
    pub(crate) task_closed_section: Option<String>,
    /// One heading per line, as the Journal pane's textarea holds them.
    #[serde(default)]
    pub(crate) read_only_sections: Option<String>,
    #[serde(default)]
    pub(crate) wiki_dir: Option<String>,
    #[serde(default)]
    pub(crate) insights_dir: Option<String>,
    #[serde(default)]
    pub(crate) entity_template_path: Option<String>,
    #[serde(default)]
    pub(crate) expected_hash: Option<String>,
}

fn malformed_body() -> Response {
    html_response("<p class=\"hint\">Malformed request.</p>".to_owned())
}

fn method_not_allowed() -> Response {
    html_response("<p class=\"hint\">Method not allowed.</p>".to_owned())
}

pub(crate) async fn write_toml_text(
    client: &McpClient,
    vault_name: &str,
    path: &str,
    content: &str,
    expected_hash: Option<String>,
) -> Result<bool, McpCallError> {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{path}")));
    args.insert("content".to_owned(), Value::String(content.to_owned()));
    if let Some(hash) = expected_hash.filter(|hash| !hash.is_empty()) {
        args.insert("expected_hash".to_owned(), Value::String(hash));
    }
    let result = client.call_tool("fs_write_file".to_owned(), args).await?;
    Ok(result.is_error != Some(true))
}

/// Re-renders the Inbox/Outbox section with `error`, `submitted` (the
/// operator's own just-submitted field values, preserved rather than
/// reverted to disk) standing in for what would otherwise come from a
/// fresh read, and the Tasks section read fresh from disk unchanged.
async fn inbox_outbox_error_response(
    client: &McpClient,
    vault_name: &str,
    submitted: InboxOutboxSectionState,
    error: String,
) -> Response {
    let section = InboxOutboxSectionState {
        error: Some(error),
        ..submitted
    };
    html_response(render_sections(client, vault_name, move |sections| sections.inbox_outbox = section).await)
}

/// Saves the Inbox/Outbox section: validates every field as a single plain path segment
/// (`inbox_dir`/`outbox_dir`/`processed_dir`) or a registered `.contextos/team.toml` agent slug (`curator_agent`)
/// before writing anything. Unlike the Tasks section, there is no "invalid TOML" failure mode here at all: the
/// operator submits four already-validated-shape fields, not free text, so the only rejections are semantic (an
/// empty directory name, an unregistered agent), never a syntax error a form could not have produced in the first
/// place. `contextos-web` never writes `team.toml` itself, so an unregistered curator is the operator's own
/// configuration to fix there, not something this save silently works around.
/// Validates the four submitted fields' own shape (each a single plain path
/// segment): pure, so [`save_inbox_outbox`] itself stays within this
/// workspace's line-count lint without repeating the identical
/// `sanitised_path_segment` match four times.
fn validate_inbox_outbox_fields(
    submitted_agent: &str,
    submitted: &InboxOutboxSectionState,
) -> Result<InboxOutboxConfig, String> {
    let curator_agent = sanitised_path_segment(submitted_agent).map_err(|reason| format!("curator_agent {reason}."))?;
    let inbox_dir =
        sanitised_path_segment(&submitted.inbox_dir_value).map_err(|reason| format!("inbox_dir {reason}."))?;
    let outbox_dir =
        sanitised_path_segment(&submitted.outbox_dir_value).map_err(|reason| format!("outbox_dir {reason}."))?;
    let processed_dir =
        sanitised_path_segment(&submitted.processed_dir_value).map_err(|reason| format!("processed_dir {reason}."))?;
    Ok(InboxOutboxConfig {
        curator_agent: curator_agent.to_owned(),
        inbox_dir: inbox_dir.to_owned(),
        outbox_dir: outbox_dir.to_owned(),
        processed_dir: processed_dir.to_owned(),
    })
}

async fn save_inbox_outbox(client: &McpClient, vault_name: &str, body: &SaveSectionBody) -> Response {
    let expected_hash = body.expected_hash.clone();
    let submitted_agent = body.curator_agent.clone().unwrap_or_default();
    let submitted = InboxOutboxSectionState {
        curator_agent_options: vec![AgentOption {
            value: submitted_agent.clone(),
            label: submitted_agent.clone(),
            selected: true,
        }],
        inbox_dir_value: body.inbox_dir.clone().unwrap_or_default(),
        outbox_dir_value: body.outbox_dir.clone().unwrap_or_default(),
        processed_dir_value: body.processed_dir.clone().unwrap_or_default(),
        expected_hash: expected_hash.clone(),
        error: None,
    };

    let config = match validate_inbox_outbox_fields(&submitted_agent, &submitted) {
        Ok(config) => config,
        Err(reason) => return Box::pin(inbox_outbox_error_response(client, vault_name, submitted, reason)).await,
    };
    if !team::load_team(client, vault_name)
        .await
        .iter()
        .any(|agent| agent.kind == TeamMemberKind::Agent && agent.slug == config.curator_agent)
    {
        let curator_agent = &config.curator_agent;
        return Box::pin(inbox_outbox_error_response(
            client,
            vault_name,
            submitted,
            format!(
                "curator_agent \"{curator_agent}\" is not registered as an agent in .contextos/team.toml. \
                 Add it there first."
            ),
        ))
        .await;
    }

    let Ok(content) = toml::to_string(&config) else {
        return Box::pin(inbox_outbox_error_response(
            client,
            vault_name,
            submitted,
            "This configuration could not be saved.".to_owned(),
        ))
        .await;
    };
    match write_toml_text(client, vault_name, INBOX_OUTBOX_TOML_PATH, &content, expected_hash).await {
        Ok(true) => html_response(Box::pin(render_current(client, vault_name)).await),
        Ok(false) => {
            Box::pin(inbox_outbox_error_response(
                client,
                vault_name,
                submitted,
                "The file could not be saved (it may have changed since you loaded this page).".to_owned(),
            ))
            .await
        }
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => unreachable(),
    }
}

/// `PATCH /{{vault_name}}/settings/`.
pub async fn mutate(
    State(state): State<VaultSettingsRoutesState>,
    Path(vault_name): Path<String>,
    method: Method,
    body: axum::body::Bytes,
) -> Response {
    if method != Method::PATCH && method != Method::PUT {
        return method_not_allowed();
    }
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let Ok(fields) = serde_json::from_slice::<Map<String, Value>>(&body) else {
        return malformed_body();
    };
    // Every section's form names itself; the Tasks section's indexed rows have a shape of their own.
    if fields.get("target").and_then(Value::as_str) == Some("tasks") {
        return Box::pin(vault_settings_tasks::save_tasks(&client, &vault_name, &fields)).await;
    }
    let Ok(parsed) = serde_json::from_value::<SaveSectionBody>(Value::Object(fields)) else {
        return malformed_body();
    };
    match parsed.target.as_deref().unwrap_or_default() {
        "inbox_outbox" => Box::pin(save_inbox_outbox(&client, &vault_name, &parsed)).await,
        "journal" => Box::pin(vault_settings_dates::save_journal(&client, &vault_name, &parsed)).await,
        "notes" => Box::pin(vault_settings_dates::save_notes(&client, &vault_name, &parsed)).await,
        "wiki" => Box::pin(vault_settings_knowledge::save_wiki(&client, &vault_name, &parsed)).await,
        "insights" => Box::pin(vault_settings_knowledge::save_insights(&client, &vault_name, &parsed)).await,
        "entity_docs" => {
            Box::pin(vault_settings_knowledge::save_entity_docs(
                &client,
                &vault_name,
                &parsed,
            ))
            .await
        }
        "profile" => Box::pin(vault_settings_profile::save_profile(&client, &vault_name, &parsed)).await,
        _ => malformed_body(),
    }
}
