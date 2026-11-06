//! `/settings/`: a `web.toml`-scoped configuration
//! UI. `GET` renders the current effective configuration; `POST` adds an
//! `[[mcp_server]]` entry; `PATCH` partially updates an entry or
//! `[server.ui]`; `PUT` fully replaces an entry; `DELETE
//! /settings/mcp-servers/{name}` removes one, the `[[mcp_server]]` name
//! carried as the URL path segment `docs/http-routing-response-contract.md`
//! requires for an identified sub-entity, rather than a JSON body field (a
//! body htmx's own `methodsThatUseUrlParams` default never actually sends
//! for a `DELETE`, which made the previous single-`/settings/`-endpoint
//! `DELETE` a silent no-op from the real browser UI: the request left with
//! an empty body, `web.toml` untouched).
//! Every writing method validates the resulting document
//! ([`crate::config_writer::WebConfigDocument`]) and checks that no
//! currently-registered app's manifest still names a server the edit would
//! remove, before persisting anything; a rejected edit leaves `web.toml`
//! byte-for-byte unchanged. No method here ever opens `config.toml`: the
//! vault list it holds is exclusively a `contextos-mcp` CLI or hand-edit
//! concern.
//!
//! Once a write does succeed, [`apply_edit_and_persist`] additionally garbage-collects any
//! `credential_ref`/`client_secret_ref` that no longer names anything in the resulting
//! `[[mcp_server]]` array at all (a `DELETE`, or a `PUT` that replaces an entry with a different
//! reference or drops `auth` entirely). This uplift's own credential store lives entirely under a
//! `credentials/` directory sibling to `web.toml`; the reaffirmation above still holds precisely
//! because of this: neither this garbage collection nor anything else this module or
//! `routes::settings_oauth` does ever reads or writes `config.toml`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use askama::Template;
use axum::Json;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use time::format_description::well_known::Rfc3339;

use crate::atomic_write::write_atomically;
use crate::config::{
    DEFAULT_SYSTEM_NAME, McpServerAuth, McpServerConfig, WebConfig, WebServerConfig, current_appearance,
};
use crate::config_writer::{WebConfigDocument, WebConfigWriterError};
use crate::credential_store::{CredentialRecord, FsCredentialStore};
use crate::logging::{Clock, SystemClock};
use crate::mcp_client::{McpCallError, McpClient, McpClientSet};
use crate::mcp_readiness::ServerReadiness;
use crate::oauth::PendingOauthConnections;
use crate::rendering::page;
use crate::rendering::shell::{self, HostScreen};
use crate::routes::settings_connections::{ConnectionView, ServerSegment};
use crate::routes::settings_credentials::{
    DetectedAuth, McpServerAuthView, SettingsQuery, credential_references_in, credential_snapshot,
    extract_pasted_secrets_from_patch, garbage_collect_credentials, mcp_server_auth_view, persist_pasted_secrets,
    settings_banner,
};
use crate::routes::settings_dependents::{DependencyCheckError, dependent_apps};
use crate::routes::settings_secrets::Revealed;

/// Shared state the settings route needs: the connected MCP sessions (to
/// run the registered-app-dependency check required before removing an
/// `[[mcp_server]]` entry), the `[[mcp_server]]` name every such check is
/// issued against, and the on-disk path of the `web.toml` this route reads
/// and writes.
///
/// `credential_store`/`pending_oauth` back the `OAuth2` connect/callback/disconnect routes
/// (`routes::settings_oauth`), a sibling module over this identical state type rather than a
/// second, separately wired one: the credential store is rooted at a `credentials/` directory
/// sibling to `web_config_path`, derived here rather than taken as a
/// separate constructor parameter, and `pending_oauth` starts empty on every process start (an
/// in-flight connect does not survive a restart, which is fine: this is
/// single-operator, loopback-only, in-memory state).
#[derive(Clone)]
pub struct SettingsRoutesState {
    clients: Arc<McpClientSet>,
    web_config_path: Arc<PathBuf>,
    credential_store: Arc<FsCredentialStore>,
    pending_oauth: PendingOauthConnections,
    /// Held while `web.toml` is read and applied to the sessions, so two applies never interleave and the last one
    /// always reflects the file as it now is.
    connections_sync: Arc<tokio::sync::Mutex<()>>,
}

