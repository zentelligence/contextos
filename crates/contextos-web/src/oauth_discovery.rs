//! Automatic `OAuth2` endpoint detection for an HTTP `[[mcp_server]]` entry: a
//! tokenless probe against the server, then RFC 9728 Protected Resource Metadata and RFC
//! 8414/OIDC Authorization Server Metadata discovery, so `/server/settings/`'s "Detect" action can
//! pre-fill `authorization_endpoint`/`token_endpoint`/`scopes` instead of always requiring an
//! operator to find and type them by hand.
//!
//! Follows the MCP specification's own "Authorization Server Discovery" algorithm
//! (`modelcontextprotocol.io/specification/draft/basic/authorization/authorization-server-discovery`),
//! not merely RFC 9728/RFC 8414 in isolation: a client **MUST** support both the `WWW-Authenticate`
//! `resource_metadata` hint *and* well-known URI probing as a fallback when that hint is absent,
//! trying the resource's own path-scoped well-known URI before the root one. Missing that fallback
//! is not a hypothetical gap: a real hosted-consent MCP provider was observed, 2026-09-20, answering
//! `401` with only `Bearer realm="https://mcp.example-provider.invalid/mcp"`, no `resource_metadata`
//! parameter at all, which a header-hint-only implementation could never resolve.
//!
//! Deliberately additive, never authoritative: every outcome here is a best-effort assist an
//! operator reviews before it reaches `web.toml` at all (`routes::settings_oauth::detect`'s own
//! caller never persists anything from this module directly). Every failure path degrades to
//! [`DiscoveryOutcome::Failed`] rather than panicking or propagating a hard error: manual entry
//! must keep working exactly as before regardless of what this module finds.

use serde::Deserialize;
use thiserror::Error;

/// The three fields a successful detection pre-fills into the `oauth2` fieldset, plus a
/// `registration_endpoint` the caller may separately choose to offer a Dynamic Client Registration
/// (RFC 7591) attempt against; this module itself never calls it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiscoveredOauthFields {
    pub(crate) authorization_endpoint: String,
    pub(crate) token_endpoint: String,
    pub(crate) scopes: Vec<String>,
    pub(crate) registration_endpoint: Option<String>,
}

/// Every way [`discover`] can end without a usable result, each with an operator-facing message:
/// distinguishing "no auth needed" and "found it" from every one of these is exactly what lets
/// `/server/settings/` degrade to today's manual-entry behaviour rather than showing a misleading or stuck
/// result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum DiscoveryFailure {
    #[error("the server could not be reached to check whether it requires authentication")]
    ProbeUnreachable,
    #[error("the server requires authentication but its protected-resource metadata could not be found or read")]
    ResourceMetadataUnusable,
    #[error("the server's protected-resource metadata named no authorization server")]
    NoAuthorizationServers,
    #[error("the named authorization server's own metadata could not be found, read, or trusted")]
    AuthorizationServerMetadataUnusable,
}

/// [`discover`]'s result: whether `endpoint` needs authentication at all and, when it does,
/// whatever this module could resolve about how to obtain it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DiscoveryOutcome {
    /// The tokenless probe did not receive a `401`: `endpoint` appears reachable without
    /// authentication.
    NoAuthRequired,
    Discovered(DiscoveredOauthFields),
    Failed(DiscoveryFailure),
}

/// RFC 9728 §3's Protected Resource Metadata document: only the one field this module reads.
#[derive(Debug, Deserialize)]
struct ProtectedResourceMetadata {
    #[serde(default)]
    authorization_servers: Vec<String>,
    /// RFC 9728 §2's own `scopes_supported`: the scopes *this resource* recognises, distinct from
    /// (and, in practice, often the only populated one of) RFC 8414's authorization-server-level
    /// field of the same name. Observed directly, 2026-09-20, against a real hosted-consent MCP
    /// provider: its protected-resource metadata listed every real scope it published, while its
    /// authorization-server metadata document carried no `scopes_supported` field at all, so
    /// reading only the authorization-server side (as an RFC 8414-only implementation would) loses
    /// every real scope a genuine provider actually publishes.
    #[serde(default)]
    scopes_supported: Vec<String>,
}

