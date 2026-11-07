use std::path::PathBuf;

use contextos_core::{
    ConfiguredTimeZone, OperatorTimeZone, PathError, TimeZoneError, VaultRoot, VaultRootInput, VaultSet,
};
use contextos_fs::default_hidden_patterns;
use contextos_search::GraphBackend;
use serde::Deserialize;
use thiserror::Error;

use crate::projection_config::ProjectionConfig;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default, rename = "vault")]
    pub vaults: Vec<VaultConfig>,
    /// `[projection]`: absent when the file has no such section.
    #[serde(default)]
    pub projection: Option<ProjectionConfig>,
    /// Where this configuration was loaded from, which decides whether projection is on by default. Never read from
    /// the file.
    #[serde(skip)]
    pub provenance: ConfigProvenance,
}

/// Where a configuration file was found.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ConfigLocation {
    /// Not loaded from a file (command-line vaults only, or built in memory).
    #[default]
    Unspecified,
    /// A file at the platform's default location that exists, which no flag or environment variable named.
    PlatformDefault,
    /// Named by `--config` or `CONTEXTOS_MCP_CONFIG`.
    Named,
}

/// Facts about how a configuration was loaded that the file itself cannot hold.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConfigProvenance {
    /// How the file was found.
    pub location: ConfigLocation,
    /// The file's path, whether or not it exists yet. Identifies the configuration that owns what projection writes.
    pub path: Option<PathBuf>,
    /// The operator's home directory, from which `~` in a projection target and the state root are resolved.
    pub home: Option<PathBuf>,
}

impl TryFrom<&str> for Config {
    type Error = ConfigError;

    fn try_from(value: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(value).map_err(|source| ConfigError::Toml {
            source: Box::new(source),
        })?;
        config.validate()?;
        Ok(config)
    }
}

impl TryFrom<Vec<PathBuf>> for Config {
    type Error = ConfigError;

    fn try_from(value: Vec<PathBuf>) -> Result<Self, Self::Error> {
        if value.is_empty() {
            return Err(ConfigError::NoVaults);
        }
        Ok(Self {
            server: ServerConfig::default(),
            vaults: value.into_iter().map(VaultConfig::from).collect(),
            projection: None,
            provenance: ConfigProvenance::default(),
        })
    }
}

impl Config {
    /// Zero configured vaults is deliberately accepted here: the server still starts, completes its MCP handshake,
    /// and lists tools with an empty vault collection, so a fresh install (or a plugin scaffolding one) does not
    /// need a vault to exist before the process can run at all. Vault-dependent tool calls fail with
    /// `contextos-core`'s own typed `PathError::NoRoots` once resolution actually needs a root.
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        if self.server.transports.is_empty() {
            return Err(ConfigError::NoTransports);
        }
        self.server.validate_logging()?;
        self.server.http.check_token_sources()?;
        if let Some(name) = self.server.timezone.as_deref() {
            ConfiguredTimeZone::try_from(name).map_err(|source| ConfigError::TimeZone { source })?;
        }
        // The inline threshold caps every part of `fs_read_text_file`, so zero would make parts impossible and
        // every whole read an empty preview.
        if self.server.resource_link_threshold_kb == 0 {
            return Err(ConfigError::InvalidLimit {
                field: "server.resource_link_threshold_kb",
            });
        }
        for vault in &self.vaults {
            if vault.limits.max_read_mb == 0 {
                return Err(ConfigError::InvalidLimit {
                    field: "vault.limits.max_read_mb",
                });
            }
            if vault.limits.max_batch_files == 0 {
                return Err(ConfigError::InvalidLimit {
                    field: "vault.limits.max_batch_files",
                });
            }
            if vault.events.retention_days == 0 {
                return Err(ConfigError::InvalidRetention);
            }
            if !is_portable_relative_path(&vault.oplog.path) {
                return Err(ConfigError::InvalidRelativePath {
                    field: "vault.oplog.path",
                    path: vault.oplog.path.clone(),
                });
            }
            if let Some(path) = vault
                .git
                .restore_exclude
                .iter()
                .find(|path| !is_portable_relative_path(path))
            {
                return Err(ConfigError::InvalidRelativePath {
                    field: "vault.git.restore_exclude",
                    path: path.clone(),
                });
            }
            if let Some(entry) = vault.links.exclude.iter().find(|entry| !is_plain_folder(entry)) {
                return Err(ConfigError::InvalidLinkExclude { entry: entry.clone() });
            }
            if !vault.search.embedding.api_key.is_empty() && !vault.search.embedding.api_key_env.is_empty() {
                return Err(ConfigError::EmbeddingKeyConflict {
                    vault: vault.path.clone(),
                });
            }
        }
        Ok(())
    }
}

