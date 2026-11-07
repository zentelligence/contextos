//! Every configured `[[mcp_server]]` and its session, kept current while the process runs.
//!
//! A configured server always has an entry here, whether or not its session is open, so its readiness can be
//! reported with the reason it is not connected ([`crate::mcp_readiness`]). A server is connected again without
//! restarting the process: on an operator's request ([`McpClientSet::reconnect`]), when its `web.toml` entry is
//! added, changed, or removed ([`McpClientSet::apply_config`]), and after an `OAuth2` connect. Nothing reconnects on
//! its own: a server that keeps failing is never relaunched in a loop.
//!
//! Replacing a session never cuts off a call already running on it: the replaced session is closed once its calls
//! finish ([`McpClient::retire`]). Attempts for the same server run one after another, so the last one requested
//! decides its state.

use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use futures_util::future::{BoxFuture, join_all};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::config::McpServerConfig;
use crate::credential_store::FsCredentialStore;
use crate::logging::Clock;
use crate::mcp_client::{McpClient, McpConnectError, error_chain, no_oauth_dependencies};
use crate::mcp_readiness::{FailedAttempt, LoginRecovery, Readiness, ServerReadiness};

/// How long one connection attempt may take, covering a stdio server launched a second time when it does not accept
/// `server/discover`. A first launch through a package runner can download the server before it answers.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);

/// How long a replaced session waits for the calls already running on it before it is closed regardless.
const RETIRE_DRAIN: Duration = Duration::from_secs(30);

/// Opens one session for a configured entry. The live implementation spawns or dials the server; tests supply
/// sessions served in process.
pub(crate) trait SessionConnector: Send + Sync {
    fn open<'a>(
        &'a self,
        entry: &'a McpServerConfig,
        shutdown: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<McpClient, McpConnectError>>;
}

/// Opens real sessions, bounding each attempt by [`CONNECT_TIMEOUT`].
struct LiveConnector {
    credential_store: Arc<FsCredentialStore>,
    clock: Arc<dyn Clock>,
}

impl SessionConnector for LiveConnector {
    fn open<'a>(
        &'a self,
        entry: &'a McpServerConfig,
        shutdown: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<McpClient, McpConnectError>> {
        Box::pin(async move {
            let attempt = McpClient::connect_until_shutdown(entry, &self.credential_store, &self.clock, shutdown);
            tokio::time::timeout(CONNECT_TIMEOUT, attempt)
                .await
                .unwrap_or_else(|_elapsed| {
                    Err(McpConnectError::TimedOut {
                        server: entry.name().to_owned(),
                        after: CONNECT_TIMEOUT,
                    })
                })
        })
    }
}

/// Whether one server's session can take calls, apart from the live probe [`McpClientSet::readiness`] adds.
#[derive(Clone, Debug)]
enum Condition {
    Connecting,
    Open,
    /// Not connectable without a credential. A session may still be held: one whose token can no longer be
    /// refreshed stays open, so its calls fail with the refresh error rather than as unreachable.
    LoginRequired(LoginRecovery),
    /// The last attempt failed; no session is held.
    Failed(Readiness),
}

struct Server {
    config: McpServerConfig,
    session: Option<Arc<McpClient>>,
    condition: Condition,
}

/// One server's state copied out of the set, so it is probed without holding the set's lock.
struct ServerSnapshot {
    name: String,
    session: Option<Arc<McpClient>>,
    condition: Condition,
}

impl From<&Server> for ServerSnapshot {
    fn from(server: &Server) -> Self {
        Self {
            name: server.config.name().to_owned(),
            session: server.session.clone(),
            condition: server.condition.clone(),
        }
    }
}

impl ServerSnapshot {
    async fn probe(self) -> ServerReadiness {
        let readiness = match (self.condition, self.session) {
            (Condition::Open, Some(session)) => {
                let status = session.status().await;
                if status.reachable {
                    Readiness::Connected {
                        tool_count: status.tool_count,
                    }
                } else {
                    Readiness::Unreachable
                }
            }
            (Condition::Open | Condition::Connecting, _) => Readiness::Connecting,
            (Condition::LoginRequired(recovery), _) => Readiness::LoginRequired(recovery),
            (Condition::Failed(readiness), _) => readiness,
        };
        ServerReadiness {
            name: self.name,
            readiness,
        }
    }
}

