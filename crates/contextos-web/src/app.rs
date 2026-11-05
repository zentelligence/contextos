//! Startup orchestration and router assembly.
//!
//! [`start_until_shutdown`] attempts every configured `[[mcp_server]]` session (by `server/discover`, or by rmcp's
//! fall back to an `initialize` handshake for a server without protocol revision 2026-07-28) and resolves once each
//! attempt has ended, so the long-running server never serves a page against a session that is still opening. A
//! server that cannot be connected is kept with its cause rather than stopping start-up, and is connected again from
//! `/settings/`. [`connect`] is the stricter form a one-shot command uses: a stdio server that cannot start ends it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use axum::Router;
use axum::routing::{get, post};

pub use crate::app_routers::shared_router;
use crate::app_routers::{
    apps_router, entity_docs_router, file_carrying_body_limit, inbox_outbox_router, insights_router, journal_router,
    notes_router, packages_router, profile_router, search_router, settings_router, shell_favourites_router,
    tasks_router, vault_health_router, vault_management_router, vault_router, vault_settings_router, wiki_router,
    workflows_router,
};
use crate::config::WebConfig;
use crate::config::WorkflowsConfig;
use crate::credential_store::{FsCredentialStore, credentials_directory};
use crate::last_visited;
use crate::logging::{Clock, SystemClock};
use crate::mcp_client::{McpClientSet, McpConnectError};
use crate::package_store::AppDataRoots;
use crate::recently_accessed;
use crate::routes::CommonRoutesState;
use crate::routes::agents::AgentsRoutesState;
use crate::routes::apps::AppRoutesState;
use crate::routes::entity_docs::EntityDocsRoutesState;
use crate::routes::events_stream::EventStreamRoutesState;
use crate::routes::inbox_outbox::InboxOutboxRoutesState;
use crate::routes::insights::InsightsRoutesState;
use crate::routes::journal::JournalRoutesState;
use crate::routes::notes::NotesRoutesState;
use crate::routes::packages::PackagesRoutesState;
use crate::routes::profile::ProfileRoutesState;
use crate::routes::search::SearchRoutesState;
use crate::routes::settings::SettingsRoutesState;
use crate::routes::shared_workflows::{SharedRoots, SharedRoutesState};
use crate::routes::shell::ShellFavouritesRoutesState;
use crate::routes::signals::SignalsRoutesState;
use crate::routes::tasks::TasksRoutesState;
use crate::routes::vault::VaultRoutesState;
use crate::routes::vault_health::VaultHealthRoutesState;
use crate::routes::vault_management::VaultManagementRoutesState;
use crate::routes::vault_settings::VaultSettingsRoutesState;
use crate::routes::wiki::WikiRoutesState;
use crate::routes::workflows::WorkflowRoutesState;
use crate::run_settings::HostContext;
use crate::{proxy, routes, static_assets};
use contextos_workflows::{Clock as WorkflowClock, SystemClock as WorkflowSystemClock};

/// Connects every `[[mcp_server]]` entry in `config`. A `Stdio` entry's connection failure still
/// fails fast (a local, fully-owned configuration or packaging bug). An `Http` entry's does not:
/// see [`McpClientSet::connect_with`] for why a remote server's own outage or a stale/revoked
/// credential must not take the whole process down with it.
///
/// `web_config_path` locates the `credentials/` directory sibling to `web.toml`
/// ([`credentials_directory`]), the same store `routes::settings::SettingsRoutesState` reads and
/// writes: an `oauth2`-mode HTTP entry's initial session, and every later refresh check
/// (`mcp_client::McpClient::ensure_oauth_token_fresh`), reads and persists its token pair there.
///
/// # Errors
///
/// Propagates the first [`McpConnectError`] any `Stdio` entry produces.
pub async fn connect(config: &WebConfig, web_config_path: &Path) -> Result<Arc<McpClientSet>, McpConnectError> {
    connect_until_shutdown(config, web_config_path, &tokio_util::sync::CancellationToken::new()).await
}