impl SettingsRoutesState {
    #[must_use]
    pub fn new(clients: Arc<McpClientSet>, web_config_path: PathBuf) -> Self {
        let credentials_dir = crate::credential_store::credentials_directory(&web_config_path);
        Self {
            clients,
            web_config_path: Arc::new(web_config_path),
            credential_store: Arc::new(FsCredentialStore::new(credentials_dir)),
            pending_oauth: PendingOauthConnections::new(),
            connections_sync: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    fn client(&self) -> Option<Arc<McpClient>> {
        self.clients.primary()
    }

    /// The connected MCP sessions, for
    /// [`routes::settings_oauth::disconnect`](crate::routes::settings_oauth::disconnect)'s
    /// post-disconnect status probe.
    pub(crate) fn clients(&self) -> &Arc<McpClientSet> {
        &self.clients
    }

    /// `web.toml`'s own path, for a sibling route module to read the current configuration the
    /// same way this module does.
    pub(crate) fn web_config_path(&self) -> Arc<PathBuf> {
        Arc::clone(&self.web_config_path)
    }

    /// The credential store backing the `OAuth2` connect/callback/disconnect routes.
    pub(crate) fn credential_store(&self) -> &Arc<FsCredentialStore> {
        &self.credential_store
    }

    /// Serialises applying `web.toml` to the sessions ([`crate::routes::settings_connections::sync_connections`]).
    pub(crate) fn connections_sync(&self) -> &tokio::sync::Mutex<()> {
        &self.connections_sync
    }

    /// The in-flight `OAuth2` connect attempts awaiting their callback.
    pub(crate) fn pending_oauth(&self) -> &PendingOauthConnections {
        &self.pending_oauth
    }
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    server: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    used_by: Vec<AppReference>,
}

#[derive(Serialize)]
struct AppReference {
    vault: String,
    slug: String,
}

fn error(status: StatusCode, code: &'static str) -> Response {
    (
        status,
        Json(ErrorBody {
            error: code,
            detail: None,
            server: None,
            used_by: Vec::new(),
        }),
    )
        .into_response()
}

fn malformed_body() -> Response {
    error(StatusCode::BAD_REQUEST, "route/malformed-body")
}

fn method_not_allowed() -> Response {
    error(StatusCode::METHOD_NOT_ALLOWED, "route/method-not-allowed")
}

fn invalid_configuration(detail: impl std::fmt::Display) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(ErrorBody {
            error: "settings/invalid-configuration",
            detail: Some(detail.to_string()),
            server: None,
            used_by: Vec::new(),
        }),
    )
        .into_response()
}

fn mcp_server_in_use(server: &str, used_by: Vec<AppReference>) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(ErrorBody {
            error: "settings/mcp-server-in-use",
            detail: Some(format!(
                "mcp_server {server:?} cannot be removed: at least one registered app's \
                 manifest still names it"
            )),
            server: Some(server.to_owned()),
            used_by,
        }),
    )
        .into_response()
}

fn unreachable() -> Response {
    error(StatusCode::BAD_GATEWAY, "mcp/unreachable")
}

/// The refusal when the primary server's vault list could not be read, so which vaults depend on a server is not known.
/// It is a bad gateway like [`unreachable`], under its own code, and carries the reason the check could not be made.
fn vault_list_unreadable(detail: String) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(ErrorBody {
            error: "mcp/vault-list-unreadable",
            detail: Some(detail),
            server: None,
            used_by: Vec::new(),
        }),
    )
        .into_response()
}

fn read_failure(source: &std::io::Error) -> Response {
    invalid_configuration(format!("web.toml could not be read: {source}"))
}

// ---------------------------------------------------------------------
// GET /settings/
// ---------------------------------------------------------------------

/// One `[[mcp_server]]` entry as the settings page renders it: `detail` is
/// the read-only summary line; the remaining fields are structured so the
/// per-entry edit form (`PATCH ... target: "mcp_server"`) can pre-fill
/// exactly the fields its own transport has, `token_env` included (the
/// "auth for an MCP server" case, distinct from `contextos-web`'s own,
/// deliberately deferred, HTTP surface auth).
struct McpServerRow {
    name: String,
    /// [`crate::inbox_outbox::encode_query_value`]'s percent-encoding of
    /// `name`, for safe embedding as the `{name}` path segment of this
    /// row's own `DELETE /settings/mcp-servers/{name}` link.
    name_encoded: String,
    transport: &'static str,
    detail: String,
    command: Option<String>,
    args: Option<String>,
    endpoint: Option<String>,
    /// The entry's connection panel: readiness, cause, remedy, and Reconnect.
    connection: ConnectionView,
    /// The `[mcp_server.auth]`/`[mcp_server.headers]` view. Always present,
    /// with meaningless defaults never rendered, on a stdio entry too: this field is never itself
    /// `Option`, matching this struct's own sibling fields' (`command`/`args`/`endpoint`) "always
    /// present, blank when not applicable" convention, rather than forcing the template to bind an
    /// owned value out of a shared reference with an `if let`.
    auth: McpServerAuthView,
}

