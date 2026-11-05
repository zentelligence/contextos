use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use time::macros::datetime;

use super::*;
use crate::credential_store::CredentialStore;

type BoxError = Box<dyn std::error::Error>;

struct FakeClock(OffsetDateTime);

impl Clock for FakeClock {
    fn now(&self) -> OffsetDateTime {
        self.0
    }
}

fn store_in(dir: &std::path::Path) -> Arc<FsCredentialStore> {
    Arc::new(FsCredentialStore::new(dir.join("credentials")))
}

fn entry(token_endpoint: &str) -> OauthRefreshEntry {
    OauthRefreshEntry {
        server_name: "hosted-oauth".to_owned(),
        token_endpoint: token_endpoint.to_owned(),
        client_id: None,
        client_secret: None,
        credential_ref: "hosted-oauth-token".to_owned(),
    }
}

/// A minimal RFC 6749 §5.1 token endpoint over an ephemeral loopback port, scripted with one
/// status/body pair and counting how many times it was actually called: the same stub shape
/// `routes::settings_oauth_test::spawn_token_stub` already establishes, extended with a call
/// counter so a test can prove the refresh grant was never attempted at all, not merely that it
/// failed.
async fn spawn_counting_token_stub(
    status: StatusCode,
    body: serde_json::Value,
) -> Result<(String, Arc<std::sync::atomic::AtomicUsize>), BoxError> {
    #[derive(Clone)]
    struct StubState {
        status: StatusCode,
        body: serde_json::Value,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }
    async fn handle(State(state): State<StubState>) -> axum::response::Response {
        state.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        (state.status, axum::Json(state.body.clone())).into_response()
    }

    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let router = Router::new()
        .route("/token", axum::routing::post(handle))
        .with_state(StubState {
            status,
            body,
            calls: Arc::clone(&calls),
        });
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok((format!("http://{addr}/token"), calls))
}

#[tokio::test]
async fn a_token_comfortably_before_its_margin_is_used_as_is_with_no_refresh_call() -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let store = store_in(dir.path());
    let now = datetime!(2026-01-01 00:00:00 UTC);
    store.set(
        "hosted-oauth-token",
        &CredentialRecord::Oauth2Token {
            access_token: "still-fresh".to_owned(),
            refresh_token: Some("some-refresh-token".to_owned()),
            expires_at: (now + Duration::hours(1)).format(&Rfc3339)?,
            obtained_at: now.format(&Rfc3339)?,
        },
    )?;
    let clock: Arc<dyn Clock> = Arc::new(FakeClock(now));
    // An address nothing listens on: if the gate mistakenly attempted a refresh, this would fail
    // the whole call rather than silently succeeding.
    let unreachable_entry = entry(&format!("{}/token", crate::test_support::unreachable_loopback_url()?));

    let result = ensure_fresh(&store, &clock, &unreachable_entry).await;

    assert_eq!(
        result.as_deref(),
        Ok("still-fresh"),
        "the stored token is the one the session presents"
    );
    let record = store.get("hosted-oauth-token")?.ok_or("record disappeared")?;
    assert_eq!(
        record,
        CredentialRecord::Oauth2Token {
            access_token: "still-fresh".to_owned(),
            refresh_token: Some("some-refresh-token".to_owned()),
            expires_at: (now + Duration::hours(1)).format(&Rfc3339)?,
            obtained_at: now.format(&Rfc3339)?,
        },
        "an unneeded refresh must never rewrite the stored record"
    );
    Ok(())
}

#[tokio::test]
async fn a_token_within_the_margin_triggers_exactly_one_refresh_call_before_proceeding() -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let store = store_in(dir.path());
    let now = datetime!(2026-01-01 00:00:00 UTC);
    store.set(
        "hosted-oauth-token",
        &CredentialRecord::Oauth2Token {
            access_token: "about-to-expire".to_owned(),
            refresh_token: Some("original-refresh-token".to_owned()),
            expires_at: (now + Duration::seconds(30)).format(&Rfc3339)?,
            obtained_at: (now - Duration::hours(1)).format(&Rfc3339)?,
        },
    )?;
    let (token_endpoint, calls) = spawn_counting_token_stub(
        StatusCode::OK,
        serde_json::json!({
            "access_token": "refreshed-access-token",
            "refresh_token": "refreshed-refresh-token",
            "expires_in": 3600,
        }),
    )
    .await?;
    let clock: Arc<dyn Clock> = Arc::new(FakeClock(now));

    let result = ensure_fresh(&store, &clock, &entry(&token_endpoint)).await;

    assert_eq!(
        result.as_deref(),
        Ok("refreshed-access-token"),
        "the refreshed token is the one the session presents from now on"
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "exactly one refresh call must have been made"
    );
    Ok(())
}

