use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use reqwest::Url;
use tower::ServiceExt as _;

use super::*;
use crate::app::build_router;
use crate::credential_store::{CredentialRecord, CredentialStore, FsCredentialStore};
use crate::mcp_client::McpClientSet;

type BoxError = Box<dyn std::error::Error>;

/// A `web.toml` with one hosted-consent-pattern (no `client_id`) `OAuth2` entry, `token_endpoint`
/// pointed at `token_endpoint` (typically a [`spawn_token_stub`] URL): the shape most tests in
/// this file build on.
fn oauth_web_toml(token_endpoint: &str) -> String {
    format!(
        r#"
[server]
bind = "127.0.0.1:7373"

[[mcp_server]]
name = "contextos"
transport = "stdio"
command = "contextos-mcp"
args = ["--config", "config.toml"]

[[mcp_server]]
name = "hosted-oauth"
transport = "http"
endpoint = "https://mcp.example-provider.invalid/mcp"

[mcp_server.auth]
mode = "oauth2"
authorization_endpoint = "https://example-provider.invalid/oauth2/authorize"
token_endpoint = "{token_endpoint}"
scopes = ["mcp.tools.read", "mcp.tools.call"]
credential_ref = "hosted-oauth-token"

[[mcp_server]]
name = "bearer-server"
transport = "http"
endpoint = "https://services.example-provider.invalid/mcp/"

[mcp_server.auth]
mode = "bearer"
credential_ref = "bearer-server-token"
"#
    )
}

async fn router_with_web_toml(contents: &str) -> Result<(tempfile::TempDir, Router), BoxError> {
    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("web.toml"), contents)?;
    let clients = Arc::new(McpClientSet::connect(&[]).await?);
    let router = build_router(clients, Some(dir.path()), &dir.path().join("web.toml"));
    Ok((dir, router))
}

async fn send(router: &Router, request: Request<Body>) -> Result<axum::response::Response, BoxError> {
    Ok(router.clone().oneshot(request).await?)
}

fn connect_request(server_name: &str, hx: bool) -> Result<Request<Body>, BoxError> {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(format!("/server/settings/oauth/{server_name}/connect"));
    if hx {
        builder = builder.header("hx-request", "true");
    }
    Ok(builder.body(Body::empty())?)
}

fn location_header(response: &axum::response::Response) -> Option<String> {
    response
        .headers()
        .get(axum::http::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

fn hx_redirect_header(response: &axum::response::Response) -> Option<String> {
    response
        .headers()
        .get("HX-Redirect")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
}

// ---------------------------------------------------------------------
// connect
// ---------------------------------------------------------------------

#[tokio::test]
async fn connect_on_an_unknown_server_is_a_404() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(&oauth_web_toml(&format!(
        "{}/token",
        crate::test_support::unreachable_loopback_url()?
    )))
    .await?;

    let response = send(&router, connect_request("does-not-exist", false)?).await?;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn connect_on_a_non_oauth2_entry_is_a_404() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(&oauth_web_toml(&format!(
        "{}/token",
        crate::test_support::unreachable_loopback_url()?
    )))
    .await?;

    let response = send(&router, connect_request("bearer-server", false)?).await?;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn connect_on_a_stdio_entry_is_a_404() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(&oauth_web_toml(&format!(
        "{}/token",
        crate::test_support::unreachable_loopback_url()?
    )))
    .await?;

    let response = send(&router, connect_request("contextos", false)?).await?;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    Ok(())
}

/// The sharpest part of this route: an `HX-Request` must get `HX-Redirect`, never a bare `303`,
/// because a fragment-swap response cannot send a real browser to an external consent page.
#[tokio::test]
async fn connect_with_hx_request_answers_with_hx_redirect_not_a_303() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(&oauth_web_toml(&format!(
        "{}/token",
        crate::test_support::unreachable_loopback_url()?
    )))
    .await?;

    let response = send(&router, connect_request("hosted-oauth", true)?).await?;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        location_header(&response).is_none(),
        "must not also send a Location header"
    );
    let target = hx_redirect_header(&response).ok_or("missing HX-Redirect header")?;
    assert!(
        target.starts_with("https://example-provider.invalid/oauth2/authorize?"),
        "{target}"
    );
    Ok(())
}

