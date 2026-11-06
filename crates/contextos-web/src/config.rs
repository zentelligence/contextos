//! `web.toml` loading. `contextos-web` never reads the MCP server's `config.toml`: it learns about vaults only
//! through MCP calls to the servers it connects to.

use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// `web.toml`'s full schema.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WebConfig {
    #[serde(default)]
    pub server: WebServerConfig,
    #[serde(default, rename = "mcp_server")]
    pub mcp_servers: Vec<McpServerConfig>,
    /// Who owns the jobs of workflows installed on this host; `None` leaves them ownerless.
    #[serde(default)]
    pub owner: Option<OwnerConfig>,
    #[serde(default)]
    pub workflows: WorkflowsConfig,
    #[serde(default)]
    pub events: EventsConfig,
    #[serde(default)]
    pub agents: AgentsConfig,
}

/// `web.toml`'s `[events]` table: how the live event stream reads the vault.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EventsConfig {
    /// Seconds between the stream reader's polls of `contextos-mcp` for new events. `None` leaves the default, 5. At
    /// least 1, because polling continuously would cost the vault more than a watcher gains.
    #[serde(default)]
    pub poll_seconds: Option<u64>,
}

/// The seconds between polls when `poll_seconds` is not set.
pub const DEFAULT_EVENT_POLL_SECONDS: u64 = 5;

impl EventsConfig {
    /// How long the stream reader waits between polls.
    #[must_use]
    pub fn poll_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.poll_seconds.unwrap_or(DEFAULT_EVENT_POLL_SECONDS).max(1))
    }
}

/// `web.toml`'s `[agents]` table: how the Agents screen reads liveness.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentsConfig {
    /// Minutes after which a session run that started and has no finish shows amber, when no task holds a claim that
    /// names it. `None` leaves the default, 120.
    #[serde(default)]
    pub run_overdue_minutes: Option<u32>,
}

/// The minutes a run may go unfinished when `run_overdue_minutes` is not set.
pub const DEFAULT_RUN_OVERDUE_MINUTES: u32 = 120;

impl AgentsConfig {
    /// How long a started run may go unfinished before it shows amber.
    #[must_use]
    pub fn run_overdue(&self) -> time::Duration {
        time::Duration::minutes(i64::from(
            self.run_overdue_minutes.unwrap_or(DEFAULT_RUN_OVERDUE_MINUTES),
        ))
    }
}

/// The `[events]` settings in effect now, or the defaults when `web.toml` cannot be read: read each time they are
/// used, so a saved change applies without a restart.
#[must_use]
pub fn current_events(path: &Path) -> EventsConfig {
    load_web_config(path).map(|config| config.events).unwrap_or_default()
}

/// The `[agents]` settings in effect now, or the defaults when `web.toml` cannot be read.
#[must_use]
pub fn current_agents(path: &Path) -> AgentsConfig {
    load_web_config(path).map(|config| config.agents).unwrap_or_default()
}

/// `web.toml`'s `[workflows]` table: defaults every workflow and job may override.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowsConfig {
    /// The minimum level a script's log messages are written at when neither its job nor its workflow sets one.
    /// `None` leaves the default, `info`.
    #[serde(default)]
    pub log_level: Option<contextos_workflows::LogLevel>,
    /// How many days a shared job's run records are kept before the job's next run deletes them.
    /// `None` leaves the default, 30. Zero is rejected, because it would delete a record the moment its run ended.
    #[serde(default)]
    pub run_retention_days: Option<u32>,
}

/// The days run records are kept when `run_retention_days` is not set.
pub const DEFAULT_RUN_RETENTION_DAYS: u32 = 30;

impl WorkflowsConfig {
    /// How long a shared job's run records are kept.
    #[must_use]
    pub fn run_retention(&self) -> time::Duration {
        time::Duration::days(i64::from(self.run_retention_days.unwrap_or(DEFAULT_RUN_RETENTION_DAYS)))
    }
}

/// `web.toml`'s `[owner]` table: a name only, because a host has no team file to resolve a member from.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OwnerConfig {
    pub name: String,
}

