//! How an HTTP `[[mcp_server]]` session authenticates: the bearer header resolved from `[mcp_server.auth]` (or the
//! legacy top-level `token_env`), the static `[mcp_server.headers]` map, and the credential a live session presents.
//!
//! A session's credential is read on every request rather than fixed when the session opens
//! ([`CredentialedHttpClient`]): an `oauth2` access token is replaced when it is refreshed, and the very next request
//! carries the new one without the session being closed and opened again.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, PoisonError, RwLock};

use axum::http::{HeaderName, HeaderValue};
use futures_util::stream::BoxStream;
use rmcp::model::ClientJsonRpcMessage;
use rmcp::transport::streamable_http_client::{
    SseError, StreamableHttpClient, StreamableHttpError, StreamableHttpPostResponse,
};
use sse_stream::Sse;

use crate::config::{McpServerAuth, McpServerConfig};
use crate::credential_store::{CredentialRecord, CredentialStore, FsCredentialStore};
use crate::mcp_client::McpConnectError;
use crate::oauth_refresh::OauthRefreshEntry;

/// The credential one HTTP session presents as its bearer token, shared between the session's own HTTP client and the
/// code that renews it. `None` sends no `Authorization` header at all.
#[derive(Clone, Default)]
pub(crate) struct SessionCredential(Arc<RwLock<Option<String>>>);

/// Shows only whether a token is held, never the token itself.
impl std::fmt::Debug for SessionCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let held = if self.current().is_some() { "held" } else { "none" };
        formatter.debug_tuple("SessionCredential").field(&held).finish()
    }
}

impl SessionCredential {
    pub(crate) fn new(token: Option<String>) -> Self {
        Self(Arc::new(RwLock::new(token)))
    }

    /// The token the next request presents. A lock poisoned by a panicking writer still holds a complete value (a
    /// write is a single assignment), so it is read rather than treated as lost.
    pub(crate) fn current(&self) -> Option<String> {
        self.0.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Replaces the token every later request presents.
    pub(crate) fn replace(&self, token: String) {
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = Some(token);
    }
}

/// rmcp's own reqwest client, presenting [`SessionCredential`]'s current token on every request in place of the
/// token the transport was configured with. rmcp fixes a transport's `auth_header` when the session opens; this is
/// what lets a refreshed `oauth2` token reach a session that is already open.
#[derive(Clone)]
pub(crate) struct CredentialedHttpClient {
    inner: reqwest::Client,
    credential: SessionCredential,
}

impl CredentialedHttpClient {
    /// A client for a streamable HTTP transport (no idle connection pool, so a reused connection never stalls on a
    /// delayed acknowledgement; no redirects, so custom headers are never replayed to another host), presenting
    /// `credential`.
    pub(crate) fn new(credential: SessionCredential) -> Result<Self, reqwest::Error> {
        let inner = reqwest::Client::builder()
            .pool_max_idle_per_host(0)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self { inner, credential })
    }
}

impl StreamableHttpClient for CredentialedHttpClient {
    type Error = reqwest::Error;

    fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        _auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> impl Future<Output = Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>>> + Send + '_ {
        self.inner
            .post_message(uri, message, session_id, self.credential.current(), custom_headers)
    }

    fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        _auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> impl Future<Output = Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>>> + Send + '_ {
        self.inner.post_message_with_max_sse_event_size(
            uri,
            message,
            session_id,
            self.credential.current(),
            custom_headers,
            max_sse_event_size,
        )
    }

    fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        _auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> impl Future<Output = Result<(), StreamableHttpError<Self::Error>>> + Send + '_ {
        self.inner
            .delete_session(uri, session_id, self.credential.current(), custom_headers)
    }

    fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        _auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> impl Future<Output = Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<Self::Error>>> + Send + '_
    {
        self.inner.get_stream(
            uri,
            session_id,
            last_event_id,
            self.credential.current(),
            custom_headers,
        )
    }

    fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        _auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_sse_event_size: usize,
    ) -> impl Future<Output = Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<Self::Error>>> + Send + '_
    {
        self.inner.get_stream_with_max_sse_event_size(
            uri,
            session_id,
            last_event_id,
            self.credential.current(),
            custom_headers,
            max_sse_event_size,
        )
    }
}

/// The [`OauthRefreshEntry`] `entry` resolves to when it is an HTTP entry whose `auth` is
/// `mode = "oauth2"`, or `None` for a stdio entry or an HTTP entry with any other `auth`
/// (including none at all): the single gate deciding whether a connected session carries
/// refresh state at all.
pub(crate) fn oauth_refresh_entry(entry: &McpServerConfig) -> Option<OauthRefreshEntry> {
    let McpServerConfig::Http { name, auth, .. } = entry else {
        return None;
    };
    let Some(
        oauth @ McpServerAuth::Oauth2 {
            client_id,
            token_endpoint,
            credential_ref,
            ..
        },
    ) = auth
    else {
        return None;
    };
    Some(OauthRefreshEntry {
        server_name: name.clone(),
        token_endpoint: token_endpoint.clone(),
        client_id: client_id.clone(),
        client_secret: oauth.client_secret(),
        credential_ref: credential_ref.clone(),
    })
}