fn is_portable_relative_path(path: &std::path::Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path.components().all(|component| {
            matches!(component, std::path::Component::Normal(_))
                && !component.as_os_str().to_string_lossy().contains(['\\', ':'])
        })
}

impl TryFrom<&Config> for VaultSet {
    type Error = ConfigError;

    fn try_from(value: &Config) -> Result<Self, Self::Error> {
        let roots = value
            .vaults
            .iter()
            .map(|vault| {
                VaultRoot::try_from(VaultRootInput {
                    path: vault.path.clone(),
                    managed: vault.managed,
                    name: vault.name.clone(),
                })
                .map_err(|source| ConfigError::VaultPath {
                    path: vault.path.clone(),
                    source,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        VaultSet::try_from(roots).map_err(|source| ConfigError::VaultSet { source })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "default_transports")]
    pub transports: Vec<Transport>,
    #[serde(default)]
    pub log_level: LogLevel,
    #[serde(default)]
    pub log_file: String,
    /// Rotates `log_file` once it reaches this size, in megabytes. `None`
    /// (the default) never rotates on size. Meaningless (and rejected by
    /// [`ServerConfig::validate_logging`]) while `log_file` is empty:
    /// there is no file to rotate when logging goes to stderr.
    #[serde(default)]
    pub log_max_size_mb: Option<u64>,
    /// Rotates `log_file` on every UTC calendar-day boundary. `false` (the
    /// default) never rotates on time. Combinable with `log_max_size_mb`:
    /// whichever condition is reached first triggers a rotation. Rejected
    /// by [`ServerConfig::validate_logging`] while `log_file` is empty.
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
    #[serde(default)]
    pub http: HttpConfig,
    /// Size (in KB) at or above which a text-reading tool result attaches
    /// a `resource_link` content block alongside a bounded inline preview,
    /// so a resource-aware host fetches the full content via
    /// `resources/read` instead of exhausting its own inline-result
    /// budget. Below this size, behaviour is unchanged.
    #[serde(default = "default_resource_link_threshold_kb")]
    pub resource_link_threshold_kb: u64,
    /// Registers the `ephemeris_*` tools into this instance's advertised
    /// catalogue. `contextos-ephemeris` and
    /// every ephemeris tool handler are always compiled in regardless of
    /// this flag; it controls runtime visibility only, and can be set here
    /// or overridden per run with `--astro`. Off by default: a niche,
    /// personal-practice capability, kept out of a standard tool list
    /// unless explicitly opted into, the same reasoning `SearchConfig`'s
    /// `semantic` field already applies to a different opt-in capability.
    #[serde(default)]
    pub astro: bool,
    /// The IANA time zone (`"Australia/Sydney"`) every time written into a vault is shown in: a note's `created`
    /// and `updated` stamps and the day an operation log entry is filed under. `None` (the default) uses the
    /// host's own zone. Never a fixed offset such as `+10:00`: a zone takes the offset it actually had at each
    /// instant, daylight saving included. Refused at load when the time zone database has no such zone.
    #[serde(default)]
    pub timezone: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            transports: default_transports(),
            log_level: LogLevel::default(),
            log_file: String::new(),
            log_max_size_mb: None,
            log_rotate_daily: false,
            log_retention_days: None,
            log_retention_files: None,
            http: HttpConfig::default(),
            resource_link_threshold_kb: default_resource_link_threshold_kb(),
            astro: false,
            timezone: None,
        }
    }
}

/// The time zone every time written into a vault is shown in: `[server] timezone` when set, else the host's own.
impl TryFrom<&ServerConfig> for OperatorTimeZone {
    type Error = ConfigError;