/// The counterpart: a plain browser request (no `HX-Request`) must get a genuine `303` with
/// `Location` set, never `HX-Redirect` (which only htmx itself understands).
#[tokio::test]
async fn connect_without_hx_request_answers_with_a_303_and_location() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(&oauth_web_toml(&format!(
        "{}/token",
        crate::test_support::unreachable_loopback_url()?
    )))
    .await?;

    let response = send(&router, connect_request("hosted-oauth", false)?).await?;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert!(
        hx_redirect_header(&response).is_none(),
        "must not also send HX-Redirect"
    );
    let target = location_header(&response).ok_or("missing Location header")?;
    assert!(
        target.starts_with("https://example-provider.invalid/oauth2/authorize?"),
        "{target}"
    );
    Ok(())
}

#[tokio::test]
async fn connect_builds_an_authorization_url_with_every_expected_parameter() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(&oauth_web_toml(&format!(
        "{}/token",
        crate::test_support::unreachable_loopback_url()?
    )))
    .await?;

    let response = send(&router, connect_request("hosted-oauth", false)?).await?;
    let target = location_header(&response).ok_or("missing Location header")?;
    let url = Url::parse(&target)?;
    let pairs: std::collections::HashMap<String, String> = url.query_pairs().into_owned().collect();

    assert_eq!(pairs.get("response_type"), Some(&"code".to_owned()));
    assert_eq!(
        pairs.get("redirect_uri"),
        Some(&"http://127.0.0.1:7373/settings/oauth/hosted-oauth/callback".to_owned())
    );
    assert_eq!(pairs.get("scope"), Some(&"mcp.tools.read mcp.tools.call".to_owned()));
    assert_eq!(pairs.get("code_challenge_method"), Some(&"S256".to_owned()));
    assert!(pairs.contains_key("state"), "{pairs:?}");
    assert!(pairs.contains_key("code_challenge"), "{pairs:?}");
    // A hosted-consent entry configures no `client_id` at all: it must not appear.
    assert!(!pairs.contains_key("client_id"), "{pairs:?}");
    Ok(())
}

#[tokio::test]
async fn connect_includes_client_id_only_when_the_entry_configures_one() -> Result<(), BoxError> {
    let web_toml = r#"
[server]
bind = "127.0.0.1:7373"

[[mcp_server]]
name = "contextos"
transport = "stdio"
command = "contextos-mcp"
args = ["--config", "config.toml"]

[[mcp_server]]
name = "preregistered"
transport = "http"
endpoint = "https://mcp.example-provider.invalid/mcp"

[mcp_server.auth]
mode = "oauth2"
client_id = "abc123.apps.example-provider.invalid"
authorization_endpoint = "https://example-provider.invalid/oauth2/authorize"
token_endpoint = "UNREACHABLE_TOKEN_ENDPOINT"
scopes = ["mcp.tools.read"]
credential_ref = "preregistered-token"
"#
    .replace(
        "UNREACHABLE_TOKEN_ENDPOINT",
        &format!("{}/token", crate::test_support::unreachable_loopback_url()?),
    );
    let (_dir, router) = router_with_web_toml(&web_toml).await?;

    let response = send(&router, connect_request("preregistered", false)?).await?;
    let target = location_header(&response).ok_or("missing Location header")?;
    let url = Url::parse(&target)?;
    let pairs: std::collections::HashMap<String, String> = url.query_pairs().into_owned().collect();

    assert_eq!(
        pairs.get("client_id"),
        Some(&"abc123.apps.example-provider.invalid".to_owned())
    );
    Ok(())
}

