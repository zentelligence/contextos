//! HTTP route handlers grouped by concern:
//! `vault` (vault content rendering), `apps` (registered app serving), and
//! `settings` (the `/settings/` configuration UI).

use std::path::PathBuf;
use std::sync::Arc;

use axum::http::HeaderMap;
use serde::Deserialize;

use crate::mcp_client::{McpClient, McpClientSet};

pub mod agents;
pub mod alerts;
pub mod apps;
pub mod entity_docs;
pub mod entity_view;
pub mod events_stream;
pub mod file_response;
pub mod inbox_outbox;
pub mod inbox_outbox_upload;
pub mod insights;
pub mod insights_create;
pub mod insights_delete;
pub mod insights_save;
pub mod journal;
pub mod journal_view;
pub mod notes;
pub mod notes_view;
mod overview;
pub mod overview_panels;
pub mod packages;
pub mod profile;
pub mod search;
pub mod settings;
pub mod settings_connections;
pub mod settings_credentials;
mod settings_dependents;
pub mod settings_oauth;
pub mod settings_secrets;
pub mod shared_packages;
pub mod shared_workflow_jobs;
pub mod shared_workflows;
pub mod shell;
pub mod signals;
pub mod successor_api;
pub mod tasks;
pub mod tasks_create;
pub mod tasks_detail;
mod tasks_detail_fields;
mod tasks_fields;
mod tasks_focus;
mod tasks_key_prefixes;
pub mod tasks_links;
mod tasks_query;
mod tasks_rows;
mod tasks_strip;
pub mod vault;
pub mod vault_health;
pub mod vault_management;
mod vault_management_activity;
mod vault_management_library;
mod vault_management_paths;
mod vault_management_register_create;
mod vault_management_registers;
pub mod vault_mutations;
pub mod vault_packages_api;
pub mod vault_packages_page;
pub mod vault_settings;
pub mod vault_settings_dates;
pub mod vault_settings_knowledge;
pub mod vault_settings_profile;
mod vault_settings_tasks;
pub mod wiki;
pub mod wiki_create;
pub mod wiki_delete;
pub mod wiki_save;
pub mod wiki_view;
pub mod workflow_jobs;
pub mod workflow_views;
pub mod workflows;

/// The sidebar wide/narrow toggle's own cookie name
/// ([`routes::shell::toggle_sidebar`](shell::toggle_sidebar) writes it,
/// every full-page route reads it here).
pub const SIDEBAR_COOKIE: &str = "cc_sidebar";

/// The primary server's open session for a page that needs it: `502` `mcp/unreachable` while the server is configured
/// but not connected (it failed, or a reconnect is under way), `500` `mcp/server-not-configured` while no server is
/// configured at all.
pub(crate) fn primary_session(clients: &McpClientSet) -> Result<Arc<McpClient>, Box<axum::response::Response>> {
    clients.primary().ok_or_else(|| {
        Box::new(if clients.primary_name().is_some() {
            vault::unreachable()
        } else {
            vault::server_not_configured()
        })
    })
}

/// Reads the sidebar wide/narrow toggle's own state from `headers`' `Cookie`
/// header: `true` (wide) whenever the cookie is absent or carries any value
/// other than `"narrow"`, so a first-ever visit (no cookie set yet) renders
/// wide, matching the design mock's own initial state.
#[must_use]
pub fn sidebar_wide_from_headers(headers: &HeaderMap) -> bool {
    let Some(raw) = headers
        .get(axum::http::header::COOKIE)
        .and_then(|value| value.to_str().ok())
    else {
        return true;
    };
    !raw.split(';').any(|pair| {
        let mut parts = pair.splitn(2, '=');
        let name = parts.next().unwrap_or("").trim();
        let value = parts.next().unwrap_or("").trim();
        name == SIDEBAR_COOKIE && value == "narrow"
    })
}

/// The Command Centre entity switcher's own selection, as a query-string
/// extractor shared by every vault-scoped full-page route that is not
/// `vault`'s own [`vault::VaultQuery`] (which carries this field alongside
/// `raw`/`view`): absent means "All entities", the always-available
/// default.
#[derive(Debug, Default, Deserialize)]
pub struct EntityQuery {
    #[serde(default)]
    pub entity: Option<String>,
}

/// The state every "simple-shape" sub-router shares (`vault`, `agents`, `tasks`,
/// `vault_settings`, `inbox_outbox`, `journal`, `notes` - every feature
/// whose own state carries nothing beyond this and issues every tool call
/// against the primary server, read from `clients` per request). `app::build_router` builds one instance per
/// call and converts it into each feature's own distinctly-typed state
/// through `From`, rather than repeating the identical three-field literal
/// at every call site - a repetition that would otherwise keep growing
/// `build_router` itself by a fixed handful of lines per feature added,
/// with nothing about that growth being genuine complexity.
#[derive(Clone)]
pub struct CommonRoutesState {
    pub clients: Arc<McpClientSet>,
    pub web_config_path: Arc<PathBuf>,
    /// Where a screen reads "now" when it decides a date for something new.
    pub clock: Arc<dyn contextos_workflows::Clock>,
    /// `clock` at the operator's local offset, in the zone the contextos server reports: every date a screen writes
    /// into a vault reads this, never `clock` directly.
    pub vault_time: Arc<crate::vault_time::VaultTime>,
}

/// What [`CommonRoutesState`] is built from: everything but the operator's local time, which it derives.
pub struct CommonRoutesInput {
    pub clients: Arc<McpClientSet>,
    pub web_config_path: Arc<PathBuf>,
    pub clock: Arc<dyn contextos_workflows::Clock>,
}

impl From<CommonRoutesInput> for CommonRoutesState {
    fn from(input: CommonRoutesInput) -> Self {
        let vault_time = Arc::new(crate::vault_time::VaultTime::from(crate::vault_time::VaultTimeInput {
            clock: Arc::clone(&input.clock),
            clients: Arc::clone(&input.clients),
        }));
        Self {
            clients: input.clients,
            web_config_path: input.web_config_path,
            clock: input.clock,
            vault_time,
        }
    }
}
