//! `/settings/oauth/{server_name}/connect`, `.../callback`, and `.../disconnect`
//!: the PKCE authorisation-code flow for an `oauth2`-mode
//! `[[mcp_server]]` entry, kept structurally separate from `routes::settings`'s own CRUD verbs
//! because a redirect-driven flow is not a form submission.
//!
//! `connect` never calls the provider itself: it only builds the `authorization_endpoint` URL
//! and sends the operator's own browser there. `callback` is the one handler in this module that
//! talks to the provider directly, exchanging `code` for a token at `token_endpoint`.
//! `disconnect` never touches the provider or the entry's own configuration at all; it clears
//! only the stored token.

use std::sync::Arc;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

use crate::config::{ClientSecretSource, McpServerAuth, McpServerConfig, WebConfig};
use crate::credential_store::{CredentialRecord, CredentialStore};
use crate::inbox_outbox::encode_query_value;
use crate::logging::{Clock, SystemClock};
use crate::oauth::{self, PendingOauthConnection};
use crate::oauth_discovery::{self, DiscoveryOutcome};
use crate::routes::settings::{SettingsRoutesState, html_response, read_current_blocking, render};
use crate::routes::settings_credentials::{DetectedAuth, DetectedAuthResult, credential_snapshot};
use crate::routes::vault::{error_response, not_found};
use crate::token_exchange::{self, TokenResponse};

/// A `state` value is single-use and expires after 10 minutes of not being redeemed by a
/// callback: long enough for an operator to complete a provider's own
/// consent screen, short enough that an abandoned connect attempt cannot be replayed much later.
const OAUTH_STATE_TTL: Duration = Duration::minutes(10);

/// The `[mcp_server.auth] mode = "oauth2"` fields one `connect`/`callback` exchange needs, owned
/// rather than borrowed from a [`WebConfig`]: both handlers re-read `web.toml` at points that
/// straddle an `.await` (a network call, in `callback`'s case), so holding a reference across
/// that boundary is not an option.
struct OauthEntry {
    client_id: Option<String>,
    client_secret: Option<ClientSecretSource>,
    authorization_endpoint: String,
    token_endpoint: String,
    scopes: Vec<String>,
    credential_ref: String,
}

/// Finds `server_name` among `config`'s `[[mcp_server]]` entries and returns its `oauth2`-mode
/// auth fields, or `None` when no such entry exists, it is not an HTTP entry, or its `auth` is
/// not `mode = "oauth2"`.
fn find_oauth_entry(config: &WebConfig, server_name: &str) -> Option<OauthEntry> {
    config.mcp_servers.iter().find_map(|entry| match entry {
        McpServerConfig::Http {
            name,
            auth:
                Some(
                    oauth @ McpServerAuth::Oauth2 {
                        client_id,
                        authorization_endpoint,
                        token_endpoint,
                        scopes,
                        credential_ref,
                        ..
                    },
                ),
            ..
        } if name == server_name => Some(OauthEntry {
            client_id: client_id.clone(),
            client_secret: oauth.client_secret(),
            authorization_endpoint: authorization_endpoint.clone(),
            token_endpoint: token_endpoint.clone(),
            scopes: scopes.clone(),
            credential_ref: credential_ref.clone(),
        }),
        _ => None,
    })
}

/// Finds `server_name` among `config`'s `[[mcp_server]]` entries and returns its HTTP `endpoint`,
/// regardless of whatever `auth` mode it currently has, or none at all: unlike [`find_oauth_entry`],
/// this must resolve *before* an operator has configured any `oauth2` fields, since detection is
/// what helps fill those fields in, in the first place.
fn find_http_endpoint(config: &WebConfig, server_name: &str) -> Option<String> {
    config.mcp_servers.iter().find_map(|entry| match entry {
        McpServerConfig::Http { name, endpoint, .. } if name == server_name => Some(endpoint.clone()),
        _ => None,
    })
}

/// The fixed loopback callback URL for `server_name`, built from `web.toml`'s own `bind`
///: the exact string an operator following the client-pre-registration
/// pattern pastes into their own provider's OAuth app console.
fn callback_redirect_uri(bind: &str, server_name: &str) -> String {
    format!("http://{bind}/settings/oauth/{server_name}/callback")
}

/// Redirects (always a genuine `303`, never `HX-Redirect`: every caller of this helper is either
/// `callback` itself, a real top-level browser navigation from the provider, or `connect`'s
/// non-htmx branch) to `/settings/?oauth_error={reason}&server={server_name}`.
fn oauth_error_redirect(server_name: &str, reason: &str) -> Response {
    let target = format!(
        "/settings/?oauth_error={}&server={}",
        encode_query_value(reason),
        encode_query_value(server_name)
    );
    Redirect::to(&target).into_response()
}