// ---------------------------------------------------------------------
// callback
// ---------------------------------------------------------------------

fn callback_request(server_name: &str, query: &str) -> Result<Request<Body>, BoxError> {
    Ok(Request::builder()
        .method(Method::GET)
        .uri(format!("/server/settings/oauth/{server_name}/callback?{query}"))
        .body(Body::empty())?)
}

/// Extracts the `state` query parameter `connect`'s own `Location` header carries, for a
/// following `callback` request in the same test.
fn state_from_authorization_url(location: &str) -> Result<String, BoxError> {
    let url = Url::parse(location)?;
    url.query_pairs()
        .find_map(|(key, value)| (key == "state").then(|| value.into_owned()))
        .ok_or_else(|| "authorization URL carried no state parameter".into())
}

fn oauth_error_and_server(location: &str) -> (Option<String>, Option<String>) {
    let Ok(url) = Url::parse(&format!("http://localhost{location}")) else {
        return (None, None);
    };
    let pairs: std::collections::HashMap<String, String> = url.query_pairs().into_owned().collect();
    (pairs.get("oauth_error").cloned(), pairs.get("server").cloned())
}

#[tokio::test]
async fn callback_with_a_provider_error_redirects_with_oauth_error() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(&oauth_web_toml(&format!(
        "{}/token",
        crate::test_support::unreachable_loopback_url()?
    )))
    .await?;

    let response = send(
        &router,
        callback_request("hosted-oauth", "error=access_denied&state=whatever")?,
    )
    .await?;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let target = location_header(&response).ok_or("missing Location header")?;
    let (oauth_error, server) = oauth_error_and_server(&target);
    assert_eq!(oauth_error.as_deref(), Some("access_denied"));
    assert_eq!(server.as_deref(), Some("hosted-oauth"));
    Ok(())
}

#[tokio::test]
async fn callback_with_missing_code_and_no_state_redirects_without_touching_pending_state() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(&oauth_web_toml(&format!(
        "{}/token",
        crate::test_support::unreachable_loopback_url()?
    )))
    .await?;

    let response = send(&router, callback_request("hosted-oauth", "")?).await?;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let (oauth_error, _server) = oauth_error_and_server(&location_header(&response).ok_or("missing Location")?);
    assert_eq!(oauth_error.as_deref(), Some("missing_parameters"));
    Ok(())
}

#[tokio::test]
async fn callback_with_an_unknown_state_redirects_with_invalid_state() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(&oauth_web_toml(&format!(
        "{}/token",
        crate::test_support::unreachable_loopback_url()?
    )))
    .await?;

    let response = send(
        &router,
        callback_request("hosted-oauth", "code=some-code&state=never-issued")?,
    )
    .await?;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let (oauth_error, _server) = oauth_error_and_server(&location_header(&response).ok_or("missing Location")?);
    assert_eq!(oauth_error.as_deref(), Some("invalid_state"));
    Ok(())
}

#[tokio::test]
async fn callback_with_an_already_used_state_is_rejected_on_the_second_attempt() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(&oauth_web_toml(&format!(
        "{}/token",
        crate::test_support::unreachable_loopback_url()?
    )))
    .await?;
    let connect_response = send(&router, connect_request("hosted-oauth", false)?).await?;
    let authorization_url = location_header(&connect_response).ok_or("missing Location header")?;
    let state_value = state_from_authorization_url(&authorization_url)?;

    // First use fails at the token exchange (nothing is listening on the token endpoint), but it
    // still consumes the state.
    let _ = send(
        &router,
        callback_request("hosted-oauth", &format!("code=some-code&state={state_value}"))?,
    )
    .await?;

    let replay = send(
        &router,
        callback_request("hosted-oauth", &format!("code=some-code&state={state_value}"))?,
    )
    .await?;
    assert_eq!(replay.status(), StatusCode::SEE_OTHER);
    let (oauth_error, _server) = oauth_error_and_server(&location_header(&replay).ok_or("missing Location")?);
    assert_eq!(oauth_error.as_deref(), Some("invalid_state"));
    Ok(())
}