impl WebConfig {
    /// Re-checks every cross-field invariant [`load_web_config`] enforces on
    /// load. `pub(crate)` so [`crate::config_writer::WebConfigDocument`]
    /// can run the identical validator against an edited document before
    /// persisting it, rather than duplicating these rules.
    pub(crate) fn validate(&self) -> Result<(), WebConfigError> {
        validate_loopback_bind(&self.server.bind)?;
        let mut seen = std::collections::HashSet::new();
        for server in &self.mcp_servers {
            if !seen.insert(server.name().to_ascii_lowercase()) {
                return Err(WebConfigError::DuplicateMcpServerName {
                    name: server.name().to_owned(),
                });
            }
            server.validate_auth()?;
        }
        self.server.validate_logging()?;
        if self.owner.as_ref().is_some_and(|owner| owner.name.trim().is_empty()) {
            return Err(WebConfigError::BlankOwnerName);
        }
        if self.workflows.run_retention_days == Some(0) {
            return Err(WebConfigError::ZeroRunRetention);
        }
        if self.events.poll_seconds == Some(0) {
            return Err(WebConfigError::InvalidPollSeconds);
        }
        if self.agents.run_overdue_minutes == Some(0) {
            return Err(WebConfigError::ZeroRunOverdue);
        }
        Ok(())
    }
}

/// Rejects any bind address that does not resolve to a loopback interface.
///
/// `contextos-web` ships with no authentication mechanism at all, so a
/// non-loopback bind would expose every proxied MCP tool (and the
/// static asset directory) to any host reachable on that interface. There is
/// deliberately no token-bypass the way `contextos-mcp`'s own
/// `validate_bind` allows for its HTTP transport:
/// no bearer-token mechanism exists in `contextos-web` to bypass with.
fn validate_loopback_bind(bind: &str) -> Result<(), WebConfigError> {
    let address = bind
        .parse::<SocketAddr>()
        .map_err(|_source| WebConfigError::InvalidBindAddress { bind: bind.to_owned() })?;
    if address.ip().is_loopback() {
        Ok(())
    } else {
        Err(WebConfigError::NonLoopbackBind { bind: bind.to_owned() })
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WebServerConfig {
    #[serde(default = "default_bind")]
    pub bind: String,
    /// An operator-configured override/addition to the crate's own bundled
    /// `static/` assets (embedded in the binary, `static_assets` module);
    /// optional, and `None` by default, since the bundled set already
    /// makes `/static/` fully servable with no configuration at all.
    #[serde(default)]
    pub static_dir: Option<PathBuf>,
    #[serde(default)]
    pub log_level: WebLogLevel,
    #[serde(default)]
    pub log_file: String,
    /// Rotates `log_file` once it reaches this size, in megabytes. `None`
    /// (the default) never rotates on size. Meaningless (and rejected by
    /// [`WebServerConfig::validate_logging`]) while `log_file` is empty:
    /// there is no file to rotate when logging goes to stderr.
    #[serde(default)]
    pub log_max_size_mb: Option<u64>,
    /// Rotates `log_file` on every UTC calendar-day boundary. `false` (the
    /// default) never rotates on time. Combinable with `log_max_size_mb`:
    /// whichever condition is reached first triggers a rotation. Rejected
    /// by [`WebServerConfig::validate_logging`] while `log_file` is empty.
    #[serde(default)]
    pub log_rotate_daily: bool,
    /// Deletes a rotated log file once it is older than this many days.
    /// Independent of `log_retention_files`: when both are set, a rotated
    /// file is deleted once it violates either bound. `None` never prunes
    /// by age. Rejected while `log_file` is empty or neither rotation
    /// trigger is set (retention with nothing that ever rotates has no
    /// effect, which is more likely a misconfiguration than a deliberate
    /// no-op).
    #[serde(default)]
    pub log_retention_days: Option<u32>,
    /// Keeps only the `N` most recently rotated log files, deleting older
    /// ones. Same validation and combination rules as `log_retention_days`.
    #[serde(default)]
    pub log_retention_files: Option<u32>,
    /// Opaque: this table belongs to the rendering and theme code, so its keys are neither enumerated nor
    /// rejected here.
    #[serde(default)]
    pub ui: toml::Table,
}

impl Default for WebServerConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            static_dir: None,
            log_level: WebLogLevel::default(),
            log_file: String::new(),
            log_max_size_mb: None,
            log_rotate_daily: false,
            log_retention_days: None,
            log_retention_files: None,
            ui: toml::Table::default(),
        }
    }
}