/// Consumes the pending connection under `state_value`, if any, without inspecting the result:
/// used on a rejected-callback path where a `state` value was present but something else about
/// the callback was already invalid, so the value must not survive to be replayed against a
/// corrected retry.
fn consume_if_present(state: &SettingsRoutesState, state_value: Option<&str>, now: OffsetDateTime) {
    if let Some(state_value) = state_value {
        let _ = state.pending_oauth().take(state_value, now, OAUTH_STATE_TTL);
    }
}

// ---------------------------------------------------------------------
// POST /settings/oauth/{server_name}/connect
// ---------------------------------------------------------------------

/// Begins the PKCE authorisation-code flow: generates the verifier/challenge/`state` triple,
/// records it against `server_name`, and sends the operator's browser to `authorization_endpoint`.
///
/// This must reach the *browser itself*, not an htmx-swapped fragment: an `HX-Request` gets
/// `HX-Redirect` (htmx's own documented "do a full top-level navigation" mechanism, the identical
/// precedent `routes::settings::html_response_with_refresh` already sets for chrome a fragment
/// swap cannot express); a plain browser request gets a genuine `303` with `Location` set.
pub async fn connect(
    State(state): State<SettingsRoutesState>,
    AxumPath(server_name): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    let config = match read_current_blocking(state.web_config_path()).await {
        Ok(config) => config,
        Err(response) => return *response,
    };
    let Some(entry) = find_oauth_entry(&config, &server_name) else {
        return not_found();
    };

    // Validated before anything is recorded in `pending_oauth`: an entry whose
    // `authorization_endpoint` cannot even parse (only reachable via a hand-edited `web.toml`
    // bypassing `/settings/`'s own writer, which does not itself validate URL syntax) must fail
    // without leaking an orphaned, never-consumable pending connection on every attempt.
    let Ok(mut authorization_url) = reqwest::Url::parse(&entry.authorization_endpoint) else {
        return error_response(StatusCode::UNPROCESSABLE_ENTITY, "settings/invalid-configuration");
    };

    let code_verifier = oauth::generate_code_verifier();
    let code_challenge = oauth::code_challenge_s256(&code_verifier);
    let state_value = oauth::generate_state();
    state.pending_oauth().insert(
        state_value.clone(),
        PendingOauthConnection {
            server_name: server_name.clone(),
            code_verifier,
            created_at: SystemClock.now(),
        },
    );

    let redirect_uri = callback_redirect_uri(&config.server.bind, &server_name);
    {
        let mut query = authorization_url.query_pairs_mut();
        query.append_pair("response_type", "code");
        if let Some(client_id) = &entry.client_id {
            query.append_pair("client_id", client_id);
        }
        query.append_pair("redirect_uri", &redirect_uri);
        query.append_pair("scope", &entry.scopes.join(" "));
        query.append_pair("state", &state_value);
        query.append_pair("code_challenge", &code_challenge);
        query.append_pair("code_challenge_method", "S256");
    }
    let authorization_url = authorization_url.to_string();

    if headers.contains_key("hx-request") {
        (StatusCode::OK, [("HX-Redirect", authorization_url)], String::new()).into_response()
    } else {
        Redirect::to(&authorization_url).into_response()
    }
}

// ---------------------------------------------------------------------
// POST /settings/oauth/{server_name}/detect
// ---------------------------------------------------------------------

/// A short, operator-facing sentence for one [`DiscoveryOutcome`], rendered exactly like the
/// connect/callback flow's own `?oauth_connected=`/`?oauth_error=` banner
/// (`settings_credentials::settings_banner`), except computed directly rather than from a query
/// parameter, since this is a same-request response, never a redirect.
fn detect_message(server_name: &str, outcome: &DiscoveryOutcome) -> (Option<String>, Option<String>) {
    match outcome {
        DiscoveryOutcome::NoAuthRequired => (
            Some(format!(
                "{server_name} does not appear to require authentication. Review, then save to apply."
            )),
            None,
        ),
        DiscoveryOutcome::Discovered(_) => (
            Some(format!(
                "Detected authentication endpoints for {server_name}. Review the pre-filled fields, then Connect."
            )),
            None,
        ),
        DiscoveryOutcome::Failed(reason) => (
            None,
            Some(format!("Could not detect authentication for {server_name}: {reason}.")),
        ),
    }
}

/// `POST .../detect`'s optional JSON body: the endpoint field's own *current, possibly unsaved*
/// value from the edit form, so clicking Detect probes what the operator is looking at right now
/// rather than silently falling back to whatever `web.toml` still has on disk. Absent or blank
/// falls back to the entry's persisted `endpoint`.
#[derive(Debug, Deserialize)]
struct DetectBody {
    #[serde(default)]
    endpoint: Option<String>,
}