    fn try_from(server: &ServerConfig) -> Result<Self, ConfigError> {
        let configured = server
            .timezone
            .as_deref()
            .map(ConfiguredTimeZone::try_from)
            .transpose()
            .map_err(|source| ConfigError::TimeZone { source })?;
        Ok(Self::from(configured))
    }
}

impl ServerConfig {
    /// Rejects a logging configuration that can never take effect: any
    /// rotation or retention setting while `log_file` is empty (logging
    /// goes to stderr, which cannot be rotated or pruned), a zero-megabyte
    /// size threshold (never satisfiable, since a freshly rotated file
    /// starts at zero bytes), a zero-day or zero-file retention bound (same
    /// "never satisfiable" reasoning), or a retention setting with neither
    /// rotation trigger enabled (nothing would ever produce a rotated file
    /// for retention to prune, so the setting could only be a
    /// misconfiguration, never a deliberate no-op).
    fn validate_logging(&self) -> Result<(), ConfigError> {
        let rotates = self.log_max_size_mb.is_some() || self.log_rotate_daily;
        let retains = self.log_retention_days.is_some() || self.log_retention_files.is_some();
        if self.log_file.is_empty() && (rotates || retains) {
            return Err(ConfigError::LogRotationWithoutFile);
        }
        if self.log_max_size_mb == Some(0) {
            return Err(ConfigError::InvalidLogRotation {
                detail: "log_max_size_mb must be at least 1".to_owned(),
            });
        }
        if self.log_retention_days == Some(0) {
            return Err(ConfigError::InvalidLogRotation {
                detail: "log_retention_days must be at least 1".to_owned(),
            });
        }
        if self.log_retention_files == Some(0) {
            return Err(ConfigError::InvalidLogRotation {
                detail: "log_retention_files must be at least 1".to_owned(),
            });
        }
        if retains && !rotates {
            return Err(ConfigError::InvalidLogRotation {
                detail: "log_retention_days/log_retention_files has no effect without \
                         log_max_size_mb or log_rotate_daily"
                    .to_owned(),
            });
        }
        Ok(())
    }
}

fn default_transports() -> Vec<Transport> {
    vec![Transport::Stdio]
}

const fn default_resource_link_threshold_kb() -> u64 {
    5
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Stdio,
    Http,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

impl TryFrom<&str> for LogLevel {
    type Error = ConfigError;

    fn try_from(value: &str) -> Result<Self, ConfigError> {
        match value {
            "error" => Ok(Self::Error),
            "warn" => Ok(Self::Warn),
            "info" => Ok(Self::Info),
            "debug" => Ok(Self::Debug),
            "trace" => Ok(Self::Trace),
            _ => Err(ConfigError::InvalidEnvironmentValue {
                variable: "CONTEXTOS_MCP_LOG_LEVEL",
                value: value.to_owned(),
            }),
        }
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HttpConfig {
    #[serde(default = "default_http_bind")]
    pub bind: String,
    /// The bearer token itself. Set this or `token_env`, never both.
    #[serde(default)]
    pub token: String,
    /// The name of an environment variable holding the bearer token, read by the process that serves HTTP.
    #[serde(default)]
    pub token_env: String,
    #[serde(default = "default_max_body_kb")]
    pub max_body_kb: u64,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            bind: default_http_bind(),
            token: String::new(),
            token_env: String::new(),
            max_body_kb: default_max_body_kb(),
        }
    }
}

fn default_http_bind() -> String {
    "127.0.0.1:3737".to_owned()
}

/// The default HTTP request body cap, in KiB: the shared request body limit
/// ([`contextos_core::MAX_REQUEST_BODY_BYTES`], 14 MiB), large enough for the
/// largest write `fs_write_file` permits once base64 encoded, so it fits over
/// HTTP as it does over stdio.
const fn default_max_body_kb() -> u64 {
    contextos_core::MAX_REQUEST_BODY_BYTES / 1024
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VaultConfig {
    pub path: PathBuf,
    /// Explicit vault name; defaults to the resolved root directory's
    /// basename when unset. Used to address this vault as
    /// `{name}://{relative-path}` instead of an absolute filesystem path,
    /// on every path-accepting tool parameter, on the existing vault
    /// selector parameters, and as the sole scheme `resources/list`,
    /// `resources/read`, and `fs_attach_file` use. Either the explicit
    /// value or the default must be a valid URI scheme token and unique
    /// across the configured vaults, checked at startup with no silent
    /// sanitisation.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default = "default_true")]
    pub managed: bool,
    #[serde(default)]
    pub limits: LimitsConfig,
    #[serde(default)]
    pub index_md: IndexMdConfig,
    #[serde(default)]
    pub oplog: OplogConfig,
    #[serde(default)]
    pub git: GitConfig,
    #[serde(default)]
    pub search: SearchConfig,
    /// Which notes `doctor`'s link integrity check and `query_graph`'s `unresolved` and `unlinked` operations look at.
    #[serde(default)]
    pub links: LinksConfig,
    /// How long the events the server writes are kept.
    #[serde(default)]
    pub events: EventsConfig,
    /// Path patterns hidden from every enumeration surface for this vault:
    /// `resources/list`, `fs_list_directory`,
    /// `fs_list_directory_with_sizes`, `fs_directory_tree`, and
    /// `fs_search_files`. Separate from `search`'s indexing scope and from
    /// `index_md.exclude` (which governs which directories need a
    /// generated `index.md`, not visibility). Governs omission from
    /// listings only; a direct, explicit-path read is never restricted by
    /// this.
    #[serde(default = "default_hidden_patterns")]
    pub hidden: Vec<String>,
    /// Glob patterns naming which files `resources/list` itself enumerates
    /// for this vault: an allowlist, the opposite of `hidden`'s denylist,
    /// scoped to this one surface only, not
    /// `fs_list_directory`/`fs_directory_tree`/`fs_search_files`. Default
    /// (empty): `resources/list` reports nothing for this vault,
    /// deliberately, since an unconfigured allowlist has no basis for
    /// guessing what is worth surfacing, and dumping every file has little
    /// discovery value once a vault holds thousands of them, the exact
    /// complaint this field exists to fix. Every pattern is still subject
    /// to `hidden`. `resources/read`, `resources/templates/list`, and
    /// every direct-path tool are entirely unaffected: this narrows
    /// autonomous enumeration only, never direct access.
    #[serde(default)]
    pub resources_list_include: Vec<String>,
    /// Override location for this vault's derived state (text index, link
    /// graph cache, and vector store). Default (when unset, `None`): a
    /// platform app-data directory keyed to this vault, so index
    /// segments, lock files, and the vector store never sit inside a
    /// directory a third-party sync tool (for example Obsidian Sync) can
    /// observe or replicate. A relative path here resolves against the
    /// vault root, opting back into pre-existing in-vault `.contextos/`
    /// behaviour; an absolute path is used exactly as given. In-vault
    /// storage is not recommended for a vault under live multi-machine
    /// sync.
    #[serde(default)]
    pub state_directory: Option<PathBuf>,
}

impl From<PathBuf> for VaultConfig {
    fn from(value: PathBuf) -> Self {
        Self {
            path: value,
            name: None,
            managed: true,
            limits: LimitsConfig::default(),
            index_md: IndexMdConfig::default(),
            oplog: OplogConfig::default(),
            git: GitConfig::default(),
            search: SearchConfig::default(),
            links: LinksConfig::default(),
            events: EventsConfig::default(),
            hidden: default_hidden_patterns(),
            resources_list_include: Vec::new(),
            state_directory: None,
        }
    }
}

/// `[vault.links]`: which notes are checked as link sources.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LinksConfig {
    /// Folders whose notes are not checked as link sources by `doctor`'s `link_integrity` check and by
    /// `query_graph`'s `unresolved` and `unlinked` operations. A configured list replaces the default, and
    /// `[vault.search] exclude` is always added to it, since a note outside the search corpus is not in the link
    /// graph. Links into these folders still resolve. Each entry is a plain vault-relative folder.
    #[serde(default = "default_link_excludes")]
    pub exclude: Vec<String>,
}

impl Default for LinksConfig {
    fn default() -> Self {
        Self {
            exclude: default_link_excludes(),
        }
    }
}

fn default_link_excludes() -> Vec<String> {
    ["templates", "memory/events", "memory/jobs", ".contextos"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// Whether `entry` is a plain vault-relative folder: forward-slash separated, with no empty, `.`, or `..` segment, no
/// drive or stream colon, and no backslash. A trailing slash is refused rather than trimmed.
fn is_plain_folder(entry: &str) -> bool {
    !entry.is_empty()
        && !entry.contains(['\\', ':'])
        && entry
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

const fn default_true() -> bool {
    true
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LimitsConfig {
    #[serde(default = "default_max_read_mb")]
    pub max_read_mb: u64,
    #[serde(default = "default_max_batch_files")]
    pub max_batch_files: usize,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_read_mb: default_max_read_mb(),
            max_batch_files: default_max_batch_files(),
        }
    }
}

const fn default_max_read_mb() -> u64 {
    5
}

const fn default_max_batch_files() -> usize {
    50
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct IndexMdConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_content_excludes")]
    pub exclude: Vec<String>,
}

impl Default for IndexMdConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            exclude: default_content_excludes(),
        }
    }
}

fn default_content_excludes() -> Vec<String> {
    [
        ".contextos",
        ".git",
        ".obsidian",
        "memory/log",
        "memory/sessions",
        "memory/jobs",
        "memory/events",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OplogConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_oplog_path")]
    pub path: PathBuf,
}

impl Default for OplogConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: default_oplog_path(),
        }
    }
}