/// `entry`'s connection panel from `readiness`. An entry missing from it was added to `web.toml` by hand since the set
/// last read it; Reconnect reads `web.toml` again and connects it.
fn connection_view(name: &str, readiness: &[ServerReadiness]) -> ConnectionView {
    readiness
        .iter()
        .find(|server| server.name == name)
        .map_or_else(|| ConnectionView::unknown_to_set(name), ConnectionView::from)
}

#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsTemplate {
    bind: String,
    static_dir: String,
    log_level: String,
    log_file: String,
    log_rotation: String,
    log_retention: String,
    ui: Vec<(String, String)>,
    system_name: String,
    theme_options: Vec<AppearanceOption>,
    size_options: Vec<AppearanceOption>,
    font_options: Vec<AppearanceOption>,
    mcp_servers: Vec<McpServerRow>,
    /// A transient, query-parameter-driven success notice (`?oauth_connected={server}`), never
    /// persisted: absent on every render this query parameter did not itself drive.
    notice: Option<String>,
    /// The same notice's failure counterpart (`?oauth_error={reason}&server={server}`).
    error: Option<String>,
}

/// One selectable choice on an Appearance picker (Theme, Font size, or
/// Font): `value` is the exact `[server.ui]` string this option's control
/// submits, `label` is its operator-facing text (also the string the
/// Font-size/Font pickers render live, at the candidate size or in the
/// candidate typeface, as their own preview), `selected` marks the option
/// matching the axis's currently effective value.
struct AppearanceOption {
    value: &'static str,
    label: &'static str,
    selected: bool,
}

/// Resolves one Appearance axis's picker options: `current` is the raw
/// `[server.ui]` string value for that axis, if any; an unrecognised or
/// absent value both resolve to `default`, matching the stylesheet's own
/// "no matching rule leaves the axis at its default" fallback. `default`
/// is independent of `choices`'s own display order (`size`'s options read
/// small-to-large for display, `"normal"`, not the first entry, is the
/// real default).
fn appearance_options(
    current: Option<&str>,
    choices: &[(&'static str, &'static str)],
    default: &'static str,
) -> Vec<AppearanceOption> {
    let effective = choices
        .iter()
        .find(|(value, _)| Some(*value) == current)
        .map_or(default, |(value, _)| *value);
    choices
        .iter()
        .map(|&(value, label)| AppearanceOption {
            value,
            label,
            selected: value == effective,
        })
        .collect()
}

/// A human-readable summary of `server`'s rotation triggers, for the
/// `/settings/` "Server & bind" pane: `"Never"` when neither
/// `log_max_size_mb` nor `log_rotate_daily` is set, otherwise whichever of
/// "every N MB" and "daily" apply, joined with "and" when both do.
fn log_rotation_display(server: &WebServerConfig) -> String {
    let mut triggers = Vec::new();
    if let Some(megabytes) = server.log_max_size_mb {
        triggers.push(format!("every {megabytes} MB"));
    }
    if server.log_rotate_daily {
        triggers.push("daily".to_owned());
    }
    if triggers.is_empty() {
        "Never".to_owned()
    } else {
        triggers.join(" and ")
    }
}

/// A human-readable summary of `server`'s retention bounds, for the
/// `/settings/` "Server & bind" pane: `"Kept indefinitely"` when neither
/// `log_retention_days` nor `log_retention_files` is set, otherwise
/// whichever of "N days" and "N files" apply, joined with ", " when both
/// do.
fn log_retention_display(server: &WebServerConfig) -> String {
    let mut bounds = Vec::new();
    if let Some(days) = server.log_retention_days {
        bounds.push(format!("{days} days"));
    }
    if let Some(files) = server.log_retention_files {
        bounds.push(format!("{files} files"));
    }
    if bounds.is_empty() {
        "Kept indefinitely".to_owned()
    } else {
        bounds.join(", ")
    }
}

