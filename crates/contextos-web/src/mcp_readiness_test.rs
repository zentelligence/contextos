use std::collections::BTreeMap;
use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::IntoResponse;

use super::*;
use crate::credential_store::{CredentialRecord, CredentialStore, FsCredentialStore};
use crate::logging::{Clock, SystemClock};
use crate::mcp_client::McpClient;

type BoxError = Box<dyn std::error::Error>;

fn stdio_entry() -> McpServerConfig {
    McpServerConfig::Stdio {
        name: "contextos".to_owned(),
        command: "contextos".to_owned(),
        args: Vec::new(),
    }
}

fn http_entry(endpoint: &str, auth: Option<McpServerAuth>) -> McpServerConfig {
    McpServerConfig::Http {
        name: "remote".to_owned(),
        endpoint: endpoint.to_owned(),
        token_env: None,
        auth,
        headers: BTreeMap::new(),
    }
}

fn oauth2() -> McpServerAuth {
    McpServerAuth::Oauth2 {
        client_id: None,
        client_secret_ref: None,
        client_secret_env: None,
        authorization_endpoint: "https://example-provider.invalid/authorize".to_owned(),
        token_endpoint: "https://example-provider.invalid/token".to_owned(),
        scopes: vec!["mcp".to_owned()],
        credential_ref: "remote-oauth-token".to_owned(),
    }
}

fn pasted_bearer() -> McpServerAuth {
    McpServerAuth::Bearer {
        token_env: None,
        credential_ref: Some("remote-token".to_owned()),
    }
}

fn readiness_after(entry: &McpServerConfig, error: &McpConnectError) -> Readiness {
    Readiness::from(FailedAttempt { entry, error })
}