fn default_oplog_path() -> PathBuf {
    PathBuf::from("memory/log")
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GitConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_commit_debounce")]
    pub commit_debounce_s: u64,
    #[serde(default = "default_author_name")]
    pub author_name: String,
    #[serde(default = "default_author_email")]
    pub author_email: String,
    #[serde(default)]
    pub destructive_delete: bool,
    #[serde(default = "default_git_restore_excludes")]
    pub restore_exclude: Vec<PathBuf>,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            commit_debounce_s: default_commit_debounce(),
            author_name: default_author_name(),
            author_email: default_author_email(),
            destructive_delete: false,
            restore_exclude: default_git_restore_excludes(),
        }
    }
}

fn default_git_restore_excludes() -> Vec<PathBuf> {
    [
        "memory/log",
        "memory/sessions",
        "memory/coding",
        "memory/jobs",
        "memory/events",
    ]
    .map(PathBuf::from)
    .to_vec()
}

const fn default_commit_debounce() -> u64 {
    30
}

fn default_author_name() -> String {
    "Context OS MCP".to_owned()
}

fn default_author_email() -> String {
    "mcp@contextos.local".to_owned()
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SearchConfig {
    #[serde(default = "default_true")]
    pub text: bool,
    #[serde(default = "default_true")]
    pub graph: bool,
    /// The link graph's persistence backend when `graph` is enabled;
    /// ignored otherwise. Additive alongside `graph`
    /// rather than nested under it, so an existing config with `graph =
    /// true` and no `graph_backend` continues to parse unchanged.
    #[serde(default)]
    pub graph_backend: GraphBackendConfig,
    #[serde(default)]
    pub semantic: bool,
    /// Paths excluded from the search corpus (text, graph, and semantic
    /// indexing). Independent of `[vault.index_md] exclude`: that field
    /// scopes managed `index.md` generation and has no bearing on what a
    /// vault's search tools can find.
    #[serde(default = "default_content_excludes")]
    pub exclude: Vec<String>,
    /// Default budget, in seconds, for `query_index_rebuild`'s semantic
    /// phase when a call omits `budget_seconds`: the phase returns early
    /// with partial progress once this elapses, rather than blocking past
    /// a caller's own request timeout. A per-call `budget_seconds` always
    /// overrides this.
    #[serde(default = "default_rebuild_budget_seconds")]
    pub rebuild_budget_seconds: u64,
    #[serde(default)]
    pub embedding: EmbeddingConfig,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            text: true,
            graph: true,
            graph_backend: GraphBackendConfig::default(),
            semantic: false,
            exclude: default_content_excludes(),
            rebuild_budget_seconds: default_rebuild_budget_seconds(),
            embedding: EmbeddingConfig::default(),
        }
    }
}