/// `pub(crate)`: `routes::settings_oauth::disconnect` re-renders the identical fragment after
/// clearing a stored token, rather than duplicating this rendering logic. `credentials` is
/// [`credential_snapshot`]'s own gathered map; `notice`/`error` are the transient banner text this
/// call's own caller resolved from `?oauth_connected=`/`?oauth_error=`, or `None` for every render
/// no such query parameter drove.
/// Builds one [`McpServerRow`] for `render`'s own `mcp_servers` list: split out purely to keep
/// `render` itself under this repository's `clippy::too_many_lines` bound, not because this piece
/// is independently reusable.
fn mcp_server_row(
    entry: &McpServerConfig,
    readiness: &[ServerReadiness],
    credentials: &HashMap<String, CredentialRecord>,
    detected: Option<&DetectedAuth<'_>>,
) -> McpServerRow {
    let connection = connection_view(entry.name(), readiness);
    match entry {
        McpServerConfig::Stdio { name, command, args } => McpServerRow {
            name: name.clone(),
            name_encoded: ServerSegment::from(name.as_str()).to_string(),
            transport: "stdio",
            detail: format!("{command} {}", args.join(" ")),
            command: Some(command.clone()),
            args: Some(args.join(" ")),
            endpoint: None,
            connection: connection.clone(),
            auth: mcp_server_auth_view(
                name,
                None,
                None,
                &std::collections::BTreeMap::new(),
                credentials,
                detected,
            ),
        },
        McpServerConfig::Http {
            name,
            endpoint,
            token_env,
            auth,
            headers,
        } => McpServerRow {
            name: name.clone(),
            name_encoded: ServerSegment::from(name.as_str()).to_string(),
            transport: "http",
            detail: match (auth, token_env) {
                (
                    Some(McpServerAuth::Bearer {
                        token_env: Some(variable),
                        ..
                    }),
                    _,
                )
                | (None, Some(variable)) => {
                    format!("{endpoint} (token via ${variable})")
                }
                (Some(McpServerAuth::Bearer { .. }), _) => format!("{endpoint} (pasted token)"),
                _ => endpoint.clone(),
            },
            command: None,
            args: None,
            endpoint: Some(endpoint.clone()),
            connection: connection.clone(),
            auth: mcp_server_auth_view(
                name,
                auth.as_ref(),
                token_env.as_deref(),
                headers,
                credentials,
                detected,
            ),
        },
    }
}

pub(crate) fn render(
    config: &WebConfig,
    readiness: &[ServerReadiness],
    credentials: &HashMap<String, CredentialRecord>,
    notice: Option<String>,
    error: Option<String>,
    detected: Option<&DetectedAuth<'_>>,
) -> String {
    let mcp_servers = config
        .mcp_servers
        .iter()
        .map(|entry| mcp_server_row(entry, readiness, credentials, detected))
        .collect();
    // `theme`/`font`/`size` each get a dedicated picker, `system_name` its
    // own dedicated field (both below); every other `[server.ui]` key
    // (`logo`, and any other unenumerated key an operator has set) keeps the
    // free-form editor, so that capability is never silently lost.
    let ui = config
        .server
        .ui
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), "theme" | "font" | "size" | "system_name"))
        .map(|(key, value)| (key.clone(), ui_value_display(value)))
        .collect();
    let string_value = |key: &str| config.server.ui.get(key).and_then(toml::Value::as_str);
    let system_name = string_value("system_name").map_or_else(|| DEFAULT_SYSTEM_NAME.to_owned(), ToOwned::to_owned);
    let theme_options = appearance_options(
        string_value("theme"),
        &[("blueprint", "Blueprint"), ("dark", "Dark"), ("light", "Light")],
        "blueprint",
    );
    let size_options = appearance_options(
        string_value("size"),
        &[("small", "Smaller"), ("normal", "Normal"), ("large", "Larger")],
        "normal",
    );
    let font_options = appearance_options(
        string_value("font"),
        &[("sans", "Sans"), ("serif", "Serif"), ("monospace", "Monospace")],
        "sans",
    );
    let template = SettingsTemplate {
        bind: config.server.bind.clone(),
        static_dir: config.server.static_dir.as_ref().map_or_else(
            || "(none configured; bundled assets serve /static/)".to_owned(),
            |path| path.display().to_string(),
        ),
        log_level: format!("{:?}", config.server.log_level).to_ascii_lowercase(),
        log_file: config.server.log_file.clone(),
        log_rotation: log_rotation_display(&config.server),
        log_retention: log_retention_display(&config.server),
        ui,
        system_name,
        theme_options,
        size_options,
        font_options,
        mcp_servers,
        notice,
        error,
    };
    template
        .render()
        .unwrap_or_else(|_| "<p>Settings could not be rendered.</p>".to_owned())
}

