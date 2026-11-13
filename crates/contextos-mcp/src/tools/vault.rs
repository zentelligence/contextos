//! `vault_info`, `vault_index_rebuild`, `vault_log_append`: whole-vault
//! reporting and maintenance that spans indexing, Git, and the operation
//! log rather than one substrate alone.

use std::sync::Arc;

use contextos_core::{Clock, Origin, RoutedEvents, VaultPath, VaultPathInput, VaultSet};
use contextos_fs::Filesystem;
use contextos_index::{IndexRebuildOptions, IndexRepair, IndexRepairAction, IndexWarning};
use contextos_oplog::ManualLogInput;
use contextos_search::VaultSearchService;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::ProtocolVersion;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, schemars, tool};
use serde::{Deserialize, Serialize};

use crate::resource_support::{ResourceError, fallible_output_schema_for};
use crate::resources::resource_eligible_file_counts;
use crate::server::{ContextOsServer, ManagedGit, VaultClock, WarningMessages};
use crate::tool_error::{ToolError, ToolFailure, evaluate, execute};
use crate::tools::index_status::IndexesStatusToolResult;
use crate::{Config, EmbeddingProvider, GraphBackendConfig, Transport, VaultConfig};

#[rmcp::tool_router(router = vault_tool_router, vis = "pub(crate)")]
impl ContextOsServer {
    #[tool(
        name = "vault_info",
        description = "Report server, transport, effective vault configuration, and substrate health without exposing secrets. \
`vaults` is in configuration order, and that order decides which vault's agent and skill definitions win when several \
vaults define the same name, both for host projection and for the plugin's lookups.",
        output_schema = fallible_output_schema_for::<VaultInfoToolResult>()
    )]
    async fn vault_info(&self, context: RequestContext<RoleServer>) -> Result<Json<VaultInfoToolResult>, ToolFailure> {
        // The revision of the calling session, which can differ between
        // clients on either transport: for a 2026-07-28 client the one its
        // request carries in `_meta`, for a legacy client the one its
        // `initialize` negotiated, and for a legacy HTTP request without a
        // version header 2025-03-26. rmcp supplies one on every path, so the
        // fallback is a defensive default only and no real request reaches it.
        let protocol_version = context.protocol_version().unwrap_or(ProtocolVersion::LATEST);
        let config = Arc::clone(&self.vault_info);
        let git = Arc::clone(&self.git);
        let search = Arc::clone(&self.search);
        let roots = Arc::clone(&self.roots);
        let filesystem = Arc::clone(&self.filesystem);
        let resources_list_include = Arc::clone(&self.resources_list_include);
        let clock = self.clock.clone();
        execute(move || {
            VaultInfoToolResult::try_from(VaultInfoSource {
                config,
                git,
                search,
                roots,
                filesystem,
                resources_list_include,
                clock,
                protocol_version,
            })
            .map_err(ToolError::from)
        })
        .await
    }

    #[tool(
        name = "vault_index_rebuild",
        description = "Rebuild managed index.md files for every non-excluded folder in a subtree",
        output_schema = fallible_output_schema_for::<IndexRebuildToolResult>()
    )]
    async fn rebuild_indexes(
        &self,
        Parameters(input): Parameters<IndexRebuildInput>,
    ) -> Result<Json<IndexRebuildToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let indexes = Arc::clone(&self.indexes);
        let router = Arc::clone(&self.router);
        let path = evaluate(move || {
            VaultPath::try_from_vault_selector(&roots, input.path.as_deref().unwrap_or(".")).map_err(ToolError::from)
        })
        .await?;
        let root_index = usize::try_from(path.root_id()).map_err(ToolError::from)?;
        let service = indexes
            .get(root_index)
            .cloned()
            .flatten()
            .ok_or_else(|| ToolFailure::from(ToolError::IndexDisabled))?;
        let guards = self
            .writes
            .lock_roots(&[path.root_id()])
            .await
            .map_err(ToolFailure::from)?;
        execute(move || {
            let _guards = guards;
            Ok(IndexRebuildToolResult::from(service.rebuild_report_with(
                &path,
                Origin::Tool("vault_index_rebuild".to_owned()),
                IndexRebuildOptions {
                    dry_run: input.dry_run,
                    repair: input.repair,
                },
                router.as_ref(),
            )?))
        })
        .await
    }

    #[tool(
        name = "vault_log_append",
        description = "Append one explicit manual entry to the shared daily operation log",
        output_schema = fallible_output_schema_for::<LogAppendToolResult>()
    )]
    async fn append_log(
        &self,
        Parameters(input): Parameters<LogAppendInput>,
    ) -> Result<Json<LogAppendToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let operation_logs = Arc::clone(&self.operation_logs);
        let router = Arc::clone(&self.router);
        let (root_id, files) = evaluate(move || {
            let mut files = input
                .files
                .iter()
                .map(|raw| VaultPath::try_from(VaultPathInput { roots: &roots, raw }))
                .collect::<Result<Vec<_>, _>>()?;
            let root_id = if let Some(path) = files.first() {
                path.root_id()
            } else {
                let path = VaultPath::try_from(VaultPathInput {
                    roots: &roots,
                    raw: ".",
                })?;
                path.root_id()
            };
            if files.iter().any(|path| path.root_id() != root_id) {
                return Err(ToolError::Invalid(
                    "manual log files must belong to one configured vault",
                ));
            }
            files.shrink_to_fit();
            Ok((root_id, files))
        })
        .await?;
        let root_index = usize::try_from(root_id).map_err(ToolError::from)?;
        let service = operation_logs
            .get(root_index)
            .cloned()
            .flatten()
            .ok_or_else(|| ToolFailure::from(ToolError::OperationLogDisabled))?;
        let guards = self.writes.lock_roots(&[root_id]).await.map_err(ToolFailure::from)?;
        let clock = self.clock.clone();
        execute(move || {
            let _guards = guards;
            let routed = service.append_manual(
                &ManualLogInput {
                    entry: input.entry,
                    files,
                    at: clock.now(),
                    requested_by: "vault_log_append".to_owned(),
                },
                router.as_ref(),
            )?;
            LogAppendToolResult::try_from(routed)
        })
        .await
    }
}

