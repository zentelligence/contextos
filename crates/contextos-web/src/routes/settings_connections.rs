//! Each configured `[[mcp_server]]`'s connection panel on `/settings/`: its readiness, why it is not connected, what
//! recovers it, and a Reconnect action that connects it again without restarting `contextos-web`.
//!
//! `POST /settings/mcp-servers/{name}/reconnect` starts a new connection attempt and answers with the panel, which
//! reads as connecting and polls `GET /settings/mcp-servers/{name}/connection` until the attempt ends.

use askama::Template;
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};

use crate::mcp_readiness::{ConnectionFailure, FailureCause, LoginRecovery, Readiness, ServerReadiness};
use crate::routes::settings::{SettingsRoutesState, read_current_blocking};

/// An `[[mcp_server]]` name as one URL path segment: every byte outside RFC 3986's unreserved set is percent-encoded,
/// a `/` included, since server names are free-form and a name holding one must still address a single segment.
pub(crate) struct ServerSegment(String);

impl From<&str> for ServerSegment {
    fn from(name: &str) -> Self {
        use std::fmt::Write as _;

        let mut encoded = String::with_capacity(name.len());
        for byte in name.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
                encoded.push(char::from(byte));
            } else {
                // Writing to a `String` cannot fail.
                let _ = write!(encoded, "%{byte:02X}");
            }
        }
        Self(encoded)
    }
}

impl std::fmt::Display for ServerSegment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One server's connection panel.
#[derive(Clone, Debug)]
pub(crate) struct ConnectionView {
    pub name_encoded: String,
    /// The status dot's modifier: `ok`, `down`, `pending`, or `attention`.
    pub dot: &'static str,
    pub label: String,
    /// The error that ended the last attempt, when there is one.
    pub cause: Option<String>,
    /// What the operator does about it.
    pub remedy: Option<String>,
    /// Offer the entry's `OAuth2` sign-in action.
    pub offers_oauth_connect: bool,
    /// An attempt is under way: the panel polls until it ends, and offers no Reconnect meanwhile.
    pub connecting: bool,
}

impl ConnectionView {
    /// The panel for an entry `web.toml` holds but the sessions do not know of: one added by hand since they last
    /// read it. Reconnect reads `web.toml` again and connects it.
    pub(crate) fn unknown_to_set(name: &str) -> Self {
        Self {
            name_encoded: ServerSegment::from(name).to_string(),
            dot: "down",
            label: "Not connected".to_owned(),
            cause: None,
            remedy: Some("This entry was added to web.toml outside this page. Reconnect to connect it.".to_owned()),
            offers_oauth_connect: false,
            connecting: false,
        }
    }
}

impl From<&ServerReadiness> for ConnectionView {
    fn from(server: &ServerReadiness) -> Self {
        let base = Self {
            name_encoded: ServerSegment::from(server.name.as_str()).to_string(),
            dot: "down",
            label: String::new(),
            cause: None,
            remedy: None,
            offers_oauth_connect: false,
            connecting: false,
        };
        match &server.readiness {
            Readiness::Connecting => Self {
                dot: "pending",
                label: "Connecting".to_owned(),
                connecting: true,
                ..base
            },
            Readiness::Connected { tool_count } => Self {
                dot: "ok",
                label: match tool_count {
                    1 => "Connected, 1 tool".to_owned(),
                    count => format!("Connected, {count} tools"),
                },
                ..base
            },
            Readiness::Unreachable => Self {
                label: "Unreachable".to_owned(),
                remedy: Some(
                    "The server stopped answering after it connected. Reconnect to start a new session.".to_owned(),
                ),
                ..base
            },
            Readiness::LoginRequired(recovery) => Self {
                dot: "attention",
                label: "Login required".to_owned(),
                remedy: Some(recovery.remedy().to_owned()),
                offers_oauth_connect: *recovery == LoginRecovery::OauthConnect,
                ..base
            },
            Readiness::Failed(failure) => Self {
                label: "Not connected".to_owned(),
                cause: Some(failure.detail.clone()),
                remedy: Some(failure.remedy()),
                ..base
            },
        }
    }
}