/// A `[server.ui]` value as plain text for the appearance form's editable
/// inputs: a bare string's own content (an operator editing "dark" should
/// see `dark`, not the TOML-syntax `"dark"` a raw `Display` would produce),
/// falling back to `toml::Value`'s own TOML-syntax rendering for every
/// other value kind (`[server.ui]`'s key set is intentionally unenumerated,
/// `config.rs`, so a non-string value is possible and must still display
/// as something re-parseable).
fn ui_value_display(value: &toml::Value) -> String {
    match value {
        toml::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// `pub(crate)`: `routes::settings_oauth::disconnect` returns the identical content type this
/// module's own successful writes do.
pub(crate) fn html_response(html: String) -> Response {
    (StatusCode::OK, [("content-type", "text/html; charset=utf-8")], html).into_response()
}

/// Adds htmx's `HX-Refresh: true` response header, which tells the client
/// to discard this response body and do a full browser reload instead of
/// swapping it in. Used only when a save changed one of
/// [`CHROME_AFFECTING_UI_KEYS`]: those five keys render inside
/// `page_shell.html` (the `<html data-theme>` attribute, the header title,
/// the header logo), all outside the `.settings` element the Appearance
/// pane's own `hx-swap="outerHTML"` targets, so swapping that fragment
/// alone leaves the new skin, title, or logo invisible until the next
/// unrelated navigation reloads the shell around it.
fn html_response_with_refresh(html: String) -> Response {
    (
        StatusCode::OK,
        [("content-type", "text/html; charset=utf-8"), ("HX-Refresh", "true")],
        html,
    )
        .into_response()
}

/// The `[server.ui]` keys `page_shell.html` renders outside `.settings`
/// (`Appearance`'s own field list, `config.rs`): a save that touches any of
/// these needs [`html_response_with_refresh`] rather than a bare fragment
/// swap. Every other `[server.ui]` key is "persisted but inert" (renders
/// nowhere yet), so a save that only touches those needs no refresh.
const CHROME_AFFECTING_UI_KEYS: [&str; 5] = ["theme", "font", "size", "system_name", "logo"];

/// Whether `edit` changed a `[server.ui]` key the nav shell renders outside
/// `.settings`, per [`CHROME_AFFECTING_UI_KEYS`].
fn touches_shell_chrome(edit: &Edit) -> bool {
    match edit {
        Edit::PatchUi(patch) => patch.keys().any(|key| CHROME_AFFECTING_UI_KEYS.contains(&key.as_str())),
        Edit::AddMcpServer(_)
        | Edit::PatchMcpServer { .. }
        | Edit::ReplaceMcpServer { .. }
        | Edit::RemoveMcpServer(_) => false,
    }
}

/// `GET /settings/`: renders the current effective `web.toml`. Follows
/// `docs/http-routing-response-contract.md`'s `HX-Request`
/// convention: a plain browser navigation (no
/// `HX-Request` header) gets the full page, chrome and all; an
/// HTMX-driven request gets the bare fragment it will swap in. The `?oauth_connected=`/
/// `?oauth_error=` banner (`settings_banner`) renders on both branches, for the same reason: a
/// real browser redirect from `routes::settings_oauth::callback` never carries an `HX-Request`
/// header, but rendering the banner unconditionally keeps the two branches' own notice-handling
/// identical rather than one silently dropping it.
pub async fn get(
    State(state): State<SettingsRoutesState>,
    headers: HeaderMap,
    Query(query): Query<SettingsQuery>,
) -> Response {
    let (notice, error) = settings_banner(&query);
    let is_hx_request = headers.contains_key("hx-request");
    if is_hx_request {
        let (config, readiness) = tokio::join!(
            read_current_blocking(Arc::clone(&state.web_config_path)),
            state.clients.readiness(),
        );
        return match config {
            Ok(config) => {
                let credentials = credential_snapshot(state.credential_store(), &config).await;
                html_response(render(&config, &readiness, &credentials, notice, error, None))
            }
            Err(response) => *response,
        };
    }
    // `web.toml`'s own read (for the page itself), the nav shell, the
    // appearance read, and the live MCP server status probes (the
    // per-server detail this page's own pane needs, and the footer's own
    // aggregate) are five independent operations: none depends on another's
    // result, so all five run concurrently rather than paying their costs
    // (two of them blocking filesystem reads, moved onto `spawn_blocking`
    // rather than the async executor thread) one after another.
    let appearance_path = Arc::clone(&state.web_config_path);
    let (config, mut nav, appearance, readiness, mcp_status) = tokio::join!(
        read_current_blocking(Arc::clone(&state.web_config_path)),
        Box::pin(settings_nav(&state, &headers)),
        async move {
            tokio::task::spawn_blocking(move || current_appearance(&appearance_path))
                .await
                .unwrap_or_default()
        },
        state.clients.readiness(),
        crate::rendering::shell::mcp_status(&state.clients),
    );
    match config {
        Ok(config) => {
            let credentials = credential_snapshot(state.credential_store(), &config).await;
            let fragment = render(&config, &readiness, &credentials, notice, error, None);
            nav.appearance = appearance;
            nav.sidebar_wide = crate::routes::sidebar_wide_from_headers(&headers);
            nav.mcp_status = mcp_status;
            html_response(page::render_page(&nav, "Settings", &fragment))
        }
        Err(response) => *response,
    }
}

/// Runs [`read_current`] (a blocking filesystem read and TOML parse) on
/// `spawn_blocking` rather than the async executor thread. The `JoinError`
/// arm is unreachable in practice, since [`read_current`] never panics, but
/// still must be handled rather than unwrapped.
///
/// `pub(crate)`: `routes::settings_oauth` reuses this rather than a second `web.toml` reader.
pub(crate) async fn read_current_blocking(path: Arc<PathBuf>) -> Result<WebConfig, Box<Response>> {
    match tokio::task::spawn_blocking(move || read_current(&path)).await {
        Ok(result) => result,
        Err(_join_error) => Err(Box::new(error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "settings/blocking-task-failed",
        ))),
    }
}

/// Builds the nav shell's data for `/settings/`: vault-independent, with the sidebar of the active vault, degrading to an
/// empty vault switcher and sidebar rather than failing the whole page when the primary MCP session is unavailable,
/// since neither is something the settings page itself depends on.
async fn settings_nav(state: &SettingsRoutesState, headers: &HeaderMap) -> page::NavData {
    match state.client() {
        Some(client) => {
            let active_vault = crate::active_vault::read_from_headers(headers);
            let server_names = state.clients.names();
            shell::build_host_nav(&client, HostScreen::Settings, active_vault.as_deref(), &server_names).await
        }
        None => page::NavData {
            vaults: Vec::new(),
            current_vault: None,
            nav_target_vault: None,
            breadcrumb: vec![page::BreadcrumbSegment {
                label: "settings".to_owned(),
                href: None,
            }],
            active_vault_screen: false,
            active_agents_screen: false,
            active_apps_screen: false,
            active_jobs_screen: false,
            active_settings_screen: true,
            current_vault_entity: None,
            entities: Vec::new(),
            current_entity: None,
            current_entity_accent: None,
            entity_base_path: None,
            appearance: crate::config::Appearance::default(),
            sidebar_groups: Vec::new(),
            host_view: None,
            sidebar_wide: true,
            mcp_status: page::McpStatus::default(),
            note_href: None,
            profile_href: None,
            vault_settings_href: None,
            page_title: None,
            events_stream_href: None,
            favourites: None,
        },
    }
}

fn read_current(path: &Path) -> Result<WebConfig, Box<Response>> {
    let source = std::fs::read_to_string(path).map_err(|source| Box::new(read_failure(&source)))?;
    let config: WebConfig = toml::from_str(&source)
        .map_err(|source| Box::new(invalid_configuration(format!("web.toml is invalid: {source}"))))?;
    Ok(config)
}

// ---------------------------------------------------------------------
// POST / PATCH / PUT / DELETE /settings/
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(tag = "target", rename_all = "snake_case", deny_unknown_fields)]
enum PatchBody {
    McpServer { name: String, patch: Map<String, Value> },
    Ui { patch: Map<String, Value> },
}