#[derive(Clone, Debug)]
pub(crate) struct VaultInfoConfig {
    transports: Vec<TransportToolResult>,
    vaults: Vec<VaultInfoConfiguredVault>,
    resource_link_threshold_kb: u64,
}

impl From<&Config> for VaultInfoConfig {
    fn from(value: &Config) -> Self {
        Self {
            transports: value
                .server
                .transports
                .iter()
                .copied()
                .map(TransportToolResult::from)
                .collect(),
            vaults: value.vaults.iter().map(VaultInfoConfiguredVault::from).collect(),
            resource_link_threshold_kb: value.server.resource_link_threshold_kb,
        }
    }
}

#[derive(Clone, Debug)]
struct VaultInfoConfiguredVault {
    path: String,
    managed: bool,
    git_enabled: bool,
    config_summary: VaultConfigSummaryToolResult,
}

impl From<&VaultConfig> for VaultInfoConfiguredVault {
    fn from(value: &VaultConfig) -> Self {
        let service_enabled = value.managed;
        Self {
            path: value.path.to_string_lossy().into_owned(),
            managed: value.managed,
            git_enabled: service_enabled && value.git.enabled,
            config_summary: VaultConfigSummaryToolResult::from(value),
        }
    }
}

#[derive(Clone, Debug)]
struct VaultInfoSource {
    config: Arc<VaultInfoConfig>,
    git: Arc<Vec<Option<ManagedGit>>>,
    search: Arc<Vec<Option<Arc<VaultSearchService>>>>,
    roots: Arc<VaultSet>,
    filesystem: Arc<Filesystem>,
    /// Threaded through so the reported
    /// `resource_eligible_files` count matches what `resources/list`
    /// itself now enumerates, not every file `resources/read` could serve.
    resources_list_include: Arc<Vec<Vec<String>>>,
    clock: VaultClock,
    /// The MCP protocol revision of the calling session: the one a
    /// 2026-07-28 request carries, or the one a legacy client negotiated
    /// (2025-03-26 for a legacy HTTP request without a version header).
    protocol_version: ProtocolVersion,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct VaultInfoToolResult {
    version: String,
    protocol_version: String,
    transports: Vec<TransportToolResult>,
    /// The configured `[server] resource_link_threshold_kb`, so an
    /// operator can confirm the effective threshold without reading config
    /// files directly.
    resource_link_threshold_kb: u64,
    /// The time zone every time the server writes into a vault is shown in, so a client dating its own writes (a
    /// new note's file name, the journal's "today") can use the same zone.
    time_zone: TimeZoneToolResult,
    vaults: Vec<VaultInfoVaultToolResult>,
}

/// The time zone in effect.
#[derive(Debug, Serialize, schemars::JsonSchema)]
struct TimeZoneToolResult {
    /// The IANA zone name (`"Australia/Sydney"`); omitted for a host zone given only as rules, with no name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    /// For a host zone with no IANA name, the POSIX `TZ` rules it was built from (`"AEST-10AEDT,M10.1.0,M4.1.0/3"`),
    /// so a client can build the same zone; omitted otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rules: Option<String>,
    /// `"configured"` (`[server] timezone`), `"system"` (the host's own zone, none being configured), or
    /// `"undetected"` (none configured and the host's could not be determined, so UTC stands in).
    source: String,
    /// The zone's offset from UTC now, as `+HH:MM` or `-HH:MM`; it changes with daylight saving.
    offset: String,
}

impl From<&VaultClock> for TimeZoneToolResult {
    fn from(clock: &VaultClock) -> Self {
        let offset = clock.now().offset();
        let (hours, minutes, _) = offset.as_hms();
        let sign = if offset.is_negative() { '-' } else { '+' };
        Self {
            name: clock.zone().name().map(str::to_owned),
            rules: clock.zone().rules().map(str::to_owned),
            source: clock.zone().source().label().to_owned(),
            offset: format!("{sign}{:02}:{:02}", hours.unsigned_abs(), minutes.unsigned_abs()),
        }
    }
}

impl TryFrom<VaultInfoSource> for VaultInfoToolResult {
    type Error = ResourceError;

