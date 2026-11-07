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

async fn serve(router: Router) -> Result<String, BoxError> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok(origin)
}

// A 307 repeats the POST, so following one would hand the client secret to wherever the provider pointed.
#[tokio::test]
async fn a_redirect_from_the_token_endpoint_is_not_followed() -> Result<(), BoxError> {
    static SECRET_SEEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let elsewhere = serve(Router::new().fallback(|body: String| async move {
        if body.contains("client-secret-value") {
            SECRET_SEEN.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        axum::Json(serde_json::json!({"access_token": "from-elsewhere"}))
    }))
    .await?;
    let origin = serve(Router::new().fallback(move || {
        let target = format!("{elsewhere}/stolen");
        async move { (StatusCode::TEMPORARY_REDIRECT, [(axum::http::header::LOCATION, target)]).into_response() }
    }))
    .await?;

    let response = request_token(
        &format!("{origin}/token"),
        &[("client_secret", "client-secret-value")],
        "some-server",
    )
    .await;

    assert!(response.is_none());
    assert!(!SECRET_SEEN.load(std::sync::atomic::Ordering::SeqCst));
    Ok(())
}

#[tokio::test]
async fn a_token_response_past_the_size_limit_is_refused() -> Result<(), BoxError> {
    let padded = format!("{{\"access_token\":\"t\"}}{}", " ".repeat(2 * 1024 * 1024));
    let origin = serve(Router::new().fallback(move || {
        let padded = padded.clone();
        async move { padded }
    }))
    .await?;

    assert!(
        request_token(&format!("{origin}/token"), &[], "some-server")
            .await
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn a_token_endpoint_that_never_answers_is_given_up_on() -> Result<(), BoxError> {
    let origin = serve(Router::new().fallback(std::future::pending::<StatusCode>)).await?;
    let limit = std::time::Duration::from_millis(50);

    assert!(
        request_token_within(limit, &format!("{origin}/token"), &[], "some-server")
            .await
            .is_none()
    );
    Ok(())
}