/// The access token currently stored for `credential_ref`, or `None` when nothing has ever been
/// stored there (the operator has not yet completed `connect`) or the store cannot be read: an
/// `oauth2`-mode entry connects unauthenticated in that case, exactly as an HTTP entry with no
/// `auth` configured at all does, rather than this module inventing a distinct startup error for
/// an ordinary state ("Connect" not yet run).
pub(crate) async fn stored_access_token(
    credential_store: &Arc<FsCredentialStore>,
    credential_ref: &str,
) -> Option<String> {
    let store = Arc::clone(credential_store);
    let credential_ref = credential_ref.to_owned();
    match tokio::task::spawn_blocking(move || store.get(&credential_ref)).await {
        Ok(Ok(Some(CredentialRecord::Oauth2Token { access_token, .. }))) => Some(access_token),
        _ => None,
    }
}

/// The pasted bearer token currently stored for `credential_ref`, or `None` when nothing has ever
/// been stored there or the store cannot be read. Distinct from [`stored_access_token`], which
/// reads an `oauth2` token pair: a `bearer`-mode `credential_ref` names a
/// [`CredentialRecord::Bearer`] record instead, and, unlike `oauth2` mode, has no legitimate
/// "not pasted yet" window once an operator has chosen this credential source at all (`connect`
/// treats an unresolved `bearer` `credential_ref` as [`McpConnectError::MissingCredential`], the
/// same class of error `token_env` already gets when its own named variable is unset).
async fn stored_bearer_token(credential_store: &Arc<FsCredentialStore>, credential_ref: &str) -> Option<String> {
    let store = Arc::clone(credential_store);
    let credential_ref = credential_ref.to_owned();
    match tokio::task::spawn_blocking(move || store.get(&credential_ref)).await {
        Ok(Ok(Some(CredentialRecord::Bearer { value, .. }))) => Some(value),
        _ => None,
    }
}

/// Reads `variable` from the process environment, mapping an unset variable to
/// [`McpConnectError::MissingTokenEnv`].
fn env_token(server: &str, variable: &str) -> Result<String, McpConnectError> {
    std::env::var(variable).map_err(|_source| McpConnectError::MissingTokenEnv {
        server: server.to_owned(),
        variable: variable.to_owned(),
    })
}

/// Resolves the bearer `Authorization` header value for an HTTP entry, given its own
/// `[mcp_server.auth]` table (`auth`) and the legacy top-level `token_env` field it may also
/// still carry (`legacy_token_env`).
///
/// An explicit `auth` table always governs once present; the legacy `token_env` field's own
/// compatibility meaning reads the other way only for `auth`'s *absence*: `mode = "none"` or
/// `mode = "oauth2"` both resolve to `Ok(None)` here regardless of any stray legacy `token_env`
/// value left over on the entry: an operator who has explicitly moved an entry onto the new
/// `auth` table must have that table's own meaning honoured, not silently overridden by a field
/// the settings UI no longer surfaces for editing once `auth` exists. `mode = "bearer"` resolves
/// its own nested `token_env`/`credential_ref` (exactly one is ever set, per
/// [`McpServerConfig::validate_auth`]), never the legacy field. Only when `auth` is entirely
/// absent does `legacy_token_env` apply, unchanged from this crate's original, `auth`-table-free
/// behaviour.
///
/// # Errors
///
/// Returns [`McpConnectError::MissingTokenEnv`] when a resolved `token_env` (legacy or nested) is
/// not set, or [`McpConnectError::MissingCredential`] when a `bearer`-mode `credential_ref` names
/// no currently-stored record.
pub(crate) async fn resolve_bearer_header(
    server: &str,
    auth: Option<&McpServerAuth>,
    legacy_token_env: Option<&String>,
    credential_store: &Arc<FsCredentialStore>,
) -> Result<Option<String>, McpConnectError> {
    match auth {
        Some(McpServerAuth::None | McpServerAuth::Oauth2 { .. }) => Ok(None),
        Some(McpServerAuth::Bearer {
            token_env,
            credential_ref,
        }) => {
            if let Some(variable) = token_env {
                env_token(server, variable).map(Some)
            } else if let Some(credential_ref) = credential_ref {
                match stored_bearer_token(credential_store, credential_ref).await {
                    Some(value) => Ok(Some(value)),
                    None => Err(McpConnectError::MissingCredential {
                        server: server.to_owned(),
                        credential_ref: credential_ref.clone(),
                    }),
                }
            } else {
                // Unreachable once `validate_auth`'s exactly-one rule has run over `web.toml`,
                // but this function does not itself assume that validation has happened (a
                // defence-in-depth stance, not dead code): neither source configured is simply
                // no authentication, not a hard error here.
                Ok(None)
            }
        }
        None => match legacy_token_env {
            Some(variable) => env_token(server, variable).map(Some),
            None => Ok(None),
        },
    }
}

/// Converts an `[[mcp_server]]` entry's `[mcp_server.headers]` static, non-secret map into the
/// transport's own header map, merged into every request the connected session sends.
///
/// # Errors
///
/// Returns [`McpConnectError::InvalidHeader`] when a key is not a valid HTTP header name or a
/// value is not a valid HTTP header value: a malformed entry is rejected outright (this crate's
/// reject-malformed-input discipline), never silently dropped or repaired.
pub(crate) fn build_custom_headers(
    server: &str,
    headers: &BTreeMap<String, String>,
) -> Result<HashMap<HeaderName, HeaderValue>, McpConnectError> {
    headers
        .iter()
        .map(|(key, value)| {
            let invalid = || McpConnectError::InvalidHeader {
                server: server.to_owned(),
                key: key.clone(),
            };
            let name = HeaderName::from_bytes(key.as_bytes()).map_err(|_source| invalid())?;
            let value = HeaderValue::from_str(value).map_err(|_source| invalid())?;
            Ok((name, value))
        })
        .collect()
}

#[cfg(test)]
#[path = "mcp_http_auth_test.rs"]
mod tests;