impl LoginRecovery {
    /// What the operator does to give the server the credential it needs, as the panel words it.
    fn remedy(self) -> &'static str {
        match self {
            LoginRecovery::OauthConnect => "Sign in to the provider. The session reconnects once you return.",
            LoginRecovery::ReplaceBearerToken => {
                "The server has no bearer token, or rejected the one it was given. Paste a new token under Edit and save, \
             or correct the environment variable the entry names."
            }
            LoginRecovery::ConfigureAuthentication => {
                "The server asks for credentials this entry does not configure. Choose an authentication mode under Edit \
             and save."
            }
        }
    }
}

impl ConnectionFailure {
    /// What the operator does about this failure, as the panel words it.
    fn remedy(&self) -> String {
        match &self.cause {
            FailureCause::NotStarted => {
                "The command could not be started. Check the command and its arguments under Edit, then save."
                    .to_owned()
            }
            FailureCause::HandshakeFailed => {
                "The server started, or its endpoint answered, but no MCP session could be \
                                          opened with it. Check that it is an MCP server, then reconnect."
                    .to_owned()
            }
            FailureCause::TimedOut => "The server did not answer in time. Reconnect to try again.".to_owned(),
            FailureCause::TokenEnvUnset { variable } => format!(
                "{variable} is not set in the environment contextos-web runs in. Set it and restart contextos-web, or \
             paste a token under Edit and save instead."
            ),
            FailureCause::InvalidHeader { key } => {
                format!("The header {key:?} is not a valid HTTP header. Correct it under Edit and save.")
            }
            FailureCause::ClientUnavailable => {
                "The HTTP client could not be built on this host; the log has the cause. Reconnect once it is fixed."
                    .to_owned()
            }
            FailureCause::Cancelled => {
                "The attempt was cancelled while contextos-web was shutting down. Reconnect to try again.".to_owned()
            }
        }
    }
}

#[derive(Template)]
#[template(path = "settings_mcp_connection.html")]
struct ConnectionTemplate<'a> {
    connection: &'a ConnectionView,
}

fn connection_panel(server: &ServerReadiness) -> Response {
    let view = ConnectionView::from(server);
    match (ConnectionTemplate { connection: &view }).render() {
        Ok(html) => Html(html).into_response(),
        Err(_source) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn not_configured() -> Response {
    crate::routes::vault::error_response(StatusCode::NOT_FOUND, "settings/unknown-mcp-server")
}

/// `GET /settings/mcp-servers/{name}/connection`: the server's connection panel as it is now.
pub async fn connection(State(state): State<SettingsRoutesState>, AxumPath(name): AxumPath<String>) -> Response {
    match state.clients().readiness_of(&name).await {
        Some(server) => connection_panel(&server),
        None => not_configured(),
    }
}

/// `POST /settings/mcp-servers/{name}/reconnect`: connects the server again from its current `web.toml` entry, and
/// answers with its panel, which reads as connecting until the attempt ends. `web.toml` is read first, so an entry
/// edited or added by hand is connected as it now reads.
///
/// Only the page's own HTMX request is accepted (`HX-Request`): a header a cross-site form or a simple cross-origin
/// request cannot set, so another page open in the browser cannot relaunch a server behind the operator's back.
pub async fn reconnect(
    State(state): State<SettingsRoutesState>,
    AxumPath(name): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    if !headers.contains_key("hx-request") {
        return crate::routes::vault::error_response(StatusCode::FORBIDDEN, "settings/not-from-page");
    }
    let connecting = sync_connections(&state).await;
    if !connecting.contains(&name) && state.clients().request_reconnect(&name).is_err() {
        return not_configured();
    }
    tracing::info!(server = %name, "reconnect requested from /settings/");
    connection(State(state), AxumPath(name)).await
}

/// Applies the `[[mcp_server]]` entries `web.toml` now holds to the sessions ([`crate::mcp_client::McpClientSet::apply_config`]),
/// returning the names of the servers now connecting. A `web.toml` that cannot be read or is invalid changes nothing:
/// the sessions keep following the last valid configuration, and the page reports the file's problem itself.
pub(crate) async fn sync_connections(state: &SettingsRoutesState) -> Vec<String> {
    let _serialised = state.connections_sync().lock().await;
    match read_current_blocking(state.web_config_path()).await {
        Ok(config) => state.clients().apply_config(&config.mcp_servers),
        Err(_response) => {
            tracing::warn!("web.toml could not be read; MCP sessions keep their last configuration");
            Vec::new()
        }
    }
}

#[cfg(test)]
#[path = "settings_connections_test.rs"]
mod tests;