/// Connects every `[[mcp_server]]` entry for the long-running server, which serves whatever the outcome: an entry that
/// cannot be connected, stdio or HTTP, is kept with its cause for `/settings/` to show and connect again
/// ([`McpClientSet::start_until_shutdown`]). The credential store is located as [`connect`] locates it.
///
/// # Errors
///
/// Returns [`McpConnectError::Shutdown`] when shutdown is requested before every attempt ends.
pub async fn start_until_shutdown(
    config: &WebConfig,
    web_config_path: &Path,
    shutdown: &tokio_util::sync::CancellationToken,
) -> Result<Arc<McpClientSet>, McpConnectError> {
    let credential_store = Arc::new(FsCredentialStore::new(credentials_directory(web_config_path)));
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let clients = McpClientSet::start_until_shutdown(&config.mcp_servers, &credential_store, &clock, shutdown).await?;
    Ok(Arc::new(clients))
}

/// Connects configured MCP sessions for a one-shot command, closing completed sessions if startup is cancelled.
///
/// # Errors
/// Returns a connection failure or [`McpConnectError::Shutdown`].
pub async fn connect_until_shutdown(
    config: &WebConfig,
    web_config_path: &Path,
    shutdown: &tokio_util::sync::CancellationToken,
) -> Result<Arc<McpClientSet>, McpConnectError> {
    let credential_store = Arc::new(FsCredentialStore::new(credentials_directory(web_config_path)));
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let clients =
        McpClientSet::connect_until_shutdown(&config.mcp_servers, &credential_store, &clock, shutdown).await?;
    Ok(Arc::new(clients))
}

/// Builds the full HTTP router: the MCP proxy route, `/static/`, the vault
/// content routes, the app registry routes, and
/// `/settings/`.
///
/// Vault content, app registry, and settings routes issue every tool call
/// against the primary server (`web.toml`'s first configured `[[mcp_server]]`
/// entry, always the local `contextos-mcp` instance), read from `clients` per
/// request so a primary added or changed in `/settings/` is used at once, distinct
/// from the MCP proxy route, which is addressed by whatever `server_name` the caller
/// names in the URL itself. A registered app's own `manifest.toml`
/// `mcp_servers` allow-list is validated against every configured
/// `[[mcp_server]]` name, connected or not (`clients.names()`, so
/// an app may depend on a non-primary server too); `/settings/`'s own
/// registered-app-dependency check reuses the identical app-discovery path
/// for the same reason.
///
/// App discovery runs lazily, on each vault's first registry-route
/// request, and is cached thereafter ([`routes::apps::AppRoutesState`]);
/// this mirrors the vault content routes' own established pattern of
/// resolving a vault's identity per request through an MCP tool call
/// rather than this crate maintaining a second, locally pre-loaded vault
/// registry. `/settings/`'s dependency check runs its own, uncached
/// discovery pass per write instead (`routes::settings`), since a stale
/// cache could let a removal through that a fresh scan would have blocked.
///
/// `web_config_path` is `web.toml`'s own path on disk, read and
/// validate-then-written by `/settings/`; it is unrelated to `static_dir`
/// even though both are configured under `web.toml`'s own `[server]` table.
///
/// `/api/v1/shared/packages` (the host's package store's own route
/// group) resolves its own store root independently
/// (`package_store::default_store_root`) rather than taking one as a
/// parameter here: no vault, no MCP client, not even `web.toml` itself is
/// consulted by that route group, which is exactly what lets it be
/// reachable before any of the rest of this router's own dependencies
/// exist. On the rare platform where no app-data directory can be
/// resolved at all, that route group is simply not mounted rather than
/// failing this whole function - every other route stays reachable.
pub fn build_router(clients: Arc<McpClientSet>, static_dir: Option<&Path>, web_config_path: &Path) -> Router {
    build_router_with(clients, static_dir, web_config_path, &WorkflowsConfig::default())
}

