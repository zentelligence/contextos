use std::collections::HashMap;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::IntoResponse;

use super::*;

type BoxError = Box<dyn std::error::Error>;

// ---------------------------------------------------------------------
// `resource_metadata_url`: pure `WWW-Authenticate` challenge parsing.
// ---------------------------------------------------------------------

#[test]
fn a_challenge_with_a_resource_metadata_parameter_yields_its_url() {
    let header = "Bearer realm=\"some-provider-mcp\", error=\"invalid_token\", \
                  resource_metadata=\"https://mcp.example-provider.invalid/.well-known/oauth-protected-resource/mcp\"";

    let url = resource_metadata_url(header);

    assert_eq!(
        url.as_deref(),
        Some("https://mcp.example-provider.invalid/.well-known/oauth-protected-resource/mcp")
    );
}

#[test]
fn a_legacy_challenge_with_no_resource_metadata_parameter_yields_none() {
    // A real hosted-consent MCP provider's own shape, observed 2026-09-20: no discovery hint
    // at all. [`resource_metadata_document`]'s well-known fallback, not this parser, is what must
    // recover from this, so `None` here is the correct, expected outcome.
    let header = "Bearer realm=\"https://mcp.example-provider.invalid/mcp\"";

    assert_eq!(resource_metadata_url(header), None);
}

#[test]
fn a_case_insensitive_parameter_name_is_still_found() {
    let header = "Bearer Resource_Metadata=\"https://example.invalid/prm\"";

    assert_eq!(
        resource_metadata_url(header),
        Some("https://example.invalid/prm".to_owned())
    );
}

#[test]
fn a_single_scheme_token_with_no_parameters_at_all_yields_none() {
    assert_eq!(resource_metadata_url("Bearer"), None);
}

// ---------------------------------------------------------------------
// `discover`: end-to-end orchestration against real local loopback servers, never a mocked HTTP
// client: a genuine well-known-URI/RFC 9728/8414 round trip is exactly what this module's own
// correctness depends on, so a real (if minimal) server proves it, matching this crate's own
// `token_exchange_test`/`settings_oauth_test` precedent for testing outbound HTTP calls.
//
// One generic stub server backs every scenario below: `probe_status`/`probe_www_authenticate`
// script the `POST /mcp` response, and `routes` names exactly which well-known/metadata GET paths
// exist (and what they return); any path not named 404s, which is precisely how a "path-scoped
// well-known URI absent, root present" or "no discovery support at all" scenario is expressed.
// `spawn_stub` takes a fallible builder closure, not a ready-built `RouteStub`, because a route's
// own body usually needs to embed the stub server's own origin (as its `authorization_servers`
// entry or `issuer`), which is only known once the listener has actually bound, and building a
// `WWW-Authenticate` value is itself fallible.
// ---------------------------------------------------------------------

#[derive(Clone, Default)]
struct RouteStub {
    probe_status: StatusCode,
    probe_www_authenticate: Option<HeaderValue>,
    routes: HashMap<String, serde_json::Value>,
}

impl RouteStub {
    fn unauthorized() -> Self {
        Self {
            probe_status: StatusCode::UNAUTHORIZED,
            ..Self::default()
        }
    }

    fn with_challenge(mut self, value: &str) -> Result<Self, BoxError> {
        self.probe_www_authenticate = Some(value.parse()?);
        Ok(self)
    }

    fn with_route(mut self, path: impl Into<String>, body: serde_json::Value) -> Self {
        self.routes.insert(path.into(), body);
        self
    }
}

async fn probe(State(stub): State<RouteStub>) -> axum::response::Response {
    let mut headers = HeaderMap::new();
    if let Some(value) = stub.probe_www_authenticate.clone() {
        headers.insert(reqwest::header::WWW_AUTHENTICATE, value);
    }
    (stub.probe_status, headers, "").into_response()
}

