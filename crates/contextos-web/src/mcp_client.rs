//! MCP client sessions to configured `[[mcp_server]]` entries.
//!
//! `contextos-web` is a real MCP client, never an embedding of a service
//! crate's trait implementations: every vault operation this crate performs is a
//! `call_tool` on a session built here.
//!
//! An `oauth2`-mode HTTP entry additionally carries an [`oauth_refresh::OauthRefreshEntry`],
//! resolved once at connect time, plus the credential store and clock its own refresh check
//! needs (`connect_with`): a stdio session, or an HTTP session whose `auth` is `None`/`bearer`,
//! carries none of this and pays no extra cost when `ensure_oauth_token_fresh` is called on it.
//!
//! Which sessions exist, and how one is replaced without restarting the process, is
//! [`crate::mcp_client_set`]'s concern; this module opens, uses, and closes one session.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rmcp::model::{CallToolRequestParams, ProtocolVersion};
use rmcp::service::{ClientInitializeError, ClientLifecycleMode, ClientServiceExt, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::common::client_side_sse::NeverRetry;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::{RoleClient, ServiceError};
use thiserror::Error;
use tokio::process::Command;

use crate::config::McpServerConfig;
use crate::credential_store::FsCredentialStore;
use crate::logging::{Clock, SystemClock};
use crate::mcp_http_auth::{
    CredentialedHttpClient, SessionCredential, build_custom_headers, oauth_refresh_entry, resolve_bearer_header,
    stored_access_token,
};
use crate::oauth_refresh::{self, OauthRefreshEntry, OauthRefreshError};

pub use crate::mcp_client_set::McpClientSet;

/// How long a session being closed waits for the call that last began on it before it is closed regardless.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

/// One session's own live status, from a single `tools/list` probe
/// ([`McpClient::status`]).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClientStatus {
    pub reachable: bool,
    pub tool_count: usize,
}

/// A live MCP client session to one configured server.
pub struct McpClient {
    name: String,
    peer: rmcp::Peer<RoleClient>,
    running: tokio::sync::Mutex<RunningService<RoleClient, ()>>,
    /// The spawned child process's OS PID, present only for a `stdio`
    /// transport. Exposed for operational diagnostics and so a test can
    /// simulate a crashed `contextos-mcp` process without this module
    /// needing to invent its own process-supervision API surface.
    pid: Option<u32>,
    /// Present only for an HTTP entry whose `auth` is `mode = "oauth2"`; `None` for every other
    /// entry (a stdio session, or an HTTP session with no `auth`/`bearer`-mode `auth`), so
    /// [`Self::ensure_oauth_token_fresh`] can short-circuit at zero cost for them.
    oauth_refresh: Option<OauthRefreshState>,
    /// Held shared by every call while it runs, and exclusively by [`Self::retire`], so a session replaced by a
    /// reconnect is closed only once the calls already running on it have finished.
    calls: tokio::sync::RwLock<()>,
    /// How many events need attention in each vault, kept briefly so a page render does not read every event note.
    /// It belongs to the session, so a replacement session starts without it.
    open_counts: crate::open_count_cache::OpenCountCache,
    /// Where the vaults' package stores are, asked of the server on first use.
    pub(crate) packages: crate::vault_packages::VaultPackages,
}

/// The resolved `oauth2` configuration plus the injected dependencies one refresh check needs,
/// held by a [`McpClient`] whose own entry is HTTP, `mode = "oauth2"`.
struct OauthRefreshState {
    entry: OauthRefreshEntry,
    credential_store: Arc<FsCredentialStore>,
    clock: Arc<dyn Clock>,
    /// The token the session presents on its next request, replaced whenever the refresh check confirms a different
    /// one is now in force.
    credential: SessionCredential,
}