enum Edit {
    AddMcpServer(McpServerConfig),
    PatchMcpServer {
        name: String,
        patch: Map<String, Value>,
    },
    PatchUi(Map<String, Value>),
    ReplaceMcpServer {
        current_name: String,
        entry: McpServerConfig,
    },
    RemoveMcpServer(String),
}

/// A parsed edit, and the pasted secrets stripped from it on their way to the credential store.
type ParsedEdit = (Edit, Vec<(String, CredentialRecord)>);

fn parse_edit(method: &Method, body: &[u8], now: &str) -> Result<ParsedEdit, Box<Response>> {
    match *method {
        Method::POST => {
            // A pasted token comes out of the submitted entry before it is read as one, since web.toml has no field
            // for a secret value.
            let mut map: Map<String, Value> =
                serde_json::from_slice(body).map_err(|_source| Box::new(malformed_body()))?;
            let secrets = extract_pasted_secrets_from_patch(&mut map, now)
                .map_err(|refusal| Box::new(refusal.into_response()))?;
            let entry: McpServerConfig =
                serde_json::from_value(Value::Object(map)).map_err(|_source| Box::new(malformed_body()))?;
            Ok((Edit::AddMcpServer(entry), secrets))
        }
        Method::PATCH => {
            let parsed: PatchBody = serde_json::from_slice(body).map_err(|_source| Box::new(malformed_body()))?;
            Ok(match parsed {
                PatchBody::McpServer { name, mut patch } => {
                    let secrets = extract_pasted_secrets_from_patch(&mut patch, now)
                        .map_err(|refusal| Box::new(refusal.into_response()))?;
                    (Edit::PatchMcpServer { name, patch }, secrets)
                }
                PatchBody::Ui { patch } => (Edit::PatchUi(patch), Vec::new()),
            })
        }
        Method::PUT => {
            let mut map: Map<String, Value> =
                serde_json::from_slice(body).map_err(|_source| Box::new(malformed_body()))?;
            let Some(Value::String(current_name)) = map.remove("current_name") else {
                return Err(Box::new(malformed_body()));
            };
            let entry: McpServerConfig =
                serde_json::from_value(Value::Object(map)).map_err(|_source| Box::new(malformed_body()))?;
            Ok((Edit::ReplaceMcpServer { current_name, entry }, Vec::new()))
        }
        _ => Err(Box::new(method_not_allowed())),
    }
}