async fn route(State(stub): State<RouteStub>, uri: Uri) -> axum::response::Response {
    match stub.routes.get(uri.path()) {
        Some(body) => (StatusCode::OK, axum::Json(body.clone())).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Binds an ephemeral loopback listener, then calls `build` with that server's own
/// `http://host:port` origin (so a route body can embed a self-reference, e.g. as its own
/// `authorization_servers` entry or `issuer`) to produce the [`RouteStub`] it then serves. Returns
/// the same origin, ready for a test to append `/mcp` (the probe path) onto.
async fn spawn_stub(build: impl FnOnce(&str) -> Result<RouteStub, BoxError>) -> Result<String, BoxError> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let origin = format!("http://{addr}");
    let stub = build(&origin)?;
    let router = Router::new()
        .route("/mcp", axum::routing::post(probe))
        .fallback(axum::routing::get(route))
        .with_state(stub);
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok(origin)
}

fn resource_metadata_body(authorization_servers: &[&str]) -> serde_json::Value {
    serde_json::json!({"authorization_servers": authorization_servers})
}

fn resource_metadata_body_with_scopes(authorization_servers: &[&str], scopes: &[&str]) -> serde_json::Value {
    serde_json::json!({"authorization_servers": authorization_servers, "scopes_supported": scopes})
}

fn authorization_server_metadata_body(issuer: &str) -> serde_json::Value {
    serde_json::json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/authorize"),
        "token_endpoint": format!("{issuer}/token"),
        "scopes_supported": ["mcp.tools.call"],
    })
}

/// The exact shape a real provider's own authorization-server metadata was observed returning,
/// 2026-09-20: no `scopes_supported` field at all (every real scope it publishes lives on the
/// resource metadata document instead).
fn authorization_server_metadata_body_without_scopes(issuer: &str) -> serde_json::Value {
    serde_json::json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/authorize"),
        "token_endpoint": format!("{issuer}/token"),
    })
}

#[tokio::test]
async fn a_non_401_response_means_no_auth_required() -> Result<(), BoxError> {
    let origin = spawn_stub(|_origin| {
        Ok(RouteStub {
            probe_status: StatusCode::OK,
            ..RouteStub::default()
        })
    })
    .await?;

    let outcome = discover(&format!("{origin}/mcp")).await;

    assert_eq!(outcome, DiscoveryOutcome::NoAuthRequired);
    Ok(())
}

#[tokio::test]
async fn an_unreachable_endpoint_fails_with_probe_unreachable() -> Result<(), BoxError> {
    let endpoint = format!("{}/mcp", crate::test_support::unreachable_loopback_url()?);

    let outcome = discover(&endpoint).await;

    assert_eq!(outcome, DiscoveryOutcome::Failed(DiscoveryFailure::ProbeUnreachable));
    Ok(())
}

#[tokio::test]
async fn a_401_with_no_header_hint_and_no_well_known_route_at_all_fails_with_resource_metadata_unusable()
-> Result<(), BoxError> {
    let origin = spawn_stub(|_origin| Ok(RouteStub::unauthorized())).await?;

    let outcome = discover(&format!("{origin}/mcp")).await;

    assert_eq!(
        outcome,
        DiscoveryOutcome::Failed(DiscoveryFailure::ResourceMetadataUnusable)
    );
    Ok(())
}

#[tokio::test]
async fn a_legacy_challenge_with_no_well_known_route_either_still_fails_with_resource_metadata_unusable()
-> Result<(), BoxError> {
    // A real provider's own challenge shape, but this time with no well-known fallback route
    // registered either: proves the legacy challenge alone does not somehow satisfy discovery by
    // itself.
    let origin = spawn_stub(|_origin| {
        RouteStub::unauthorized().with_challenge("Bearer realm=\"https://mcp.example-provider.invalid/mcp\"")
    })
    .await?;

    let outcome = discover(&format!("{origin}/mcp")).await;

    assert_eq!(
        outcome,
        DiscoveryOutcome::Failed(DiscoveryFailure::ResourceMetadataUnusable)
    );
    Ok(())
}

