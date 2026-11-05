//! Each configured `[[mcp_server]]`'s connection readiness, as an operator sees it: whether its session is up and,
//! when it is not, why, and what recovers it.
//!
//! Readiness is reported for every configured server, never only the connected ones, so a server that failed to
//! connect is shown with its cause rather than left out.

use std::error::Error as StdError;

use rmcp::service::ClientInitializeError;
use rmcp::transport::streamable_http_client::{AuthRequiredError, InsufficientScopeError};

use crate::config::{McpServerAuth, McpServerConfig};
use crate::mcp_client::{McpConnectError, error_chain};

/// One configured server's readiness, in `web.toml` order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerReadiness {
    pub name: String,
    pub readiness: Readiness,
}

/// Whether a configured server's session can take calls now.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Readiness {
    /// A connection attempt is under way: at start-up, after a reconnect, or after its entry was saved.
    Connecting,
    /// The session is open and answered a `tools/list` probe just now.
    Connected { tool_count: usize },
    /// The session opened, but the server has stopped answering since: a stdio child that exited, or an HTTP server
    /// that went away.
    Unreachable,
    /// The server needs a credential it does not have, or rejected the one it was given.
    LoginRequired(LoginRecovery),
    /// The last connection attempt failed for a reason other than authentication.
    Failed(ConnectionFailure),
}

impl Readiness {
    /// Whether calls to the server can be expected to succeed now.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected { .. })
    }
}

/// What an operator does to give a server the credential it needs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoginRecovery {
    /// Run the entry's `OAuth2` connect flow.
    OauthConnect,
    /// Paste a new bearer token, or set the environment variable the entry names.
    ReplaceBearerToken,
    /// The server asked for credentials the entry does not configure: choose an authentication mode for it.
    ConfigureAuthentication,
}

/// Why the last connection attempt failed, with the error that ended it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectionFailure {
    pub cause: FailureCause,
    /// The error and its causes, outermost first, cut to a bounded length. It names the server and the variable or
    /// header at fault, never a credential value.
    pub detail: String,
}

/// The kind of connection failure, which decides the remedy an operator is offered.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FailureCause {
    /// The stdio command could not be started.
    NotStarted,
    /// The server started, or the endpoint answered, but no MCP session could be opened with it.
    HandshakeFailed,
    /// No session was opened within the time allowed for one attempt.
    TimedOut,
    /// The environment variable holding the bearer token is not set in `contextos-web`'s own environment.
    TokenEnvUnset { variable: String },
    /// A `[mcp_server.headers]` entry is not a valid HTTP header.
    InvalidHeader { key: String },
    /// The HTTP client for the server could not be built on this host.
    ClientUnavailable,
    /// The attempt was abandoned because `contextos-web` is shutting down.
    Cancelled,
}

/// A connection attempt for `entry` that ended in `error`.
pub(crate) struct FailedAttempt<'a> {
    pub entry: &'a McpServerConfig,
    pub error: &'a McpConnectError,
}

impl From<FailedAttempt<'_>> for Readiness {
    fn from(attempt: FailedAttempt<'_>) -> Self {
        let failure = |cause| {
            Self::Failed(ConnectionFailure {
                cause,
                detail: error_chain(attempt.error),
            })
        };
        match attempt.error {
            McpConnectError::MissingCredential { .. } => Self::LoginRequired(LoginRecovery::ReplaceBearerToken),
            McpConnectError::Handshake { source, .. } if rejected_credentials(source) => {
                Self::LoginRequired(LoginRecovery::from(attempt.entry))
            }
            McpConnectError::Handshake { .. } => failure(FailureCause::HandshakeFailed),
            McpConnectError::Spawn { .. } => failure(FailureCause::NotStarted),
            McpConnectError::TimedOut { .. } => failure(FailureCause::TimedOut),
            McpConnectError::MissingTokenEnv { variable, .. } => failure(FailureCause::TokenEnvUnset {
                variable: variable.clone(),
            }),
            McpConnectError::InvalidHeader { key, .. } => failure(FailureCause::InvalidHeader { key: key.clone() }),
            McpConnectError::HttpClient { .. } => failure(FailureCause::ClientUnavailable),
            McpConnectError::Shutdown => failure(FailureCause::Cancelled),
        }
    }
}

/// The remedy for a server that rejected `entry`'s credentials, which depends on how the entry authenticates.
impl From<&McpServerConfig> for LoginRecovery {
    fn from(entry: &McpServerConfig) -> Self {
        match entry {
            McpServerConfig::Http {
                auth: Some(McpServerAuth::Oauth2 { .. }),
                ..
            } => Self::OauthConnect,
            McpServerConfig::Http {
                auth: Some(McpServerAuth::Bearer { .. }),
                ..
            } => Self::ReplaceBearerToken,
            McpServerConfig::Http { .. } | McpServerConfig::Stdio { .. } => Self::ConfigureAuthentication,
        }
    }
}

/// Whether the handshake ended because the server answered `401` or `403`. rmcp keeps that answer as the transport
/// error's cause; [`ClientInitializeError`] holds the transport error without exposing it as its own source, so it
/// is reached directly rather than through the error chain alone.
fn rejected_credentials(error: &ClientInitializeError) -> bool {
    let ClientInitializeError::TransportError { error, .. } = error else {
        return false;
    };
    let mut current: Option<&(dyn StdError + 'static)> = Some(error);
    while let Some(cause) = current {
        if cause.is::<AuthRequiredError>() || cause.is::<InsufficientScopeError>() {
            return true;
        }
        current = cause.source();
    }
    false
}

#[cfg(test)]
#[path = "mcp_readiness_test.rs"]
mod tests;