/// A dummy, never-touched credential store rooted at an empty path, paired with [`SystemClock`]:
/// the default dependencies [`McpClient::connect`]/[`McpClientSet::connect`] supply to
/// [`McpClient::connect_with`]/[`McpClientSet::connect_with`] for a caller that has no
/// `oauth2`-mode entry to wire up (every existing call site before this module gained the
/// dependency, plus every test that only exercises stdio or non-`oauth2` HTTP entries). Neither
/// value is ever read unless a connected entry actually resolves to `mode = "oauth2"`.
pub(crate) fn no_oauth_dependencies() -> (Arc<FsCredentialStore>, Arc<dyn Clock>) {
    (Arc::new(FsCredentialStore::new(PathBuf::new())), Arc::new(SystemClock))
}

/// Launches a stdio server and opens its MCP session with `lifecycle`, returning the session and the
/// child's process id. On failure the child is reaped before the error returns.
async fn connect_stdio(
    name: &str,
    command: &str,
    args: &[String],
    lifecycle: ClientLifecycleMode,
    shutdown: &tokio_util::sync::CancellationToken,
) -> Result<(RunningService<RoleClient, ()>, Option<u32>), McpConnectError> {
    let mut cmd = Command::new(command);
    cmd.args(args);
    let transport = crate::stdio_transport::StdioTransport::spawn(cmd, name)
        .await
        .map_err(|source| McpConnectError::Spawn {
            server: name.to_owned(),
            command: command.to_owned(),
            source,
        })?;
    let pid = transport.id();
    let finished = transport.finished();
    let result = tokio::select! {
        biased;
        () = shutdown.cancelled() => Err(McpConnectError::Shutdown),
        result = ().serve_with_lifecycle(transport, lifecycle) => result.map_err(|source| McpConnectError::Handshake {
            server: name.to_owned(), source: Box::new(source),
        }),
    };
    match result {
        Ok(running) => Ok((running, pid)),
        Err(error) => {
            finished.cancelled().await;
            Err(error)
        }
    }
}

/// Opens an MCP session over streamable HTTP with `lifecycle`, presenting `client`'s credential on every request.
async fn connect_http(
    name: &str,
    client: CredentialedHttpClient,
    config: StreamableHttpClientTransportConfig,
    lifecycle: ClientLifecycleMode,
    shutdown: &tokio_util::sync::CancellationToken,
) -> Result<RunningService<RoleClient, ()>, McpConnectError> {
    let transport = StreamableHttpClientTransport::with_client(client, config);
    tokio::select! {
        biased;
        () = shutdown.cancelled() => Err(McpConnectError::Shutdown),
        result = ().serve_with_lifecycle(transport, lifecycle) => result.map_err(|source| McpConnectError::Handshake {
            server: name.to_owned(), source: Box::new(source),
        }),
    }
}

/// The most of an error chain a log line carries. rmcp includes a server's response body in some errors, and a
/// third-party server's body is neither bounded nor ours to log in full.
const MAX_ERROR_CHAIN_CHARS: usize = 512;

/// `error` and every error beneath it, outermost first, so a log line names the cause, not only the outcome;
/// cut at [`MAX_ERROR_CHAIN_CHARS`] characters.
pub(crate) fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut chain = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        chain.push_str(": ");
        chain.push_str(&cause.to_string());
        source = cause.source();
    }
    if let Some((cut, _)) = chain.char_indices().nth(MAX_ERROR_CHAIN_CHARS) {
        chain.truncate(cut);
        chain.push('…');
    }
    chain
}

/// How every connection opens its MCP session: by asking the server for
/// `server/discover` and speaking protocol revision 2026-07-28 when it
/// supports it, or, for a server that supports only the earlier,
/// handshake-based revisions, through rmcp's own fall back to `initialize`
/// at the newest revision that still has one. A server that ignores the
/// probe is given rmcp's ten seconds before the fall back. A stdio server
/// that exits on the probe, which rmcp cannot fall back from, is launched
/// once more with `initialize` (see [`McpClient::connect_with`]).
fn lifecycle() -> ClientLifecycleMode {
    ClientLifecycleMode::Auto {
        preferred_versions: vec![ProtocolVersion::V_2026_07_28],
        legacy_version: Some(ProtocolVersion::LATEST_WITH_INITIALIZE),
    }
}