#[tokio::test]
async fn a_401_with_no_header_hint_falls_back_to_the_path_scoped_well_known_uri() -> Result<(), BoxError> {
    // The actual real-world-shaped fix: no `resource_metadata` parameter in the challenge at all, but
    // the MCP specification's own well-known fallback (path-scoped candidate first, since the
    // probe path here is `/mcp`) resolves it regardless.
    let origin = spawn_stub(|origin| {
        Ok(RouteStub::unauthorized()
            .with_route(
                "/.well-known/oauth-protected-resource/mcp",
                resource_metadata_body(&[origin]),
            )
            .with_route(
                "/.well-known/oauth-authorization-server",
                authorization_server_metadata_body(origin),
            ))
    })
    .await?;

    let outcome = discover(&format!("{origin}/mcp")).await;

    let DiscoveryOutcome::Discovered(fields) = outcome else {
        return Err(format!("expected a Discovered outcome, got {outcome:?}").into());
    };
    assert!(fields.authorization_endpoint.ends_with("/authorize"));
    assert!(fields.token_endpoint.ends_with("/token"));
    Ok(())
}

#[tokio::test]
async fn a_path_scoped_well_known_404_falls_back_to_the_root_well_known_uri() -> Result<(), BoxError> {
    // Only the root candidate is registered (`/.well-known/oauth-protected-resource`, no `/mcp`
    // suffix): proves the two-step priority order actually tries both, not just the first.
    let origin = spawn_stub(|origin| {
        Ok(RouteStub::unauthorized()
            .with_route(
                "/.well-known/oauth-protected-resource",
                resource_metadata_body(&[origin]),
            )
            .with_route(
                "/.well-known/oauth-authorization-server",
                authorization_server_metadata_body(origin),
            ))
    })
    .await?;

    let outcome = discover(&format!("{origin}/mcp")).await;

    assert!(matches!(outcome, DiscoveryOutcome::Discovered(_)), "{outcome:?}");
    Ok(())
}

#[tokio::test]
async fn a_header_hint_is_preferred_over_well_known_probing_and_resolves_every_field() -> Result<(), BoxError> {
    let origin = spawn_stub(|origin| {
        Ok(RouteStub::unauthorized()
            .with_challenge(&format!("Bearer resource_metadata=\"{origin}/prm\""))?
            .with_route("/prm", resource_metadata_body(&[origin]))
            .with_route(
                "/.well-known/oauth-authorization-server",
                authorization_server_metadata_body(origin),
            ))
    })
    .await?;

    let outcome = discover(&format!("{origin}/mcp")).await;

    let DiscoveryOutcome::Discovered(fields) = outcome else {
        return Err(format!("expected a Discovered outcome, got {outcome:?}").into());
    };
    assert_eq!(fields.authorization_endpoint, format!("{origin}/authorize"));
    assert_eq!(fields.token_endpoint, format!("{origin}/token"));
    assert_eq!(fields.scopes, vec!["mcp.tools.call".to_owned()]);
    assert_eq!(fields.registration_endpoint, None);
    Ok(())
}

#[tokio::test]
async fn a_resource_metadata_document_naming_no_authorization_server_fails_accordingly() -> Result<(), BoxError> {
    let origin = spawn_stub(|origin| {
        Ok(RouteStub::unauthorized()
            .with_challenge(&format!("Bearer resource_metadata=\"{origin}/prm\""))?
            .with_route("/prm", resource_metadata_body(&[])))
    })
    .await?;

    let outcome = discover(&format!("{origin}/mcp")).await;

    assert_eq!(
        outcome,
        DiscoveryOutcome::Failed(DiscoveryFailure::NoAuthorizationServers)
    );
    Ok(())
}

#[tokio::test]
async fn an_unresolvable_authorization_server_fails_with_metadata_unusable() -> Result<(), BoxError> {
    let unreachable = crate::test_support::unreachable_loopback_url()?;
    let origin = spawn_stub(|origin| {
        Ok(RouteStub::unauthorized()
            .with_challenge(&format!("Bearer resource_metadata=\"{origin}/prm\""))?
            .with_route("/prm", resource_metadata_body(&[unreachable.as_str()])))
    })
    .await?;

    let outcome = discover(&format!("{origin}/mcp")).await;

    assert_eq!(
        outcome,
        DiscoveryOutcome::Failed(DiscoveryFailure::AuthorizationServerMetadataUnusable)
    );
    Ok(())
}