/// The job routes' state: the primary server and the `[workflows]` defaults a manually triggered run starts with.
fn job_routes_state(
    common: &CommonRoutesState,
    workflows: &WorkflowsConfig,
    roots: Option<&AppDataRoots>,
) -> WorkflowRoutesState {
    WorkflowRoutesState::new(
        Arc::clone(&common.clients),
        HostContext {
            attention: crate::job_attention::JobAttentionMemory::default(),
            log_level: workflows.log_level,
            claims_root: roots.map(|roots| roots.claims.clone()),
            clock: Arc::clone(&common.vault_time),
        },
        Arc::clone(&common.web_config_path),
    )
}

/// [`build_router`], with the `[workflows]` defaults from `web.toml` that a manually triggered job run uses.
/// [`build_router`] itself runs jobs with the built-in defaults.
pub fn build_router_with(
    clients: Arc<McpClientSet>,
    static_dir: Option<&Path>,
    web_config_path: &Path,
    workflows: &WorkflowsConfig,
) -> Router {
    let roots = AppDataRoots::platform()
        .map_err(|error| {
            tracing::warn!(%error, "the shared workflow pages are not mounted and no job can be run from a page: the app-data directory could not be resolved");
        })
        .ok();
    build_router_in(clients, static_dir, web_config_path, workflows, roots.as_ref())
}

/// [`build_router_with`], with the host's job data, shared workflows, claims, and run records under `roots`, or none when
/// there is nowhere to keep them. A test passes a temporary directory, so it never touches the operator's own.
pub fn build_router_in(
    clients: Arc<McpClientSet>,
    static_dir: Option<&Path>,
    web_config_path: &Path,
    workflows: &WorkflowsConfig,
    roots: Option<&AppDataRoots>,
) -> Router {
    build_router_at(
        clients,
        static_dir,
        web_config_path,
        workflows,
        roots,
        Arc::new(WorkflowSystemClock),
    )
}

/// [`build_router_in`], reading "now" from `clock` wherever a screen decides a date for a new record (a new
/// decision's date, a held task's review date), so a test can fix the instant.
pub fn build_router_at(
    clients: Arc<McpClientSet>,
    static_dir: Option<&Path>,
    web_config_path: &Path,
    workflows: &WorkflowsConfig,
    roots: Option<&AppDataRoots>,
    clock: Arc<dyn WorkflowClock>,
) -> Router {
    build_router_closing(
        clients,
        static_dir,
        web_config_path,
        workflows,
        roots,
        clock,
        CancellationToken::new(),
    )
}

/// [`build_router_with`] for the host: every open live event stream ends promptly once `shutdown` is cancelled, so the
/// server's request drain is not held by a connection that never ends on its own.
pub fn build_router_with_shutdown(
    clients: Arc<McpClientSet>,
    static_dir: Option<&Path>,
    web_config_path: &Path,
    workflows: &WorkflowsConfig,
    shutdown: CancellationToken,
) -> Router {
    let roots = AppDataRoots::platform()
        .map_err(|error| {
            tracing::warn!(%error, "the shared workflow pages are not mounted and no job can be run from a page: the app-data directory could not be resolved");
        })
        .ok();
    build_router_closing(
        clients,
        static_dir,
        web_config_path,
        workflows,
        roots.as_ref(),
        Arc::new(WorkflowSystemClock),
        shutdown,
    )
}

