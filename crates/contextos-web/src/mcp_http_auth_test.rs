use super::*;

use crate::config::McpServerAuth;
use crate::credential_store::{CredentialRecord, FsCredentialStore};

// ---------------------------------------------------------------------
// `oauth_refresh_entry`: the pure gate deciding which sessions ever pay the
// refresh-check cost at all.
// ---------------------------------------------------------------------

fn oauth2_auth() -> McpServerAuth {
    McpServerAuth::Oauth2 {
        client_id: Some("some-client-id".to_owned()),
        client_secret_ref: Some("some-client-secret-ref".to_owned()),
        client_secret_env: None,
        authorization_endpoint: "https://example-provider.invalid/authorize".to_owned(),
        token_endpoint: "https://example-provider.invalid/token".to_owned(),
        scopes: vec!["mcp.tools.call".to_owned()],
        credential_ref: "some-credential-ref".to_owned(),
    }
}

#[test]
fn a_stdio_entry_never_resolves_an_oauth_refresh_entry() {
    let entry = McpServerConfig::Stdio {
        name: "contextos".to_owned(),
        command: "contextos-mcp".to_owned(),
        args: Vec::new(),
    };

    assert!(oauth_refresh_entry(&entry).is_none());
}

#[test]
fn an_http_entry_with_no_auth_never_resolves_an_oauth_refresh_entry() {
    let entry = McpServerConfig::Http {
        name: "some-server".to_owned(),
        endpoint: "https://mcp.example-provider.invalid/mcp".to_owned(),
        token_env: None,
        auth: None,
        headers: std::collections::BTreeMap::new(),
    };

    assert!(oauth_refresh_entry(&entry).is_none());
}

#[test]
fn a_bearer_mode_http_entry_never_resolves_an_oauth_refresh_entry() {
    let entry = McpServerConfig::Http {
        name: "some-server".to_owned(),
        endpoint: "https://mcp.example-provider.invalid/mcp".to_owned(),
        token_env: None,
        auth: Some(McpServerAuth::Bearer {
            token_env: None,
            credential_ref: Some("some-credential-ref".to_owned()),
        }),
        headers: std::collections::BTreeMap::new(),
    };

    assert!(oauth_refresh_entry(&entry).is_none());
}

#[test]
fn an_oauth2_mode_http_entry_resolves_every_field_a_refresh_needs() -> Result<(), Box<dyn std::error::Error>> {
    let entry = McpServerConfig::Http {
        name: "hosted-oauth".to_owned(),
        endpoint: "https://mcp.example-provider.invalid/mcp".to_owned(),
        token_env: None,
        auth: Some(oauth2_auth()),
        headers: std::collections::BTreeMap::new(),
    };

    let resolved = oauth_refresh_entry(&entry).ok_or("an oauth2-mode entry must resolve")?;

    assert_eq!(resolved.server_name, "hosted-oauth");
    assert_eq!(resolved.token_endpoint, "https://example-provider.invalid/token");
    assert_eq!(resolved.client_id.as_deref(), Some("some-client-id"));
    assert_eq!(
        resolved.client_secret,
        Some(crate::config::ClientSecretSource::Stored(
            "some-client-secret-ref".to_owned()
        ))
    );
    assert_eq!(resolved.credential_ref, "some-credential-ref");
    Ok(())
}

// ---------------------------------------------------------------------
// `resolve_bearer_header`: precedence between an explicit `[mcp_server.auth]`
// table and the legacy top-level `token_env` field.
//
// None of these tests ever sets a real process environment variable
// (`std::env::set_var` is `unsafe` from the 2024 edition on, and this crate
// forbids `unsafe_code` entirely, matching the pre-existing
// `an_http_entry_with_an_unset_token_env_fails_before_connecting` test's own
// documented reason for only ever exercising the *unset* case). Precedence
// is instead proven through which variable name a resulting
// `MissingTokenEnv` error names: that alone already distinguishes "legacy
// consulted" from "nested consulted" from "neither consulted", with no
// variable ever needing to actually be set.
// ---------------------------------------------------------------------