    /// Assembles the report. Each vault's Git and search state is reported even when it cannot be read, naming why;
    /// only a vault whose files cannot be enumerated at all fails the call.
    fn try_from(value: VaultInfoSource) -> Result<Self, Self::Error> {
        let resource_eligible_files =
            resource_eligible_file_counts(&value.roots, &value.filesystem, &value.resources_list_include)?;
        let vaults = value
            .config
            .vaults
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, configured)| {
                // The effective name (explicit or defaulted from
                // the root directory's basename) lives on the resolved
                // `VaultRoot`, not the raw `VaultConfig` `configured` is
                // built from, so it is looked up here by the same index
                // rather than threaded through `VaultInfoConfiguredVault`.
                // Defensive only: `value.roots` is always built index-
                // aligned, one root per `value.config.vaults` entry, in the
                // same order (`TryFrom<&Config> for VaultSet`), so this
                // index should never actually miss.
                let name = value
                    .roots
                    .iter()
                    .nth(index)
                    .map_or_else(String::new, |root| root.name().to_owned());
                VaultInfoVaultToolResult::from((
                    configured,
                    value.git.get(index).and_then(Option::as_ref),
                    value.search.get(index).and_then(Option::as_ref),
                    resource_eligible_files.get(index).copied().unwrap_or(0),
                    name,
                ))
            })
            .collect();
        Ok(Self {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            protocol_version: value.protocol_version.to_string(),
            transports: value.config.transports.clone(),
            resource_link_threshold_kb: value.config.resource_link_threshold_kb,
            time_zone: TimeZoneToolResult::from(&value.clock),
            vaults,
        })
    }
}

#[derive(Clone, Copy, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum TransportToolResult {
    Stdio,
    Http,
}

