use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;

use super::*;

type BoxError = Box<dyn std::error::Error>;

/// A minimal token endpoint over an ephemeral loopback port, scripted with one status/body pair,
/// alive for the duration of one test. Mirrors
/// `routes::settings_oauth_test::spawn_token_stub`'s identical shape: both test files stand up the
/// same kind of stub against the same shared [`request_token`] implementation.
async fn spawn_token_stub(status: StatusCode, body: serde_json::Value) -> Result<String, BoxError> {
    #[derive(Clone)]
    struct StubState {
        status: StatusCode,
        body: serde_json::Value,
    }
    async fn handle(State(state): State<StubState>) -> axum::response::Response {
        (state.status, axum::Json(state.body.clone())).into_response()
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let router = Router::new()
        .route("/token", axum::routing::post(handle))
        .with_state(StubState { status, body });
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok(format!("http://{addr}/token"))
}

#[tokio::test]
async fn a_successful_response_parses_every_documented_field() -> Result<(), BoxError> {
    let endpoint = spawn_token_stub(
        StatusCode::OK,
        serde_json::json!({
            "access_token": "issued-access-token",
            "refresh_token": "issued-refresh-token",
            "expires_in": 3600,
        }),
    )
    .await?;

    let response = request_token(&endpoint, &[("grant_type", "refresh_token")], "some-server")
        .await
        .ok_or("expected a parsed token response")?;

    assert_eq!(response.access_token, "issued-access-token");
    assert_eq!(response.refresh_token.as_deref(), Some("issued-refresh-token"));
    assert_eq!(response.expires_in, Some(3600));
    Ok(())
}

#[tokio::test]
async fn a_response_with_no_optional_fields_still_parses() -> Result<(), BoxError> {
    let endpoint = spawn_token_stub(StatusCode::OK, serde_json::json!({"access_token": "only-token"})).await?;

    let response = request_token(&endpoint, &[("grant_type", "refresh_token")], "some-server")
        .await
        .ok_or("expected a parsed token response")?;

    assert_eq!(response.access_token, "only-token");
    assert_eq!(response.refresh_token, None);
    assert_eq!(response.expires_in, None);
    Ok(())
}

#[tokio::test]
async fn a_non_success_status_is_none() -> Result<(), BoxError> {
    let endpoint = spawn_token_stub(StatusCode::BAD_REQUEST, serde_json::json!({"error": "invalid_grant"})).await?;

    let response = request_token(&endpoint, &[("grant_type", "refresh_token")], "some-server").await;

    assert!(response.is_none());
    Ok(())
}

#[tokio::test]
async fn an_unreachable_endpoint_is_none() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = format!("{}/token", crate::test_support::unreachable_loopback_url()?);
    let response = request_token(&endpoint, &[("grant_type", "refresh_token")], "some-server").await;

    assert!(response.is_none());
    Ok(())
}