/// [`build_router_at`], with the token that ends the live event streams.
fn build_router_closing(
    clients: Arc<McpClientSet>,
    static_dir: Option<&Path>,
    web_config_path: &Path,
    workflows: &WorkflowsConfig,
    roots: Option<&AppDataRoots>,
    clock: Arc<dyn WorkflowClock>,
    shutdown: CancellationToken,
) -> Router {
    let web_config_path_buf = Arc::new(web_config_path.to_path_buf());
    // Four separate state types cannot share one `Router<S>`'s single state slot, so each sub-router resolves its own
    // state before all are merged into one `Router<()>`.
    let proxy_and_static = Router::new()
        .route(
            "/mcp/{server_name}/{tool_name}",
            post(proxy::handle).layer(file_carrying_body_limit()),
        )
        .route("/shell/sidebar-toggle", post(routes::shell::toggle))
        .nest_service("/static", static_assets::service(static_dir))
        .with_state(Arc::clone(&clients));
    let router = proxy_and_static.merge(feature_routers(
        &clients,
        &web_config_path_buf,
        web_config_path,
        workflows,
        roots,
        clock,
        shutdown,
    ));
    let router = mount_host_routes(router, clients, &web_config_path_buf, roots, workflows);
    router
        // Whole-router, applied last (outermost): records every full-page navigation as the header
        // Favourites panel's own "Recently accessed" entry (`recently_accessed::track`'s own doc
        // comment), regardless of which merged sub-router actually served the request.
        .layer(axum::middleware::from_fn(recently_accessed::track))
        // Whole-router, same placement: records a real Wiki/Insights/entity-folder file visit as
        // that screen's own "resume where I left off" cookie entry (`last_visited::track`'s own doc
        // comment) - a distinct cookie from `recently_accessed`'s, so layer order between the
        // two does not matter.
        .layer(axum::middleware::from_fn(last_visited::track))
        // Outermost of all: a request naming another site's host is refused before any route or tracker runs.
        .layer(axum::middleware::from_fn(crate::loopback_host::refuse_other_hosts))
}

/// Every feature's routes merged into one router. Every "simple-shape" feature's own state is the identical triple
/// (`routes::CommonRoutesState`'s own doc), built once and converted per feature.
fn feature_routers(
    clients: &Arc<McpClientSet>,
    web_config_path_buf: &Arc<PathBuf>,
    web_config_path: &Path,
    workflows: &WorkflowsConfig,
    roots: Option<&AppDataRoots>,
    clock: Arc<dyn WorkflowClock>,
    shutdown: CancellationToken,
) -> Router {
    let common = CommonRoutesState::from(crate::routes::CommonRoutesInput {
        clients: Arc::clone(clients),
        web_config_path: Arc::clone(web_config_path_buf),
        clock,
    });
    let vault_state: VaultRoutesState = common.clone().into();
    let agents_state: AgentsRoutesState = common.clone().into();
    let signals_state: SignalsRoutesState = common.clone().into();
    let overview_state: crate::routes::overview_panels::OverviewRoutesState = common.clone().into();
    let event_stream_state = event_stream_state(clients, web_config_path_buf, &common, shutdown);
    let app_state = AppRoutesState::new(Arc::clone(clients), Arc::clone(web_config_path_buf));
    let job_state = job_routes_state(&common, workflows, roots);
    let tasks_state: TasksRoutesState = common.clone().into();
    let vault_settings_state: VaultSettingsRoutesState = common.clone().into();
    let vault_management_state: VaultManagementRoutesState = common.clone().into();
    let vault_health_state: VaultHealthRoutesState = common.clone().into();
    let inbox_outbox_state: InboxOutboxRoutesState = common.clone().into();
    let journal_state: JournalRoutesState = common.clone().into();
    let notes_state: NotesRoutesState = common.clone().into();
    let wiki_state: WikiRoutesState = common.clone().into();
    let insights_state: InsightsRoutesState = common.clone().into();
    let entity_docs_state: EntityDocsRoutesState = common.clone().into();
    let shell_favourites_state: ShellFavouritesRoutesState = common.clone().into();
    let search_state: SearchRoutesState = common.clone().into();
    let profile_state: ProfileRoutesState = common.into();
    let settings_state = SettingsRoutesState::new(Arc::clone(clients), web_config_path.to_path_buf());
    // Every "collection root" path is registered twice, bare and with a trailing slash, both resolving to the
    // identical handler: axum's `{*wildcard}` segment cannot match zero path segments, so without the bare form a
    // request that omits the trailing slash (`/vault/apps` rather than `/vault/apps/`) would fall through to no route
    // at all rather than 404 with the same handler that the trailing-slash form reaches.
    let agents = Router::new()
        .route("/{vault_name}/agents", get(routes::agents::list))
        .route("/{vault_name}/agents/", get(routes::agents::list))
        .with_state(agents_state);
    let signals = Router::new()
        .route("/{vault_name}/signals", get(routes::signals::page))
        .route("/{vault_name}/signals/", get(routes::signals::page))
        .route("/{vault_name}/signals/feed", get(routes::signals::feed))
        .route("/{vault_name}/signals/attention", get(routes::signals::attention))
        .route("/{vault_name}/signals/background", get(routes::signals::background))
        .with_state(signals_state);
    let overview = Router::new()
        .route("/{vault_name}/overview/bridge", get(routes::overview_panels::bridge))
        .route("/{vault_name}/overview/status", get(routes::overview_panels::status))
        .route("/{vault_name}/overview/inputs", get(routes::overview_panels::inputs))
        .with_state(overview_state);
    let event_stream = Router::new()
        .route("/{vault_name}/events/stream", get(routes::events_stream::stream))
        .with_state(event_stream_state);
    agents
        .merge(signals)
        .merge(overview)
        .merge(event_stream)
        .merge(apps_router(app_state))
        .merge(workflows_router(job_state))
        .merge(tasks_router(tasks_state))
        .merge(vault_settings_router(vault_settings_state))
        .merge(vault_management_router(vault_management_state))
        .merge(vault_health_router(vault_health_state))
        .merge(inbox_outbox_router(inbox_outbox_state))
        .merge(journal_router(journal_state))
        .merge(notes_router(notes_state))
        .merge(wiki_router(wiki_state))
        .merge(insights_router(insights_state))
        .merge(entity_docs_router(entity_docs_state))
        .merge(profile_router(profile_state))
        .merge(shell_favourites_router(shell_favourites_state))
        .merge(search_router(search_state))
        .merge(vault_router(vault_state))
        .merge(settings_router(settings_state))
}