impl McpClient {
    /// The session's memory of how many events need attention in each vault.
    #[must_use]
    pub const fn open_counts(&self) -> &crate::open_count_cache::OpenCountCache {
        &self.open_counts
    }

    /// [`Self::connect_with`] using [`no_oauth_dependencies`]: the entry-point every caller before
    /// `oauth2`-mode support existed keeps using unchanged, since none of them connects an
    /// `oauth2`-mode entry.
    ///
    /// # Errors
    ///
    /// See [`Self::connect_with`].
    pub async fn connect(entry: &McpServerConfig) -> Result<Self, McpConnectError> {
        let (credential_store, clock) = no_oauth_dependencies();
        Self::connect_with(entry, &credential_store, &clock).await
    }

    /// Spawns (stdio) or connects (HTTP) to `entry`, establishing the MCP
    /// session before returning (see [`lifecycle`]). A failed spawn or handshake
    /// is returned as an error, never retried or silently degraded: the caller
    /// records it as the server's readiness ([`crate::mcp_client_set`]).
    ///
    /// For an HTTP entry whose `auth` is `mode = "oauth2"`, the initial `Authorization` header is
    /// whatever access token `credential_store` currently holds for its `credential_ref` (`None`
    /// when nothing has ever been stored there), and `credential_store`/`clock` are retained so a
    /// later [`Self::ensure_oauth_token_fresh`] call can refresh and re-persist that token without
    /// this connection needing to be rebuilt for every check.
    ///
    /// # Errors
    ///
    /// Returns [`McpConnectError::Spawn`] when the configured `command`
    /// cannot be started, [`McpConnectError::MissingTokenEnv`] when an HTTP
    /// entry names a `token_env` variable that is not set, or
    /// [`McpConnectError::Handshake`] when the process starts (or the HTTP
    /// endpoint connects) but establishing the session itself fails.
    pub async fn connect_with(
        entry: &McpServerConfig,
        credential_store: &Arc<FsCredentialStore>,
        clock: &Arc<dyn Clock>,
    ) -> Result<Self, McpConnectError> {
        Self::connect_until_shutdown(
            entry,
            credential_store,
            clock,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
    }

    pub(crate) async fn connect_until_shutdown(
        entry: &McpServerConfig,
        credential_store: &Arc<FsCredentialStore>,
        clock: &Arc<dyn Clock>,
        shutdown: &tokio_util::sync::CancellationToken,
    ) -> Result<Self, McpConnectError> {
        if shutdown.is_cancelled() {
            return Err(McpConnectError::Shutdown);
        }
        match entry {
            McpServerConfig::Stdio { name, command, args } => {
                let (running, pid) = match connect_stdio(name, command, args, lifecycle(), shutdown).await {
                    Err(McpConnectError::Handshake { source, .. }) => {
                        // Many servers from before protocol revision 2026-07-28 (rmcp before 3.5, other
                        // SDKs of that time) exit, or answer in a way rmcp cannot read as legacy, when the
                        // first message is the `server/discover` probe rather than `initialize`. A process
                        // that has exited cannot take rmcp's own fall back, so it is launched once more and
                        // reached through `initialize` directly.
                        tracing::info!(
                            server = %name,
                            error = %error_chain(source.as_ref()),
                            "MCP server did not accept server/discover; launching it again with initialize"
                        );
                        connect_stdio(name, command, args, ClientLifecycleMode::Initialize, shutdown).await?
                    }
                    other => other?,
                };
                Ok(Self::from_parts(name.clone(), running, pid, None))
            }
            McpServerConfig::Http {
                name,
                endpoint,
                token_env,
                auth,
                headers,
            } => {
                let mut config = StreamableHttpClientTransportConfig::with_uri(endpoint.clone());
                // The default `ExponentialBackoff` retry policy applies to
                // the first request of a session too, not just an
                // already-established session's reconnects: against a
                // genuinely unreachable endpoint it retries for minutes
                // rather than failing, which would turn "a failed handshake
                // is a startup error, not a lazily-discovered one" into an
                // indefinite hang instead. A single attempt is correct
                // here: this is a startup connection, not a live session
                // worth reconnecting.
                config.retry_config = Arc::new(NeverRetry::default());
                if !headers.is_empty() {
                    config = config.custom_headers(build_custom_headers(name, headers)?);
                }
                let refresh_entry = oauth_refresh_entry(entry);
                let initial_token = match &refresh_entry {
                    Some(refresh_entry) => stored_access_token(credential_store, &refresh_entry.credential_ref).await,
                    None => resolve_bearer_header(name, auth.as_ref(), token_env.as_ref(), credential_store).await?,
                };
                let credential = SessionCredential::new(initial_token);
                let client =
                    CredentialedHttpClient::new(credential.clone()).map_err(|source| McpConnectError::HttpClient {
                        server: name.clone(),
                        source,
                    })?;
                let running = match connect_http(name, client.clone(), config.clone(), lifecycle(), shutdown).await {
                    Err(McpConnectError::Handshake { source, .. }) => {
                        // rmcp's own fall back follows only a refusal it can read as a legacy reply (a
                        // JSON-RPC error answering the probe, or a 4xx); a server error, an empty reply, and
                        // other refusals end the attempt instead, so the session is opened again through
                        // `initialize` directly.
                        tracing::info!(
                            server = %name,
                            error = %error_chain(source.as_ref()),
                            "MCP server did not accept server/discover; connecting again with initialize"
                        );
                        connect_http(name, client, config, ClientLifecycleMode::Initialize, shutdown).await?
                    }
                    other => other?,
                };
                let oauth_refresh = refresh_entry.map(|resolved_entry| OauthRefreshState {
                    entry: resolved_entry,
                    credential_store: Arc::clone(credential_store),
                    clock: Arc::clone(clock),
                    credential,
                });
                Ok(Self::from_parts(name.clone(), running, None, oauth_refresh))
            }
        }
    }

    fn from_parts(
        name: String,
        running: RunningService<RoleClient, ()>,
        pid: Option<u32>,
        oauth_refresh: Option<OauthRefreshState>,
    ) -> Self {
        Self {
            name,
            peer: running.peer().clone(),
            running: tokio::sync::Mutex::new(running),
            pid,
            oauth_refresh,
            calls: tokio::sync::RwLock::new(()),
            open_counts: crate::open_count_cache::OpenCountCache::default(),
            packages: crate::vault_packages::VaultPackages::default(),
        }
    }

    /// A session over an already-running service, for tests that serve MCP in process.
    #[cfg(test)]
    pub(crate) fn over(name: &str, running: RunningService<RoleClient, ()>) -> Self {
        Self::from_parts(name.to_owned(), running, None, None)
    }

    /// Closes this session once every call already running on it has finished, or once `drain` has passed,
    /// whichever comes first. A call that begins after this starts waits until the session is closed, then finds it so.
    pub(crate) async fn retire(&self, drain: Duration) {
        let drained = tokio::time::timeout(drain, self.calls.write()).await;
        if drained.is_err() {
            tracing::warn!(
                server = %self.name,
                "closing a replaced MCP session while calls on it are still running"
            );
        }
        self.close().await;
        drop(drained);
    }

    /// Closes this session, allowing [`CLOSE_TIMEOUT`] for the transport to clean up: a stdio session closes the
    /// child's input and waits for it to exit; an HTTP session deletes the remote session.
    pub(crate) async fn close(&self) {
        let mut running = self.running.lock().await;
        match running.close_with_timeout(CLOSE_TIMEOUT).await {
            Ok(Some(_)) => tracing::debug!(server = %self.name, "MCP session closed"),
            Ok(None) => tracing::warn!(server = %self.name, "MCP shutdown timed out; check server health"),
            Err(_) => tracing::warn!(server = %self.name, "MCP shutdown task failed; check server health"),
        }
    }

    /// Whether the session has been closed, by [`Self::close`] or because its transport ended.
    #[cfg(test)]
    pub(crate) async fn is_closed(&self) -> bool {
        self.running.lock().await.is_closed()
    }

    /// Checks and, if needed, refreshes this session's stored `oauth2` token before a proxied
    /// call reaches it (`routes::proxy::handle`'s own, sole call site, immediately before
    /// [`Self::call_tool`]). A no-op returning `Ok(())` at zero cost for every session that is not
    /// an HTTP, `mode = "oauth2"` entry.
    ///
    /// # Errors
    ///
    /// Returns [`OauthRefreshError`] when the stored token could not be confirmed fresh: see
    /// [`oauth_refresh::ensure_fresh`] for every specific cause this collapses.
    pub async fn ensure_oauth_token_fresh(&self) -> Result<(), OauthRefreshError> {
        let Some(state) = &self.oauth_refresh else {
            return Ok(());
        };
        let access_token = oauth_refresh::ensure_fresh(&state.credential_store, &state.clock, &state.entry).await?;
        if state.credential.current().as_deref() != Some(access_token.as_str()) {
            state.credential.replace(access_token);
        }
        Ok(())
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// A generic liveness probe: `tools/list` is a capability every MCP
    /// server supports (unlike a ContextOS-specific tool such as
    /// `vault_info`, which a third-party `[[mcp_server]]` entry has no
    /// obligation to expose), so it is the cheapest transport-agnostic way
    /// to tell a session that is still genuinely responding from one whose
    /// underlying process or connection has died since the initial
    /// handshake. Used by the `/settings/` MCP servers pane to show each
    /// configured entry's live status rather than only its static
    /// configuration.
    #[must_use]
    pub async fn is_reachable(&self) -> bool {
        self.status().await.reachable
    }

    /// The identical `tools/list` probe [`is_reachable`](Self::is_reachable)
    /// uses, additionally keeping the returned tool count rather than
    /// discarding it: the shell footer's status bar needs both "is this
    /// server still responding" and "how many tools does it expose" from
    /// one round trip, not two. Only the first page of results is counted
    /// (no `cursor` follow-up): a status-bar tool count is a coarse
    /// operator-facing figure, not a total a paginating server's tool
    /// catalogue must be exhaustively walked to report.
    #[must_use]
    pub async fn status(&self) -> ClientStatus {
        let _call = self.calls.read().await;
        match self.peer.list_tools(None).await {
            Ok(result) => ClientStatus {
                reachable: true,
                tool_count: result.tools.len(),
            },
            Err(_source) => ClientStatus {
                reachable: false,
                tool_count: 0,
            },
        }
    }

    /// Calls `tool_name` on this session with `arguments`, relaying whatever
    /// the MCP server itself returns (including an MCP-level tool error
    /// result) unmodified.
    ///
    /// # Errors
    ///
    /// Returns [`McpCallError::Unreachable`] when the transport itself
    /// fails (the server process died, the HTTP connection dropped), or
    /// [`McpCallError::Refused`] when the server received the call but
    /// answered with a protocol error rather than a result. Both are distinct
    /// from an MCP-level tool error, which is a successful
    /// `Ok(CallToolResult)` with `is_error: Some(true)`.
    pub async fn call_tool(
        &self,
        tool_name: String,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<rmcp::model::CallToolResult, McpCallError> {
        // The tool name only, never its arguments, which can hold vault paths and content. Counting these events is
        // how the cost of rendering a page in round trips is measured.
        tracing::debug!(target: "contextos_web::mcp_call", tool = %tool_name, server = %self.name, "mcp tool call");
        let _call = self.calls.read().await;
        self.peer
            .call_tool(CallToolRequestParams::new(tool_name).with_arguments(arguments))
            .await
            .map_err(|source| classify_call_failure(&self.name, source))
    }

    /// Every tool this session's server advertises, following pagination
    /// fully (`rmcp::Peer::list_all_tools`) rather than [`Self::status`]'s
    /// own deliberate first-page-only simplification: a coarse status-bar
    /// count tolerates an undercount, but looking up one specific named
    /// tool's own declared schema (`dry_run_caller.rs`) does not.
    ///
    /// # Errors
    ///
    /// Returns [`McpCallError::Unreachable`] when the transport itself
    /// fails, or [`McpCallError::Refused`] when the server answers with a
    /// protocol error.
    pub async fn list_all_tools(&self) -> Result<Vec<rmcp::model::Tool>, McpCallError> {
        let _call = self.calls.read().await;
        self.peer
            .list_all_tools()
            .await
            .map_err(|source| classify_call_failure(&self.name, source))
    }
}

/// Failures opening an MCP client session (`McpClient::connect`, and every attempt
/// `McpClientSet` makes at start-up or on a reconnect).
#[derive(Debug, Error)]
pub enum McpConnectError {
    #[error("MCP startup cancelled by shutdown request")]
    Shutdown,
    #[error("MCP session with server {server:?} was not established within {} seconds", after.as_secs())]
    TimedOut { server: String, after: Duration },
    #[error("the HTTP client for MCP server {server:?} could not be built")]
    HttpClient {
        server: String,
        #[source]
        source: reqwest::Error,
    },
    /// The command is named so an operator can see what failed to start; its arguments are not, since they can carry
    /// a credential.
    #[error("failed to spawn MCP server {server:?} (command {command:?})")]
    Spawn {
        server: String,
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("MCP session could not be established with server {server:?}")]
    Handshake {
        server: String,
        #[source]
        source: Box<ClientInitializeError>,
    },
    #[error("environment variable {variable} (server {server:?}'s token_env) is not set")]
    MissingTokenEnv { server: String, variable: String },
    #[error(
        "no credential is stored under credential_ref {credential_ref:?} for server {server:?}; \
         paste one via /settings/ first"
    )]
    MissingCredential { server: String, credential_ref: String },
    #[error("[mcp_server.headers] entry {key:?} for server {server:?} is not a valid HTTP header name or value")]
    InvalidHeader { server: String, key: String },
}

/// Failures making a `call_tool` request against an already-connected
/// session: always a request-time concern, never a startup one.
/// Kept as its own type rather than sharing [`McpConnectError`] so the
/// proxy route's error handling stays exhaustive without a catch-all arm
/// for connect-only variants that can never occur here.
#[derive(Debug, Error)]
pub enum McpCallError {
    #[error("MCP server {server:?} is unreachable")]
    Unreachable {
        server: String,
        #[source]
        source: Box<ServiceError>,
    },
    /// The server received the call and answered with a protocol error (an
    /// unknown tool, invalid parameters, a reply this client cannot use, such
    /// as a request for input it cannot give) rather than a result.
    #[error("MCP server {server:?} refused the call")]
    Refused {
        server: String,
        #[source]
        source: Box<ServiceError>,
    },
}

/// Tells a call the server answered with a protocol error apart from one
/// that never reached it, so neither is reported as the other.
fn classify_call_failure(server: &str, source: ServiceError) -> McpCallError {
    let server = server.to_owned();
    let source = Box::new(source);
    match *source {
        ServiceError::McpError(_)
        | ServiceError::UnexpectedResponse
        | ServiceError::InputRequiredRoundsExceeded { .. } => McpCallError::Refused { server, source },
        _ => McpCallError::Unreachable { server, source },
    }
}

#[cfg(test)]
#[path = "mcp_client_test.rs"]
mod tests;

#[cfg(test)]
#[path = "mcp_shutdown_test.rs"]
mod shutdown_tests;