impl WebServerConfig {
    /// Rejects a logging configuration that can never take effect: any
    /// rotation or retention setting while `log_file` is empty (logging
    /// goes to stderr, which cannot be rotated or pruned), a zero-megabyte
    /// size threshold (never satisfiable, since a freshly rotated file
    /// starts at zero bytes), a zero-day or zero-file retention bound (same
    /// "never satisfiable" reasoning), or a retention setting with neither
    /// rotation trigger enabled (nothing would ever produce a rotated file
    /// for retention to prune, so the setting could only be a
    /// misconfiguration, never a deliberate no-op).
    fn validate_logging(&self) -> Result<(), WebConfigError> {
        let rotates = self.log_max_size_mb.is_some() || self.log_rotate_daily;
        let retains = self.log_retention_days.is_some() || self.log_retention_files.is_some();
        if self.log_file.is_empty() && (rotates || retains) {
            return Err(WebConfigError::LogRotationWithoutFile);
        }
        if self.log_max_size_mb == Some(0) {
            return Err(WebConfigError::InvalidLogRotation {
                detail: "log_max_size_mb must be at least 1".to_owned(),
            });
        }
        if self.log_retention_days == Some(0) {
            return Err(WebConfigError::InvalidLogRotation {
                detail: "log_retention_days must be at least 1".to_owned(),
            });
        }
        if self.log_retention_files == Some(0) {
            return Err(WebConfigError::InvalidLogRotation {
                detail: "log_retention_files must be at least 1".to_owned(),
            });
        }
        if retains && !rotates {
            return Err(WebConfigError::InvalidLogRotation {
                detail: "log_retention_days/log_retention_files has no effect without \
                         log_max_size_mb or log_rotate_daily"
                    .to_owned(),
            });
        }
        Ok(())
    }
}

