use std::sync::Arc;

use contextos_core::{
    FirstReportedCreation, LocalClock, LocalClockInput, LogsOperations, MaintainsIndexes, OperationEvent,
    OperationRouter, OperationWarning, OperatorTimeZone, VaultRootId, VaultSet,
};
use contextos_fs::{
    FileNameIndex, Filesystem, FilesystemService, FilesystemServiceConfig, FsError, RoutedFilesystemServiceConfig,
};
use contextos_git::{Git2Vault, GitCommitResult, GitError};
use contextos_index::IndexService;
use contextos_mermaid::MermanParser;
use contextos_oplog::{OperationLog, OperationLogError};
use contextos_search::{
    ApiKeySource, EmbeddingProviderConfig, EmbedsText, SearchError, SemanticConfig, VaultSearchConfig,
    VaultSearchService,
};
use rmcp::handler::server::router::tool::ToolRoute;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::schemars;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::commit_schedule::VersionCollection;
use crate::module::{ModuleContext, ModuleRegistry};
use crate::projection_wiring::Projection;
use crate::server_clock::ServerClock;
use crate::server_compose as compose;
pub use crate::server_error::{ServerBuildError, ShutdownFlushError};
use crate::tools::vault::VaultInfoConfig;
use crate::vault_events::VaultEvents;
pub(crate) use crate::write_coordinator::WriteCoordinator;
use crate::{Config, EmbeddingProvider, VaultConfig};

/// The most files one vault's file name index holds. A vault with more fails to build the index, and links naming a
/// file outside the linking note's own folder tree then fail rather than resolve against a partial list.
pub(crate) const MAX_INDEXED_FILES: usize = 500_000;

#[derive(Clone)]
pub struct ContextOsServer {
    /// The effective configuration this server was built from, retained so
    /// `doctor`/`doctor_resolve` can reuse
    /// `DoctorReport::try_from(&Config)` unchanged, giving identical
    /// diagnostic content to `contextos doctor` on the CLI.
    pub(crate) config: Arc<Config>,
    /// The operator's local clock, for every time a tool writes into a vault.
    pub(crate) clock: VaultClock,
    pub(crate) roots: Arc<VaultSet>,
    pub(crate) filesystem: Arc<Filesystem>,
    pub(crate) mutations: Arc<RoutedMutationService>,
    /// The router `mutations` delivers to, kept so a service that writes on its own account (a rebuild, a manual log
    /// entry, a log flush) hands every write it makes to the same Git and search services.
    pub(crate) router: Arc<SubstrateRouter>,
    pub(crate) writes: WriteCoordinator,
    pub(crate) destructive_delete: Arc<Vec<bool>>,
    pub(crate) indexes: Arc<Vec<Option<ManagedIndexService>>>,
    pub(crate) operation_logs: Arc<Vec<Option<ManagedOperationLog>>>,
    pub(crate) git: Arc<Vec<Option<ManagedGit>>>,
    pub(crate) git_debounce_seconds: Arc<Vec<u64>>,
    pub(crate) search: Arc<Vec<Option<Arc<VaultSearchService>>>>,
    /// Agent host projection as `doctor` and `doctor_resolve` assess and repair it; [`Projection::Disabled`] for a
    /// configuration that does not project.
    pub(crate) projection: Projection,
    /// Per-vault, index-aligned with `roots`: every file in the vault by name, which answers a link that names a file
    /// outside the linking note's own folder tree.
    pub(crate) file_names: Arc<Vec<FileNameIndex>>,
    pub(crate) rebuild_budget_seconds: Arc<Vec<u64>>,
    pub(crate) vault_info: Arc<VaultInfoConfig>,
    pub(crate) mermaid: Arc<MermanParser>,
    /// Per-vault, index-aligned with `roots`: glob patterns naming which
    /// files `resources/list` enumerates for that vault; empty means that vault reports nothing via
    /// `resources/list`. Scoped to this one surface only; every other
    /// enumeration tool keeps using `hidden` alone via `filesystem`.
    pub(crate) resources_list_include: Arc<Vec<Vec<String>>>,
    pub(crate) module_router: Arc<rmcp::handler::server::router::tool::ToolRouter<Self>>,
    /// Size in bytes at or above which a text-reading tool result attaches
    /// a `resource_link` content block; precomputed once
    /// from `[server] resource_link_threshold_kb` at construction.
    pub(crate) resource_link_threshold_bytes: u64,
}

