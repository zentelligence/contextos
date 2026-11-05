use super::*;
use rmcp::ServiceExt;

#[tokio::test]
async fn connecting_to_a_nonexistent_command_fails_to_spawn() {
    let entry = McpServerConfig::Stdio {
        name: "contextos".to_owned(),
        command: "this-command-definitely-does-not-exist-9f3c".to_owned(),
        args: Vec::new(),
    };

    let result = McpClient::connect(&entry).await;

    assert!(matches!(result, Err(McpConnectError::Spawn { .. })));
}

#[tokio::test]
async fn a_command_that_cannot_start_is_named_in_the_error_without_its_arguments() -> Result<(), &'static str> {
    let entry = McpServerConfig::Stdio {
        name: "contextos".to_owned(),
        command: "this-command-definitely-does-not-exist-9f3c".to_owned(),
        args: vec!["--token".to_owned(), "secret-argument".to_owned()],
    };

    let error = McpClient::connect(&entry)
        .await
        .err()
        .ok_or("a command that does not exist cannot start")?;

    let message = error.to_string();
    assert!(
        message.contains("this-command-definitely-does-not-exist-9f3c"),
        "{message}"
    );
    assert!(!message.contains("secret-argument"), "{message}");
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_process_that_exits_without_ever_completing_the_handshake_is_a_startup_error()
-> Result<(), Box<dyn std::error::Error>> {
    // The process exits immediately without writing a response: the
    // transport observes a closed stream while still waiting for the
    // `initialize` response, so the handshake fails fast rather than
    // hanging, exercising the "startup error, not a lazily-discovered one"
    // requirement without needing a real MCP server binary. Bounded by an explicit
    // timeout as a safety net, not as the thing under test: the assertion
    // on the inner `Result` is what proves this fails fast, not the bound.
    let entry = McpServerConfig::Stdio {
        name: "contextos".to_owned(),
        command: "sh".to_owned(),
        args: vec!["-c".to_owned(), "exit 0".to_owned()],
    };

    let Ok(result) = tokio::time::timeout(std::time::Duration::from_secs(5), McpClient::connect(&entry)).await else {
        return Err("connect() must fail fast, not hang, once the child process exits".into());
    };

    assert!(matches!(result, Err(McpConnectError::Handshake { .. })));
    Ok(())
}

#[tokio::test]
async fn an_http_entry_with_an_unset_token_env_fails_before_connecting() -> std::io::Result<()> {
    // Deliberately not cleared via `std::env::remove_var`: this workspace
    // forbids `unsafe_code`, and mutating process environment variables is
    // `unsafe` from the 2024 edition on. A name this specific is not
    // expected to be set in any real environment this test runs in.
    let variable = "CONTEXTOS_WEB_TEST_UNSET_TOKEN_9F3C";
    assert!(
        std::env::var(variable).is_err(),
        "test precondition: {variable} must not be set in this environment"
    );
    let entry = McpServerConfig::Http {
        name: "some-other-server".to_owned(),
        endpoint: crate::test_support::unreachable_loopback_url()?,
        token_env: Some(variable.to_owned()),
        auth: None,
        headers: std::collections::BTreeMap::new(),
    };

    let result = McpClient::connect(&entry).await;

    assert!(matches!(result, Err(McpConnectError::MissingTokenEnv { .. })));
    Ok(())
}

#[tokio::test]
async fn an_http_entry_with_a_bearer_credential_ref_that_was_never_pasted_fails_before_connecting()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = Arc::new(FsCredentialStore::new(dir.path().to_path_buf()));
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let entry = McpServerConfig::Http {
        name: "some-server".to_owned(),
        endpoint: crate::test_support::unreachable_loopback_url()?,
        token_env: None,
        auth: Some(crate::config::McpServerAuth::Bearer {
            token_env: None,
            credential_ref: Some("never-pasted".to_owned()),
        }),
        headers: std::collections::BTreeMap::new(),
    };

    let result = McpClient::connect_with(&entry, &store, &clock).await;

    assert!(matches!(result, Err(McpConnectError::MissingCredential { .. })));
    Ok(())
}

/// A session over a server served in process, which exposes no tools and answers every request.
async fn in_process_session(name: &str) -> Result<McpClient, Box<dyn std::error::Error>> {
    struct TestServer;
    impl rmcp::ServerHandler for TestServer {}
    let (client_io, server_io) = tokio::io::duplex(4096);
    tokio::spawn(async move {
        if let Ok(server) = TestServer.serve(server_io).await {
            let _ = server.waiting().await;
        }
    });
    Ok(McpClient::over(name, ().serve(client_io).await?))
}

#[tokio::test]
async fn closing_a_shared_session_closes_it_for_every_owner() -> Result<(), Box<dyn std::error::Error>> {
    let client = Arc::new(in_process_session("test").await?);
    let other_owner = Arc::clone(&client);

    client.close().await;

    assert!(
        other_owner.is_closed().await,
        "close must close a session even with other owners"
    );
    Ok(())
}