fn default_bind() -> String {
    "127.0.0.1:7373".to_owned()
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum WebLogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

/// One `[[mcp_server]]` entry. Internally tagged on `transport` so a stdio and an HTTP entry can carry their own,
/// non-overlapping required fields (`command`/`args` versus `endpoint`/`token_env`) rather than making every field
/// optional on a single flat shape.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "transport", rename_all = "lowercase", deny_unknown_fields)]
pub enum McpServerConfig {
    Stdio {
        name: String,
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
    Http {
        name: String,
        endpoint: String,
        /// `skip_serializing_if` for the identical reason `auth`'s own doc comment below gives:
        /// this type round-trips through `/server/settings/`'s JSON-to-TOML bridge, which rejects a
        /// literal `null` value. Missed when that precedent was first established (`auth` and
        /// `headers` both got it, this field did not), the gap meant a `PUT`/`POST` submitting an
        /// HTTP entry with `auth` but no `token_env` at all (every `bearer`/`oauth2`-mode entry
        /// that does not also set a `token_env` bearer source, which is the normal case)
        /// unconditionally serialised `"token_env": null` and was rejected as an unrepresentable
        /// TOML value before ever reaching validation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token_env: Option<String>,
        /// The entry's `[mcp_server.auth]` table. `None`, combined with a bare `token_env` above, continues to mean
        /// exactly what it meant before this field existed: an optional bearer token sourced from that environment
        /// variable, or no authentication when both are absent. `skip_serializing_if` matters beyond tidiness here:
        /// this type also round-trips through `/server/settings/`'s JSON-to-TOML bridge (`routes::settings::json_object`),
        /// which rejects a literal `null` value rather than treating it as "field absent", so an unconditionally
        /// emitted `auth: null` would turn every write to an HTTP entry with no `auth` table into a hard failure.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth: Option<McpServerAuth>,
        /// The entry's `[mcp_server.headers]` table: plain, non-secret name/value pairs merged verbatim into every
        /// proxied request to this server, alongside whichever `Authorization` header `auth` produces. Never a
        /// place for a secret value; a secret-valued header belongs in a credential store as its own reference
        /// instead, once one exists. Skipped when empty so a write through the same JSON-to-TOML bridge does not
        /// introduce a stray empty `[mcp_server.headers]` table on every entry.
        #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
        headers: std::collections::BTreeMap<String, String>,
    },
}

impl McpServerConfig {
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Stdio { name, .. } | Self::Http { name, .. } => name,
        }
    }

    /// Every `credential_ref`/`client_secret_ref` this entry's own `[mcp_server.auth]` names, in no
    /// particular order: empty for a stdio entry, an HTTP entry with no `auth`, or `mode = "none"`;
    /// one value for `mode = "bearer"` with a `credential_ref` (never for a `token_env`-sourced
    /// entry, which names no credential-store reference at all); one or two values for
    /// `mode = "oauth2"` (its own always-required `credential_ref`, plus `client_secret_ref` when
    /// set). Used by `routes::settings::apply_edit_and_persist` to garbage-collect a
    /// credential-store record that no longer appears anywhere in a `[[mcp_server]]` array after an
    /// edit.
    #[must_use]
    pub(crate) fn credential_references(&self) -> Vec<String> {
        let Self::Http { auth: Some(auth), .. } = self else {
            return Vec::new();
        };
        match auth {
            McpServerAuth::None => Vec::new(),
            McpServerAuth::Bearer { credential_ref, .. } => credential_ref.iter().cloned().collect(),
            McpServerAuth::Oauth2 {
                credential_ref,
                client_secret_ref,
                ..
            } => std::iter::once(credential_ref.clone())
                .chain(client_secret_ref.iter().cloned())
                .collect(),
        }
    }

    /// Checks the `[mcp_server.auth]` cross-field invariants on an HTTP entry; a no-op for a stdio entry (which
    /// cannot carry `auth` at all, `deny_unknown_fields` already refuses it at parse time) and for an HTTP entry
    /// with no `auth` table.
    fn validate_auth(&self) -> Result<(), WebConfigError> {
        let Self::Http {
            name, auth: Some(auth), ..
        } = self
        else {
            return Ok(());
        };
        match auth {
            McpServerAuth::None => Ok(()),
            McpServerAuth::Bearer {
                token_env,
                credential_ref,
            } => {
                if token_env.is_some() == credential_ref.is_some() {
                    Err(WebConfigError::BearerAuthCredentialSourceAmbiguous { name: name.clone() })
                } else {
                    Ok(())
                }
            }
            McpServerAuth::Oauth2 {
                client_id,
                client_secret_ref,
                client_secret_env,
                ..
            } => {
                if client_secret_ref.is_some() && client_secret_env.is_some() {
                    Err(WebConfigError::Oauth2ClientSecretSourceAmbiguous { name: name.clone() })
                } else if (client_secret_ref.is_some() || client_secret_env.is_some()) && client_id.is_none() {
                    Err(WebConfigError::Oauth2ClientSecretRefWithoutClientId { name: name.clone() })
                } else {
                    Ok(())
                }
            }
        }
    }
}

/// An HTTP `[[mcp_server]]` entry's `[mcp_server.auth]` table. Internally tagged on `mode`, mirroring
/// [`McpServerConfig`]'s own tagging on `transport`, so each mode carries only the fields that apply to it rather
/// than making every field optional on a single flat shape.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
pub enum McpServerAuth {
    /// An explicit statement of no authentication, for symmetry with `Bearer` and `Oauth2`. Does not change what
    /// an *absent* `[mcp_server.auth]` table means; that continues to be governed solely by the top-level
    /// `token_env` field.
    None,
    /// Exactly one of `token_env` (an environment variable name, unchanged from the existing top-level field) or
    /// `credential_ref` (a name resolved against a credential store) supplies the bearer token. Neither is a
    /// fallback of the other; [`McpServerConfig::validate_auth`] rejects both set and neither set.
    ///
    /// Both fields carry `skip_serializing_if` for the same reason [`McpServerConfig::Http::auth`]'s own doc
    /// comment gives: this type round-trips through `/server/settings/`'s JSON-to-TOML bridge, which rejects a literal
    /// `null` value. Exactly one of these two is always unset by this variant's own contract, so without this
    /// attribute every `bearer`-mode submission would unconditionally fail on its unset half.
    Bearer {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token_env: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credential_ref: Option<String>,
    },
    /// `authorization_endpoint`, `token_endpoint`, `scopes`, and `credential_ref` are always required;
    /// `client_id` and its client secret are optional, covering the client-pre-registration pattern. The secret
    /// comes from at most one of `client_secret_ref` (a name resolved against the credential store) or
    /// `client_secret_env` (an environment variable name), and only once `client_id` is set.
    ///
    /// Both carry `skip_serializing_if` for the identical `null`-rejecting JSON-to-TOML bridge reason as `Bearer`'s
    /// own fields above: the hosted-consent pattern, where the provider issues the client itself, leaves both
    /// unset entirely, which is the common case, not an edge case.
    Oauth2 {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_secret_ref: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_secret_env: Option<String>,
        authorization_endpoint: String,
        token_endpoint: String,
        scopes: Vec<String>,
        credential_ref: String,
    },
}