struct Shared {
    /// In `web.toml` order; the first is the primary server.
    servers: RwLock<Vec<Server>>,
    connector: Arc<dyn SessionConnector>,
    /// One lock per server name, held for the whole of an attempt, so attempts for one server never overlap.
    attempts: std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Attempts and retirements running in the background, which shutdown waits for.
    tasks: TaskTracker,
    shutdown: CancellationToken,
}

/// No configured server is named this.
#[derive(Debug, thiserror::Error, Eq, PartialEq)]
#[error("no MCP server named {server:?} is configured")]
pub struct NotConfigured {
    pub server: String,
}

/// Every configured `[[mcp_server]]` entry and its session. Cloning shares the same set.
#[derive(Clone)]
pub struct McpClientSet {
    shared: Arc<Shared>,
}

/// A set with no servers configured: every lookup misses.
impl Default for McpClientSet {
    fn default() -> Self {
        let (credential_store, clock) = no_oauth_dependencies();
        Self::empty(
            Arc::new(LiveConnector {
                credential_store,
                clock,
            }),
            CancellationToken::new(),
        )
    }
}

impl McpClientSet {
    fn empty(connector: Arc<dyn SessionConnector>, shutdown: CancellationToken) -> Self {
        Self {
            shared: Arc::new(Shared {
                servers: RwLock::new(Vec::new()),
                connector,
                attempts: std::sync::Mutex::new(HashMap::new()),
                tasks: TaskTracker::new(),
                shutdown,
            }),
        }
    }

    /// [`Self::connect_with`] using [`no_oauth_dependencies`], for a caller that connects no `oauth2`-mode entry.
    ///
    /// # Errors
    ///
    /// See [`Self::connect_with`].
    pub async fn connect(entries: &[McpServerConfig]) -> Result<Self, McpConnectError> {
        let (credential_store, clock) = no_oauth_dependencies();
        Self::connect_with(entries, &credential_store, &clock).await
    }

    /// [`Self::connect_until_shutdown`] with no shutdown request to observe.
    ///
    /// # Errors
    ///
    /// See [`Self::connect_until_shutdown`].
    pub async fn connect_with(
        entries: &[McpServerConfig],
        credential_store: &Arc<FsCredentialStore>,
        clock: &Arc<dyn Clock>,
    ) -> Result<Self, McpConnectError> {
        Self::connect_until_shutdown(entries, credential_store, clock, &CancellationToken::new()).await
    }

    /// Connects every entry for a command that needs its stdio servers to run at all, such as running one job from
    /// the command line: a stdio entry that cannot be connected ends the command with its error. An HTTP entry that
    /// cannot be connected is kept with its failure, as [`Self::start_until_shutdown`] keeps every failure.
    ///
    /// # Errors
    ///
    /// Returns the error of the first stdio entry, in `web.toml` order, that could not be connected, or
    /// [`McpConnectError::Shutdown`] when shutdown was requested first. Every session already open is closed first.
    pub async fn connect_until_shutdown(
        entries: &[McpServerConfig],
        credential_store: &Arc<FsCredentialStore>,
        clock: &Arc<dyn Clock>,
        shutdown: &CancellationToken,
    ) -> Result<Self, McpConnectError> {
        let connector = Arc::new(LiveConnector {
            credential_store: Arc::clone(credential_store),
            clock: Arc::clone(clock),
        });
        Self::open_all(connector, entries, shutdown, Strictness::StdioRequired).await
    }

    /// Connects every entry for the long-running server, which serves its pages whatever the outcome: a server that
    /// cannot be connected is kept with its cause, shown in `/server/settings/`, and connected again from there.
    ///
    /// Every entry is attempted at once, and this returns when each attempt has ended or timed out.
    ///
    /// # Errors
    ///
    /// Returns [`McpConnectError::Shutdown`] when shutdown is requested before every attempt ends; every session
    /// already open is closed first.
    pub async fn start_until_shutdown(
        entries: &[McpServerConfig],
        credential_store: &Arc<FsCredentialStore>,
        clock: &Arc<dyn Clock>,
        shutdown: &CancellationToken,
    ) -> Result<Self, McpConnectError> {
        let connector = Arc::new(LiveConnector {
            credential_store: Arc::clone(credential_store),
            clock: Arc::clone(clock),
        });
        Self::open_all(connector, entries, shutdown, Strictness::Lenient).await
    }