/// The clock behind every time the server writes into a vault: the current instant at the operator's local offset
/// (`[server] timezone`, else the host's zone), never UTC.
pub(crate) type VaultClock = LocalClock<ServerClock>;
pub(crate) type ManagedIndexService =
    IndexService<Filesystem, FilesystemService<VaultClock>, VaultClock, ManagedCreation>;
type ManagedCreation = FirstReportedCreation<Option<ManagedGit>, Filesystem>;
pub(crate) type ManagedOperationLog = OperationLog<FilesystemService<VaultClock>>;
pub(crate) type ManagedGit = Git2Vault<VaultClock>;
pub(crate) type SubstrateRouter = OperationRouter<
    IndexCollection,
    OperationLogCollection,
    VersionCollection,
    SearchCollection,
    Projection,
    VaultEvents,
>;
pub(crate) type RoutedMutationService = FilesystemService<VaultClock, SubstrateRouter>;

#[derive(Clone, Debug)]
pub(crate) struct IndexCollection {
    pub(crate) services: Arc<Vec<Option<ManagedIndexService>>>,
}

impl MaintainsIndexes for IndexCollection {
    fn reconcile(&self, event: &OperationEvent) -> Result<Vec<OperationEvent>, OperationWarning> {
        let mut events = Vec::new();
        for service in self.services.iter().flatten() {
            events.extend(service.reconcile(event)?);
        }
        Ok(events)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct OperationLogCollection {
    pub(crate) services: Arc<Vec<Option<ManagedOperationLog>>>,
}

impl LogsOperations for OperationLogCollection {
    fn append(&self, event: &OperationEvent) -> Result<Vec<OperationEvent>, OperationWarning> {
        let mut events = Vec::new();
        for service in self.services.iter().flatten() {
            events.extend(service.append(event)?);
        }
        Ok(events)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SearchCollection {
    pub(crate) services: Arc<Vec<Option<Arc<VaultSearchService>>>>,
}

impl contextos_core::UpdatesSearch for SearchCollection {
    fn update(&self, event: &OperationEvent) -> Result<(), OperationWarning> {
        for service in self.services.iter().flatten() {
            service.update(event)?;
        }
        Ok(())
    }

    fn update_all(&self, events: &[OperationEvent]) -> Result<(), OperationWarning> {
        for service in self.services.iter().flatten() {
            service.update_all(events)?;
        }
        Ok(())
    }
}

impl ContextOsServer {
    /// Every tool that ships in this binary regardless of runtime `Config`,
    /// i.e. the complete build-time set, with no extension module tools
    /// merged in. Every ephemeris tool handler is always compiled
    /// in and so always appears here, whether or not `[server] astro` is
    /// set; this is the base build-complete/reserved-name set transport
    /// parity tests and module-name collision checks reason about, not
    /// what any one running instance actually advertises. See
    /// [`Self::effective_catalogue`] for that.
    #[must_use]
    pub fn catalogue() -> rmcp::handler::server::router::tool::ToolRouter<Self> {
        let mut router = Self::core_tool_router();
        router.merge(Self::ephemeris_tool_router());
        sanitise_router_schemas(&mut router);
        router
    }

    /// Every built-in tool this instance actually advertises and can
    /// dispatch: the core catalogue, the `ephemeris_*` tools only if this
    /// instance's `[server] astro` is set (runtime, not a Cargo
    /// feature, so a single build serves every operator, opted in or not),
    /// plus every registered extension module's namespaced tools. This is
    /// what dispatch actually uses.
    #[must_use]
    pub fn effective_catalogue(&self) -> rmcp::handler::server::router::tool::ToolRouter<Self> {
        let mut router = Self::core_tool_router();
        if self.config.server.astro {
            router.merge(Self::ephemeris_tool_router());
        }
        router.merge(self.module_router.as_ref().clone());
        sanitise_router_schemas(&mut router);
        router
    }

    /// Every built-in tool that carries no runtime visibility toggle:
    /// shared by [`Self::catalogue`] (which always adds the ephemeris
    /// tools on top) and [`Self::effective_catalogue`] (which adds them
    /// only when `[server] astro` is set for this instance).
    fn core_tool_router() -> rmcp::handler::server::router::tool::ToolRouter<Self> {
        let mut router = Self::tool_router();
        router.merge(Self::mermaid_tool_router());
        router.merge(Self::git_tool_router());
        router.merge(Self::query_tool_router());
        router.merge(Self::query_blended_tool_router());
        router.merge(Self::vault_tool_router());
        router.merge(Self::agents_tool_router());
        router.merge(Self::obsidian_tool_router());
        router.merge(Self::doctor_tool_router());
        router
    }

    /// Builds the per-call capability context handed to every extension
    /// module tool. Cloning is cheap: every field is an `Arc` or an
    /// `Arc`-backed coordinator.
    fn module_context(&self) -> ModuleContext {
        ModuleContext {
            roots: Arc::clone(&self.roots),
            reads: Arc::clone(&self.filesystem),
            mutations: Arc::clone(&self.mutations),
            writes: self.writes.clone(),
            search: Arc::clone(&self.search),
        }
    }
}

/// Construction input pairing server configuration with the extension
/// modules to register. [`TryFrom<Config>`] delegates
/// here with an empty [`ModuleRegistry`], so every existing caller and test
/// that only supplies [`Config`] is unaffected: the effective catalogue
/// stays byte-identical to the core catalogue whenever no module is
/// registered.
pub struct ServerBuildConfig {
    pub config: Config,
    pub modules: ModuleRegistry,
}

/// What a composed server is for, which decides how its start-up treats Git and search.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Purpose {
    /// A server that goes on to serve: Git's start-up work is performed (maintaining the ignore rules, giving a
    /// repository with no commits its first commit, and committing changes an earlier process left pending), and a
    /// vault whose search state cannot be opened stops construction, since every later write would otherwise skip it.
    Serve,
    /// The one-shot `contextos doctor --resolve`, which must report every write it makes and do exactly what its dry
    /// run previewed: Git's start-up work is left to the findings that resolve it, and a vault whose search state
    /// cannot be opened is resolved without search, reported as a [`SearchUnavailable`] beside the outcomes.
    Resolve,
}

/// A vault whose search state could not be opened while composing for [`Purpose::Resolve`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SearchUnavailable {
    /// The vault's index into `Config::vaults`.
    pub(crate) vault_index: usize,
    /// Why its search state could not be opened.
    pub(crate) reason: String,
}

impl TryFrom<Config> for ContextOsServer {
    type Error = ServerBuildError;

    fn try_from(value: Config) -> Result<Self, Self::Error> {
        Self::try_from(ServerBuildConfig {
            config: value,
            modules: ModuleRegistry::new(),
        })
    }
}

impl TryFrom<ServerBuildConfig> for ContextOsServer {
    type Error = ServerBuildError;

    fn try_from(build: ServerBuildConfig) -> Result<Self, Self::Error> {
        Self::compose(build, Purpose::Serve, ServerClock::default()).map(|(server, _)| server)
    }
}

impl ContextOsServer {
    /// Builds a serving server whose every reading of the time comes from `clock` instead of the system clock, so
    /// anything that depends on today's date (the retention sweep, an event's `at`, the day an entry is filed under)
    /// can be made deterministic.
    ///
    /// # Errors
    ///
    /// Returns [`ServerBuildError`] when the configuration cannot be composed into a server.
    pub fn with_clock(build: ServerBuildConfig, clock: ServerClock) -> Result<Self, ServerBuildError> {
        Self::compose(build, Purpose::Serve, clock).map(|(server, _)| server)
    }

    /// Composes every service for the configured vaults for `purpose`, returning with it every vault whose search
    /// state could not be opened (always none for [`Purpose::Serve`], which fails instead). Crate-private so only the
    /// one-shot doctor resolution, which reports every write it makes, can compose for [`Purpose::Resolve`], and no
    /// server that goes on to serve is built without Git's start-up work or with a vault's search missing.
    pub(crate) fn compose(
        build: ServerBuildConfig,
        purpose: Purpose,
        time: ServerClock,
    ) -> Result<(Self, Vec<SearchUnavailable>), ServerBuildError> {
        let ServerBuildConfig { config: value, modules } = build;
        let roots = VaultSet::try_from(&value)?;
        let clock = VaultClock::from(LocalClockInput {
            clock: time,
            zone: OperatorTimeZone::try_from(&value.server)?,
        });
        let filesystem = compose::filesystem(&value, &roots)?;
        let plain_mutations = FilesystemService::from(FilesystemServiceConfig {
            filesystem: filesystem.clone(),
            clock: clock.clone(),
        });
        let git = Arc::new(compose::git_services(&value, &roots, &clock)?);
        let indexes = Arc::new(compose::index_services(&compose::IndexWiring {
            config: &value,
            roots: &roots,
            filesystem: &filesystem,
            writer: &plain_mutations,
            clock: &clock,
            git: &git,
        })?);
        let operation_logs = Arc::new(compose::operation_log_services(&value, &roots, &plain_mutations)?);
        if purpose == Purpose::Serve {
            compose::start_git(&git, &plain_mutations)?;
        }
        let debounce_seconds = Arc::new(
            value
                .vaults
                .iter()
                .map(|vault| vault.git.commit_debounce_s)
                .collect::<Vec<_>>(),
        );
        let (search, search_unavailable) = compose::search_services(&value, &roots, purpose)?;
        let search = Arc::new(search);
        let (doctor_projection, projection) = compose::projections(&value, &roots, &filesystem, purpose)?;
        let state_directories = compose::state_directories(&value, &roots)?;
        let events = compose::vault_events(
            &value,
            &state_directories,
            compose::VaultEventsParts {
                roots: roots.clone(),
                filesystem: filesystem.clone(),
                writer: plain_mutations.clone(),
                clock: clock.clone(),
            },
        )?;
        let writes = compose::write_coordinator(&state_directories);
        if purpose == Purpose::Serve {
            // Tasks already in the vault are recorded without events, so only later changes fire. Each vault is seeded
            // under its write lock, so a table another process has written since is never overwritten.
            events.seed_missing_tables(|index| writes.hold_host_lock(index));
        }
        let router = compose::router(compose::RouterParts {
            indexes: Arc::clone(&indexes),
            operation_logs: Arc::clone(&operation_logs),
            git: Arc::clone(&git),
            debounce_seconds: Arc::clone(&debounce_seconds),
            search: Arc::clone(&search),
            projection: projection.clone(),
            events,
        });
        let mutations = FilesystemService::from(RoutedFilesystemServiceConfig {
            filesystem: filesystem
                .clone()
                .with_server_owned_folders(crate::server_owned_folders::server_owned_folders()),
            clock: clock.clone(),
            services: router.clone(),
        });
        let module_router = Self::module_router(&modules)?;
        let resource_link_threshold_bytes = Self::resource_link_threshold_bytes(&value)?;
        let file_names = compose::file_name_indexes(&roots, &filesystem)?;
        // After every service is composed and before any transport can exist: edits made outside the server (in
        // Obsidian, or on the filesystem) reach the hosts at the next start. Never fails start-up.
        projection.refresh();
        let settings = compose::VaultSettings::from(&value);
        let server = Self {
            vault_info: Arc::new(VaultInfoConfig::from(&value)),
            destructive_delete: Arc::new(settings.destructive_delete),
            rebuild_budget_seconds: Arc::new(settings.rebuild_budget_seconds),
            resources_list_include: Arc::new(settings.resources_list_include),
            config: Arc::new(value),
            clock,
            roots: Arc::new(roots),
            // The same owned folders as the mutation service, so a tool that plans before it writes can be refused up
            // front by the same rule.
            filesystem: Arc::new(
                filesystem.with_server_owned_folders(crate::server_owned_folders::server_owned_folders()),
            ),
            file_names: Arc::new(file_names),
            mutations: Arc::new(mutations),
            router: Arc::new(router),
            writes,
            indexes,
            operation_logs,
            git,
            git_debounce_seconds: debounce_seconds,
            search,
            projection: doctor_projection,
            mermaid: Arc::new(MermanParser::new()),
            resource_link_threshold_bytes,
            module_router: Arc::new(module_router),
        };
        Ok((server, search_unavailable))
    }

    /// The size at or above which a text-reading result attaches a `resource_link`, from `[server]
    /// resource_link_threshold_kb`.
    fn resource_link_threshold_bytes(config: &Config) -> Result<u64, ServerBuildError> {
        if config.server.resource_link_threshold_kb == 0 {
            return Err(ServerBuildError::ResourceLinkThresholdZero);
        }
        config
            .server
            .resource_link_threshold_kb
            .checked_mul(1024)
            .ok_or(ServerBuildError::ResourceLinkThresholdOverflow)
    }

    /// The dispatch routes of the registered modules, after checking that none reuses a tool name this server owns.
    fn module_router(
        modules: &ModuleRegistry,
    ) -> Result<rmcp::handler::server::router::tool::ToolRouter<Self>, ServerBuildError> {
        let reserved_tool_names: std::collections::HashSet<String> = Self::catalogue()
            .list_all()
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect();
        modules.validate(&reserved_tool_names)?;
        Ok(build_module_router(modules))
    }
}

/// Applies [`crate::resource_support::sanitise_nullable_unions`] to every
/// route's advertised `input_schema` and `output_schema` in place, without
/// touching its dispatch handler: the single choke point [`Self::catalogue`]
/// and [`Self::effective_catalogue`] both call after every `merge` (core,
/// ephemeris, and registered modules alike), so no tool this server can
/// ever advertise, built in or extension, carries a nullable-union shape
/// confirmed to take down Cowork's tool registry.
fn sanitise_router_schemas(router: &mut rmcp::handler::server::router::tool::ToolRouter<ContextOsServer>) {
    for route in router.map.values_mut() {
        let mut input = serde_json::Value::Object((*route.attr.input_schema).clone());
        crate::resource_support::sanitise_nullable_unions(&mut input);
        crate::resource_support::inline_local_refs(&mut input);
        if let serde_json::Value::Object(map) = input {
            route.attr.input_schema = Arc::new(map);
        }

        if let Some(output) = route.attr.output_schema.take() {
            let mut value = serde_json::Value::Object((*output).clone());
            crate::resource_support::sanitise_nullable_unions(&mut value);
            crate::resource_support::inline_local_refs(&mut value);
            if let serde_json::Value::Object(map) = value {
                route.attr.output_schema = Some(Arc::new(map));
            }
        }
    }
}

/// Builds the dispatch route for every tool every registered module
/// contributes. Each route resolves its [`ModuleContext`] fresh from the
/// live server instance at call time (`tcc.service`), so a module never
/// holds server state directly.
fn build_module_router(modules: &ModuleRegistry) -> rmcp::handler::server::router::tool::ToolRouter<ContextOsServer> {
    let mut router = rmcp::handler::server::router::tool::ToolRouter::<ContextOsServer>::new();
    for module in modules.iter() {
        for tool in module.tools() {
            let module = Arc::clone(module);
            router.add_route(ToolRoute::new_dyn(
                tool,
                move |tcc: ToolCallContext<'_, ContextOsServer>| {
                    let module = Arc::clone(&module);
                    let ctx = tcc.service.module_context();
                    let call = crate::module::ModuleCall {
                        name: tcc.name.to_string(),
                        arguments: tcc.arguments.clone().unwrap_or_default(),
                        input_responses: tcc.input_responses.clone(),
                        request_state: tcc.request_state.clone(),
                        protocol_version: tcc.request_context.protocol_version(),
                        client_capabilities: tcc.request_context.client_capabilities(),
                    };
                    Box::pin(async move { module.handle(call, &ctx).await.map(crate::module::refuse_tasks) })
                },
            ));
        }
    }
    router
}

/// Opens one vault's search service from its configuration: its state directory, its semantic provider, and the
/// indexes it keeps there.
pub(crate) fn open_vault_search(
    config: &VaultConfig,
    root: &contextos_core::VaultRoot,
    root_id: VaultRootId,
    purpose: Purpose,
) -> Result<VaultSearchService, ServerBuildError> {
    let state_directory = crate::state_dir::resolve_state_directory(config.state_directory.as_deref(), root.path())?;
    let semantic = semantic_config(config, &state_directory).inspect_err(|error| {
        log_search_unopened(root, &state_directory, error, purpose);
    })?;
    tracing::info!(
        vault = %root.path().display(),
        state_directory = %state_directory.display(),
        text_enabled = config.search.text,
        graph_enabled = config.search.graph,
        graph_backend = ?config.search.graph_backend,
        semantic_enabled = semantic.is_some(),
        "opening vault search service"
    );
    VaultSearchService::try_from(VaultSearchConfig {
        root_id,
        root: root.path().to_path_buf(),
        excludes: config.search.exclude.clone(),
        link_excludes: config.links.exclude.clone(),
        link_ignore_targets: config.links.ignore_targets.clone(),
        hidden: config.hidden.clone(),
        state_directory: state_directory.clone(),
        text_enabled: config.search.text,
        graph_enabled: config.search.graph,
        graph_backend: config.search.graph_backend.into(),
        semantic,
    })
    .inspect_err(|error| log_search_unopened(root, &state_directory, error, purpose))
    .map_err(ServerBuildError::from)
}

/// Records why a vault's search could not be opened, pairing the vault and its resolved state directory with the
/// stable error code, so a hard-to-reproduce start-up failure is diagnosable from a log line alone. For
/// [`Purpose::Serve`] it is the last line an operator sees before construction stops over this one vault
/// (`ServerBuildError` has no further context to add).
fn log_search_unopened(
    root: &contextos_core::VaultRoot,
    state_directory: &std::path::Path,
    error: &SearchError,
    purpose: Purpose,
) {
    match purpose {
        Purpose::Serve => tracing::error!(
            vault = %root.path().display(),
            state_directory = %state_directory.display(),
            code = error.code(),
            error = %error,
            "vault search service failed to open; aborting server construction"
        ),
        Purpose::Resolve => tracing::warn!(
            vault = %root.path().display(),
            state_directory = %state_directory.display(),
            code = error.code(),
            error = %error,
            "vault search service failed to open; resolving this vault without search"
        ),
    }
}

/// Builds this vault's semantic search capability from configuration
/// (`[vault.search] semantic`, `[vault.search.embedding]`), or `None` when
/// semantic search is off for this vault. The vector store lives at
/// `<state_directory>/vectors.db`, matching the vault's other derived
/// state.
pub(crate) fn semantic_config(
    config: &VaultConfig,
    state_directory: &std::path::Path,
) -> Result<Option<SemanticConfig>, SearchError> {
    if !config.search.semantic {
        return Ok(None);
    }
    let provider_config = embedding_provider_config(&config.search.embedding)?;
    let embedder: Box<dyn EmbedsText> = provider_config.try_into()?;
    Ok(Some(SemanticConfig {
        embedder,
        vector_store_path: state_directory.join("vectors.db"),
    }))
}

/// Converts the server's TOML-facing embedding configuration into
/// `contextos-search`'s vault-agnostic provider selection, so provider
/// construction stays purely configuration-driven (swapping `provider`
/// changes which variant is built, with no code change).
///
/// # Errors
///
/// Returns [`SearchError::EmbeddingConfig`] when `provider = "local"` and no
/// `model_directory` is configured; the directory to look in must always
/// be explicit. That directory need not be the exact flat directory
/// `FastembedLocal` reads from: `crate::model_cli::resolve_model_directory`
/// resolves a shared `contextos model download` cache root to the actual
/// model snapshot directory first.
fn embedding_provider_config(embedding: &crate::EmbeddingConfig) -> Result<EmbeddingProviderConfig, SearchError> {
    match embedding.provider {
        EmbeddingProvider::Local => {
            let model_directory = embedding
                .model_directory
                .clone()
                .ok_or_else(|| SearchError::EmbeddingConfig {
                    reason: "provider = \"local\" requires [vault.search.embedding] \
                             model_directory to be configured"
                        .to_owned(),
                })?;
            let model_directory = crate::model_cli::resolve_model_directory(&model_directory);
            Ok(EmbeddingProviderConfig::Local { model_directory })
        }
        EmbeddingProvider::OpenaiCompatible => {
            // Configuration load refuses both being set, so at most one is non-empty here.
            let api_key = if !embedding.api_key.is_empty() {
                ApiKeySource::Value(embedding.api_key.clone())
            } else if !embedding.api_key_env.is_empty() {
                ApiKeySource::Environment(embedding.api_key_env.clone())
            } else {
                return Err(SearchError::EmbeddingConfig {
                    reason: "provider = \"openai-compatible\" requires [vault.search.embedding] api_key or \
                             api_key_env to be configured"
                        .to_owned(),
                });
            };
            Ok(EmbeddingProviderConfig::OpenAiCompatible {
                endpoint: embedding.endpoint.clone(),
                model: embedding.model.clone(),
                api_key,
            })
        }
    }
}

impl ContextOsServer {
    /// The same server, waiting at most `wait` for another process to release the host-level projection lock before
    /// reporting `projection/busy`. A test seam: the bound is otherwise the write coordinator's.
    #[doc(hidden)]
    #[must_use]
    pub fn with_projection_lock_wait(self, wait: std::time::Duration) -> Self {
        self.projection.set_lock_wait(wait);
        self
    }