/// A `state` value issued for one server must never be honoured on a different server's own
/// callback path: a real cross-server-confusion attack surface if this check were skipped.
#[tokio::test]
async fn callback_rejects_a_state_issued_for_a_different_server() -> Result<(), BoxError> {
    let unreachable = format!("{}/token", crate::test_support::unreachable_loopback_url()?);
    let web_toml = format!(
        "{}\n[[mcp_server]]\nname = \"other-oauth\"\ntransport = \"http\"\nendpoint = \"https://mcp.other.invalid/mcp\"\n\n\
         [mcp_server.auth]\nmode = \"oauth2\"\nauthorization_endpoint = \"https://other.invalid/authorize\"\n\
         token_endpoint = \"{unreachable}\"\nscopes = [\"x\"]\ncredential_ref = \"other-oauth-token\"\n",
        oauth_web_toml(&unreachable)
    );
    let (_dir, router) = router_with_web_toml(&web_toml).await?;

    let connect_response = send(&router, connect_request("hosted-oauth", false)?).await?;
    let authorization_url = location_header(&connect_response).ok_or("missing Location header")?;
    let state_value = state_from_authorization_url(&authorization_url)?;

    // The same `state` value, but presented at a *different* server's own callback path.
    let response = send(
        &router,
        callback_request("other-oauth", &format!("code=some-code&state={state_value}"))?,
    )
    .await?;

    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let (oauth_error, _server) = oauth_error_and_server(&location_header(&response).ok_or("missing Location")?);
    assert_eq!(oauth_error.as_deref(), Some("invalid_state"));
    Ok(())
}

/// A minimal RFC 6749 §5.1 token endpoint over an ephemeral loopback port, scripted with one
/// status/body pair, alive for the duration of one test.
async fn spawn_token_stub(
    status: StatusCode,
    body: serde_json::Value,
) -> Result<(String, tokio::task::JoinHandle<()>), BoxError> {
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
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok((format!("http://{addr}/token"), handle))
}

#[tokio::test]
async fn a_full_connect_then_callback_round_trip_stores_the_token_and_redirects() -> Result<(), BoxError> {
    let (token_endpoint, _stub) = spawn_token_stub(
        StatusCode::OK,
        serde_json::json!({
            "access_token": "issued-access-token",
            "refresh_token": "issued-refresh-token",
            "expires_in": 3600,
        }),
    )
    .await?;
    let (dir, router) = router_with_web_toml(&oauth_web_toml(&token_endpoint)).await?;

    let connect_response = send(&router, connect_request("hosted-oauth", false)?).await?;
    let authorization_url = location_header(&connect_response).ok_or("missing Location header")?;
    let state_value = state_from_authorization_url(&authorization_url)?;

    let callback_response = send(
        &router,
        callback_request(
            "hosted-oauth",
            &format!("code=provider-issued-code&state={state_value}"),
        )?,
    )
    .await?;

    assert_eq!(callback_response.status(), StatusCode::SEE_OTHER);
    let target = location_header(&callback_response).ok_or("missing Location header")?;
    assert!(
        target.starts_with("/server/settings/?oauth_connected=hosted-oauth"),
        "{target}"
    );

    let store = FsCredentialStore::new(dir.path().join("credentials"));
    let record = store
        .get("hosted-oauth-token")?
        .ok_or("no credential record was written")?;
    match record {
        CredentialRecord::Oauth2Token {
            access_token,
            refresh_token,
            expires_at,
            obtained_at,
        } => {
            assert_eq!(access_token, "issued-access-token");
            assert_eq!(refresh_token.as_deref(), Some("issued-refresh-token"));
            assert!(!expires_at.is_empty(), "`expires_at` should not be empty");
            assert!(!obtained_at.is_empty(), "`obtained_at` should not be empty");
        }
        other => return Err(format!("expected an Oauth2Token record, got {other:?}").into()),
    }
    Ok(())
}