#[tokio::test]
async fn a_retired_session_closes_only_once_the_call_running_on_it_has_finished()
-> Result<(), Box<dyn std::error::Error>> {
    let client = Arc::new(in_process_session("test").await?);
    let running_call = client.calls.read().await;
    let retiring = tokio::spawn({
        let client = Arc::clone(&client);
        async move { client.retire(std::time::Duration::from_secs(60)).await }
    });
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }

    assert!(!client.is_closed().await, "a running call keeps its session open");
    drop(running_call);
    retiring.await?;
    assert!(
        client.is_closed().await,
        "the session closes once the call has finished"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn a_retired_session_closes_after_its_drain_period_even_with_a_call_still_running()
-> Result<(), Box<dyn std::error::Error>> {
    let client = Arc::new(in_process_session("test").await?);
    let _running_call = client.calls.read().await;

    client.retire(std::time::Duration::from_secs(30)).await;

    assert!(
        client.is_closed().await,
        "a call that never ends cannot hold a replaced session open forever"
    );
    Ok(())
}

#[derive(Debug, thiserror::Error)]
#[error("outer")]
struct Outer(#[source] Inner);

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct Inner(String);

#[test]
fn an_error_chain_names_every_cause_outermost_first() {
    let error = Outer(Inner("HTTP 500 Internal Server Error".to_owned()));
    assert_eq!(error_chain(&error), "outer: HTTP 500 Internal Server Error");
}

#[test]
fn an_error_chain_is_cut_at_its_limit() {
    let error = Outer(Inner("x".repeat(2 * MAX_ERROR_CHAIN_CHARS)));
    let chain = error_chain(&error);
    assert_eq!(chain.chars().count(), MAX_ERROR_CHAIN_CHARS + 1);
    assert!(chain.ends_with('…'));
}

// ---------------------------------------------------------------------
// A renewed `oauth2` token reaches a session that is already open.
// ---------------------------------------------------------------------

/// A legacy, session-based MCP endpoint that records the `Authorization` header of each `tools/list` request.
async fn recording_mcp_endpoint() -> Result<(String, Arc<std::sync::Mutex<Vec<String>>>), Box<dyn std::error::Error>> {
    use axum::response::IntoResponse;

    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    let router = axum::Router::new().route(
        "/mcp",
        axum::routing::post(
            move |headers: axum::http::HeaderMap, axum::Json(request): axum::Json<serde_json::Value>| {
                let recorded = Arc::clone(&recorded);
                async move {
                    if request["method"] == "initialize" {
                        (
                            [("mcp-session-id", "fixture")],
                            axum::Json(serde_json::json!({
                                "jsonrpc": "2.0", "id": request["id"], "result": {
                                    "protocolVersion": "2025-11-25", "capabilities": {"tools": {}},
                                    "serverInfo": {"name": "fixture", "version": "1"}
                                }
                            })),
                        )
                            .into_response()
                    } else if request["method"] == "tools/list" {
                        let authorization = headers
                            .get(axum::http::header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or_default()
                            .to_owned();
                        recorded
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(authorization);
                        axum::Json(serde_json::json!({"jsonrpc": "2.0", "id": request["id"], "result": {"tools": []}}))
                            .into_response()
                    } else if request.get("id").is_some() {
                        axum::Json(serde_json::json!({
                            "jsonrpc": "2.0", "id": request["id"],
                            "error": {"code": -32601, "message": "Method not found"}
                        }))
                        .into_response()
                    } else {
                        axum::http::StatusCode::ACCEPTED.into_response()
                    }
                }
            },
        )
        .delete(|| async { axum::http::StatusCode::OK }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/mcp", listener.local_addr()?);
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok((endpoint, seen))
}

fn fresh_token(access_token: &str) -> Result<crate::credential_store::CredentialRecord, Box<dyn std::error::Error>> {
    use time::format_description::well_known::Rfc3339;
    let now = time::OffsetDateTime::now_utc();
    Ok(crate::credential_store::CredentialRecord::Oauth2Token {
        access_token: access_token.to_owned(),
        refresh_token: Some("refresh".to_owned()),
        expires_at: (now + time::Duration::hours(1)).format(&Rfc3339)?,
        obtained_at: now.format(&Rfc3339)?,
    })
}

#[tokio::test]
async fn a_token_renewed_after_the_session_opened_is_presented_on_its_next_request()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::credential_store::CredentialStore;

    let (endpoint, seen) = recording_mcp_endpoint().await?;
    let dir = tempfile::tempdir()?;
    let store = Arc::new(FsCredentialStore::new(dir.path().to_path_buf()));
    store.set("provider-token", &fresh_token("first")?)?;
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let entry = McpServerConfig::Http {
        name: "provider".to_owned(),
        endpoint,
        token_env: None,
        auth: Some(crate::config::McpServerAuth::Oauth2 {
            client_id: None,
            client_secret_ref: None,
            client_secret_env: None,
            authorization_endpoint: "https://example-provider.invalid/authorize".to_owned(),
            token_endpoint: "https://example-provider.invalid/token".to_owned(),
            scopes: vec!["mcp".to_owned()],
            credential_ref: "provider-token".to_owned(),
        }),
        headers: std::collections::BTreeMap::new(),
    };
    let client = McpClient::connect_with(&entry, &store, &clock).await?;
    client.ensure_oauth_token_fresh().await?;
    client.list_all_tools().await?;

    store.set("provider-token", &fresh_token("second")?)?;
    client.ensure_oauth_token_fresh().await?;
    client.list_all_tools().await?;

    let seen = seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    assert_eq!(seen, ["Bearer first", "Bearer second"]);
    client.close().await;
    Ok(())
}