/// Probes `server_name`'s own HTTP endpoint (regardless of its currently configured `auth` mode,
/// or lack of one) for its authentication requirement, pre-filling the `oauth2` fieldset's
/// endpoint/scope fields for the operator to review. Never writes anything to `web.toml` or the
/// credential store itself: only the operator's own subsequent edit-and-save, or `Connect`, does
/// that, exactly the same review-before-anything-is-written guarantee every other field on this
/// page already gives.
pub async fn detect(
    State(state): State<SettingsRoutesState>,
    AxumPath(server_name): AxumPath<String>,
    body: axum::body::Bytes,
) -> Response {
    let current = match read_current_blocking(state.web_config_path()).await {
        Ok(config) => config,
        Err(response) => return *response,
    };
    let Some(persisted_endpoint) = find_http_endpoint(&current, &server_name) else {
        return not_found();
    };
    let endpoint = serde_json::from_slice::<DetectBody>(&body)
        .ok()
        .and_then(|parsed| parsed.endpoint)
        .filter(|endpoint| !endpoint.trim().is_empty())
        .unwrap_or(persisted_endpoint);

    let outcome = oauth_discovery::discover(&reqwest::Client::new(), &endpoint).await;
    if let DiscoveryOutcome::Failed(reason) = &outcome {
        tracing::info!(server = %server_name, reason = %reason, "auth detection did not resolve a usable result");
    }
    let (notice, error) = detect_message(&server_name, &outcome);
    let result = match outcome {
        DiscoveryOutcome::NoAuthRequired => Some(DetectedAuthResult::NoAuthRequired),
        DiscoveryOutcome::Discovered(fields) => Some(DetectedAuthResult::Discovered {
            authorization_endpoint: fields.authorization_endpoint,
            token_endpoint: fields.token_endpoint,
            scopes: fields.scopes.join(" "),
        }),
        DiscoveryOutcome::Failed(_) => None,
    };
    let detected = result.map(|result| DetectedAuth {
        server_name: &server_name,
        result,
    });

    let (config, readiness) = tokio::join!(
        read_current_blocking(state.web_config_path()),
        state.clients().readiness()
    );
    match config {
        Ok(config) => {
            let credentials = credential_snapshot(state.credential_store(), &config).await;
            html_response(render(
                &config,
                &readiness,
                &credentials,
                notice,
                error,
                detected.as_ref(),
            ))
        }
        Err(response) => *response,
    }
}

// ---------------------------------------------------------------------
// GET /settings/oauth/{server_name}/callback
// ---------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CallbackParams {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    state: Option<String>,
    /// Present instead of `code`/`state` when the operator declined the provider's consent
    /// screen, or the provider otherwise rejected the request.
    #[serde(default)]
    error: Option<String>,
}

/// Exchanges `code` for a token at `entry.token_endpoint` via the shared
/// [`token_exchange::request_token`] helper (the identical RFC 6749 §5.1 POST-and-parse
/// implementation `oauth_refresh::ensure_fresh`'s refresh-token grant also calls), supplying this
/// grant's own `grant_type`, `code`, `redirect_uri`, and `code_verifier` fields.
async fn exchange_token(
    entry: &OauthEntry,
    redirect_uri: &str,
    code: &str,
    code_verifier: &str,
    client_secret: Option<&str>,
    server_name: &str,
) -> Option<TokenResponse> {
    let mut form: Vec<(&str, &str)> = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", code_verifier),
    ];
    if let Some(client_id) = &entry.client_id {
        form.push(("client_id", client_id));
    }
    if let Some(client_secret) = client_secret {
        form.push(("client_secret", client_secret));
    }

    token_exchange::request_token(&entry.token_endpoint, &form, server_name).await
}