#[tokio::test]
async fn a_successful_refresh_persists_the_new_pair_to_the_credential_store() -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let store = store_in(dir.path());
    let now = datetime!(2026-01-01 00:00:00 UTC);
    store.set(
        "hosted-oauth-token",
        &CredentialRecord::Oauth2Token {
            access_token: "about-to-expire".to_owned(),
            refresh_token: Some("original-refresh-token".to_owned()),
            expires_at: (now + Duration::seconds(1)).format(&Rfc3339)?,
            obtained_at: (now - Duration::hours(1)).format(&Rfc3339)?,
        },
    )?;
    let (token_endpoint, _calls) = spawn_counting_token_stub(
        StatusCode::OK,
        serde_json::json!({
            "access_token": "refreshed-access-token",
            "refresh_token": "refreshed-refresh-token",
            "expires_in": 3600,
        }),
    )
    .await?;
    let clock: Arc<dyn Clock> = Arc::new(FakeClock(now));

    ensure_fresh(&store, &clock, &entry(&token_endpoint)).await?;

    let record = store.get("hosted-oauth-token")?.ok_or("record disappeared")?;
    match record {
        CredentialRecord::Oauth2Token {
            access_token,
            refresh_token,
            expires_at,
            obtained_at,
        } => {
            assert_eq!(access_token, "refreshed-access-token");
            assert_eq!(refresh_token.as_deref(), Some("refreshed-refresh-token"));
            assert_eq!(expires_at, (now + Duration::hours(1)).format(&Rfc3339)?);
            assert_eq!(obtained_at, now.format(&Rfc3339)?);
        }
        other => return Err(format!("expected an Oauth2Token record, got {other:?}").into()),
    }
    Ok(())
}

#[tokio::test]
async fn a_refresh_endpoint_failure_surfaces_oauth_refresh_failed_and_never_attempts_the_proxied_call()
-> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let store = store_in(dir.path());
    let now = datetime!(2026-01-01 00:00:00 UTC);
    let original = CredentialRecord::Oauth2Token {
        access_token: "about-to-expire".to_owned(),
        refresh_token: Some("original-refresh-token".to_owned()),
        expires_at: (now + Duration::seconds(1)).format(&Rfc3339)?,
        obtained_at: (now - Duration::hours(1)).format(&Rfc3339)?,
    };
    store.set("hosted-oauth-token", &original)?;
    let (token_endpoint, _calls) =
        spawn_counting_token_stub(StatusCode::BAD_REQUEST, serde_json::json!({"error": "invalid_grant"})).await?;
    let clock: Arc<dyn Clock> = Arc::new(FakeClock(now));

    let result = ensure_fresh(&store, &clock, &entry(&token_endpoint)).await;

    assert_eq!(
        result,
        Err(OauthRefreshError {
            server_name: "hosted-oauth".to_owned()
        })
    );
    let record = store.get("hosted-oauth-token")?.ok_or("record disappeared")?;
    assert_eq!(
        record, original,
        "a failed refresh must never overwrite the stored record"
    );
    Ok(())
}

#[tokio::test]
async fn an_expired_token_with_no_refresh_token_fails_without_ever_calling_the_token_endpoint() -> Result<(), BoxError>
{
    let dir = tempfile::tempdir()?;
    let store = store_in(dir.path());
    let now = datetime!(2026-01-01 00:00:00 UTC);
    let original = CredentialRecord::Oauth2Token {
        access_token: "expired".to_owned(),
        refresh_token: None,
        expires_at: (now - Duration::hours(1)).format(&Rfc3339)?,
        obtained_at: (now - Duration::hours(2)).format(&Rfc3339)?,
    };
    store.set("hosted-oauth-token", &original)?;
    // A stub that would succeed if it were ever called: proves the gate never attempts a doomed
    // request when there is no refresh token to send.
    let (token_endpoint, calls) = spawn_counting_token_stub(
        StatusCode::OK,
        serde_json::json!({"access_token": "should-never-be-issued", "expires_in": 3600}),
    )
    .await?;
    let clock: Arc<dyn Clock> = Arc::new(FakeClock(now));

    let result = ensure_fresh(&store, &clock, &entry(&token_endpoint)).await;

    assert_eq!(
        result,
        Err(OauthRefreshError {
            server_name: "hosted-oauth".to_owned()
        })
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the token endpoint must never be called when no refresh token was ever issued"
    );
    let record = store.get("hosted-oauth-token")?.ok_or("record disappeared")?;
    assert_eq!(record, original);
    Ok(())
}

