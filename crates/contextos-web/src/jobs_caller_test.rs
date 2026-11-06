use std::sync::Arc;

use axum::response::IntoResponse;
use contextos_workflows::McpCaller;
use time::format_description::well_known::Rfc3339;

use super::*;
use crate::config::{McpServerAuth, McpServerConfig};
use crate::credential_store::{CredentialRecord, CredentialStore, FsCredentialStore};
use crate::logging::{Clock, SystemClock};
use crate::mcp_readiness::{LoginRecovery, Readiness};

type BoxError = Box<dyn std::error::Error>;

/// A legacy, session-based MCP endpoint that answers `initialize` and any tool call, counting the tool calls.
async fn answering_endpoint() -> Result<(String, Arc<std::sync::atomic::AtomicUsize>), BoxError> {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let router = axum::Router::new().route(
        "/mcp",
        axum::routing::post(move |axum::Json(request): axum::Json<serde_json::Value>| {
            let counted = Arc::clone(&counted);
            async move {
                match request["method"].as_str() {
                    Some("initialize") => (
                        [("mcp-session-id", "fixture")],
                        axum::Json(serde_json::json!({
                            "jsonrpc": "2.0", "id": request["id"], "result": {
                                "protocolVersion": "2025-11-25", "capabilities": {"tools": {}},
                                "serverInfo": {"name": "fixture", "version": "1"}
                            }
                        })),
                    )
                        .into_response(),
                    Some("tools/call") => {
                        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        axum::Json(serde_json::json!({
                            "jsonrpc": "2.0", "id": request["id"],
                            "result": {"content": [{"type": "text", "text": "{}"}]}
                        }))
                        .into_response()
                    }
                    _ if request.get("id").is_some() => axum::Json(serde_json::json!({
                        "jsonrpc": "2.0", "id": request["id"],
                        "error": {"code": -32601, "message": "Method not found"}
                    }))
                    .into_response(),
                    _ => axum::http::StatusCode::ACCEPTED.into_response(),
                }
            }
        })
        .delete(|| async { axum::http::StatusCode::OK }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/mcp", listener.local_addr()?);
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok((endpoint, calls))
}

#[tokio::test]
async fn a_run_never_calls_with_a_token_that_has_expired_and_cannot_be_refreshed() -> Result<(), BoxError> {
    let (endpoint, calls) = answering_endpoint().await?;
    let dir = tempfile::tempdir()?;
    let store = Arc::new(FsCredentialStore::new(dir.path().to_path_buf()));
    let an_hour_ago = time::OffsetDateTime::now_utc() - time::Duration::hours(1);
    store.set(
        "provider-token",
        &CredentialRecord::Oauth2Token {
            access_token: "expired".to_owned(),
            refresh_token: None,
            expires_at: an_hour_ago.format(&Rfc3339)?,
            obtained_at: (an_hour_ago - time::Duration::hours(1)).format(&Rfc3339)?,
        },
    )?;
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let entry = McpServerConfig::Http {
        name: "provider".to_owned(),
        endpoint,
        token_env: None,
        auth: Some(McpServerAuth::Oauth2 {
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
    let clients = Arc::new(McpClientSet::connect_with(&[entry], &store, &clock).await?);
    let caller = WebMcpCaller::new(Arc::clone(&clients));

    let result = caller.call("provider", "anything", serde_json::Map::new()).await;

    let Err(failure) = result else {
        return Err("a token that cannot be refreshed must stop the call".into());
    };
    assert_eq!(failure.kind, WorkflowFailureKind::ServerUnavailable);
    assert!(
        failure.message.contains("/server/settings/oauth/provider/connect"),
        "{}",
        failure.message
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the stale token is never sent"
    );
    assert_eq!(
        clients.readiness_of("provider").await.map(|server| server.readiness),
        Some(Readiness::LoginRequired(LoginRecovery::OauthConnect))
    );
    clients.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn a_run_calling_a_configured_server_that_is_not_connected_says_so() -> Result<(), BoxError> {
    let entry = McpServerConfig::Http {
        name: "remote".to_owned(),
        endpoint: crate::test_support::unreachable_loopback_url()?,
        token_env: None,
        auth: None,
        headers: std::collections::BTreeMap::new(),
    };
    let clients = Arc::new(McpClientSet::connect(&[entry]).await?);
    let caller = WebMcpCaller::new(clients);

    let Err(failure) = caller.call("remote", "anything", serde_json::Map::new()).await else {
        return Err("an unconnected server cannot answer".into());
    };

    assert_eq!(failure.kind, WorkflowFailureKind::ServerUnavailable);
    assert!(failure.message.contains("not connected"), "{}", failure.message);
    Ok(())
}