impl From<Transport> for TransportToolResult {
    fn from(value: Transport) -> Self {
        match value {
            Transport::Stdio => Self::Stdio,
            Transport::Http => Self::Http,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct VaultInfoVaultToolResult {
    /// This vault's configured or default-derived name, used to
    /// address it as `{name}://{relative-path}`.
    name: String,
    path: String,
    managed: bool,
    git: VaultGitStatusToolResult,
    indexes: IndexesStatusToolResult,
    config_summary: VaultConfigSummaryToolResult,
    /// Count of files this root currently exposes through
    /// `resources/list` (`hidden`-eligible, any format), reusing the same
    /// enumeration `resources/list` itself uses rather than a second walk.
    resource_eligible_files: usize,
}

impl
    From<(
        VaultInfoConfiguredVault,
        Option<&ManagedGit>,
        Option<&Arc<VaultSearchService>>,
        usize,
        String,
    )> for VaultInfoVaultToolResult
{
    fn from(
        value: (
            VaultInfoConfiguredVault,
            Option<&ManagedGit>,
            Option<&Arc<VaultSearchService>>,
            usize,
            String,
        ),
    ) -> Self {
        // A substrate that cannot report its state is reported as failing, naming why, rather than failing the whole
        // call: `vault_info` exists to show configuration and substrate health, including when a substrate is broken.
        let (pending_commits, git_failure) = match value.1.map(ManagedGit::pending_commit_count) {
            None => (0, None),
            Some(Ok(count)) => (count, None),
            Some(Err(error)) => (
                0,
                Some(SubstrateFailureToolResult::from(ToolFailure::from(ToolError::Git(
                    error,
                )))),
            ),
        };
        let indexes = match value.2 {
            None => IndexesStatusToolResult::default(),
            Some(service) => IndexesStatusToolResult::from(service.status()),
        };
        Self {
            name: value.4,
            path: value.0.path,
            managed: value.0.managed,
            git: VaultGitStatusToolResult {
                enabled: value.0.git_enabled,
                repo: value.1.is_some_and(ManagedGit::is_repository),
                pending_commits,
                failure: git_failure,
            },
            indexes,
            config_summary: value.0.config_summary,
            resource_eligible_files: value.3,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct VaultGitStatusToolResult {
    enabled: bool,
    repo: bool,
    pending_commits: usize,
    /// Why the repository's pending changes could not be read; present only then, when `pending_commits` is `0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failure: Option<SubstrateFailureToolResult>,
}

/// Why a substrate could not report its state.
#[derive(Debug, Serialize, schemars::JsonSchema)]
struct SubstrateFailureToolResult {
    /// Stable machine-readable code.
    code: String,
    message: String,
    /// What to do about it.
    remediation: String,
}

impl From<ToolFailure> for SubstrateFailureToolResult {
    fn from(value: ToolFailure) -> Self {
        Self {
            code: value.code,
            message: value.message,
            remediation: value.remediation,
        }
    }
}

#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
struct VaultConfigSummaryToolResult {
    limits: LimitsConfigSummaryToolResult,
    index_md: IndexMdConfigSummaryToolResult,
    oplog: OplogConfigSummaryToolResult,
    git: GitConfigSummaryToolResult,
    search: SearchConfigSummaryToolResult,
    links: LinksConfigSummaryToolResult,
}

impl From<&VaultConfig> for VaultConfigSummaryToolResult {
    fn from(value: &VaultConfig) -> Self {
        let managed = value.managed;
        Self {
            limits: LimitsConfigSummaryToolResult {
                max_read_mb: value.limits.max_read_mb,
                max_batch_files: value.limits.max_batch_files,
            },
            index_md: IndexMdConfigSummaryToolResult {
                enabled: managed && value.index_md.enabled,
                exclude: value.index_md.exclude.clone(),
            },
            oplog: OplogConfigSummaryToolResult {
                enabled: managed && value.oplog.enabled,
                path: value.oplog.path.to_string_lossy().into_owned(),
            },
            git: GitConfigSummaryToolResult {
                enabled: managed && value.git.enabled,
                commit_debounce_s: value.git.commit_debounce_s,
                author_name: value.git.author_name.clone(),
                author_email: value.git.author_email.clone(),
                destructive_delete: value.git.destructive_delete,
                restore_exclude: value
                    .git
                    .restore_exclude
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect(),
            },
            links: LinksConfigSummaryToolResult {
                exclude: value.links.exclude.clone(),
                ignore_targets: value.links.ignore_targets.clone(),
            },
            search: SearchConfigSummaryToolResult {
                text: managed && value.search.text,
                graph: managed && value.search.graph,
                graph_backend: GraphBackendToolResult::from(value.search.graph_backend),
                semantic: managed && value.search.semantic,
                exclude: value.search.exclude.clone(),
                rebuild_budget_seconds: value.search.rebuild_budget_seconds,
                embedding: EmbeddingConfigSummaryToolResult::from(&value.search.embedding),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, schemars::JsonSchema)]
struct LimitsConfigSummaryToolResult {
    max_read_mb: u64,
    max_batch_files: usize,
}

/// `[vault.links]`: the folders whose notes are not checked as link sources, and those whose files are not checked as
/// link targets.
#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
struct LinksConfigSummaryToolResult {
    exclude: Vec<String>,
    ignore_targets: Vec<String>,
}

#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
struct IndexMdConfigSummaryToolResult {
    enabled: bool,
    exclude: Vec<String>,
}

#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
struct OplogConfigSummaryToolResult {
    enabled: bool,
    path: String,
}

#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
struct GitConfigSummaryToolResult {
    enabled: bool,
    commit_debounce_s: u64,
    author_name: String,
    author_email: String,
    destructive_delete: bool,
    restore_exclude: Vec<String>,
}

#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
struct SearchConfigSummaryToolResult {
    text: bool,
    graph: bool,
    /// The link graph's configured persistence backend, reported
    /// regardless of `graph`'s own `managed` gating:
    /// an operator diagnosing a graph-availability problem (for example
    /// `fjall`'s single-process lock under Cowork's multi-instance
    /// pattern) needs to see which backend is configured without reading
    /// the TOML file blind.
    graph_backend: GraphBackendToolResult,
    semantic: bool,
    exclude: Vec<String>,
    rebuild_budget_seconds: u64,
    embedding: EmbeddingConfigSummaryToolResult,
}

/// What `vault_info` reports in place of a secret written in the configuration.
const REDACTED: &str = "<redacted>";

#[derive(Clone, Debug, Serialize, schemars::JsonSchema)]
struct EmbeddingConfigSummaryToolResult {
    provider: EmbeddingProviderToolResult,
    model: String,
    endpoint: String,
    /// `"<redacted>"` when a key is written in the configuration, absent otherwise: the key itself is never echoed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    api_key: Option<&'static str>,
    api_key_env: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    model_directory: Option<String>,
}

impl From<&crate::EmbeddingConfig> for EmbeddingConfigSummaryToolResult {
    fn from(value: &crate::EmbeddingConfig) -> Self {
        Self {
            provider: EmbeddingProviderToolResult::from(value.provider),
            model: value.model.clone(),
            endpoint: value.endpoint.clone(),
            api_key: (!value.api_key.is_empty()).then_some(REDACTED),
            api_key_env: value.api_key_env.clone(),
            model_directory: value.model_directory.as_deref().map(|path| path.display().to_string()),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
enum EmbeddingProviderToolResult {
    Local,
    OpenaiCompatible,
}

impl From<EmbeddingProvider> for EmbeddingProviderToolResult {
    fn from(value: EmbeddingProvider) -> Self {
        match value {
            EmbeddingProvider::Local => Self::Local,
            EmbeddingProvider::OpenaiCompatible => Self::OpenaiCompatible,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
enum GraphBackendToolResult {
    Serde,
    Fjall,
    Sqlite,
}

impl From<GraphBackendConfig> for GraphBackendToolResult {
    fn from(value: GraphBackendConfig) -> Self {
        match value {
            GraphBackendConfig::Serde => Self::Serde,
            GraphBackendConfig::Fjall => Self::Fjall,
            GraphBackendConfig::Sqlite => Self::Sqlite,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct IndexRebuildToolResult {
    directories_scanned: usize,
    indexes_updated: usize,
    indexes_created: usize,
    skipped: usize,
    /// Every frontmatter value a `repair` rebuild corrected (or, with `dry_run`, would correct); empty otherwise.
    repairs: Vec<IndexRepairToolResult>,
    /// Non-fatal conditions, such as an index whose own frontmatter had to be left as found because its layout
    /// cannot be edited line by line; the rest of that index is still rebuilt.
    warnings: Vec<IndexWarningToolResult>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct IndexRepairToolResult {
    /// The repaired index's vault-relative path, forward-slash separated on every platform.
    path: String,
    actions: Vec<IndexRepairActionResult>,
}

/// One corrected frontmatter property.
#[derive(Debug, Serialize, schemars::JsonSchema)]
struct IndexRepairActionResult {
    /// `entity`, `status`, or `created`.
    property: &'static str,
    /// The value found before the correction.
    from: Option<String>,
    /// The value written; `null` when the property was removed.
    to: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct IndexWarningToolResult {
    /// The index's vault-relative path, forward-slash separated on every platform.
    path: String,
    message: String,
}

impl From<&IndexRepairAction> for IndexRepairActionResult {
    fn from(value: &IndexRepairAction) -> Self {
        Self {
            property: value.property(),
            from: value.from_value().map(str::to_owned),
            to: value.to_value().map(str::to_owned),
        }
    }
}

impl From<IndexRepair> for IndexRepairToolResult {
    fn from(value: IndexRepair) -> Self {
        Self {
            path: value.path,
            actions: value.actions.iter().map(IndexRepairActionResult::from).collect(),
        }
    }
}

impl From<IndexWarning> for IndexWarningToolResult {
    fn from(value: IndexWarning) -> Self {
        Self {
            path: value.path,
            message: value.message,
        }
    }
}

impl From<contextos_index::IndexRebuildResult> for IndexRebuildToolResult {
    fn from(value: contextos_index::IndexRebuildResult) -> Self {
        Self {
            directories_scanned: value.directories_scanned,
            indexes_updated: value.indexes_updated,
            indexes_created: value.indexes_created,
            skipped: value.skipped,
            repairs: value.repairs.into_iter().map(IndexRepairToolResult::from).collect(),
            warnings: value.warnings.into_iter().map(IndexWarningToolResult::from).collect(),
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct LogAppendToolResult {
    path: String,
    appended: bool,
    warnings: Vec<String>,
}

impl TryFrom<RoutedEvents> for LogAppendToolResult {
    type Error = ToolError;

    fn try_from(value: RoutedEvents) -> Result<Self, Self::Error> {
        let path = value
            .events
            .last()
            .and_then(|event| event.paths.first())
            .ok_or(ToolError::ManualLogEventMissing)?
            .portable_relative();
        Ok(Self {
            path,
            appended: true,
            warnings: WarningMessages::from(value.warnings).0,
        })
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct IndexRebuildInput {
    /// Scope the rebuild to this vault or subtree: a configured vault's
    /// bare name to rebuild that whole vault, or a path/
    /// `{name}://{relative-path}` to scope to a specific
    /// subtree; omit to rebuild the sole configured vault.
    path: Option<String>,
    #[serde(default)]
    dry_run: bool,
    /// Also correct the frontmatter values an earlier defective rebuild wrote wrongly: an `entity` that disagrees
    /// with the folder's location, a `status: new` stamped on every index, and a `created` that only repeats
    /// `updated` where Git history or the filesystem knows an earlier date. Off by default, since it overrides values
    /// that may have been set on purpose; combine with `dry_run` to list the corrections first.
    #[serde(default)]
    repair: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct LogAppendInput {
    entry: String,
    /// Each entry is vault-relative or absolute, or
    /// `{name}://{relative-path}`; every file must resolve to
    /// the same configured vault.
    #[serde(default)]
    files: Vec<String>,
}