#[tokio::test]
async fn an_authorization_server_metadata_document_with_a_mismatched_issuer_is_rejected() -> Result<(), BoxError> {
    // RFC 8414 §3.3's own defence, and the MCP specification's explicit attacker example: a
    // metadata document is only trusted when its own `issuer` field equals the URL it was fetched
    // from. Here the document is reachable and otherwise well-formed, but claims to be issued by a
    // different origin entirely, so it must be rejected, not silently accepted.
    let origin = spawn_stub(|origin| {
        Ok(RouteStub::unauthorized()
            .with_route(
                "/.well-known/oauth-protected-resource/mcp",
                resource_metadata_body(&[origin]),
            )
            .with_route(
                "/.well-known/oauth-authorization-server",
                authorization_server_metadata_body("https://attacker.invalid"),
            ))
    })
    .await?;

    let outcome = discover(&format!("{origin}/mcp")).await;

    assert_eq!(
        outcome,
        DiscoveryOutcome::Failed(DiscoveryFailure::AuthorizationServerMetadataUnusable)
    );
    Ok(())
}

#[tokio::test]
async fn the_resources_own_scopes_are_preferred_over_the_authorization_servers_when_both_are_present()
-> Result<(), BoxError> {
    // Reproduces a real hosted-consent provider's shape end-to-end, verified live 2026-09-20:
    // no `resource_metadata` header parameter, a path-scoped
    // well-known protected-resource document naming nine real scopes, and an authorization-server
    // metadata document with no `scopes_supported` field at all.
    let origin = spawn_stub(|origin| {
        Ok(RouteStub::unauthorized()
            .with_route(
                "/.well-known/oauth-protected-resource/mcp",
                resource_metadata_body_with_scopes(&[origin], &["openid", "profile", "accounting.settings"]),
            )
            .with_route(
                "/.well-known/oauth-authorization-server",
                authorization_server_metadata_body_without_scopes(origin),
            ))
    })
    .await?;

    let outcome = discover(&format!("{origin}/mcp")).await;

    let DiscoveryOutcome::Discovered(fields) = outcome else {
        return Err(format!("expected a Discovered outcome, got {outcome:?}").into());
    };
    assert_eq!(
        fields.scopes,
        vec![
            "openid".to_owned(),
            "profile".to_owned(),
            "accounting.settings".to_owned()
        ]
    );
    Ok(())
}

// A public provider cannot use this process to reach the operator's own services: from its point of view the stub's
// loopback address is one it should have no way to touch.
#[tokio::test]
async fn a_public_endpoint_naming_an_internal_address_gets_nothing_from_it() -> Result<(), BoxError> {
    let origin = spawn_stub(|origin| {
        RouteStub::unauthorized()
            .with_challenge(&format!(
                "Bearer resource_metadata=\"{origin}/.well-known/oauth-protected-resource\""
            ))
            .map(|stub| {
                stub.with_route(
                    "/.well-known/oauth-protected-resource",
                    resource_metadata_body(&[origin]),
                )
                .with_route(
                    "/.well-known/oauth-authorization-server",
                    authorization_server_metadata_body(origin),
                )
            })
    })
    .await?;
    let fetcher = OauthFetcher::public_only()?;

    let outcome = discover_with(&fetcher, &format!("{origin}/mcp")).await;

    assert_eq!(
        outcome,
        DiscoveryOutcome::Failed(DiscoveryFailure::ResourceMetadataUnusable)
    );
    Ok(())
}