/// `McpServerConfig` round-trips through JSON as an object (an internally
/// tagged enum over string/array fields only): the only failure mode left
/// once a value of this type exists is a bug in this conversion itself, not
/// anything a caller's request body could trigger.
fn json_object(entry: &McpServerConfig) -> Map<String, Value> {
    match serde_json::to_value(entry) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

enum ApplyOutcome {
    Ok,
    UnknownMcpServer(String),
    Invalid(WebConfigWriterError),
}

fn apply_edit(document: &mut WebConfigDocument, edit: &Edit) -> ApplyOutcome {
    let result = match edit {
        Edit::AddMcpServer(entry) => document.add_mcp_server(&json_object(entry)),
        Edit::PatchMcpServer { name, patch } => document.patch_mcp_server(name, patch),
        Edit::PatchUi(patch) => document.patch_ui(patch),
        Edit::ReplaceMcpServer { current_name, entry } => {
            document.replace_mcp_server(current_name, &json_object(entry))
        }
        Edit::RemoveMcpServer(name) => document.remove_mcp_server(name),
    };
    match result {
        Ok(()) => ApplyOutcome::Ok,
        Err(WebConfigWriterError::UnknownMcpServerName { name }) => ApplyOutcome::UnknownMcpServer(name),
        Err(other) => ApplyOutcome::Invalid(other),
    }
}

/// `POST`/`PATCH`/`PUT /settings/`.
pub async fn mutate(State(state): State<SettingsRoutesState>, method: Method, body: axum::body::Bytes) -> Response {
    let now = SystemClock.now().format(&Rfc3339).unwrap_or_default();
    let (edit, pending_secret_writes) = match parse_edit(&method, &body, &now) {
        Ok(parsed) => parsed,
        Err(response) => return *response,
    };
    apply_edit_and_persist(&state, edit, pending_secret_writes).await
}

/// `DELETE /settings/mcp-servers/{name}`: `name` is the URL path segment
/// itself (`docs/http-routing-response-contract.md`'s sub-entity form),
/// never a request body field. htmx's own default
/// `methodsThatUseUrlParams` list includes `delete`, so any `hx-vals` on a
/// `hx-delete` button lands in the query string, not the body; a body-only
/// `DeleteBody` here would silently see an empty body on every real browser
/// request and reject it, which is exactly the no-op this route replaces.
pub async fn delete_mcp_server(State(state): State<SettingsRoutesState>, AxumPath(name): AxumPath<String>) -> Response {
    apply_edit_and_persist(&state, Edit::RemoveMcpServer(name), Vec::new()).await
}

/// Applies `edit` to `web.toml`, then writes `pending_secret_writes` to the credential store. A pasted secret value
/// never reaches `WebConfigDocument`'s JSON-to-TOML bridge, which has no schema notion of it: [`parse_edit`] has
/// already stripped it out, and it is written only once `web.toml`'s own write has succeeded.
async fn apply_edit_and_persist(
    state: &SettingsRoutesState,
    edit: Edit,
    pending_secret_writes: Vec<(String, CredentialRecord)>,
) -> Response {
    let mut document = match load_document(state).await {
        Ok(document) => document,
        Err(response) => return *response,
    };
    let before_names = document.mcp_server_names();
    let before_credential_refs = credential_references_in(&document.mcp_server_entries());

    match apply_edit(&mut document, &edit) {
        ApplyOutcome::Ok => {}
        ApplyOutcome::UnknownMcpServer(name) => return unknown_mcp_server(&name),
        ApplyOutcome::Invalid(error) => return invalid_configuration(error),
    }

    let after_names = document.mcp_server_names();
    let after_credential_refs = credential_references_in(&document.mcp_server_entries());
    let removed: Vec<String> = before_names
        .iter()
        .filter(|name| !after_names.contains(name))
        .cloned()
        .collect();
    if let Some(refusal) = refuse_removal_in_use(state, &before_names, &removed).await {
        return refusal;
    }
    if let Some(failure) = write_document(state, &document).await {
        return failure;
    }

    // Only now, after `web.toml`'s own write has already succeeded, is it safe to touch the
    // credential store at all: an edit this function rejected earlier (an unknown name, an
    // invalid resulting document, an in-use dependency) must never have written or deleted
    // anything there, and it never did, since every return above this point happens before a byte
    // of `web.toml` changes.
    let pasted_secret = !pending_secret_writes.is_empty();
    let unstored_secret = persist_pasted_secrets(state.credential_store(), pending_secret_writes).await;
    let removed_credential_refs: Vec<String> = before_credential_refs
        .difference(&after_credential_refs)
        .cloned()
        .collect();
    garbage_collect_credentials(state.credential_store(), removed_credential_refs).await;
    reconnect_after(state, &edit, pasted_secret).await;
    render_after(state, &edit, unstored_secret).await
}

/// Reads and parses `web.toml`.
async fn load_document(state: &SettingsRoutesState) -> Result<WebConfigDocument, Box<Response>> {
    let read_path = state.web_config_path.as_path().to_owned();
    let source = match tokio::task::spawn_blocking(move || std::fs::read_to_string(read_path)).await {
        Ok(Ok(source)) => source,
        Ok(Err(source)) => return Err(Box::new(read_failure(&source))),
        Err(_join_error) => {
            return Err(Box::new(error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "settings/blocking-task-failed",
            )));
        }
    };
    WebConfigDocument::parse(&source).map_err(|failure| {
        Box::new(match failure {
            WebConfigWriterError::Toml { source } => invalid_configuration(format!("web.toml is invalid: {source}")),
            other => invalid_configuration(other),
        })
    })
}