/// The live event stream's state: one hub, reading events through the primary session.
fn event_stream_state(
    clients: &Arc<McpClientSet>,
    web_config_path: &Arc<PathBuf>,
    common: &CommonRoutesState,
    shutdown: CancellationToken,
) -> EventStreamRoutesState {
    EventStreamRoutesState {
        clients: Arc::clone(clients),
        web_config_path: Arc::clone(web_config_path),
        hub: crate::events_hub::EventHub::with_shutdown(
            Arc::new(crate::events_hub::McpEventSources {
                clients: Arc::clone(clients),
            }),
            Arc::clone(&common.clock),
            Arc::new(|vault, event, labels| {
                crate::signals_view::render_event(&crate::urls::VaultUrls::from(vault), event, labels)
            }),
            shutdown,
        ),
    }
}

/// The routes that read the host's own app-data directory and no vault: the package store's JSON API and the shared workflow
/// pages. Each is left out, with its reason logged, when the directory cannot be resolved.
fn mount_host_routes(
    router: Router,
    clients: Arc<McpClientSet>,
    web_config_path: &Arc<PathBuf>,
    roots: Option<&AppDataRoots>,
    workflows: &WorkflowsConfig,
) -> Router {
    let Some(roots) = roots else {
        tracing::error!(
            "package store app-data directory unavailable; /api/v1/shared/packages and the shared workflow pages will not be mounted"
        );
        return router;
    };
    router
        .merge(packages_router(PackagesRoutesState::new(
            roots.store.clone(),
            roots.jobs.clone(),
            roots.state.clone(),
        )))
        .merge(shared_router(
            SharedRoutesState::new(
                SharedRoots {
                    store_root: roots.store.clone(),
                    jobs_root: roots.jobs.clone(),
                    runs_root: roots.runs.clone(),
                    state_root: roots.state.clone(),
                    claims_root: roots.claims.clone(),
                },
                clients,
                Arc::clone(web_config_path),
            )
            .with_workflows(workflows),
        ))
}

#[cfg(test)]
#[path = "app_test.rs"]
mod tests;