// Metadata from a trusted place can still name an endpoint Connect should never use.
#[tokio::test]
async fn an_authorization_server_naming_an_out_of_bounds_endpoint_is_not_offered() -> Result<(), BoxError> {
    for (authorization, token) in [
        ("http://169.254.169.254/authorize", "{issuer}/token"),
        ("{issuer}/authorize", "http://169.254.169.254/token"),
        ("javascript:alert(1)", "{issuer}/token"),
        ("{issuer}/authorize", "file:///etc/passwd"),
    ] {
        let origin = spawn_stub(|origin| {
            Ok(RouteStub::unauthorized()
                .with_route(
                    "/.well-known/oauth-protected-resource/mcp",
                    resource_metadata_body(&[origin]),
                )
                .with_route(
                    "/.well-known/oauth-authorization-server",
                    serde_json::json!({
                        "issuer": origin,
                        "authorization_endpoint": authorization.replace("{issuer}", origin),
                        "token_endpoint": token.replace("{issuer}", origin),
                    }),
                ))
        })
        .await?;

        let outcome = discover(&format!("{origin}/mcp")).await;

        assert_eq!(
            outcome,
            DiscoveryOutcome::Failed(DiscoveryFailure::AuthorizationServerMetadataUnusable),
            "{authorization} {token}"
        );
    }
    Ok(())
}

/// The probe is a request a 2026-07-28 server accepts as well formed, so a
/// public server answers it normally rather than refusing it for reasons that
/// have nothing to do with authentication.
#[tokio::test]
async fn the_probe_is_a_2026_07_28_server_discover_request() -> Result<(), BoxError> {
    type Seen = std::sync::Arc<std::sync::Mutex<Option<(HeaderMap, serde_json::Value)>>>;
    async fn record(
        State(seen): State<Seen>,
        headers: HeaderMap,
        axum::Json(body): axum::Json<serde_json::Value>,
    ) -> StatusCode {
        if let Ok(mut slot) = seen.lock() {
            *slot = Some((headers, body));
        }
        StatusCode::OK
    }
    let seen: Seen = std::sync::Arc::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let router = Router::new()
        .route("/mcp", axum::routing::post(record))
        .with_state(seen.clone());
    tokio::spawn(async move { axum::serve(listener, router).await });

    let outcome = discover(&format!("http://{addr}/mcp")).await;
    assert_eq!(outcome, DiscoveryOutcome::NoAuthRequired);

    let (headers, body) = seen
        .lock()
        .map_err(|_| "probe recorder poisoned")?
        .clone()
        .ok_or("the stub never received the probe")?;
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    assert_eq!(header("MCP-Protocol-Version").as_deref(), Some("2026-07-28"));
    assert_eq!(header("Mcp-Method").as_deref(), Some("server/discover"));
    assert_eq!(body["method"], serde_json::json!("server/discover"));
    assert_eq!(
        body["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
        serde_json::json!("2026-07-28")
    );
    assert!(body["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"].is_object());
    Ok(())
}

/// A real contextos server accepts the probe as a well-formed 2026-07-28
/// request and answers it, so a public server is never mistaken for one
/// refusing the probe for reasons other than authentication.
#[tokio::test]
async fn a_contextos_server_answers_the_probe_as_a_well_formed_request() -> Result<(), BoxError> {
    let dir = tempfile::tempdir()?;
    let vault = dir.path().join("vault");
    std::fs::create_dir_all(&vault)?;
    let mut config = contextos_mcp::Config::try_from(vec![vault])?;
    for entry in &mut config.vaults {
        entry.state_directory = Some(std::path::PathBuf::from(".contextos"));
    }
    let server = contextos_mcp::ContextOsServer::try_from(config)?;
    let http = contextos_mcp::HttpConfig {
        bind: "127.0.0.1:0".to_owned(),
        token: String::new(),
        token_env: String::new(),
        max_body_kb: 2048,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let router = contextos_mcp::build_router(server, &http)?;
    tokio::spawn(async move { axum::serve(listener, router).await });

    let response = PROBE_HEADERS
        .iter()
        .fold(
            reqwest::Client::new().post(format!("http://{addr}{}", contextos_mcp::HTTP_MOUNT_PATH)),
            |request, (name, value)| request.header(*name, *value),
        )
        .header(reqwest::header::ACCEPT, "application/json, text/event-stream")
        .json(&probe_body())
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let text = response.text().await?;
    let payload = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .find(|data| !data.is_empty())
        .unwrap_or(&text);
    let message: serde_json::Value = serde_json::from_str(payload)?;
    assert!(
        message["result"]["supportedVersions"]
            .as_array()
            .is_some_and(|versions| versions.contains(&serde_json::json!("2026-07-28"))),
        "{message}"
    );
    Ok(())
}