impl McpServerAuth {
    /// Where an `oauth2` entry's client secret comes from, or `None` when it has none (a hosted-consent provider)
    /// or is not an `oauth2` entry. [`McpServerConfig::validate_auth`] guarantees at most one source is set.
    #[must_use]
    pub(crate) fn client_secret(&self) -> Option<ClientSecretSource> {
        let Self::Oauth2 {
            client_secret_ref,
            client_secret_env,
            ..
        } = self
        else {
            return None;
        };
        client_secret_ref
            .clone()
            .map(ClientSecretSource::Stored)
            .or_else(|| client_secret_env.clone().map(ClientSecretSource::Environment))
    }
}

/// Where an `oauth2` entry's client secret comes from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientSecretSource {
    /// The credential store record named by `client_secret_ref`.
    Stored(String),
    /// The environment variable named by `client_secret_env`.
    Environment(String),
}

/// Loads and validates `web.toml` from `path`.
///
/// # Errors
///
/// Returns [`WebConfigError::Read`] when the file cannot be read,
/// [`WebConfigError::Toml`] when it does not parse against
/// [`WebConfig`]'s schema, or one of the validation variants
/// (`NonLoopbackBind`, `InvalidBindAddress`, `DuplicateMcpServerName`) when
/// it parses but violates a documented invariant.
pub fn load_web_config(path: &Path) -> Result<WebConfig, WebConfigError> {
    let source = fs::read_to_string(path).map_err(|source| WebConfigError::Read {
        path: path.to_owned(),
        source,
    })?;
    let config: WebConfig = toml::from_str(&source).map_err(|source| WebConfigError::Toml {
        source: Box::new(source),
    })?;
    config.validate()?;
    Ok(config)
}

/// The default `system_name` (`page_shell.html`'s header title) when
/// `[server.ui]` sets none: the shell must always show some title, so this
/// is the resolved value, not merely a placeholder string shown in a form.
pub const DEFAULT_SYSTEM_NAME: &str = "Command Centre";

/// The default `logo` (`page_shell.html`'s header mark), a filename under
/// `/static/`: the bundled default ships at this exact path
/// (`static/logo.png`), so the header always has a mark to show even
/// before an operator overrides it. Overriding means adding a differently
/// named file to the configured `static_dir` and setting `logo` to that
/// filename in `/server/settings/`, the same "add a file, point `[server.ui]` at
/// it" pattern every other static-folder override already follows.
pub const DEFAULT_LOGO_FILENAME: &str = "logo.png";

/// The five `[server.ui]` keys `/server/settings/`'s Appearance pane,
/// `contextos-web.css`, and `page_shell.html`'s header give real,
/// operator-visible effect (colour theme, font, base text size, the
/// header's own title, and the header's own logo mark); every other
/// `[server.ui]` key stays persisted but inert, matching `config.rs`'s own
/// "a rendering/theme concern deferred ... not enumerated here" stance for
/// the table as a whole.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Appearance {
    pub theme: Option<String>,
    pub font: Option<String>,
    pub size: Option<String>,
    /// The header's own title, next to the logo (`page_shell.html`).
    /// Unlike `theme`/`font`/`size` (each `None` when unset, so the
    /// corresponding CSS attribute selector is simply absent), the header
    /// always needs a title to show, so this is a resolved `String`, never
    /// unset: [`current_appearance`] and [`Appearance::default`] both fall
    /// back to [`DEFAULT_SYSTEM_NAME`].
    pub system_name: String,
    /// The header's own logo mark: a filename served from `/static/`
    /// (the bundled default, or an operator-added override in a configured
    /// `static_dir`). Resolved the same way as `system_name`, never unset:
    /// [`current_appearance`] and [`Appearance::default`] both fall back to
    /// [`DEFAULT_LOGO_FILENAME`].
    pub logo: String,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: None,
            font: None,
            size: None,
            system_name: DEFAULT_SYSTEM_NAME.to_owned(),
            logo: DEFAULT_LOGO_FILENAME.to_owned(),
        }
    }
}

