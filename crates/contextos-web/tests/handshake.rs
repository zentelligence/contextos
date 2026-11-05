//! `contextos-web` refuses to serve any HTTP request until every startup connection attempt has
//! resolved. A misconfigured `Stdio` entry (a local command wholly within this operator's own
//! configuration) is still a startup error, never a request-time surprise. An unreachable `Http`
//! entry is not: it reaches a third-party server whose own outage or a stale credential must not
//! take the whole process down with it, so it is simply left out of the connected set instead
//! (`mcp_client::McpClientSet::connect_with`'s own doc comment has the full rationale).

mod support;

use contextos_web::config::McpServerConfig;
use contextos_web::mcp_client::McpClientSet;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[tokio::test]
async fn a_misconfigured_command_fails_the_whole_connect_call_before_any_request_is_possible() -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let vault = dir.path().join("vault");
    std::fs::create_dir_all(&vault)?;
    let config_path = support::write_vault_config(dir.path(), &vault)?;
    let good = support::real_contextos_entry("contextos", &config_path)?;
    let bad = McpServerConfig::Stdio {
        name: "broken".to_owned(),
        command: "this-command-definitely-does-not-exist-9f3c".to_owned(),
        args: Vec::new(),
    };

    // The good entry is listed first: if `connect` degraded to
    // "connect what you can, report the rest", this would be the case most
    // likely to hide it (a partially-populated, seemingly-usable set).
    let result = McpClientSet::connect(&[good, bad]).await;

    assert!(
        result.is_err(),
        "a misconfigured entry must fail the whole connect call, not just be reported \
         alongside a partially-live set"
    );
    Ok(())
}

#[tokio::test]
async fn an_unreachable_http_endpoint_is_skipped_rather_than_failing_startup() -> Result<(), BoxError> {
    // Bind an ephemeral loopback port, then drop the listener before
    // connecting: the OS refuses the next connection to that exact port
    // immediately (a real, deterministic "nothing is listening here"),
    // unlike a fixed low port number, which some sandboxed network
    // environments silently drop instead of refusing, turning the
    // connection attempt into an OS-level timeout of a minute or more
    // rather than the fast failure this test exists to prove.
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);

    let entry = McpServerConfig::Http {
        name: "unreachable".to_owned(),
        endpoint: format!("http://127.0.0.1:{port}"),
        token_env: None,
        auth: None,
        headers: std::collections::BTreeMap::new(),
    };

    // Observed in practice (2026-09-20): a single `[[mcp_server]]` entry with a stale credential
    // used to abort `contextos-web`'s entire startup with a hard `Error: Handshake { .. }`, taking
    // down every other configured server, the vault browser, apps, and jobs along with it. An
    // unreachable/misauthenticated `Http` entry must instead resolve `Ok`, fast, with that one
    // entry simply absent from the connected set.
    let Ok(result) = tokio::time::timeout(std::time::Duration::from_secs(10), McpClientSet::connect(&[entry])).await
    else {
        return Err("an unreachable endpoint must fail fast, not hang the whole startup".into());
    };

    let set = result?;
    assert!(
        set.get("unreachable").is_none(),
        "an unreachable Http entry must be absent from the connected set, not connected"
    );
    Ok(())
}