fn bearer_auth(token_env: Option<&str>, credential_ref: Option<&str>) -> McpServerAuth {
    McpServerAuth::Bearer {
        token_env: token_env.map(ToOwned::to_owned),
        credential_ref: credential_ref.map(ToOwned::to_owned),
    }
}

fn no_oauth_credential_store() -> Arc<FsCredentialStore> {
    Arc::new(FsCredentialStore::new(std::path::PathBuf::new()))
}

#[tokio::test]
async fn resolve_bearer_header_with_no_auth_table_falls_back_to_the_legacy_token_env() {
    let variable = "CONTEXTOS_WEB_TEST_LEGACY_TOKEN_7A1B";

    let legacy = variable.to_owned();
    let result = resolve_bearer_header("some-server", None, Some(&legacy), &no_oauth_credential_store()).await;

    assert!(
        matches!(result, Err(McpConnectError::MissingTokenEnv { variable: named, .. }) if named == variable),
        "an absent auth table must fall back to the legacy token_env, naming it in the error"
    );
}

#[tokio::test]
async fn resolve_bearer_header_ignores_a_stray_legacy_token_env_once_auth_mode_none_is_explicit()
-> Result<(), Box<dyn std::error::Error>> {
    let variable = "CONTEXTOS_WEB_TEST_STRAY_TOKEN_2C4D";

    let auth = McpServerAuth::None;
    let legacy = variable.to_owned();
    let resolved =
        resolve_bearer_header("some-server", Some(&auth), Some(&legacy), &no_oauth_credential_store()).await?;

    assert_eq!(
        resolved, None,
        "an explicit auth.mode = \"none\" must win over a stray legacy token_env: if the legacy \
         field had been consulted despite this unset variable, this would have failed with \
         MissingTokenEnv instead of resolving to no auth at all"
    );
    Ok(())
}

#[tokio::test]
async fn resolve_bearer_header_returns_none_for_an_oauth2_mode_entry_leaving_it_to_the_refresh_path()
-> Result<(), Box<dyn std::error::Error>> {
    let auth = oauth2_auth();
    let resolved = resolve_bearer_header("hosted-oauth", Some(&auth), None, &no_oauth_credential_store()).await?;

    assert_eq!(resolved, None);
    Ok(())
}

#[tokio::test]
async fn resolve_bearer_header_via_the_nested_auth_token_env_ignores_the_legacy_field() {
    let nested_variable = "CONTEXTOS_WEB_TEST_NESTED_TOKEN_5E6F";
    let legacy_variable = "CONTEXTOS_WEB_TEST_LEGACY_UNUSED_8G9H";

    let auth = bearer_auth(Some(nested_variable), None);
    let legacy = legacy_variable.to_owned();
    let result = resolve_bearer_header("some-server", Some(&auth), Some(&legacy), &no_oauth_credential_store()).await;

    assert!(
        matches!(result, Err(McpConnectError::MissingTokenEnv { variable: named, .. }) if named == nested_variable),
        "an explicit auth.mode = \"bearer\" with its own token_env must resolve that nested \
         variable, never the legacy one: the error must name {nested_variable:?}, not {legacy_variable:?}"
    );
}

#[tokio::test]
async fn resolve_bearer_header_via_credential_ref_reads_the_stored_bearer_record()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::credential_store::CredentialStore;

    let dir = tempfile::tempdir()?;
    let store = Arc::new(FsCredentialStore::new(dir.path().to_path_buf()));
    store.set(
        "pasted-token",
        &CredentialRecord::Bearer {
            value: "pasted-value".to_owned(),
            updated_at: "2026-09-20T00:00:00Z".to_owned(),
        },
    )?;

    let auth = bearer_auth(None, Some("pasted-token"));
    let resolved = resolve_bearer_header("some-server", Some(&auth), None, &store).await?;

    assert_eq!(resolved.as_deref(), Some("pasted-value"));
    Ok(())
}

