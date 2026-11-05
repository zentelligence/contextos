//! The stored-token refresh gate a proxied call to an `oauth2`-mode HTTP `[[mcp_server]]` entry
//! passes through first: checks the stored token's own `expires_at`
//! against a fixed safety margin and, when it is missing, unparseable, or within that margin,
//! runs the refresh-token grant against `token_endpoint` and persists the refreshed pair before
//! letting the call through.
//!
//! Deliberately free of `McpClient`/`rmcp` concerns, mirroring `oauth.rs`'s own "pure and testable
//! in isolation" stance: everything here is credential-store reads/writes plus one HTTP exchange
//! (via the shared [`crate::token_exchange`] helper), so the whole gate is exercised without a
//! live MCP session at all. `mcp_client::McpClient::ensure_oauth_token_fresh` is the only caller,
//! and `routes::proxy::handle` is the only place that calls it, immediately before `call_tool`,
//! for exactly the HTTP-transport, `oauth2`-mode sessions this gate applies to.

use std::sync::Arc;

use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

use crate::config::ClientSecretSource;
use crate::credential_store::{CredentialRecord, CredentialStore, FsCredentialStore};
use crate::logging::Clock;
use crate::token_exchange;

/// The fixed safety margin a proxied call's own refresh check applies: a token expiring within
/// this window of "now" is refreshed before the call it is guarding proceeds, rather than being
/// used right up to the instant it lapses.
pub const REFRESH_MARGIN: Duration = Duration::seconds(60);

/// The `mode = "oauth2"` fields one refresh attempt needs, resolved once from a `[[mcp_server]]`
/// entry at connect time (`mcp_client::McpClient::connect_with`): structural configuration that
/// does not change without a restart, distinct from the live token pair itself, which always
/// comes fresh from the credential store on every check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OauthRefreshEntry {
    pub server_name: String,
    pub token_endpoint: String,
    pub client_id: Option<String>,
    pub client_secret: Option<ClientSecretSource>,
    pub credential_ref: String,
}

/// A refresh attempt failed: the provider rejected the refresh token, `token_endpoint` was
/// unreachable, no refresh token was ever issued, no token had ever been stored at all (the
/// operator never completed `connect`), or the credential store itself could not be read or
/// written. Every one of those collapses to this single, actionable outcome: the caller must
/// reconnect, regardless of which specific cause applied.
#[derive(Debug, Error, Eq, PartialEq)]
#[error(
    "OAuth2 token refresh failed for MCP server {server_name:?}; reconnect via \
     POST /settings/oauth/{server_name}/connect"
)]
pub struct OauthRefreshError {
    pub server_name: String,
}

/// Ensures `entry`'s stored token is fresh, refreshing it first when it is missing, unparseable,
/// or within [`REFRESH_MARGIN`] of `expires_at`. Returns the access token now in force once a
/// still-valid one is confirmed to exist in `credential_store` (whether or not a refresh actually
/// ran), so the session presents exactly the token the store holds; never mutates the stored
/// record when no refresh was needed.
///
/// # Errors
///
/// Returns [`OauthRefreshError`] when no usable token pair could be confirmed: no record was ever
/// stored, the stored record carries no refresh token and is due for refresh, a configured
/// client secret could not be resolved, the refresh-token grant against `token_endpoint`
/// failed or was rejected, or the refreshed pair could not be persisted back to the store.
pub async fn ensure_fresh(
    credential_store: &Arc<FsCredentialStore>,
    clock: &Arc<dyn Clock>,
    entry: &OauthRefreshEntry,
) -> Result<String, OauthRefreshError> {
    let failed = || OauthRefreshError {
        server_name: entry.server_name.clone(),
    };

    let stored = read_record(credential_store, &entry.credential_ref).await;
    let Some(CredentialRecord::Oauth2Token {
        access_token,
        refresh_token,
        expires_at,
        ..
    }) = stored
    else {
        return Err(failed());
    };

    if !needs_refresh(&expires_at, clock) {
        return Ok(access_token);
    }

    let Some(refresh_token) = refresh_token else {
        return Err(failed());
    };

    let client_secret = resolve_client_secret(credential_store, entry.client_secret.as_ref())
        .await
        .map_err(|()| failed())?;

    let mut form: Vec<(&str, &str)> = vec![("grant_type", "refresh_token"), ("refresh_token", &refresh_token)];
    if let Some(client_id) = &entry.client_id {
        form.push(("client_id", client_id));
    }
    if let Some(client_secret) = client_secret.as_deref() {
        form.push(("client_secret", client_secret));
    }

    let response = token_exchange::request_token(&entry.token_endpoint, &form, &entry.server_name)
        .await
        .ok_or_else(failed)?;
    let Some((expires_at, obtained_at)) = rfc3339_pair(clock.now(), response.expires_in) else {
        return Err(failed());
    };
    let refreshed_access_token = response.access_token.clone();
    let record = CredentialRecord::Oauth2Token {
        access_token: response.access_token,
        refresh_token: response.refresh_token.or(Some(refresh_token)),
        expires_at,
        obtained_at,
    };

    let store = Arc::clone(credential_store);
    let credential_ref = entry.credential_ref.clone();
    let write_result = tokio::task::spawn_blocking(move || store.set(&credential_ref, &record)).await;
    match write_result {
        Ok(Ok(())) => Ok(refreshed_access_token),
        _ => Err(failed()),
    }
}