/// A loopback MCP endpoint that answers every request with `status`, and a `WWW-Authenticate` challenge with a 401.
async fn refusing_endpoint(status: StatusCode) -> std::io::Result<String> {
    let router = axum::Router::new().route(
        "/mcp",
        axum::routing::post(move || async move {
            if status == StatusCode::UNAUTHORIZED {
                (status, [("www-authenticate", "Bearer realm=\"mcp\"")]).into_response()
            } else {
                status.into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/mcp", listener.local_addr()?);
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok(endpoint)
}

async fn connect_error(entry: &McpServerConfig, store: &Arc<FsCredentialStore>) -> Result<McpConnectError, BoxError> {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    match McpClient::connect_with(entry, store, &clock).await {
        Ok(_) => Err("the endpoint refuses every request, so no session can open".into()),
        Err(error) => Ok(error),
    }
}

#[test]
fn a_command_that_cannot_start_is_a_failure_with_its_cause() {
    let error = McpConnectError::Spawn {
        server: "contextos".to_owned(),
        command: "contextos".to_owned(),
        source: std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
    };

    let readiness = readiness_after(&stdio_entry(), &error);

    assert_eq!(
        readiness,
        Readiness::Failed(ConnectionFailure {
            cause: FailureCause::NotStarted,
            detail: "failed to spawn MCP server \"contextos\" (command \"contextos\"): no such file".to_owned(),
        })
    );
}

#[test]
fn an_unset_token_variable_names_the_variable() {
    let error = McpConnectError::MissingTokenEnv {
        server: "remote".to_owned(),
        variable: "REMOTE_TOKEN".to_owned(),
    };

    let readiness = readiness_after(&http_entry("http://127.0.0.1:1/mcp", None), &error);

    assert!(matches!(
        readiness,
        Readiness::Failed(ConnectionFailure { cause: FailureCause::TokenEnvUnset { variable }, .. }) if variable == "REMOTE_TOKEN"
    ));
}

#[test]
fn a_bearer_token_never_pasted_needs_one_pasted() {
    let error = McpConnectError::MissingCredential {
        server: "remote".to_owned(),
        credential_ref: "remote-token".to_owned(),
    };

    let readiness = readiness_after(&http_entry("http://127.0.0.1:1/mcp", Some(pasted_bearer())), &error);

    assert_eq!(readiness, Readiness::LoginRequired(LoginRecovery::ReplaceBearerToken));
}

#[test]
fn an_invalid_header_names_the_header_and_a_timeout_reads_as_one() {
    let entry = http_entry("http://127.0.0.1:1/mcp", None);
    let invalid = McpConnectError::InvalidHeader {
        server: "remote".to_owned(),
        key: "bad header".to_owned(),
    };
    let timed_out = McpConnectError::TimedOut {
        server: "remote".to_owned(),
        after: std::time::Duration::from_secs(60),
    };

    assert!(matches!(
        readiness_after(&entry, &invalid),
        Readiness::Failed(ConnectionFailure { cause: FailureCause::InvalidHeader { key }, .. }) if key == "bad header"
    ));
    assert!(matches!(
        readiness_after(&entry, &timed_out),
        Readiness::Failed(ConnectionFailure {
            cause: FailureCause::TimedOut,
            ..
        })
    ));
}

#[tokio::test]
async fn an_oauth2_server_that_answers_401_needs_its_connect_flow() -> Result<(), BoxError> {
    let endpoint = refusing_endpoint(StatusCode::UNAUTHORIZED).await?;
    let dir = tempfile::tempdir()?;
    let store = Arc::new(FsCredentialStore::new(dir.path().to_path_buf()));
    let entry = http_entry(&endpoint, Some(oauth2()));

    let error = connect_error(&entry, &store).await?;

    assert_eq!(
        readiness_after(&entry, &error),
        Readiness::LoginRequired(LoginRecovery::OauthConnect)
    );
    Ok(())
}

#[tokio::test]
async fn a_rejected_bearer_token_needs_a_new_one_and_is_never_shown_in_the_cause() -> Result<(), BoxError> {
    let endpoint = refusing_endpoint(StatusCode::UNAUTHORIZED).await?;
    let dir = tempfile::tempdir()?;
    let store = Arc::new(FsCredentialStore::new(dir.path().to_path_buf()));
    store.set(
        "remote-token",
        &CredentialRecord::Bearer {
            value: "super-secret-token".to_owned(),
            updated_at: "2026-01-01T00:00:00Z".to_owned(),
        },
    )?;
    let entry = http_entry(&endpoint, Some(pasted_bearer()));

    let error = connect_error(&entry, &store).await?;

    assert_eq!(
        readiness_after(&entry, &error),
        Readiness::LoginRequired(LoginRecovery::ReplaceBearerToken)
    );
    assert!(!error_chain(&error).contains("super-secret-token"));
    Ok(())
}

#[tokio::test]
async fn a_server_asking_for_credentials_the_entry_does_not_configure_needs_an_authentication_mode()
-> Result<(), BoxError> {
    let endpoint = refusing_endpoint(StatusCode::UNAUTHORIZED).await?;
    let dir = tempfile::tempdir()?;
    let store = Arc::new(FsCredentialStore::new(dir.path().to_path_buf()));
    let entry = http_entry(&endpoint, None);

    let error = connect_error(&entry, &store).await?;

    assert_eq!(
        readiness_after(&entry, &error),
        Readiness::LoginRequired(LoginRecovery::ConfigureAuthentication)
    );
    Ok(())
}

#[tokio::test]
async fn a_server_error_during_the_handshake_is_a_failure_not_a_login() -> Result<(), BoxError> {
    let endpoint = refusing_endpoint(StatusCode::INTERNAL_SERVER_ERROR).await?;
    let dir = tempfile::tempdir()?;
    let store = Arc::new(FsCredentialStore::new(dir.path().to_path_buf()));
    let entry = http_entry(&endpoint, Some(oauth2()));

    let error = connect_error(&entry, &store).await?;

    assert!(
        matches!(
            readiness_after(&entry, &error),
            Readiness::Failed(ConnectionFailure {
                cause: FailureCause::HandshakeFailed,
                ..
            })
        ),
        "{error:?}"
    );
    Ok(())
}