    pub(crate) async fn open_all(
        connector: Arc<dyn SessionConnector>,
        entries: &[McpServerConfig],
        shutdown: &CancellationToken,
        strictness: Strictness,
    ) -> Result<Self, McpConnectError> {
        if shutdown.is_cancelled() {
            return Err(McpConnectError::Shutdown);
        }
        // The set observes its own token, cancelled by the caller's, so closing the set never cancels the caller.
        let set = Self::empty(connector, shutdown.child_token());
        set.write_servers(|servers| {
            servers.extend(entries.iter().map(|config| Server {
                config: config.clone(),
                session: None,
                condition: Condition::Connecting,
            }));
        });
        let outcomes = join_all(entries.iter().map(|entry| set.attempt(entry.name().to_owned()))).await;
        let first_failure = entries.iter().zip(outcomes).find_map(|(entry, outcome)| match outcome {
            Err(McpConnectError::Shutdown) => Some(McpConnectError::Shutdown),
            Err(error) if strictness == Strictness::StdioRequired && matches!(entry, McpServerConfig::Stdio { .. }) => {
                Some(error)
            }
            Ok(()) | Err(_) => None,
        });
        if let Some(error) = first_failure {
            set.shutdown().await;
            return Err(error);
        }
        let unconnected = set.read_servers(|servers| servers.iter().filter(|server| server.session.is_none()).count());
        if unconnected > 0 {
            tracing::warn!(
                unconnected,
                total = entries.len(),
                "contextos-web starting with one or more MCP servers not connected; see /server/settings/ for each cause"
            );
        }
        Ok(set)
    }