/// Whether `expires_at` (an RFC 3339 timestamp) is within [`REFRESH_MARGIN`] of `clock`'s own
/// "now", or cannot be parsed at all: an unparseable value can never be proven fresh, so it is
/// treated the same as an imminent expiry rather than silently trusted.
fn needs_refresh(expires_at: &str, clock: &Arc<dyn Clock>) -> bool {
    match OffsetDateTime::parse(expires_at, &Rfc3339) {
        Ok(expiry) => clock.now() + REFRESH_MARGIN >= expiry,
        Err(_source) => true,
    }
}

/// Reads `credential_ref`'s current record, or `None` on any store error (an invalid reference,
/// an unreadable or unparseable file) exactly as on a genuinely absent record: every one of those
/// means "no usable token pair currently exists", which the caller treats identically.
async fn read_record(credential_store: &Arc<FsCredentialStore>, credential_ref: &str) -> Option<CredentialRecord> {
    let store = Arc::clone(credential_store);
    let credential_ref = credential_ref.to_owned();
    tokio::task::spawn_blocking(move || store.get(&credential_ref))
        .await
        .ok()?
        .ok()?
}

/// Resolves an entry's client secret for a token request, whichever grant makes it: `Ok(None)` when the entry has
/// none (a hosted-consent provider), `Ok(Some(value))` once found, and `Err(())` when one is configured but cannot be
/// had: no [`CredentialRecord::Oauth2ClientSecret`] under the reference, or a variable that is unset or empty. A
/// variable is read from `contextos-web`'s own environment as it is now.
pub(crate) async fn resolve_client_secret(
    credential_store: &Arc<FsCredentialStore>,
    source: Option<&ClientSecretSource>,
) -> Result<Option<String>, ()> {
    match source {
        None => Ok(None),
        Some(ClientSecretSource::Environment(variable)) => match std::env::var(variable) {
            Ok(value) if !value.is_empty() => Ok(Some(value)),
            _ => Err(()),
        },
        Some(ClientSecretSource::Stored(reference)) => match read_record(credential_store, reference).await {
            Some(CredentialRecord::Oauth2ClientSecret { value, .. }) => Ok(Some(value)),
            _ => Err(()),
        },
    }
}

/// The RFC 6749 §5.1 token response's `expires_in` resolved against `now` into the credential
/// store's own RFC 3339 pair, or `None` when either timestamp fails to format (`time`'s own
/// `OffsetDateTime` always formats to RFC 3339 successfully in practice, but this module never
/// panics on it regardless). `expires_in`'s absence falls back to one hour, matching
/// `routes::settings_oauth::callback`'s identical fallback for the authorization-code grant.
fn rfc3339_pair(now: OffsetDateTime, expires_in: Option<u64>) -> Option<(String, String)> {
    let expires_in_seconds = expires_in
        .and_then(|seconds| i64::try_from(seconds).ok())
        .unwrap_or(3600);
    let expires_at = (now + Duration::seconds(expires_in_seconds)).format(&Rfc3339).ok()?;
    let obtained_at = now.format(&Rfc3339).ok()?;
    Some((expires_at, obtained_at))
}

#[cfg(test)]
#[path = "oauth_refresh_test.rs"]
mod tests;