/// RFC 8414 §2's (or OIDC Discovery's identically shaped) Authorization Server Metadata document:
/// only the fields this module reads. `issuer`/`authorization_endpoint`/`token_endpoint` are
/// mandatory by both specifications; a document missing any of them fails to deserialize, which
/// [`fetch_json`] already collapses into the same "unusable" outcome as an unreachable or
/// malformed document. `issuer` itself is read only to validate against, per RFC 8414 §3.3, never
/// surfaced to an operator.
#[derive(Debug, Deserialize)]
struct AuthorizationServerMetadata {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    #[serde(default)]
    scopes_supported: Vec<String>,
    #[serde(default)]
    registration_endpoint: Option<String>,
}

/// A minimal, spec-shaped MCP protocol revision 2026-07-28 `server/discover` request: enough for a
/// genuine MCP server's own auth middleware to run (and, for a public one, to respond normally)
/// without this module needing a full `rmcp` client session just to observe whether `endpoint`
/// challenges it. Sent with the [`PROBE_HEADERS`] a 2026-07-28 server requires to match it.
fn probe_body() -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "server/discover",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": {"name": "contextos-web-detect", "version": env!("CARGO_PKG_VERSION")},
                "io.modelcontextprotocol/clientCapabilities": {},
            },
        },
    })
}

/// The headers a 2026-07-28 server requires to mirror [`probe_body`]'s revision and method.
const PROBE_HEADERS: [(&str, &str); 2] = [
    ("MCP-Protocol-Version", "2026-07-28"),
    ("Mcp-Method", "server/discover"),
];

/// Detects whether `endpoint` requires authentication and, when it does, resolves the `OAuth2`
/// endpoints an operator would otherwise have to find and type by hand.
///
/// Never issues the probe with an `Authorization` header: a server behind an already-working
/// credential would answer normally regardless, but the whole point of this call is to observe
/// what an *unauthenticated* request gets back.
pub(crate) async fn discover(http: &reqwest::Client, endpoint: &str) -> DiscoveryOutcome {
    let probe = PROBE_HEADERS
        .iter()
        .fold(http.post(endpoint), |request, (name, value)| {
            request.header(*name, *value)
        })
        .header(reqwest::header::ACCEPT, "application/json, text/event-stream")
        .json(&probe_body())
        .send()
        .await;
    let response = match probe {
        Ok(response) => response,
        Err(_source) => return DiscoveryOutcome::Failed(DiscoveryFailure::ProbeUnreachable),
    };
    if response.status() != reqwest::StatusCode::UNAUTHORIZED {
        return DiscoveryOutcome::NoAuthRequired;
    }
    let challenge = response
        .headers()
        .get(reqwest::header::WWW_AUTHENTICATE)
        .and_then(|value| value.to_str().ok());

    let Some(resource_metadata) = resource_metadata_document(http, endpoint, challenge).await else {
        return DiscoveryOutcome::Failed(DiscoveryFailure::ResourceMetadataUnusable);
    };
    let Some(authorization_server) = resource_metadata.authorization_servers.first().cloned() else {
        return DiscoveryOutcome::Failed(DiscoveryFailure::NoAuthorizationServers);
    };

    let Some(server_metadata) = fetch_authorization_server_metadata(http, &authorization_server).await else {
        return DiscoveryOutcome::Failed(DiscoveryFailure::AuthorizationServerMetadataUnusable);
    };

    // Prefer the *resource's* own `scopes_supported` (RFC 9728 §2) when it names any: in practice
    // a provider is at least as likely to publish the scopes this specific resource actually needs
    // there as on the authorization-server-wide document, and a real hosted-consent provider was
    // observed doing exactly that (real scopes on the resource side, none at all on the
    // authorization-server side).
    let scopes = if resource_metadata.scopes_supported.is_empty() {
        server_metadata.scopes_supported
    } else {
        resource_metadata.scopes_supported
    };

    DiscoveryOutcome::Discovered(DiscoveredOauthFields {
        authorization_endpoint: server_metadata.authorization_endpoint,
        token_endpoint: server_metadata.token_endpoint,
        scopes,
        registration_endpoint: server_metadata.registration_endpoint,
    })
}

