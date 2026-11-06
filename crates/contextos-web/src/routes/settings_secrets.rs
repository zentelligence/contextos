//! The obscured secret controls on `/server/settings/`, and the route that reveals one on request.
//!
//! A configured secret renders as a fixed mask with a Show control, never its value. Show asks
//! `GET /server/settings/mcp-servers/{name}/secrets/{field}`, which answers with the value and a Hide control;
//! Hide asks the same route with `?view=obscured`, which answers with the mask again and reads nothing. A stored
//! value comes from the credential store; a variable's value is the one `contextos-web` itself sees now.

use std::sync::Arc;

use askama::Template;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use serde::Deserialize;

use crate::config::{McpServerAuth, McpServerConfig};
use crate::credential_store::{CredentialRecord, CredentialStore};
use crate::routes::settings::{SettingsRoutesState, read_current_blocking};
use crate::routes::settings_connections::ServerSegment;

/// A secret an `[[mcp_server]]` entry can hold, as the last segment of its reveal route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SecretField {
    /// The pasted bearer token stored under the entry's `credential_ref`.
    BearerToken,
    /// The current value of the variable the entry's bearer `token_env` names.
    BearerTokenVariable,
    /// The pasted `OAuth2` client secret stored under the entry's `client_secret_ref`.
    ClientSecret,
    /// The current value of the variable the entry's `client_secret_env` names.
    ClientSecretVariable,
}

impl SecretField {
    const fn segment(self) -> &'static str {
        match self {
            Self::BearerToken => "bearer-token",
            Self::BearerTokenVariable => "bearer-token-variable",
            Self::ClientSecret => "client-secret",
            Self::ClientSecretVariable => "client-secret-variable",
        }
    }
}

/// A reveal route segment that names no [`SecretField`].
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct UnknownSecretField;

impl TryFrom<&str> for SecretField {
    type Error = UnknownSecretField;

    fn try_from(segment: &str) -> Result<Self, Self::Error> {
        match segment {
            "bearer-token" => Ok(Self::BearerToken),
            "bearer-token-variable" => Ok(Self::BearerTokenVariable),
            "client-secret" => Ok(Self::ClientSecret),
            "client-secret-variable" => Ok(Self::ClientSecretVariable),
            _ => Err(UnknownSecretField),
        }
    }
}

/// One secret's control: obscured until the operator asks for it.
#[derive(Clone, Debug)]
pub(crate) struct SecretControl {
    /// This secret's reveal route.
    pub(crate) url: String,
    /// What a reveal found; `None` while obscured.
    pub(crate) revealed: Option<Revealed>,
}

impl SecretControl {
    /// The obscured control for `field` of the entry named `server`.
    pub(crate) fn obscured(server: &str, field: SecretField) -> Self {
        Self {
            url: format!(
                "/server/settings/mcp-servers/{}/secrets/{}",
                ServerSegment::from(server),
                field.segment()
            ),
            revealed: None,
        }
    }
}

/// What revealing a secret found.
#[derive(Clone)]
pub(crate) enum Revealed {
    Value(String),
    /// The named variable is not set in `contextos-web`'s environment.
    UnsetVariable(String),
}

impl std::fmt::Debug for Revealed {
    /// Names an unset variable, but never prints a revealed value.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Value(_) => formatter.write_str("Value(\"<redacted>\")"),
            Self::UnsetVariable(variable) => formatter.debug_tuple("UnsetVariable").field(variable).finish(),
        }
    }
}

#[derive(Template)]
#[template(path = "settings_secret.html")]
struct SecretTemplate<'a> {
    secret: &'a SecretControl,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretQuery {
    view: Option<SecretView>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum SecretView {
    Revealed,
    Obscured,
}