/// The fixed redirect target an operator's provider sends the browser back to. Always a real
/// top-level browser navigation, never an `HX-Request`, so this always answers with a genuine
/// redirect, never `HX-Redirect`.
pub async fn callback(
    State(state): State<SettingsRoutesState>,
    AxumPath(server_name): AxumPath<String>,
    Query(params): Query<CallbackParams>,
) -> Response {
    let now = SystemClock.now();

    if let Some(reason) = params.error {
        consume_if_present(&state, params.state.as_deref(), now);
        return oauth_error_redirect(&server_name, &reason);
    }
    let Some(code) = params.code else {
        consume_if_present(&state, params.state.as_deref(), now);
        return oauth_error_redirect(&server_name, "missing_parameters");
    };
    let Some(state_value) = params.state else {
        return oauth_error_redirect(&server_name, "missing_parameters");
    };

    let Some(pending) = state.pending_oauth().take(&state_value, now, OAUTH_STATE_TTL) else {
        return oauth_error_redirect(&server_name, "invalid_state");
    };
    if pending.server_name != server_name {
        return oauth_error_redirect(&server_name, "invalid_state");
    }

    // The entry's own fields may have changed since `connect` ran a few minutes ago: always the
    // *current* document, never anything cached from `connect`.
    let config = match read_current_blocking(state.web_config_path()).await {
        Ok(config) => config,
        Err(_response) => return oauth_error_redirect(&server_name, "server_removed"),
    };
    let Some(entry) = find_oauth_entry(&config, &server_name) else {
        return oauth_error_redirect(&server_name, "server_removed");
    };

    let Ok(client_secret) =
        crate::oauth_refresh::resolve_client_secret(state.credential_store(), entry.client_secret.as_ref()).await
    else {
        return oauth_error_redirect(&server_name, "client_secret_missing");
    };

    let redirect_uri = callback_redirect_uri(&config.server.bind, &server_name);
    let Some(token_response) = exchange_token(
        &entry,
        &redirect_uri,
        &code,
        &pending.code_verifier,
        client_secret.as_deref(),
        &server_name,
    )
    .await
    else {
        return oauth_error_redirect(&server_name, "token_exchange_failed");
    };

    let expires_in = token_response
        .expires_in
        .and_then(|seconds| i64::try_from(seconds).ok())
        .unwrap_or(3600);
    let obtained_at = now;
    let expires_at = obtained_at + Duration::seconds(expires_in);
    let (Ok(expires_at), Ok(obtained_at)) = (expires_at.format(&Rfc3339), obtained_at.format(&Rfc3339)) else {
        return oauth_error_redirect(&server_name, "token_exchange_failed");
    };

    let record = CredentialRecord::Oauth2Token {
        access_token: token_response.access_token,
        refresh_token: token_response.refresh_token,
        expires_at,
        obtained_at,
    };
    let store = Arc::clone(state.credential_store());
    let credential_ref = entry.credential_ref;
    let write_result = tokio::task::spawn_blocking(move || store.set(&credential_ref, &record)).await;
    if !matches!(write_result, Ok(Ok(()))) {
        return oauth_error_redirect(&server_name, "token_exchange_failed");
    }
    reconnect_after_credential_change(&state, &server_name).await;

    Redirect::to(&format!(
        "/settings/?oauth_connected={}",
        encode_query_value(&server_name)
    ))
    .into_response()
}

// ---------------------------------------------------------------------
// DELETE /settings/oauth/{server_name}/connect
// ---------------------------------------------------------------------

/// Clears `server_name`'s stored token only; its own endpoints/`client_id`/`client_secret_ref`
/// are untouched, so reconnecting is just repeating `connect`. Idempotent, matching
/// [`CredentialStore::delete`]'s own contract: disconnecting an entry with no stored token is not
/// an error.
pub async fn disconnect(State(state): State<SettingsRoutesState>, AxumPath(server_name): AxumPath<String>) -> Response {
    let config = match read_current_blocking(state.web_config_path()).await {
        Ok(config) => config,
        Err(response) => return *response,
    };
    let Some(entry) = find_oauth_entry(&config, &server_name) else {
        return not_found();
    };

    let store = Arc::clone(state.credential_store());
    let credential_ref = entry.credential_ref;
    let delete_result = tokio::task::spawn_blocking(move || store.delete(&credential_ref)).await;
    if !matches!(delete_result, Ok(Ok(()))) {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "settings/credential-store-unavailable",
        );
    }

    reconnect_after_credential_change(&state, &server_name).await;

    // Nothing about `web.toml` itself changed, so no `HX-Refresh` concern here: the settings
    // fragment alone already reflects the disconnect, matching
    // `routes::settings::apply_edit_and_persist`'s own no-refresh success path.
    let (config, readiness) = tokio::join!(
        read_current_blocking(state.web_config_path()),
        state.clients().readiness()
    );
    match config {
        Ok(config) => {
            let credentials = credential_snapshot(state.credential_store(), &config).await;
            html_response(render(&config, &readiness, &credentials, None, None, None))
        }
        Err(response) => *response,
    }
}

/// Connects `server_name` again once its stored token has changed, so the session stops presenting the old token, or
/// presenting none: after a connect it signs in with the new token, and after a disconnect it no longer uses the
/// cleared one. The settings page shows the attempt as connecting until it ends.
async fn reconnect_after_credential_change(state: &SettingsRoutesState, server_name: &str) {
    let connecting = crate::routes::settings_connections::sync_connections(state).await;
    if !connecting.iter().any(|name| name == server_name) && state.clients().request_reconnect(server_name).is_err() {
        tracing::warn!(server = %server_name, "OAuth2 credential changed for a server the sessions do not know of");
    }
}

#[cfg(test)]
#[path = "settings_oauth_test.rs"]
mod tests;