/// Resolves the Protected Resource Metadata document per the MCP specification's own priority
/// order: the `WWW-Authenticate` challenge's `resource_metadata` parameter first, when `challenge`
/// carries one; otherwise (or when that URL turns out unusable) well-known URI probing against
/// `endpoint`'s own origin, path-scoped candidate first, then the origin root, exactly the
/// fallback a header-hint-only implementation cannot express (this module's own top-level doc
/// comment has the real-world provider shape that makes this fallback necessary, not optional).
async fn resource_metadata_document(
    http: &reqwest::Client,
    endpoint: &str,
    challenge: Option<&str>,
) -> Option<ProtectedResourceMetadata> {
    if let Some(url) = challenge.and_then(resource_metadata_url)
        && let Ok(document) = fetch_json::<ProtectedResourceMetadata>(http, &url).await
    {
        return Some(document);
    }
    let (origin, path) = split_origin_and_path(endpoint)?;
    if !path.is_empty()
        && let Ok(document) = fetch_json::<ProtectedResourceMetadata>(
            http,
            &format!("{origin}/.well-known/oauth-protected-resource{path}"),
        )
        .await
    {
        return Some(document);
    }
    fetch_json::<ProtectedResourceMetadata>(http, &format!("{origin}/.well-known/oauth-protected-resource"))
        .await
        .ok()
}

/// The well-known candidate URLs for `authorization_server`'s own metadata, in the MCP
/// specification's own priority order, which differs by whether the issuer URL carries a path
/// component: a bare origin tries the two default suffixes at the root; an issuer with a path
/// (e.g. a multi-tenant provider) tries OAuth-metadata path-insertion, then OIDC path-insertion,
/// then OIDC path-appending, since a provider may implement any one of the three.
fn authorization_server_metadata_candidates(origin: &str, path: &str) -> Vec<String> {
    if path.is_empty() {
        vec![
            format!("{origin}/.well-known/oauth-authorization-server"),
            format!("{origin}/.well-known/openid-configuration"),
        ]
    } else {
        vec![
            format!("{origin}/.well-known/oauth-authorization-server{path}"),
            format!("{origin}/.well-known/openid-configuration{path}"),
            format!("{origin}{path}/.well-known/openid-configuration"),
        ]
    }
}

/// Fetches `authorization_server`'s own metadata by trying every well-known candidate
/// [`authorization_server_metadata_candidates`] names, in order, accepting the first one that both
/// parses and passes RFC 8414 §3.3's mandatory `issuer` validation: the document's own `issuer`
/// field must equal `authorization_server` (trailing slash insensitive), or it is rejected outright
/// rather than trusted, exactly the "a metadata document from an unrelated origin must never be
/// used just because it happened to parse" defence RFC 8414 itself calls out by example.
async fn fetch_authorization_server_metadata(
    http: &reqwest::Client,
    authorization_server: &str,
) -> Option<AuthorizationServerMetadata> {
    let (origin, path) = split_origin_and_path(authorization_server)?;
    let expected_issuer = authorization_server.trim_end_matches('/');
    for candidate in authorization_server_metadata_candidates(&origin, &path) {
        if let Ok(metadata) = fetch_json::<AuthorizationServerMetadata>(http, &candidate).await
            && metadata.issuer.trim_end_matches('/') == expected_issuer
        {
            return Some(metadata);
        }
    }
    None
}