#[tokio::test]
async fn resolve_bearer_header_via_credential_ref_fails_when_nothing_is_stored_yet()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = Arc::new(FsCredentialStore::new(dir.path().to_path_buf()));
    let auth = bearer_auth(None, Some("never-pasted"));

    let result = resolve_bearer_header("some-server", Some(&auth), None, &store).await;

    assert!(matches!(
        result,
        Err(McpConnectError::MissingCredential { credential_ref, .. }) if credential_ref == "never-pasted"
    ));
    Ok(())
}

// ---------------------------------------------------------------------
// `build_custom_headers`: an `[[mcp_server]]` entry's static `[mcp_server.headers]` map converted
// into the transport's own header map.
// ---------------------------------------------------------------------

#[test]
fn build_custom_headers_converts_a_valid_map() -> Result<(), Box<dyn std::error::Error>> {
    let mut headers = std::collections::BTreeMap::new();
    headers.insert("locationId".to_owned(), "abc123".to_owned());

    let built = build_custom_headers("some-server", &headers)?;

    let value = built
        .get(&HeaderName::from_static("locationid"))
        .ok_or("header missing")?;
    assert_eq!(value.to_str()?, "abc123");
    Ok(())
}

#[test]
fn build_custom_headers_rejects_an_invalid_header_name() {
    let mut headers = std::collections::BTreeMap::new();
    headers.insert("not a valid header name".to_owned(), "abc123".to_owned());

    let result = build_custom_headers("some-server", &headers);

    assert!(matches!(result, Err(McpConnectError::InvalidHeader { .. })));
}

// ---------------------------------------------------------------------
// `CredentialedHttpClient`: a session's credential is read per request, so a renewed token reaches a session that
// is already open.
// ---------------------------------------------------------------------

/// A loopback endpoint recording the `Authorization` header of every request it receives.
async fn header_recorder() -> std::io::Result<(Arc<str>, Arc<std::sync::Mutex<Vec<Option<String>>>>)> {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    let router = axum::Router::new().route(
        "/mcp",
        axum::routing::post(move |headers: axum::http::HeaderMap| {
            let recorded = Arc::clone(&recorded);
            async move {
                let authorization = headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok())
                    .map(ToOwned::to_owned);
                recorded
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(authorization);
                axum::http::StatusCode::ACCEPTED
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint: Arc<str> = format!("http://{}/mcp", listener.local_addr()?).into();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok((endpoint, seen))
}

fn notification() -> Result<ClientJsonRpcMessage, serde_json::Error> {
    serde_json::from_value(serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
}

#[tokio::test]
async fn a_replaced_credential_is_presented_on_the_next_request_in_place_of_the_configured_one()
-> Result<(), Box<dyn std::error::Error>> {
    let (endpoint, seen) = header_recorder().await?;
    let credential = SessionCredential::new(Some("first".to_owned()));
    let client = CredentialedHttpClient::new(credential.clone())?;

    client
        .post_message(
            Arc::clone(&endpoint),
            notification()?,
            None,
            Some("configured".to_owned()),
            HashMap::new(),
        )
        .await?;
    credential.replace("renewed".to_owned());
    client
        .post_message(
            endpoint,
            notification()?,
            None,
            Some("configured".to_owned()),
            HashMap::new(),
        )
        .await?;

    let seen = seen.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(
        seen,
        [Some("Bearer first".to_owned()), Some("Bearer renewed".to_owned())]
    );
    Ok(())
}

#[tokio::test]
async fn an_empty_credential_sends_no_authorization_header() -> Result<(), Box<dyn std::error::Error>> {
    let (endpoint, seen) = header_recorder().await?;
    let client = CredentialedHttpClient::new(SessionCredential::new(None))?;

    client
        .post_message(
            endpoint,
            notification()?,
            None,
            Some("configured".to_owned()),
            HashMap::new(),
        )
        .await?;

    assert_eq!(*seen.lock().unwrap_or_else(PoisonError::into_inner), [None]);
    Ok(())
}