const fn default_rebuild_budget_seconds() -> u64 {
    25
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingConfig {
    #[serde(default)]
    pub provider: EmbeddingProvider,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub endpoint: String,
    /// The provider's API key itself. Set this or `api_key_env`, never both.
    #[serde(default)]
    pub api_key: String,
    /// The name of an environment variable holding the provider's API key.
    #[serde(default)]
    pub api_key_env: String,
    /// Directory holding the local ONNX provider's model and tokenizer
    /// files (platform app-data, never the vault). Required when `provider =
    /// "local"` and `[vault.search] semantic = true`; unused otherwise.
    /// Populating this directory (the pre-fetch tool) is separate,
    /// follow-up work: this field only says where to look.
    #[serde(default)]
    pub model_directory: Option<PathBuf>,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            provider: EmbeddingProvider::Local,
            model: String::new(),
            endpoint: String::new(),
            api_key: String::new(),
            api_key_env: String::new(),
            model_directory: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum EmbeddingProvider {
    #[default]
    Local,
    OpenaiCompatible,
}

/// TOML-facing counterpart of `contextos_search::GraphBackend`: a closed
/// three-value enum, rejected at config-parse time
/// for any other value, converted to the crate-internal type at the
/// composition-root boundary (`From<GraphBackendConfig> for GraphBackend`
/// below) rather than deriving `Deserialize` on the search crate's own
/// type, matching `EmbeddingProvider`'s precedent of keeping TOML-parsing
/// concerns in the composition root.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum GraphBackendConfig {
    Serde,
    Fjall,
    #[default]
    Sqlite,
}

impl From<GraphBackendConfig> for GraphBackend {
    fn from(value: GraphBackendConfig) -> Self {
        match value {
            GraphBackendConfig::Serde => Self::Serde,
            GraphBackendConfig::Fjall => Self::Fjall,
            GraphBackendConfig::Sqlite => Self::Sqlite,
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("configuration TOML is invalid")]
    Toml {
        #[source]
        source: Box<toml::de::Error>,
    },
    #[error("configuration file could not be read: {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("environment variable {variable} has an invalid value: {value}")]
    InvalidEnvironmentValue { variable: &'static str, value: String },
    #[error("at least one allowed vault directory must be configured")]
    NoVaults,
    #[error("at least one transport (\"stdio\" or \"http\") must be configured")]
    NoTransports,
    #[error(
        "`--no-stdio` leaves nothing to serve; add `--http` to serve HTTP instead, or list \"http\" in \
         `[server] transports`"
    )]
    NoStdioWithoutHttp,
    #[error("configuration limit must be greater than zero: {field}")]
    InvalidLimit { field: &'static str },
    #[error("vault.events.retention_days must be at least 1; events cannot be kept for no days")]
    InvalidRetention,
    #[error("log rotation and retention settings have no effect while log_file is empty (logging goes to stderr)")]
    LogRotationWithoutFile,
    #[error("invalid log rotation/retention configuration: {detail}")]
    InvalidLogRotation { detail: String },
    #[error(
        "[vault.search.embedding] for the vault at {vault} sets both api_key and api_key_env; keep the key itself in \
         api_key, or the name of the environment variable holding it in api_key_env, and remove the other"
    )]
    EmbeddingKeyConflict { vault: PathBuf },
    #[error(
        "[server.http] sets both token and token_env; keep the token itself in token, or the name of the environment \
         variable holding it in token_env, and remove the other"
    )]
    HttpTokenConflict,
    #[error(
        "[server.http] token_env names {variable}, which is not set or is empty in this process's environment; set \
         it where the server runs (a background service does not see a shell's variables), or write the token itself \
         in token"
    )]
    HttpTokenVariableUnset { variable: String },
    #[error("configuration path must be a portable relative path: {field} = {path}")]
    InvalidRelativePath { field: &'static str, path: PathBuf },
    #[error(
        "[vault.links] exclude entry {entry:?} is not a plain vault-relative folder; write it as a forward-slash path \
         such as \"templates\" or \"memory/events\", with no leading or trailing slash, backslash, drive, `.` or `..`"
    )]
    InvalidLinkExclude { entry: String },
    #[error("vault path is invalid: {path}: {source}. {}", .source.remediation())]
    VaultPath {
        path: PathBuf,
        #[source]
        source: PathError,
    },
    #[error("configured vault roots are invalid: {source}. {}", .source.remediation())]
    VaultSet {
        #[source]
        source: PathError,
    },
    #[error(
        "[server] timezone is invalid: {source}; set an IANA zone such as \"Australia/Sydney\", or remove it to use the host's own zone"
    )]
    TimeZone {
        #[source]
        source: TimeZoneError,
    },
    #[error(
        "projection is enabled but the operator's home directory is unknown; set the HOME (or USERPROFILE) \
         environment variable, or set [projection] enabled = false"
    )]
    ProjectionHomeUnavailable,
    #[error(
        "projection is enabled but this configuration was not loaded from a file, so it has no identity to own what \
         it projects; load it with --config or CONTEXTOS_MCP_CONFIG, or set [projection] enabled = false"
    )]
    ProjectionConfigurationUnnamed,
    #[error(
        "{}; projection replaces whole entries inside these folders and will not follow a link, so point each at a real \
         folder, or switch projection off with `[projection] enabled = false` (or `enabled = false` in the host's \
         table, `[projection.claude]` or `[projection.codex]`)",
        LinkedFolders(folders)
    )]
    ProjectionLinkedFolders {
        /// Each linked folder as `(configuration key, the link, where it points)`.
        folders: Vec<(String, PathBuf, PathBuf)>,
    },
    #[error(
        "[{key}] must be an absolute path after `~` is expanded, but is {path}; write it as `~/...` or a full path"
    )]
    ProjectionTargetNotAbsolute { key: String, path: PathBuf },
    #[error(
        "[{key}] is {path}, which lies inside the vault {vault}; the server writes agent host files only outside \
         vaults, so choose a folder outside every vault root"
    )]
    ProjectionTargetInVault { key: String, path: PathBuf, vault: String },
    #[error(
        "[{key}] is {path}, a folder that contains the vault {vault}; projecting an entry named like the vault's \
         folder would replace the vault, so choose a folder that holds no vault root"
    )]
    ProjectionVaultInTarget { key: String, path: PathBuf, vault: String },
    #[error(
        "[{key}] is {path}, which lies inside or is the server's state folder; the state folder holds the ownership \
         records and locks, so choose a folder outside it"
    )]
    ProjectionTargetInStateRoot { key: String, path: PathBuf },
    #[error(
        "[{key}] is {path}, a folder that contains the server's state folder; projecting an entry named like the state \
         folder would replace it, so choose a folder that does not hold it"
    )]
    ProjectionStateRootInTarget { key: String, path: PathBuf },
    #[error(
        "[{key}] is {path}, which lies inside, contains, or is the folder of {other}; two projection targets may not \
         share a folder, so give each its own"
    )]
    ProjectionTargetsOverlap { key: String, path: PathBuf, other: String },
    #[error("[{key}] could not be resolved: {path}: {source}; check the folder is readable")]
    ProjectionTargetUnresolvable {
        key: String,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("[{key}] is invalid: {source}; correct it or remove it to use the default")]
    ProjectionSetting {
        key: String,
        #[source]
        source: Box<contextos_projection::ProjectionError>,
    },
}