/// Splits `url` into its origin (`scheme://host[:port]`, no trailing slash) and path (empty for
/// the root, otherwise the exact path component, no trailing slash): the shared decomposition both
/// [`resource_metadata_document`]'s and [`fetch_authorization_server_metadata`]'s own well-known
/// candidate construction need. `None` when `url` does not parse at all, or names no host (a
/// malformed `endpoint`/`authorization_server` value, only reachable via a hand-edited `web.toml`
/// or a resource metadata document naming a nonsensical authorization server).
fn split_origin_and_path(url: &str) -> Option<(String, String)> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let host = parsed.host_str()?;
    let origin = parsed.port().map_or_else(
        || format!("{}://{host}", parsed.scheme()),
        |port| format!("{}://{host}:{port}", parsed.scheme()),
    );
    let path = parsed.path().trim_end_matches('/');
    Some((origin, path.to_owned()))
}

/// `GET url`, collapsing any transport failure, non-2xx status, or body this crate's own type
/// cannot deserialize into a single `Err(())`: every metadata fetch in this module treats all
/// three identically (a best-effort assist has no use for which specific reason a document could
/// not be used), so the distinction is never propagated past this point.
async fn fetch_json<T: serde::de::DeserializeOwned>(http: &reqwest::Client, url: &str) -> Result<T, ()> {
    let response = http.get(url).send().await.map_err(|_source| ())?;
    if !response.status().is_success() {
        return Err(());
    }
    response.json::<T>().await.map_err(|_source| ())
}

/// Extracts RFC 9728 §5.1's `resource_metadata` parameter from a `WWW-Authenticate` challenge
/// header value, e.g. `Bearer realm="some-provider-mcp", error="invalid_token",
/// resource_metadata="https://mcp.example-provider.invalid/.well-known/oauth-protected-resource/mcp"`
/// (a real provider's own `401` was observed returning exactly this shape, 2026-09-20). `None`
/// when the challenge carries no such parameter at all: an ordinary legacy `WWW-Authenticate:
/// Bearer` challenge with no discovery hint (a different real provider's own shape, this module's
/// top-level doc comment), not this parser failing; the MCP specification requires
/// [`resource_metadata_document`]'s well-known fallback for exactly this case, not treating it as
/// terminal.
#[must_use]
fn resource_metadata_url(header_value: &str) -> Option<String> {
    auth_params(header_value)
        .into_iter()
        .find(|(key, _value)| key.eq_ignore_ascii_case("resource_metadata"))
        .map(|(_key, value)| value)
}

/// Parses a `WWW-Authenticate` challenge's `auth-param` list (RFC 7235 §2.1: `token "="
/// ( token / quoted-string )`, comma-separated, after a leading auth-scheme token this function
/// skips) into `(key, value)` pairs, unquoting a quoted value. Deliberately narrow: not a general
/// HTTP challenge grammar parser, only enough of one to find the handful of named parameters this
/// module reads. A challenge this cannot make sense of simply yields no pairs, degrading
/// [`resource_metadata_url`] to `None` rather than panicking or erroring on its own.
fn auth_params(header_value: &str) -> Vec<(String, String)> {
    let params_start = header_value
        .find(char::is_whitespace)
        .map_or(header_value.len(), |index| index + 1);
    split_top_level_commas(&header_value[params_start..])
        .into_iter()
        .filter_map(|segment| {
            let (key, value) = segment.trim().split_once('=')?;
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
                .unwrap_or(value);
            Some((key.trim().to_owned(), value.to_owned()))
        })
        .collect()
}

/// Splits `input` on commas that are not inside a double-quoted value: every parameter value this
/// module reads is a URL, never expected to contain a literal comma, but a quoted value is
/// respected regardless in case one theoretically did.
fn split_top_level_commas(input: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut in_quotes = false;
    let mut start = 0;
    for (index, ch) in input.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                segments.push(&input[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    segments.push(&input[start..]);
    segments
}

#[cfg(test)]
#[path = "oauth_discovery_test.rs"]
mod tests;
