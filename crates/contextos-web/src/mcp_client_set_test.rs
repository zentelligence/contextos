use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use rmcp::ServiceExt;

use super::*;
use crate::config::McpServerAuth;
use crate::mcp_readiness::{ConnectionFailure, FailureCause};

type BoxError = Box<dyn std::error::Error>;

// ---------------------------------------------------------------------
// A connector whose outcome for each server is scripted, serving successful sessions in process.
// ---------------------------------------------------------------------

/// What one connection attempt does.
enum Plan {
    Open,
    FailToStart,
    /// Waits for the signal before opening, so a test can act while the attempt is under way.
    OpenAfter(tokio::sync::oneshot::Receiver<()>),
}

#[derive(Default)]
struct ScriptedConnector {
    plans: Mutex<HashMap<String, VecDeque<Plan>>>,
    /// Every configuration an attempt was made with, in order.
    attempted: Mutex<Vec<McpServerConfig>>,
    /// How many sessions it opened have since been closed by the client.
    closed: Arc<std::sync::atomic::AtomicUsize>,
}

impl ScriptedConnector {
    fn plan(&self, server: &str, plan: Plan) {
        self.plans
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(server.to_owned())
            .or_default()
            .push_back(plan);
    }

    fn attempted(&self) -> Vec<McpServerConfig> {
        self.attempted.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl SessionConnector for ScriptedConnector {
    fn open<'a>(
        &'a self,
        entry: &'a McpServerConfig,
        shutdown: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<McpClient, McpConnectError>> {
        Box::pin(async move {
            self.attempted
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(entry.clone());
            let plan = self
                .plans
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get_mut(entry.name())
                .and_then(VecDeque::pop_front)
                .unwrap_or(Plan::Open);
            match plan {
                Plan::Open => in_process_session(entry.name(), &self.closed).await,
                Plan::FailToStart => Err(McpConnectError::Spawn {
                    server: entry.name().to_owned(),
                    command: "missing".to_owned(),
                    source: std::io::Error::new(std::io::ErrorKind::NotFound, "no such command"),
                }),
                Plan::OpenAfter(signal) => {
                    tokio::select! {
                        () = shutdown.cancelled() => Err(McpConnectError::Shutdown),
                        _ = signal => in_process_session(entry.name(), &self.closed).await,
                    }
                }
            }
        })
    }
}

/// A session over a server served in process, which exposes no tools, counting in `closed` once the client closes it.
async fn in_process_session(
    name: &str,
    closed: &Arc<std::sync::atomic::AtomicUsize>,
) -> Result<McpClient, McpConnectError> {
    struct TestServer;
    impl rmcp::ServerHandler for TestServer {}
    let (client_io, server_io) = tokio::io::duplex(4096);
    let closed = Arc::clone(closed);
    tokio::spawn(async move {
        if let Ok(server) = TestServer.serve(server_io).await {
            let _ = server.waiting().await;
            closed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    });
    let running = ().serve(client_io).await.map_err(|source| McpConnectError::Handshake {
        server: name.to_owned(),
        source: Box::new(source),
    })?;
    Ok(McpClient::over(name, running))
}

fn stdio(name: &str, command: &str) -> McpServerConfig {
    McpServerConfig::Stdio {
        name: name.to_owned(),
        command: command.to_owned(),
        args: Vec::new(),
    }
}

fn oauth2_http(name: &str) -> McpServerConfig {
    McpServerConfig::Http {
        name: name.to_owned(),
        endpoint: "https://mcp.example-provider.invalid/mcp".to_owned(),
        token_env: None,
        auth: Some(McpServerAuth::Oauth2 {
            client_id: None,
            client_secret_ref: None,
            client_secret_env: None,
            authorization_endpoint: "https://example-provider.invalid/authorize".to_owned(),
            token_endpoint: "https://example-provider.invalid/token".to_owned(),
            scopes: vec!["mcp".to_owned()],
            credential_ref: "token".to_owned(),
        }),
        headers: std::collections::BTreeMap::new(),
    }
}

async fn started(
    connector: &Arc<ScriptedConnector>,
    entries: &[McpServerConfig],
) -> Result<McpClientSet, McpConnectError> {
    let connector: Arc<dyn SessionConnector> = Arc::clone(connector) as Arc<dyn SessionConnector>;
    McpClientSet::open_all(connector, entries, &CancellationToken::new(), Strictness::Lenient).await
}

/// Waits for every background attempt and retirement the set has started, by closing and draining its tracker.
async fn settled(set: &McpClientSet) {
    set.shared.tasks.close();
    set.shared.tasks.wait().await;
    set.shared.tasks.reopen();
}

/// Waits until `count` attempts have begun, bounded so a regression fails rather than hangs.
async fn attempts_begun(connector: &ScriptedConnector, count: usize) -> Result<(), tokio::time::error::Elapsed> {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while connector.attempted().len() < count {
            tokio::task::yield_now().await;
        }
    })
    .await
}

fn readiness_of(readiness: &[ServerReadiness], name: &str) -> Option<Readiness> {
    readiness
        .iter()
        .find(|server| server.name == name)
        .map(|server| server.readiness.clone())
}

// ---------------------------------------------------------------------
// Start-up
// ---------------------------------------------------------------------

#[tokio::test]
async fn start_up_keeps_a_stdio_server_that_cannot_start_with_its_cause_and_serves_the_others() -> Result<(), BoxError>
{
    let connector = Arc::new(ScriptedConnector::default());
    connector.plan("contextos", Plan::FailToStart);

    let set = started(&connector, &[stdio("contextos", "missing"), stdio("git", "git-mcp")]).await?;

    let readiness = set.readiness().await;
    assert!(
        matches!(
            readiness_of(&readiness, "contextos"),
            Some(Readiness::Failed(ConnectionFailure {
                cause: FailureCause::NotStarted,
                ..
            }))
        ),
        "{readiness:?}"
    );
    assert_eq!(
        readiness_of(&readiness, "git"),
        Some(Readiness::Connected { tool_count: 0 })
    );
    assert!(set.get("contextos").is_none());
    assert!(set.get("git").is_some());
    Ok(())
}

#[tokio::test]
async fn a_command_that_needs_its_stdio_servers_fails_with_the_first_one_that_cannot_start() -> Result<(), BoxError> {
    let connector = Arc::new(ScriptedConnector::default());
    connector.plan("contextos", Plan::FailToStart);
    let connector: Arc<dyn SessionConnector> = connector;

    let result = McpClientSet::open_all(
        connector,
        &[stdio("git", "git-mcp"), stdio("contextos", "missing")],
        &CancellationToken::new(),
        Strictness::StdioRequired,
    )
    .await;

    assert!(matches!(result, Err(McpConnectError::Spawn { server, .. }) if server == "contextos"));
    Ok(())
}

#[tokio::test]
async fn with_no_servers_configured_the_set_starts_empty_with_no_primary() -> Result<(), BoxError> {
    let connector = Arc::new(ScriptedConnector::default());

    let set = started(&connector, &[]).await?;

    assert_eq!(set.readiness().await, []);
    assert_eq!(set.primary_name(), None);
    assert!(set.primary().is_none());
    assert_eq!(set.status_summary().await, (0, 0, 0));
    Ok(())
}

#[tokio::test]
async fn every_configured_server_is_named_and_counted_whether_or_not_it_is_connected() -> Result<(), BoxError> {
    let connector = Arc::new(ScriptedConnector::default());
    connector.plan("contextos", Plan::FailToStart);

    let set = started(&connector, &[stdio("contextos", "missing"), stdio("git", "git-mcp")]).await?;

    assert_eq!(set.names(), ["contextos", "git"]);
    assert!(set.is_configured("contextos"));
    assert!(!set.is_configured("elsewhere"));
    assert_eq!(set.primary_name().as_deref(), Some("contextos"));
    assert!(
        set.primary().is_none(),
        "the primary is named even while it is not connected"
    );
    assert_eq!(set.status_summary().await, (1, 2, 0));
    Ok(())
}

// ---------------------------------------------------------------------
// Reconnect
// ---------------------------------------------------------------------

#[tokio::test]
async fn reconnecting_a_server_that_failed_connects_it_once_it_can_start() -> Result<(), BoxError> {
    let connector = Arc::new(ScriptedConnector::default());
    connector.plan("contextos", Plan::FailToStart);
    let set = started(&connector, &[stdio("contextos", "contextos")]).await?;
    assert!(set.primary().is_none());

    set.reconnect("contextos").await?;

    assert_eq!(
        set.readiness_of("contextos").await.map(|server| server.readiness),
        Some(Readiness::Connected { tool_count: 0 })
    );
    assert!(set.primary().is_some());
    Ok(())
}

#[tokio::test]
async fn reconnecting_replaces_the_session_and_closes_the_one_it_replaced() -> Result<(), BoxError> {
    let connector = Arc::new(ScriptedConnector::default());
    let set = started(&connector, &[stdio("contextos", "contextos")]).await?;
    let replaced = set.get("contextos").ok_or("connected at start-up")?;

    set.reconnect("contextos").await?;
    settled(&set).await;

    let current = set.get("contextos").ok_or("connected again")?;
    assert!(!Arc::ptr_eq(&replaced, &current), "a reconnect opens a new session");
    assert!(replaced.is_closed().await, "the replaced session is closed");
    assert!(!current.is_closed().await);
    Ok(())
}

#[tokio::test]
async fn a_server_reads_as_connecting_while_its_reconnect_is_under_way() -> Result<(), BoxError> {
    let connector = Arc::new(ScriptedConnector::default());
    let set = started(&connector, &[stdio("contextos", "contextos")]).await?;
    let (release, signal) = tokio::sync::oneshot::channel();
    connector.plan("contextos", Plan::OpenAfter(signal));

    set.request_reconnect("contextos")?;

    assert_eq!(
        set.readiness_of("contextos").await.map(|server| server.readiness),
        Some(Readiness::Connecting)
    );
    assert!(
        set.get("contextos").is_some(),
        "the old session keeps serving until the new one is open"
    );
    let _ = release.send(());
    settled(&set).await;
    assert_eq!(
        set.readiness_of("contextos").await.map(|server| server.readiness),
        Some(Readiness::Connected { tool_count: 0 })
    );
    Ok(())
}

#[tokio::test]
async fn a_reconnect_that_fails_drops_the_old_session_and_reports_the_cause() -> Result<(), BoxError> {
    let connector = Arc::new(ScriptedConnector::default());
    let set = started(&connector, &[stdio("contextos", "contextos")]).await?;
    let replaced = set.get("contextos").ok_or("connected at start-up")?;
    connector.plan("contextos", Plan::FailToStart);

    set.reconnect("contextos").await?;
    settled(&set).await;

    assert!(set.get("contextos").is_none());
    assert!(replaced.is_closed().await);
    assert!(matches!(
        set.readiness_of("contextos").await.map(|server| server.readiness),
        Some(Readiness::Failed(ConnectionFailure {
            cause: FailureCause::NotStarted,
            ..
        }))
    ));
    Ok(())
}

#[tokio::test]
async fn reconnecting_a_server_that_is_not_configured_is_refused() -> Result<(), BoxError> {
    let connector = Arc::new(ScriptedConnector::default());
    let set = started(&connector, &[stdio("contextos", "contextos")]).await?;

    assert_eq!(
        set.reconnect("elsewhere").await,
        Err(NotConfigured {
            server: "elsewhere".to_owned()
        })
    );
    assert!(set.request_reconnect("elsewhere").is_err());
    Ok(())
}

// ---------------------------------------------------------------------
// Applying `web.toml`
// ---------------------------------------------------------------------

#[tokio::test]
async fn applying_the_configuration_connects_an_added_server_and_closes_a_removed_one() -> Result<(), BoxError> {
    let connector = Arc::new(ScriptedConnector::default());
    let set = started(&connector, &[stdio("contextos", "contextos"), stdio("git", "git-mcp")]).await?;
    let removed = set.get("git").ok_or("connected at start-up")?;
    let kept = set.get("contextos").ok_or("connected at start-up")?;

    let connecting = set.apply_config(&[stdio("contextos", "contextos"), stdio("markitdown", "uvx")]);
    settled(&set).await;

    assert_eq!(connecting, ["markitdown"]);
    assert_eq!(set.names(), ["contextos", "markitdown"]);
    assert!(set.get("markitdown").is_some());
    assert!(set.get("git").is_none());
    assert!(removed.is_closed().await, "a removed server's session is closed");
    let unchanged = set.get("contextos").ok_or("still connected")?;
    assert!(Arc::ptr_eq(&kept, &unchanged), "an unchanged server keeps its session");
    Ok(())
}

#[tokio::test]
async fn applying_a_changed_entry_connects_it_again_with_its_new_configuration() -> Result<(), BoxError> {
    let connector = Arc::new(ScriptedConnector::default());
    let set = started(&connector, &[stdio("contextos", "old-command")]).await?;

    let connecting = set.apply_config(&[stdio("contextos", "new-command")]);
    settled(&set).await;

    assert_eq!(connecting, ["contextos"]);
    assert_eq!(connector.attempted().last(), Some(&stdio("contextos", "new-command")));
    assert!(set.primary().is_some());
    Ok(())
}

#[tokio::test]
async fn the_first_server_added_to_an_empty_configuration_becomes_the_primary() -> Result<(), BoxError> {
    let connector = Arc::new(ScriptedConnector::default());
    let set = started(&connector, &[]).await?;

    set.apply_config(&[stdio("contextos", "contextos")]);
    settled(&set).await;

    assert_eq!(set.primary_name().as_deref(), Some("contextos"));
    assert!(set.primary().is_some());
    Ok(())
}

#[tokio::test]
async fn an_attempt_overtaken_by_a_newer_configuration_never_overwrites_it() -> Result<(), BoxError> {
    let connector = Arc::new(ScriptedConnector::default());
    connector.plan("contextos", Plan::FailToStart);
    let set = started(&connector, &[stdio("contextos", "first")]).await?;
    let (release_stale, stale) = tokio::sync::oneshot::channel();
    let (release_current, current) = tokio::sync::oneshot::channel();
    connector.plan("contextos", Plan::OpenAfter(stale));
    connector.plan("contextos", Plan::OpenAfter(current));

    set.request_reconnect("contextos")?;
    attempts_begun(&connector, 2).await?;
    set.apply_config(&[stdio("contextos", "second")]);
    let _ = release_stale.send(());
    // The stale attempt has ended once the attempt for the newer configuration has begun.
    attempts_begun(&connector, 3).await?;

    assert_eq!(
        set.readiness_of("contextos").await.map(|server| server.readiness),
        Some(Readiness::Connecting),
        "a session opened for the older configuration is never installed"
    );
    assert!(set.get("contextos").is_none());
    let _ = release_current.send(());
    settled(&set).await;
    assert_eq!(connector.attempted().last(), Some(&stdio("contextos", "second")));
    assert!(set.get("contextos").is_some());
    Ok(())
}

// ---------------------------------------------------------------------
// Login required, and shutdown
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_token_that_can_no_longer_be_refreshed_reads_as_needing_a_login_until_reconnected() -> Result<(), BoxError> {
    let connector = Arc::new(ScriptedConnector::default());
    let set = started(&connector, &[oauth2_http("provider")]).await?;
    let session = set.get("provider").ok_or("connected at start-up")?;

    set.note_refresh_outcome("provider", &session, false);

    assert_eq!(
        set.readiness_of("provider").await.map(|server| server.readiness),
        Some(Readiness::LoginRequired(LoginRecovery::OauthConnect))
    );
    assert!(
        set.get("provider").is_some(),
        "its calls still reach the refresh gate, which names the remedy"
    );
    set.reconnect("provider").await?;
    assert_eq!(
        set.readiness_of("provider").await.map(|server| server.readiness),
        Some(Readiness::Connected { tool_count: 0 })
    );
    Ok(())
}

#[tokio::test]
async fn shutdown_closes_every_session_even_one_with_other_owners() -> Result<(), BoxError> {
    let connector = Arc::new(ScriptedConnector::default());
    let set = started(&connector, &[stdio("contextos", "contextos")]).await?;
    let held_elsewhere = set.get("contextos").ok_or("connected at start-up")?;

    set.shutdown().await;

    assert!(held_elsewhere.is_closed().await);
    assert!(set.get("contextos").is_none());
    Ok(())
}

#[tokio::test]
async fn shutdown_cancels_an_attempt_still_under_way() -> Result<(), BoxError> {
    let connector = Arc::new(ScriptedConnector::default());
    let set = started(&connector, &[stdio("contextos", "contextos")]).await?;
    let (_release, signal) = tokio::sync::oneshot::channel::<()>();
    connector.plan("contextos", Plan::OpenAfter(signal));
    set.request_reconnect("contextos")?;

    let finished = tokio::time::timeout(std::time::Duration::from_secs(5), set.shutdown()).await;

    assert!(finished.is_ok(), "shutdown must not wait on an attempt that never ends");
    Ok(())
}

// ---------------------------------------------------------------------
// `McpClientSet::connect_with`: an `Http` entry's own connection failure must
// never abort the whole startup, unlike a `Stdio` entry's.
// ---------------------------------------------------------------------

fn unreachable_http_entry(name: &str) -> std::io::Result<McpServerConfig> {
    // A connection to it is refused at once, with no server needing to exist.
    Ok(McpServerConfig::Http {
        name: name.to_owned(),
        endpoint: crate::test_support::unreachable_loopback_url()?,
        token_env: None,
        auth: None,
        headers: std::collections::BTreeMap::new(),
    })
}

#[tokio::test]
async fn an_unreachable_http_entry_is_skipped_rather_than_failing_the_whole_set()
-> Result<(), Box<dyn std::error::Error>> {
    let entries = [unreachable_http_entry("unreachable")?];
    let (store, clock) = no_oauth_dependencies();

    let set = McpClientSet::connect_with(&entries, &store, &clock).await?;

    assert!(
        set.get("unreachable").is_none(),
        "an entry whose connection failed must be absent from the set, not cause connect_with itself to fail"
    );
    Ok(())
}

#[tokio::test]
async fn a_stdio_failure_after_a_skipped_http_failure_still_aborts_the_set() -> std::io::Result<()> {
    // Order matters here: the `Http` entry's failure must be caught and continued past *before*
    // the loop ever reaches the `Stdio` entry, proving that skip does not somehow swallow or
    // interfere with a later `Stdio` entry's own, still fail-fast, failure.
    let entries = [
        unreachable_http_entry("unreachable")?,
        McpServerConfig::Stdio {
            name: "contextos".to_owned(),
            command: "this-command-definitely-does-not-exist-9f3c".to_owned(),
            args: Vec::new(),
        },
    ];
    let (store, clock) = no_oauth_dependencies();

    let result = McpClientSet::connect_with(&entries, &store, &clock).await;

    assert!(
        matches!(result, Err(McpConnectError::Spawn { .. })),
        "a later Stdio entry's own failure must still abort the set, even after an earlier Http \
         entry's failure was already skipped"
    );
    Ok(())
}

#[tokio::test]
async fn a_failing_stdio_entry_still_fails_the_whole_set_fast() {
    let entries = [McpServerConfig::Stdio {
        name: "contextos".to_owned(),
        command: "this-command-definitely-does-not-exist-9f3c".to_owned(),
        args: Vec::new(),
    }];
    let (store, clock) = no_oauth_dependencies();

    let result = McpClientSet::connect_with(&entries, &store, &clock).await;

    assert!(
        matches!(result, Err(McpConnectError::Spawn { .. })),
        "a Stdio entry's own connection failure must still abort the whole set, unlike an Http entry's"
    );
}

// ---------------------------------------------------------------------
// Refresh outcomes, and repeated reconnect requests
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_refresh_that_succeeds_again_clears_a_login_required_mark() -> Result<(), BoxError> {
    let connector = Arc::new(ScriptedConnector::default());
    let set = started(&connector, &[oauth2_http("provider")]).await?;
    let session = set.get("provider").ok_or("connected at start-up")?;

    set.note_refresh_outcome("provider", &session, false);
    assert_eq!(
        set.readiness_of("provider").await.map(|server| server.readiness),
        Some(Readiness::LoginRequired(LoginRecovery::OauthConnect))
    );
    set.note_refresh_outcome("provider", &session, true);

    assert_eq!(
        set.readiness_of("provider").await.map(|server| server.readiness),
        Some(Readiness::Connected { tool_count: 0 }),
        "a provider that was briefly unreachable is not left marked once a refresh succeeds"
    );
    Ok(())
}

#[tokio::test]
async fn a_failed_refresh_on_a_replaced_session_never_marks_its_successor() -> Result<(), BoxError> {
    let connector = Arc::new(ScriptedConnector::default());
    let set = started(&connector, &[oauth2_http("provider")]).await?;
    let replaced = set.get("provider").ok_or("connected at start-up")?;
    set.reconnect("provider").await?;

    set.note_refresh_outcome("provider", &replaced, false);

    assert_eq!(
        set.readiness_of("provider").await.map(|server| server.readiness),
        Some(Readiness::Connected { tool_count: 0 })
    );
    Ok(())
}

#[tokio::test]
async fn repeated_reconnect_requests_while_one_is_under_way_make_one_attempt() -> Result<(), BoxError> {
    let connector = Arc::new(ScriptedConnector::default());
    let set = started(&connector, &[stdio("contextos", "contextos")]).await?;
    let (release, signal) = tokio::sync::oneshot::channel();
    connector.plan("contextos", Plan::OpenAfter(signal));

    for _ in 0..5 {
        set.request_reconnect("contextos")?;
    }
    let _ = release.send(());
    settled(&set).await;

    assert_eq!(connector.attempted().len(), 2, "the start-up attempt and one reconnect");
    assert!(set.get("contextos").is_some());
    Ok(())
}

#[tokio::test]
async fn the_session_an_overtaken_attempt_opened_is_closed() -> Result<(), BoxError> {
    let connector = Arc::new(ScriptedConnector::default());
    connector.plan("contextos", Plan::FailToStart);
    let set = started(&connector, &[stdio("contextos", "first")]).await?;
    let (release, signal) = tokio::sync::oneshot::channel();
    connector.plan("contextos", Plan::OpenAfter(signal));
    connector.plan("contextos", Plan::FailToStart);
    set.request_reconnect("contextos")?;
    attempts_begun(&connector, 2).await?;
    set.apply_config(&[stdio("contextos", "second")]);

    let _ = release.send(());
    settled(&set).await;

    assert!(set.get("contextos").is_none());
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while connector.closed.load(std::sync::atomic::Ordering::SeqCst) < 1 {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    Ok(())
}