    /// The same server, with `observer` told each step of a skill folder swap into an agent host folder, so a test can
    /// fail one at an exact point. A test seam: nothing in production sets an observer.
    #[doc(hidden)]
    #[must_use]
    pub fn with_projection_folder_observer(
        self,
        observer: std::sync::Arc<dyn contextos_fs::host_file::FolderReplaceObserver + Send + Sync>,
    ) -> Self {
        self.projection.set_folder_observer(observer);
        self
    }

    /// Flushes every pending MCP-owned Git batch, including during graceful shutdown.
    ///
    /// # Errors
    ///
    /// Returns the first typed Git failure while committing a configured vault.
    pub fn flush_git(&self) -> Result<Vec<GitCommitResult>, GitError> {
        let mut results = Vec::new();
        let mut first_error = None;
        for service in self.git.iter().flatten() {
            match service.commit(None) {
                Ok(result) => results.push(result),
                Err(error) if first_error.is_none() => first_error = Some(error),
                Err(error) => tracing::warn!(
                    code = error.code(),
                    error = %error,
                    "additional Git vault failed during graceful flush"
                ),
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(results),
        }
    }

    /// Retries buffered operation-log records for every configured vault.
    ///
    /// All vaults are attempted before the first typed failure is returned.
    ///
    /// # Errors
    ///
    /// Returns the first operation-log failure after attempting every vault.
    pub fn flush_operation_logs(&self) -> Result<Vec<OperationEvent>, OperationLogError<FsError>> {
        let mut events = Vec::new();
        let mut first_error = None;
        for service in self.operation_logs.iter().flatten() {
            match service.flush(self.router.as_ref()) {
                Ok(flushed) => {
                    for warning in &flushed.warnings {
                        tracing::warn!(
                            code = warning.code,
                            error = warning.message,
                            "flushed operation-log entry was written but not fully routed"
                        );
                    }
                    events.extend(flushed.events);
                }
                Err(error) if first_error.is_none() => first_error = Some(error),
                Err(error) => tracing::warn!(
                    code = error.code(),
                    error = %error,
                    "additional operation-log vault failed during graceful flush"
                ),
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(events),
        }
    }

    /// Flushes retryable operation-log state and pending Git batches at shutdown.
    ///
    /// Both substrates are attempted even when one reports a failure.
    ///
    /// # Errors
    ///
    /// Returns the first typed substrate failure after both flushes are attempted.
    pub fn flush_substrates(&self) -> Result<(), ShutdownFlushError> {
        let operation_log_error = self.flush_operation_logs().err();
        let git_error = self.flush_git().err();
        match (operation_log_error, git_error) {
            (Some(error), Some(git_error)) => {
                tracing::warn!(
                    code = git_error.code(),
                    error = %git_error,
                    "Git also failed during graceful substrate flush"
                );
                Err(ShutdownFlushError::OperationLog(error))
            }
            (Some(error), None) => Err(ShutdownFlushError::OperationLog(error)),
            (None, Some(error)) => Err(ShutdownFlushError::Git(error)),
            (None, None) => Ok(()),
        }
    }

    /// Starts building each vault's file name index in the background, so the first link that needs it finds it
    /// ready. Nothing waits on this: an index not yet built is built by the first question that needs it, and one that
    /// fails to build is retried then. It does nothing outside a Tokio runtime.
    ///
    /// A walk in progress cannot be cancelled, so shutting the runtime down waits for the walks still running, which
    /// is one walk of each vault at most.
    pub fn warm_file_name_indexes(&self) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        for position in 0..self.file_names.len() {
            let indexes = Arc::clone(&self.file_names);
            runtime.spawn_blocking(move || {
                if let Some(Err(error)) = indexes.get(position).map(FileNameIndex::rescan) {
                    tracing::warn!(code = error.code(), "file name index could not be built at startup");
                }
            });
        }
    }

    /// Spawns one background task per vault with semantic search enabled,
    /// each repeatedly draining that vault's embedding queue on its own
    /// (`crate::semantic_drain`) so the semantic index keeps pace with
    /// live edits without an operator ever having to call
    /// `query_index_rebuild` or `contextos index` manually. `shutdown`
    /// stops every spawned task promptly; the caller should await each
    /// returned handle as part of its own graceful-shutdown sequence.
    #[must_use]
    pub fn spawn_semantic_drain(&self, shutdown: &CancellationToken) -> Vec<tokio::task::JoinHandle<()>> {
        crate::semantic_drain::spawn(&self.search, &self.rebuild_budget_seconds, shutdown)
    }
}

pub(crate) struct WarningMessages(pub(crate) Vec<String>);

impl From<Vec<OperationWarning>> for WarningMessages {
    fn from(value: Vec<OperationWarning>) -> Self {
        Self(
            value
                .into_iter()
                .map(|warning| format!("{}: {}", warning.code, warning.message))
                .collect(),
        )
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct PathInput {
    /// Vault-relative or absolute path, or `{name}://{relative-path}` to
    /// address a specific configured vault by name; use
    /// `{name}://.` to address that vault's root.
    pub(crate) path: String,
}

#[cfg(test)]
#[path = "server_test.rs"]
mod tests;