    /// The open session for `name`, or `None` when no server of that name is configured or its session is not open.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<McpClient>> {
        self.read_servers(|servers| {
            servers
                .iter()
                .find(|server| server.config.name() == name)
                .and_then(|server| server.session.clone())
        })
    }

    /// Whether a server named `name` is configured, whether or not it is connected.
    #[must_use]
    pub fn is_configured(&self, name: &str) -> bool {
        self.read_servers(|servers| servers.iter().any(|server| server.config.name() == name))
    }

    /// Every configured server's name, in `web.toml` order, whether or not it is connected: what a manifest's
    /// `mcp_servers` list is validated against, so a server that is down does not make an app or job invalid.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.read_servers(|servers| servers.iter().map(|server| server.config.name().to_owned()).collect())
    }

    /// The primary server's name: the first configured entry, the local `contextos` server every vault page uses.
    /// `None` while no server is configured.
    #[must_use]
    pub fn primary_name(&self) -> Option<String> {
        self.read_servers(|servers| servers.first().map(|server| server.config.name().to_owned()))
    }

    /// The primary server's name with its open session, read together so they always belong to the same server.
    #[must_use]
    pub fn primary_named(&self) -> Option<(String, Arc<McpClient>)> {
        self.read_servers(|servers| {
            servers
                .first()
                .and_then(|server| Some((server.config.name().to_owned(), server.session.clone()?)))
        })
    }

    /// The primary server's open session, if it has one.
    #[must_use]
    pub fn primary(&self) -> Option<Arc<McpClient>> {
        self.read_servers(|servers| servers.first().and_then(|server| server.session.clone()))
    }

    /// Every configured server's readiness, in `web.toml` order. Each open session is probed with `tools/list`, all
    /// at once, so a session whose server has stopped answering is reported as unreachable.
    pub async fn readiness(&self) -> Vec<ServerReadiness> {
        let snapshot = self.read_servers(|servers| servers.iter().map(ServerSnapshot::from).collect::<Vec<_>>());
        join_all(snapshot.into_iter().map(ServerSnapshot::probe)).await
    }

    /// One configured server's readiness, probed as [`Self::readiness`] probes it; `None` when no server of that name
    /// is configured.
    pub async fn readiness_of(&self, server: &str) -> Option<ServerReadiness> {
        let snapshot = self.read_servers(|servers| {
            servers
                .iter()
                .find(|entry| entry.config.name() == server)
                .map(ServerSnapshot::from)
        })?;
        Some(snapshot.probe().await)
    }

    /// The shell footer's figures: how many configured servers are connected, how many are configured, and the
    /// combined tool count of the connected ones.
    pub async fn status_summary(&self) -> (usize, usize, usize) {
        let readiness = self.readiness().await;
        let (connected, tools) = readiness
            .iter()
            .filter_map(|server| match server.readiness {
                Readiness::Connected { tool_count } => Some(tool_count),
                _ => None,
            })
            .fold((0, 0), |(connected, tools), tool_count| {
                (connected + 1, tools + tool_count)
            });
        (connected, readiness.len(), tools)
    }

    /// Connects `server` again from its current configuration and waits for the attempt to end. Its old session, if
    /// any, keeps serving until the new one is open, and is then closed once its running calls finish.
    ///
    /// # Errors
    ///
    /// Returns [`NotConfigured`] when no server of that name is configured.
    pub async fn reconnect(&self, server: &str) -> Result<(), NotConfigured> {
        let _already = self.mark_connecting(server)?;
        // The outcome is recorded as the server's readiness, which is how every caller reads it.
        let _outcome = self.attempt(server.to_owned()).await;
        Ok(())
    }

    /// [`Self::reconnect`] without waiting: the server reads as connecting until the attempt ends. A request made
    /// while an attempt is already under way joins it rather than queueing another, so repeated requests never
    /// relaunch a server over and over.
    ///
    /// # Errors
    ///
    /// Returns [`NotConfigured`] when no server of that name is configured.
    pub fn request_reconnect(&self, server: &str) -> Result<(), NotConfigured> {
        if self.mark_connecting(server)? == AlreadyConnecting::No {
            self.spawn_attempt(server.to_owned());
        }
        Ok(())
    }

    /// Brings the set in line with `entries`, the `[[mcp_server]]` entries `web.toml` now holds: a new entry is
    /// connected, a changed one is connected again, a removed one is closed, and an unchanged one is left as it is.
    /// Returns once the change is recorded, with the names of the servers now connecting; connecting happens in the
    /// background.
    pub fn apply_config(&self, entries: &[McpServerConfig]) -> Vec<String> {
        let (to_connect, removed) = self.write_servers(|servers| {
            let mut previous = std::mem::take(servers);
            let mut to_connect = Vec::new();
            for config in entries {
                let kept = previous
                    .iter()
                    .position(|server| server.config.name() == config.name())
                    .map(|index| previous.remove(index));
                match kept {
                    Some(server) if server.config == *config => servers.push(server),
                    Some(server) => {
                        to_connect.push(config.name().to_owned());
                        servers.push(Server {
                            config: config.clone(),
                            session: server.session,
                            condition: Condition::Connecting,
                        });
                    }
                    None => {
                        to_connect.push(config.name().to_owned());
                        servers.push(Server {
                            config: config.clone(),
                            session: None,
                            condition: Condition::Connecting,
                        });
                    }
                }
            }
            (to_connect, previous)
        });
        for server in removed {
            tracing::info!(
                server = server.config.name(),
                "MCP server removed from web.toml; closing its session"
            );
            if let Some(session) = server.session {
                self.spawn_retire(session);
            }
        }
        for name in &to_connect {
            self.spawn_attempt(name.clone());
        }
        to_connect
    }

    /// Records the outcome of the `OAuth2` refresh check `session` just passed through. A failure makes `server` read
    /// as needing a login: its session stays open, and its calls fail with the refresh error, which names the remedy.
    /// A later success clears that again, so a refresh that failed only because the provider was briefly unreachable
    /// does not leave the server marked once it recovers. Only the session the server holds now counts: the outcome
    /// of a session already replaced never marks its successor.
    pub fn note_refresh_outcome(&self, server: &str, session: &Arc<McpClient>, refreshed: bool) {
        self.write_servers(|servers| {
            let Some(entry) = servers.iter_mut().find(|entry| {
                entry.config.name() == server
                    && entry
                        .session
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, session))
            }) else {
                return;
            };
            match (&entry.condition, refreshed) {
                (Condition::Open, false) => {
                    entry.condition = Condition::LoginRequired(LoginRecovery::from(&entry.config));
                }
                (Condition::LoginRequired(_), true) => entry.condition = Condition::Open,
                _ => {}
            }
        });
    }

    /// Stops every attempt, closes every session, and waits for sessions already being replaced to close.
    pub async fn shutdown(&self) {
        self.shared.shutdown.cancel();
        self.shared.tasks.close();
        self.shared.tasks.wait().await;
        let sessions = self.write_servers(|servers| {
            servers
                .iter_mut()
                .filter_map(|server| server.session.take())
                .collect::<Vec<_>>()
        });
        join_all(sessions.iter().map(|session| session.close())).await;
    }

    /// Marks `server` as connecting, reporting whether an attempt was already under way for it.
    fn mark_connecting(&self, server: &str) -> Result<AlreadyConnecting, NotConfigured> {
        self.write_servers(
            |servers| match servers.iter_mut().find(|entry| entry.config.name() == server) {
                Some(entry) => {
                    let already = if matches!(entry.condition, Condition::Connecting) {
                        AlreadyConnecting::Yes
                    } else {
                        AlreadyConnecting::No
                    };
                    entry.condition = Condition::Connecting;
                    Ok(already)
                }
                None => Err(NotConfigured {
                    server: server.to_owned(),
                }),
            },
        )
    }

    fn spawn_attempt(&self, server: String) {
        let set = self.clone();
        self.shared.tasks.spawn(async move {
            let _outcome = set.attempt(server).await;
        });
    }

    fn spawn_retire(&self, session: Arc<McpClient>) {
        let shutdown = self.shared.shutdown.clone();
        self.shared.tasks.spawn(async move {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => session.close().await,
                () = session.retire(RETIRE_DRAIN) => {}
            }
        });
    }

    /// One connection attempt for `server`, from the configuration it has when the attempt starts. The outcome is
    /// recorded only if that configuration is still current when the attempt ends; otherwise a later attempt, already
    /// requested for the new configuration, decides it.
    async fn attempt(&self, server: String) -> Result<(), McpConnectError> {
        let lock = self.attempt_lock(&server);
        let _serialised = lock.lock().await;
        let Some(config) = self.read_servers(|servers| {
            servers
                .iter()
                .find(|entry| entry.config.name() == server)
                .map(|entry| entry.config.clone())
        }) else {
            return Ok(());
        };
        let (session, error) = match self.shared.connector.open(&config, &self.shared.shutdown).await {
            Ok(session) => (Some(Arc::new(session)), None),
            Err(error) => (None, Some(error)),
        };
        let failure = error
            .as_ref()
            .filter(|error| !matches!(error, McpConnectError::Shutdown))
            .map(|error| Readiness::from(FailedAttempt { entry: &config, error }));
        match (&error, &failure) {
            (Some(error), Some(_)) => tracing::warn!(
                server = %server,
                error = %error_chain(error),
                "MCP server could not be connected; connect it again from /server/settings/ once the cause is fixed"
            ),
            (None, _) => tracing::info!(server = %server, "MCP server connected"),
            (Some(_), None) => {}
        }
        let (unused, replaced) = self.write_servers(|servers| {
            let Some(entry) = servers
                .iter_mut()
                .find(|entry| entry.config.name() == server && entry.config == config)
            else {
                return (session, None);
            };
            match (session, failure) {
                (Some(session), _) => {
                    entry.condition = Condition::Open;
                    (None, entry.session.replace(session))
                }
                (None, Some(Readiness::LoginRequired(recovery))) => {
                    entry.condition = Condition::LoginRequired(recovery);
                    (None, entry.session.take())
                }
                (None, Some(failure)) => {
                    entry.condition = Condition::Failed(failure);
                    (None, entry.session.take())
                }
                (None, None) => (None, None),
            }
        });
        for session in unused.into_iter().chain(replaced) {
            self.spawn_retire(session);
        }
        error.map_or(Ok(()), Err)
    }

    fn attempt_lock(&self, server: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.shared.attempts.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(locks.entry(server.to_owned()).or_default())
    }

    fn read_servers<T>(&self, read: impl FnOnce(&Vec<Server>) -> T) -> T {
        read(&self.shared.servers.read().unwrap_or_else(PoisonError::into_inner))
    }

    fn write_servers<T>(&self, write: impl FnOnce(&mut Vec<Server>) -> T) -> T {
        write(&mut self.shared.servers.write().unwrap_or_else(PoisonError::into_inner))
    }
}

/// Whether an attempt was already under way when a reconnect was asked for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AlreadyConnecting {
    Yes,
    No,
}

/// Whether a stdio entry that cannot be connected ends start-up.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Strictness {
    /// Keep every failure and serve regardless.
    Lenient,
    /// Fail on the first stdio entry that cannot be connected.
    StdioRequired,
}

#[cfg(test)]
#[path = "mcp_client_set_test.rs"]
mod tests;