/// The refusal for removing a server some registered app still names, or that the check cannot be made for; `None`
/// when nothing is removed or no app depends on what is.
async fn refuse_removal_in_use(
    state: &SettingsRoutesState,
    before_names: &[String],
    removed: &[String],
) -> Option<Response> {
    if removed.is_empty() {
        return None;
    }
    // Which apps still name a server is read through the primary server; while it is not connected that cannot
    // be checked, so the removal is refused as unreachable rather than allowed unchecked. Correcting the
    // primary's entry, which needs no check, is how to recover.
    let Some(client) = state.client() else {
        return Some(unreachable());
    };
    match dependent_apps(&client, before_names, removed).await {
        Ok(conflicts) if conflicts.is_empty() => None,
        Ok(conflicts) => {
            let server = removed
                .iter()
                .find(|name| conflicts.iter().any(|c| &c.server == *name))
                .cloned()
                .unwrap_or_else(|| removed[0].clone());
            let used_by = conflicts
                .into_iter()
                .filter(|c| c.server == server)
                .map(|c| AppReference {
                    vault: c.vault,
                    slug: c.slug,
                })
                .collect();
            Some(mcp_server_in_use(&server, used_by))
        }
        Err(DependencyCheckError::Call(McpCallError::Unreachable { .. } | McpCallError::Refused { .. })) => {
            Some(unreachable())
        }
        Err(error @ DependencyCheckError::VaultListUnreadable) => Some(vault_list_unreadable(error.to_string())),
    }
}

/// Writes `document` to `web.toml` atomically; the failure response when it could not be.
async fn write_document(state: &SettingsRoutesState, document: &WebConfigDocument) -> Option<Response> {
    let write_path = state.web_config_path.as_path().to_owned();
    let rendered = document.render().into_bytes();
    match tokio::task::spawn_blocking(move || write_atomically(&write_path, &rendered)).await {
        Ok(Ok(())) => None,
        Ok(Err(source)) => Some(read_failure(&source)),
        Err(_join_error) => Some(error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "settings/blocking-task-failed",
        )),
    }
}

/// Makes the sessions follow the entries just written: a new one connects, a changed one reconnects, a removed one
/// closes.
async fn reconnect_after(state: &SettingsRoutesState, edit: &Edit, pasted_secret: bool) {
    let connecting = crate::routes::settings_connections::sync_connections(state).await;
    // A pasted token is stored under the same reference, so the entry itself is unchanged and the sync above leaves
    // its session alone: connect it again so the new token is used.
    if let (true, Edit::PatchMcpServer { name, .. }) = (pasted_secret, edit)
        && !connecting.contains(name)
        && state.clients.request_reconnect(name).is_err()
    {
        tracing::warn!(server = %name, "a token was pasted for a server the sessions do not know of");
    }
}

/// The settings fragment as the written configuration now reads, refreshing the shell when the edit changes it.
async fn render_after(state: &SettingsRoutesState, edit: &Edit, unstored_secret: Option<String>) -> Response {
    let refresh = touches_shell_chrome(edit);
    let (config, readiness) = tokio::join!(
        read_current_blocking(Arc::clone(&state.web_config_path)),
        state.clients.readiness(),
    );
    match config {
        Ok(config) => {
            let credentials = credential_snapshot(state.credential_store(), &config).await;
            let fragment = render(&config, &readiness, &credentials, None, unstored_secret, None);
            if refresh {
                html_response_with_refresh(fragment)
            } else {
                html_response(fragment)
            }
        }
        Err(response) => *response,
    }
}

fn unknown_mcp_server(name: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorBody {
            error: "settings/unknown-mcp-server",
            detail: Some(format!("no [[mcp_server]] entry named {name:?} is configured")),
            server: Some(name.to_owned()),
            used_by: Vec::new(),
        }),
    )
        .into_response()
}

#[cfg(test)]
#[path = "settings_test.rs"]
mod tests;