/// Reads [`Appearance`] from `[server.ui]` fresh on every call (no
/// caching): a `/server/settings/` appearance save takes effect on the very next
/// page render. Never propagates a load failure to the caller (a
/// nav-shell-only concern, per
/// [`shell::build_nav`](crate::rendering::shell::build_nav)'s own "degrade
/// rather than fail the page" contract) or coerces a non-string value (an
/// operator-set number or table under one of these keys has no defined
/// meaning to render, so it is treated as unset rather than guessed at).
#[must_use]
pub fn current_appearance(path: &Path) -> Appearance {
    let Ok(config) = load_web_config(path) else {
        return Appearance::default();
    };
    let string_value = |key: &str| match config.server.ui.get(key) {
        Some(toml::Value::String(text)) => Some(text.clone()),
        _ => None,
    };
    Appearance {
        theme: string_value("theme"),
        font: string_value("font"),
        size: string_value("size"),
        system_name: string_value("system_name").unwrap_or_else(|| DEFAULT_SYSTEM_NAME.to_owned()),
        logo: string_value("logo").unwrap_or_else(|| DEFAULT_LOGO_FILENAME.to_owned()),
    }
}

#[derive(Debug, Error)]
pub enum WebConfigError {
    #[error("web configuration file could not be read: {path}", path = path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("web configuration TOML is invalid")]
    Toml {
        #[source]
        source: Box<toml::de::Error>,
    },
    #[error(
        "server.bind must be a loopback address in this release (no authentication mechanism \
         exists to protect a non-loopback bind); got {bind}"
    )]
    NonLoopbackBind { bind: String },
    #[error(
        "server.bind is invalid; expected a host:port socket address such as 127.0.0.1:7373, \
         got {bind}"
    )]
    InvalidBindAddress { bind: String },
    #[error("[owner] name must not be blank; remove the table to leave shared jobs without an owner")]
    BlankOwnerName,
    #[error(
        "[workflows] run_retention_days must be at least 1: zero would delete a run record the moment its run ended"
    )]
    ZeroRunRetention,
    #[error(
        "[events] poll_seconds must be at least 1: polling more often than once a second costs the vault more than it shows"
    )]
    InvalidPollSeconds,
    #[error("[agents] run_overdue_minutes must be at least 1: a run would show overdue the moment it started")]
    ZeroRunOverdue,
    #[error("duplicate [[mcp_server]] name: {name}")]
    DuplicateMcpServerName { name: String },
    #[error(
        "[mcp_server.auth] for {name} must set exactly one of token_env or credential_ref when \
         mode = \"bearer\", not both and not neither"
    )]
    BearerAuthCredentialSourceAmbiguous { name: String },
    #[error(
        "[mcp_server.auth] for {name} sets a client secret (client_secret_ref or client_secret_env) without \
         client_id; a client secret is only meaningful when client_id is also set"
    )]
    Oauth2ClientSecretRefWithoutClientId { name: String },
    #[error(
        "[mcp_server.auth] for {name} sets both client_secret_ref and client_secret_env; set client_secret_ref or \
         client_secret_env, not both: a pasted secret or the name of the environment variable holding it"
    )]
    Oauth2ClientSecretSourceAmbiguous { name: String },
    #[error("log rotation and retention settings have no effect while log_file is empty (logging goes to stderr)")]
    LogRotationWithoutFile,
    #[error("invalid log rotation/retention configuration: {detail}")]
    InvalidLogRotation { detail: String },
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;

#[cfg(test)]
#[path = "config_appearance_test.rs"]
mod appearance_tests;

#[cfg(test)]
#[path = "config_owner_test.rs"]
mod owner_tests;

#[cfg(test)]
#[path = "config_workflows_test.rs"]
mod workflows_tests;

#[cfg(test)]
#[path = "config_events_test.rs"]
mod events_tests;

#[cfg(test)]
#[path = "config_client_secret_test.rs"]
mod client_secret_tests;