/// Where an entry keeps the secret a [`SecretField`] names.
enum SecretSource<'a> {
    Stored(&'a str),
    Variable(&'a str),
}

fn source_of(entry: &McpServerConfig, field: SecretField) -> Option<SecretSource<'_>> {
    let McpServerConfig::Http {
        token_env: legacy_token_env,
        auth,
        ..
    } = entry
    else {
        return None;
    };
    match (field, auth) {
        (SecretField::BearerToken, Some(McpServerAuth::Bearer { credential_ref, .. })) => {
            credential_ref.as_deref().map(SecretSource::Stored)
        }
        (SecretField::BearerTokenVariable, Some(McpServerAuth::Bearer { token_env, .. })) => {
            token_env.as_deref().map(SecretSource::Variable)
        }
        // With no `[mcp_server.auth]` table, the entry's top-level `token_env` supplies the bearer token.
        (SecretField::BearerTokenVariable, None) => legacy_token_env.as_deref().map(SecretSource::Variable),
        (SecretField::ClientSecret, Some(McpServerAuth::Oauth2 { client_secret_ref, .. })) => {
            client_secret_ref.as_deref().map(SecretSource::Stored)
        }
        (SecretField::ClientSecretVariable, Some(McpServerAuth::Oauth2 { client_secret_env, .. })) => {
            client_secret_env.as_deref().map(SecretSource::Variable)
        }
        _ => None,
    }
}

fn secret_response(secret: &SecretControl) -> Response {
    match (SecretTemplate { secret }).render() {
        Ok(html) => {
            let mut response = Html(html).into_response();
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
        Err(_source) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn refused(status: StatusCode, code: &'static str) -> Response {
    crate::routes::vault::error_response(status, code)
}

/// `GET /server/settings/mcp-servers/{name}/secrets/{field}`: the secret's value with a Hide control, or, with
/// `?view=obscured`, the mask with a Show control.
///
/// Only the page's own HTMX request is accepted (`HX-Request`), as for Reconnect, and the answer is never cached.
pub async fn secret(
    State(state): State<SettingsRoutesState>,
    AxumPath((name, segment)): AxumPath<(String, String)>,
    Query(query): Query<SecretQuery>,
    headers: HeaderMap,
) -> Response {
    if !headers.contains_key("hx-request") {
        return refused(StatusCode::FORBIDDEN, "settings/not-from-page");
    }
    let Ok(field) = SecretField::try_from(segment.as_str()) else {
        return refused(StatusCode::NOT_FOUND, "settings/unknown-secret");
    };
    let config = match read_current_blocking(state.web_config_path()).await {
        Ok(config) => config,
        Err(response) => return *response,
    };
    let Some(entry) = config.mcp_servers.iter().find(|entry| entry.name() == name) else {
        return refused(StatusCode::NOT_FOUND, "settings/unknown-mcp-server");
    };
    let Some(source) = source_of(entry, field) else {
        return refused(StatusCode::NOT_FOUND, "settings/secret-not-configured");
    };
    let mut control = SecretControl::obscured(&name, field);
    if query.view == Some(SecretView::Obscured) {
        return secret_response(&control);
    }

    let revealed = match source {
        SecretSource::Variable(variable) => match std::env::var(variable) {
            Ok(value) => Revealed::Value(value),
            Err(_source) => Revealed::UnsetVariable(variable.to_owned()),
        },
        SecretSource::Stored(reference) => {
            let store = Arc::clone(state.credential_store());
            let reference = reference.to_owned();
            let record = tokio::task::spawn_blocking(move || store.get(&reference)).await;
            match (field, record) {
                (SecretField::BearerToken, Ok(Ok(Some(CredentialRecord::Bearer { value, .. }))))
                | (SecretField::ClientSecret, Ok(Ok(Some(CredentialRecord::Oauth2ClientSecret { value, .. })))) => {
                    Revealed::Value(value)
                }
                (_, Ok(Ok(_))) => return refused(StatusCode::NOT_FOUND, "settings/secret-not-configured"),
                (_, Ok(Err(_)) | Err(_)) => {
                    return refused(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "settings/credential-store-unreadable",
                    );
                }
            }
        }
    };
    tracing::info!(server = %name, field = field.segment(), "secret revealed on /server/settings/");
    control.revealed = Some(revealed);
    secret_response(&control)
}

#[cfg(test)]
#[path = "settings_secrets_test.rs"]
mod tests;