#[tokio::test]
async fn no_stored_record_at_all_fails_rather_than_attempting_a_doomed_call() -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let store = store_in(dir.path());
    let clock: Arc<dyn Clock> = Arc::new(FakeClock(datetime!(2026-01-01 00:00:00 UTC)));

    let unreachable = format!("{}/token", crate::test_support::unreachable_loopback_url()?);
    let result = ensure_fresh(&store, &clock, &entry(&unreachable)).await;

    assert_eq!(
        result,
        Err(OauthRefreshError {
            server_name: "hosted-oauth".to_owned()
        })
    );
    Ok(())
}

#[tokio::test]
async fn a_client_secret_variable_resolves_to_its_current_value() -> Result<(), BoxError> {
    // Cargo sets CARGO_PKG_NAME for every test binary it runs, and a test cannot set a variable without `unsafe`.
    let dir = tempfile::tempdir()?;

    let resolved = resolve_client_secret(
        &store_in(dir.path()),
        Some(&ClientSecretSource::Environment("CARGO_PKG_NAME".to_owned())),
    )
    .await;

    assert_eq!(resolved, Ok(Some(env!("CARGO_PKG_NAME").to_owned())));
    Ok(())
}

#[tokio::test]
async fn an_unset_client_secret_variable_does_not_resolve() -> Result<(), BoxError> {
    let variable = "CONTEXTOS_WEB_TEST_UNSET_CLIENT_SECRET";
    assert!(
        std::env::var_os(variable).is_none(),
        "test precondition: {variable} is unset"
    );
    let dir = tempfile::tempdir()?;

    let resolved = resolve_client_secret(
        &store_in(dir.path()),
        Some(&ClientSecretSource::Environment(variable.to_owned())),
    )
    .await;

    assert_eq!(resolved, Err(()));
    Ok(())
}

#[tokio::test]
async fn a_stored_client_secret_resolves_from_the_credential_store() -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let store = store_in(dir.path());
    store.set(
        "hosted-oauth-client-secret",
        &CredentialRecord::Oauth2ClientSecret {
            value: "stored-client-secret".to_owned(),
            updated_at: "2026-01-01T00:00:00Z".to_owned(),
        },
    )?;

    let resolved = resolve_client_secret(
        &store,
        Some(&ClientSecretSource::Stored("hosted-oauth-client-secret".to_owned())),
    )
    .await;

    assert_eq!(resolved, Ok(Some("stored-client-secret".to_owned())));
    Ok(())
}

#[tokio::test]
async fn a_refresh_whose_client_secret_variable_is_unset_fails_without_calling_the_token_endpoint()
-> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let store = store_in(dir.path());
    let now = datetime!(2026-01-01 00:00:00 UTC);
    store.set(
        "hosted-oauth-token",
        &CredentialRecord::Oauth2Token {
            access_token: "about-to-expire".to_owned(),
            refresh_token: Some("refresh-token".to_owned()),
            expires_at: (now + Duration::seconds(30)).format(&Rfc3339)?,
            obtained_at: (now - Duration::hours(1)).format(&Rfc3339)?,
        },
    )?;
    let (token_endpoint, calls) = spawn_counting_token_stub(StatusCode::OK, serde_json::json!({})).await?;
    let clock: Arc<dyn Clock> = Arc::new(FakeClock(now));
    let entry = OauthRefreshEntry {
        client_id: Some("abc".to_owned()),
        client_secret: Some(ClientSecretSource::Environment(
            "CONTEXTOS_WEB_TEST_UNSET_REFRESH_CLIENT_SECRET".to_owned(),
        )),
        ..entry(&token_endpoint)
    };

    let result = ensure_fresh(&store, &clock, &entry).await;

    assert!(result.is_err());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    Ok(())
}