#[tokio::test]
async fn callback_redirects_with_token_exchange_failed_when_the_provider_rejects_the_code() -> Result<(), BoxError> {
    let (token_endpoint, _stub) =
        spawn_token_stub(StatusCode::BAD_REQUEST, serde_json::json!({"error": "invalid_grant"})).await?;
    let (_dir, router) = router_with_web_toml(&oauth_web_toml(&token_endpoint)).await?;

    let connect_response = send(&router, connect_request("hosted-oauth", false)?).await?;
    let authorization_url = location_header(&connect_response).ok_or("missing Location header")?;
    let state_value = state_from_authorization_url(&authorization_url)?;

    let callback_response = send(
        &router,
        callback_request("hosted-oauth", &format!("code=bad-code&state={state_value}"))?,
    )
    .await?;

    assert_eq!(callback_response.status(), StatusCode::SEE_OTHER);
    let (oauth_error, _server) =
        oauth_error_and_server(&location_header(&callback_response).ok_or("missing Location")?);
    assert_eq!(oauth_error.as_deref(), Some("token_exchange_failed"));
    Ok(())
}

// ---------------------------------------------------------------------
// disconnect
// ---------------------------------------------------------------------

fn disconnect_request(server_name: &str) -> Result<Request<Body>, BoxError> {
    Ok(Request::builder()
        .method(Method::DELETE)
        .uri(format!("/server/settings/oauth/{server_name}/connect"))
        .body(Body::empty())?)
}

#[tokio::test]
async fn disconnect_on_an_unknown_server_is_a_404() -> Result<(), BoxError> {
    let (_dir, router) = router_with_web_toml(&oauth_web_toml(&format!(
        "{}/token",
        crate::test_support::unreachable_loopback_url()?
    )))
    .await?;

    let response = send(&router, disconnect_request("does-not-exist")?).await?;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn disconnect_removes_a_previously_connected_token_and_is_idempotent() -> Result<(), BoxError> {
    let (dir, router) = router_with_web_toml(&oauth_web_toml(&format!(
        "{}/token",
        crate::test_support::unreachable_loopback_url()?
    )))
    .await?;
    let store = FsCredentialStore::new(dir.path().join("credentials"));
    store.set(
        "hosted-oauth-token",
        &CredentialRecord::Oauth2Token {
            access_token: "pre-existing".to_owned(),
            refresh_token: None,
            expires_at: "2026-01-01T00:00:00Z".to_owned(),
            obtained_at: "2026-01-01T00:00:00Z".to_owned(),
        },
    )?;

    let first = send(&router, disconnect_request("hosted-oauth")?).await?;
    assert_eq!(first.status(), StatusCode::OK);
    assert!(store.get("hosted-oauth-token")?.is_none());

    // Idempotent: disconnecting again (nothing left to remove) still succeeds.
    let second = send(&router, disconnect_request("hosted-oauth")?).await?;
    assert_eq!(second.status(), StatusCode::OK);

    let bytes = to_bytes(second.into_body(), usize::MAX).await?;
    let body = String::from_utf8_lossy(&bytes);
    assert!(body.contains("hosted-oauth"), "{body}");
    Ok(())
}

#[tokio::test]
async fn disconnect_never_touches_the_entrys_own_configuration() -> Result<(), BoxError> {
    let (dir, router) = router_with_web_toml(&oauth_web_toml(&format!(
        "{}/token",
        crate::test_support::unreachable_loopback_url()?
    )))
    .await?;
    let before = std::fs::read_to_string(dir.path().join("web.toml"))?;

    let _ = send(&router, disconnect_request("hosted-oauth")?).await?;

    assert_eq!(std::fs::read_to_string(dir.path().join("web.toml"))?, before);
    Ok(())
}