/// The linked folders of [`ConfigError::ProjectionLinkedFolders`], one clause each.
struct LinkedFolders<'a>(&'a [(String, PathBuf, PathBuf)]);

impl std::fmt::Display for LinkedFolders<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "a projection folder must not be a symbolic link: ")?;
        for (index, (key, link, destination)) in self.0.iter().enumerate() {
            if index > 0 {
                formatter.write_str("; ")?;
            }
            write!(
                formatter,
                "[{key}] is {} and links to {}",
                link.display(),
                destination.display()
            )?;
        }
        Ok(())
    }
}

impl ConfigError {
    /// A stable machine-readable code for this failure.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Toml { .. } => "config/invalid-toml",
            Self::Read { .. } => "config/unreadable",
            Self::InvalidEnvironmentValue { .. } => "config/invalid-environment-value",
            Self::NoVaults => "config/no-vaults",
            Self::NoTransports | Self::NoStdioWithoutHttp => "config/no-transports",
            Self::InvalidLimit { .. } => "config/invalid-limit",
            Self::InvalidRetention => "config/invalid-retention",
            Self::LogRotationWithoutFile | Self::InvalidLogRotation { .. } => "config/invalid-log-rotation",
            Self::InvalidRelativePath { .. } => "config/invalid-relative-path",
            Self::InvalidLinkExclude { .. } => "config/invalid-link-exclude",
            Self::EmbeddingKeyConflict { .. } | Self::HttpTokenConflict => "config/conflicting-secret",
            Self::HttpTokenVariableUnset { .. } => "config/missing-secret-variable",
            Self::VaultPath { .. } | Self::VaultSet { .. } => "config/invalid-vault",
            Self::TimeZone { .. } => "config/invalid-time-zone",
            Self::ProjectionHomeUnavailable => "projection/home-unavailable",
            Self::ProjectionConfigurationUnnamed => "projection/configuration-unnamed",
            Self::ProjectionLinkedFolders { .. } => "projection/linked-folder",
            Self::ProjectionTargetNotAbsolute { .. } => "projection/target-not-absolute",
            Self::ProjectionTargetInVault { .. } => "projection/target-in-vault",
            Self::ProjectionVaultInTarget { .. } => "projection/vault-in-target",
            Self::ProjectionTargetInStateRoot { .. } => "projection/target-in-state-root",
            Self::ProjectionStateRootInTarget { .. } => "projection/state-root-in-target",
            Self::ProjectionTargetsOverlap { .. } => "projection/targets-overlap",
            Self::ProjectionTargetUnresolvable { .. } => "projection/target-unresolvable",
            Self::ProjectionSetting { .. } => "projection/invalid-configuration",
        }
    }
}

#[path = "config_secrets.rs"]
mod secrets;

#[path = "config_events.rs"]
mod events;
pub use events::EventsConfig;

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
