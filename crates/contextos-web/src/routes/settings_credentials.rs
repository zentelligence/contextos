//! `[mcp_server.auth]`/`[mcp_server.headers]` view-building for the `/settings/` per-entry edit
//! form, the credential-store snapshot `routes::settings::render` reads before rendering, the
//! `?oauth_connected=`/`?oauth_error=` banner `GET /settings/` shows, and the pasted-secret
//! extraction `PATCH /settings/` runs before a submitted `auth` patch ever reaches
//! [`crate::config_writer::WebConfigDocument`]'s JSON-to-TOML bridge.
//!
//! Split out of `routes::settings` to keep that file under this repository's own per-file line
//! limit: every item here is `pub(crate)` back to that module and to `routes::settings_oauth`,
//! its only two callers.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use serde::Deserialize;
use serde_json::Value;

use crate::config::{McpServerAuth, McpServerConfig, WebConfig};
use crate::credential_store::{CredentialRecord, CredentialStore, FsCredentialStore};
use crate::routes::settings_secrets::{SecretControl, SecretField};

// ---------------------------------------------------------------------
// Per-entry `[mcp_server.auth]`/`[mcp_server.headers]` view
// ---------------------------------------------------------------------

/// The `[mcp_server.auth]`/`[mcp_server.headers]` view for one HTTP `[[mcp_server]]` entry's
/// per-entry edit form. Every string field is blank, never `None`, when the
/// corresponding value is unset, so the template can interpolate it directly without an
/// intermediate `if let`.
pub(crate) struct McpServerAuthView {
    /// `"none"`, `"bearer"`, or `"oauth2"`. An absent `[mcp_server.auth]` table combined with a set
    /// legacy top-level `token_env` displays as `"bearer"` for continuity with how that entry
    /// already behaved before this auth table existed; combined with no `token_env` at all, it
    /// displays as `"none"`.
    pub(crate) mode: &'static str,
    /// `[mcp_server.auth] mode = "bearer"`'s own `token_env` field: distinct from the entry's
    /// legacy top-level `token_env` (still rendered separately, unchanged, above this section),
    /// blank when unset.
    pub(crate) bearer_token_env: String,
    /// The `credential_ref` a pasted bearer token is stored under: the entry's own configured
    /// value if it has one, otherwise a generated default name (`"{name}-token"`) ready to use the
    /// first time an operator pastes one.
    pub(crate) bearer_credential_ref: String,
    /// Whether `bearer_credential_ref` currently resolves to a stored
    /// [`CredentialRecord::Bearer`] record. Never the value itself: `/settings/` shows a stored
    /// secret only when the operator asks for it (`routes::settings_secrets`).
    pub(crate) bearer_credential_configured: bool,
    pub(crate) oauth_client_id: String,
    /// The `credential_ref` a pasted `OAuth2` client secret is stored under: the entry's own
    /// configured value if it has one, otherwise a generated default name
    /// (`"{name}-client-secret"`).
    pub(crate) oauth_client_secret_ref: String,
    /// `mode = "oauth2"`'s own `client_secret_env`, blank when unset.
    pub(crate) oauth_client_secret_env: String,
    /// Whether `oauth_client_secret_ref` currently resolves to a stored
    /// [`CredentialRecord::Oauth2ClientSecret`] record.
    pub(crate) oauth_client_secret_configured: bool,
    pub(crate) oauth_authorization_endpoint: String,
    pub(crate) oauth_token_endpoint: String,
    /// Space-separated, matching the existing `args` field's own collection convention.
    pub(crate) oauth_scopes: String,
    /// Where the connect/callback flow stores the resulting token pair: the entry's own configured
    /// value if it has one, otherwise a generated default name (`"{name}-oauth-token"`). Never a
    /// user-editable field in the template, matching `bearer_credential_ref`'s own "managed, not
    /// typed in" treatment.
    pub(crate) oauth_credential_ref: String,
    /// Whether `oauth_credential_ref` currently resolves to a stored
    /// [`CredentialRecord::Oauth2Token`].
    pub(crate) oauth_connected: bool,
    /// That token's own `expires_at`, blank when not connected.
    pub(crate) oauth_expires_at: String,
    /// The entry's own `[mcp_server.headers]` pairs, in file order.
    pub(crate) headers: Vec<(String, String)>,
    /// The obscured control for the stored bearer token, when one is stored.
    pub(crate) bearer_token_secret: Option<SecretControl>,
    /// The obscured control for the bearer token variable's current value, when a variable is named.
    pub(crate) bearer_variable_secret: Option<SecretControl>,
    /// The obscured control for the stored `OAuth2` client secret, when one is stored.
    pub(crate) oauth_client_secret_secret: Option<SecretControl>,
    /// The obscured control for the client secret variable's current value, when a variable is named.
    pub(crate) oauth_client_secret_variable_secret: Option<SecretControl>,
}

impl McpServerAuthView {
    /// Sets the obscured controls for the secrets this entry holds. A control is offered only where the reveal route
    /// would find the secret: a record under a generated default name that the entry does not itself reference is not
    /// this entry's.
    fn offer_secret_controls(&mut self, server_name: &str, auth: Option<&McpServerAuth>) {
        let references_bearer = matches!(
            auth,
            Some(McpServerAuth::Bearer {
                credential_ref: Some(_),
                ..
            })
        );
        let references_client_secret = matches!(
            auth,
            Some(McpServerAuth::Oauth2 {
                client_secret_ref: Some(_),
                ..
            })
        );
        self.bearer_token_secret = (references_bearer && self.bearer_credential_configured)
            .then(|| SecretControl::obscured(server_name, SecretField::BearerToken));
        self.bearer_variable_secret = (!self.bearer_token_env.is_empty())
            .then(|| SecretControl::obscured(server_name, SecretField::BearerTokenVariable));
        self.oauth_client_secret_secret = (references_client_secret && self.oauth_client_secret_configured)
            .then(|| SecretControl::obscured(server_name, SecretField::ClientSecret));
        self.oauth_client_secret_variable_secret = (!self.oauth_client_secret_env.is_empty())
            .then(|| SecretControl::obscured(server_name, SecretField::ClientSecretVariable));
    }
}

/// A `POST /settings/oauth/{server_name}/detect` result for exactly one named entry, applied only
/// to that entry's own [`McpServerAuthView`] by [`mcp_server_auth_view`] and never persisted: the
/// operator's own explicit edit-and-save step is what would write any of this to `web.toml`, not a
/// detection render. `server_name` lets [`mcp_server_auth_view`]'s every other caller apply this
/// unconditionally (`None` most of the time; on the one render right after a `detect` call, the
/// same value for every entry, matching only the one it names).
pub(crate) struct DetectedAuth<'a> {
    pub(crate) server_name: &'a str,
    pub(crate) result: DetectedAuthResult,
}

pub(crate) enum DetectedAuthResult {
    /// The probe found no authentication requirement at all.
    NoAuthRequired,
    /// Endpoints (and, when present, scopes) resolved via RFC 9728/RFC 8414 discovery.
    Discovered {
        authorization_endpoint: String,
        token_endpoint: String,
        /// Space-separated, matching [`McpServerAuthView::oauth_scopes`]'s own convention.
        scopes: String,
    },
}

/// `"{server_name}-{suffix}"`: the generated default `credential_ref`/`client_secret_ref` name for
/// an entry that has not yet configured one of its own, so a fresh `bearer`/`oauth2` selection
/// always has somewhere to store a pasted secret or connect token from the first save onward.
fn default_credential_ref(server_name: &str, suffix: &str) -> String {
    format!("{server_name}-{suffix}")
}

/// Builds [`McpServerAuthView`] for one `[[mcp_server]]` entry: `auth` is the entry's own
/// `[mcp_server.auth]` table (`None` for a stdio entry, or an HTTP entry with none configured),
/// `legacy_token_env` is the entry's top-level `token_env` (meaningful only when `auth` is `None`,
/// the continuity rule above), `headers` is the entry's own `[mcp_server.headers]` table (empty
/// for a stdio entry, which cannot carry one at all), `credentials` is [`credential_snapshot`]'s
/// own gathered map, keyed by `credential_ref`/`client_secret_ref` name, and `detected`, when it
/// names this exact `server_name`, overrides the resulting view's mode/endpoint/scope fields with
/// a just-run `detect` result that has not been saved to `web.toml` at all yet.
pub(crate) fn mcp_server_auth_view(
    server_name: &str,
    auth: Option<&McpServerAuth>,
    legacy_token_env: Option<&str>,
    headers: &std::collections::BTreeMap<String, String>,
    credentials: &HashMap<String, CredentialRecord>,
    detected: Option<&DetectedAuth<'_>>,
) -> McpServerAuthView {
    let mut view = McpServerAuthView {
        mode: "none",
        bearer_token_env: String::new(),
        bearer_credential_ref: default_credential_ref(server_name, "token"),
        bearer_credential_configured: false,
        oauth_client_id: String::new(),
        oauth_client_secret_ref: default_credential_ref(server_name, "client-secret"),
        oauth_client_secret_configured: false,
        oauth_authorization_endpoint: String::new(),
        oauth_token_endpoint: String::new(),
        oauth_scopes: String::new(),
        oauth_credential_ref: default_credential_ref(server_name, "oauth-token"),
        oauth_connected: false,
        oauth_expires_at: String::new(),
        headers: headers
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
        bearer_token_secret: None,
        bearer_variable_secret: None,
        oauth_client_secret_secret: None,
        oauth_client_secret_variable_secret: None,
        oauth_client_secret_env: String::new(),
    };

    match auth {
        None => {
            if let Some(token_env) = legacy_token_env {
                view.mode = "bearer";
                token_env.clone_into(&mut view.bearer_token_env);
            }
        }
        Some(McpServerAuth::None) => {}
        Some(McpServerAuth::Bearer {
            token_env,
            credential_ref,
        }) => {
            view.mode = "bearer";
            view.bearer_token_env = token_env.clone().unwrap_or_default();
            if let Some(reference) = credential_ref {
                view.bearer_credential_ref.clone_from(reference);
            }
        }
        Some(McpServerAuth::Oauth2 {
            client_id,
            client_secret_ref,
            client_secret_env,
            authorization_endpoint,
            token_endpoint,
            scopes,
            credential_ref,
        }) => {
            view.mode = "oauth2";
            view.oauth_client_id = client_id.clone().unwrap_or_default();
            view.oauth_client_secret_env = client_secret_env.clone().unwrap_or_default();
            if let Some(reference) = client_secret_ref {
                view.oauth_client_secret_ref.clone_from(reference);
            }
            view.oauth_authorization_endpoint.clone_from(authorization_endpoint);
            view.oauth_token_endpoint.clone_from(token_endpoint);
            view.oauth_scopes = scopes.join(" ");
            view.oauth_credential_ref.clone_from(credential_ref);
        }
    }

    view.bearer_credential_configured = matches!(
        credentials.get(&view.bearer_credential_ref),
        Some(CredentialRecord::Bearer { .. })
    );
    view.oauth_client_secret_configured = matches!(
        credentials.get(&view.oauth_client_secret_ref),
        Some(CredentialRecord::Oauth2ClientSecret { .. })
    );
    if let Some(CredentialRecord::Oauth2Token { expires_at, .. }) = credentials.get(&view.oauth_credential_ref) {
        view.oauth_connected = true;
        view.oauth_expires_at = expires_at.clone();
    }

    view.offer_secret_controls(server_name, auth);

    if let Some(detected) = detected
        && detected.server_name == server_name
    {
        match &detected.result {
            DetectedAuthResult::NoAuthRequired => view.mode = "none",
            DetectedAuthResult::Discovered {
                authorization_endpoint,
                token_endpoint,
                scopes,
            } => {
                view.mode = "oauth2";
                view.oauth_authorization_endpoint.clone_from(authorization_endpoint);
                view.oauth_token_endpoint.clone_from(token_endpoint);
                view.oauth_scopes.clone_from(scopes);
            }
        }
    }

    view
}

/// Every `credential_ref`/`client_secret_ref` name `config`'s `[[mcp_server]]` entries reference,
/// resolved against `store`: `render()` never itself touches the
/// filesystem, so this is gathered once per render, mirroring how `readiness`
/// (`McpClientSet::readiness`) is already gathered by every caller before calling `render()`. A
/// reference with no stored record at all is simply absent from the returned map, never an error:
/// [`mcp_server_auth_view`] reads absence as "not configured", the correct display for an
/// `oauth2` entry an operator has not yet connected.
pub(crate) async fn credential_snapshot(
    store: &Arc<FsCredentialStore>,
    config: &WebConfig,
) -> HashMap<String, CredentialRecord> {
    let references: Vec<String> = config
        .mcp_servers
        .iter()
        .flat_map(McpServerConfig::credential_references)
        .collect();
    if references.is_empty() {
        return HashMap::new();
    }
    let store = Arc::clone(store);
    tokio::task::spawn_blocking(move || {
        references
            .into_iter()
            .filter_map(|reference| {
                let record = store.get(&reference).ok().flatten()?;
                Some((reference, record))
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

// ---------------------------------------------------------------------
// `GET /settings/`'s `?oauth_connected=`/`?oauth_error=` banner
// ---------------------------------------------------------------------

/// `GET /settings/`'s optional query parameters: the `OAuth2`
/// connect/callback flow's own redirect target, carrying its outcome back as a plain query
/// string rather than any persisted state.
#[derive(Debug, Default, Deserialize)]
pub struct SettingsQuery {
    #[serde(default)]
    pub oauth_connected: Option<String>,
    #[serde(default)]
    pub oauth_error: Option<String>,
    #[serde(default)]
    pub server: Option<String>,
}

/// Maps one `?oauth_error=` reason to a short, human sentence fragment: every reason
/// `routes::settings_oauth::callback` actually redirects with, plus a generic fallback for a
/// provider-supplied `error` value passed straight through or any other reason this page has not
/// seen before. Never fails to render something: an unrecognised reason is not a rendering error.
fn oauth_error_detail(reason: &str) -> &'static str {
    match reason {
        "invalid_state" => "the connection attempt could not be verified; it may have expired or already been used",
        "missing_parameters" => "the provider did not return the expected authorisation response",
        "server_removed" => "this MCP server is no longer configured",
        "client_secret_missing" => "the stored client secret could not be read",
        "token_exchange_failed" => "the provider rejected the token exchange",
        _ => "the connection failed",
    }
}

/// The transient, query-parameter-driven notice/error `GET /settings/` shows after an `OAuth2`
/// connect attempt redirects back here: never persisted, so it does not reappear on the next plain
/// navigation once these query parameters are gone. Returns `(notice, error)`, at most one of
/// which is ever `Some` (`?oauth_connected=` and `?oauth_error=` are mutually exclusive outcomes
/// of the same redirect).
pub(crate) fn settings_banner(query: &SettingsQuery) -> (Option<String>, Option<String>) {
    if let Some(server) = &query.oauth_connected {
        return (Some(format!("Connected to {server}.")), None);
    }
    if let Some(reason) = &query.oauth_error {
        let server = query.server.as_deref().unwrap_or("the server");
        let detail = oauth_error_detail(reason);
        return (None, Some(format!("Could not connect {server}: {detail}.")));
    }
    (None, None)
}

// ---------------------------------------------------------------------
// Pasted-secret extraction and credential garbage collection
// ---------------------------------------------------------------------

/// Strips a pasted secret value from a `PATCH /settings/` `auth` object, if present, before it
/// ever reaches [`crate::config_writer::WebConfigDocument`]'s JSON-to-TOML bridge (which has no
/// schema notion of a request-only `credential_value`/`client_secret_value` field at all):
/// `credential_value` pairs with `mode = "bearer"`'s own `credential_ref`, `client_secret_value`
/// pairs with `mode = "oauth2"`'s own `client_secret_ref`. A no-op when `patch` carries no `auth`
/// object, or an `auth` object with neither key. Returns the `(credential_ref, record)` pairs
/// still to be written to the credential store, each stamped with `now` as its own `updated_at`.
///
/// Takes the entry's own JSON map directly, rather than `routes::settings`'s private `Edit` enum,
/// so this module never needs visibility into that enum's variants: the caller passes the patch of
/// an `Edit::PatchMcpServer`, or the whole submitted entry of an add, straight through.
///
/// # Errors
///
/// Returns a [`PastedSecretRefusal`] rather than dropping or guessing: a value without the reference it would be
/// stored under, a reference the store cannot hold, an empty value, or a pasted value beside the variable that would
/// otherwise supply it (`token_env` or `client_secret_env`): a value and its variable together are refused, naming
/// both.
pub(crate) fn extract_pasted_secrets_from_patch(
    patch: &mut serde_json::Map<String, Value>,
    now: &str,
) -> Result<Vec<(String, CredentialRecord)>, PastedSecretRefusal> {
    let Some(Value::Object(auth)) = patch.get_mut("auth") else {
        return Ok(Vec::new());
    };
    let mut writes = Vec::new();
    if let Some(value) = pasted(auth, "credential_value", "credential_ref")? {
        if auth.contains_key("token_env") {
            return Err(PastedSecretRefusal::BesideVariable {
                value: "a pasted bearer token",
                variable: "token_env",
            });
        }
        writes.push((
            value.0,
            CredentialRecord::Bearer {
                value: value.1,
                updated_at: now.to_owned(),
            },
        ));
    }
    if let Some(value) = pasted(auth, "client_secret_value", "client_secret_ref")? {
        if auth.contains_key("client_secret_env") {
            return Err(PastedSecretRefusal::BesideVariable {
                value: "a pasted client secret",
                variable: "client_secret_env",
            });
        }
        writes.push((
            value.0,
            CredentialRecord::Oauth2ClientSecret {
                value: value.1,
                updated_at: now.to_owned(),
            },
        ));
    }
    Ok(writes)
}

/// Removes `value_field` from `auth` and pairs it with the reference `reference_field` names, or `None` when no
/// value was pasted.
fn pasted(
    auth: &mut serde_json::Map<String, Value>,
    value_field: &'static str,
    reference_field: &'static str,
) -> Result<Option<(String, String)>, PastedSecretRefusal> {
    let Some(Value::String(value)) = auth.remove(value_field) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Err(PastedSecretRefusal::Empty { field: value_field });
    }
    let Some(Value::String(reference)) = auth.get(reference_field) else {
        return Err(PastedSecretRefusal::WithoutReference {
            reference: reference_field,
        });
    };
    if !crate::credential_store::can_hold(reference) {
        return Err(PastedSecretRefusal::UnstorableReference {
            reference: reference_field,
            name: reference.clone(),
        });
    }
    Ok(Some((reference.clone(), value)))
}

/// Why a pasted secret was refused. Never carries the value itself.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum PastedSecretRefusal {
    #[error("a pasted secret arrived without {reference}, the credential store name to keep it under")]
    WithoutReference { reference: &'static str },
    #[error(
        "{reference} {name:?} cannot name a stored secret: it must not be empty or contain /, \\, or ..; rename the \
         server, or choose another name"
    )]
    UnstorableReference { reference: &'static str, name: String },
    #[error("{field} is empty; paste the secret, or leave the field empty to keep the one already stored")]
    Empty { field: &'static str },
    #[error("{value} and {variable} were both given; keep the pasted value or the environment variable name, not both")]
    BesideVariable {
        value: &'static str,
        variable: &'static str,
    },
}

impl axum::response::IntoResponse for PastedSecretRefusal {
    fn into_response(self) -> axum::response::Response {
        #[derive(serde::Serialize)]
        struct Body {
            error: &'static str,
            detail: String,
        }
        (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(Body {
                error: "settings/pasted-secret-refused",
                detail: self.to_string(),
            }),
        )
            .into_response()
    }
}

/// Writes every `(credential_ref, record)` pair [`extract_pasted_secrets_from_patch`] pulled out
/// of an edit. This runs only after `web.toml`'s own write has already succeeded, so a failure here
/// is not the edit failing: it is returned as a message for the page to show, naming the
/// references that could not be written, so the operator knows to paste again rather than finding
/// out from a failed connection later.
pub(crate) async fn persist_pasted_secrets(
    credential_store: &Arc<FsCredentialStore>,
    writes: Vec<(String, CredentialRecord)>,
) -> Option<String> {
    if writes.is_empty() {
        return None;
    }
    let store = Arc::clone(credential_store);
    let failed = tokio::task::spawn_blocking(move || {
        let mut failed = Vec::new();
        for (credential_ref, record) in writes {
            if let Err(source) = store.set(&credential_ref, &record) {
                tracing::warn!(
                    credential_ref = %credential_ref,
                    error = %source,
                    "failed to persist a pasted secret to the credential store after a successful mcp_server edit"
                );
                failed.push(credential_ref);
            }
        }
        failed
    })
    .await
    .unwrap_or_else(|_join_error| vec!["(unknown)".to_owned()]);
    (!failed.is_empty()).then(|| {
        format!(
            "The entry was saved, but the pasted secret could not be stored under {}; the log has the cause. Check \
             the credentials folder beside web.toml can be written, then paste it again.",
            failed.join(", ")
        )
    })
}

/// Every `credential_ref`/`client_secret_ref` named anywhere across `entries`, deduplicated. Used
/// to diff the `[[mcp_server]]` array before and after an edit: a reference absent from the
/// *after* set anywhere in the document, not just on the specific entry the edit touched, is
/// garbage-collected, so a renamed or otherwise restructured entry that keeps the same
/// `credential_ref` is never disturbed, and only a reference that has genuinely become orphaned is
/// ever deleted.
pub(crate) fn credential_references_in(entries: &[McpServerConfig]) -> BTreeSet<String> {
    entries
        .iter()
        .flat_map(McpServerConfig::credential_references)
        .collect()
}

/// Deletes every credential-store record named in `removed_refs`, logging (never failing the
/// caller) on an individual deletion failure: this runs only after `web.toml`'s own write has
/// already succeeded, so a credential-store hiccup here must never be reported back as the edit
/// itself having failed. [`CredentialStore::delete`] is already idempotent, so a reference that
/// was configured but never actually populated (an `oauth2` entry an operator added but never
/// connected) deletes cleanly as a no-op.
pub(crate) async fn garbage_collect_credentials(credential_store: &Arc<FsCredentialStore>, removed_refs: Vec<String>) {
    if removed_refs.is_empty() {
        return;
    }
    let store = Arc::clone(credential_store);
    let _ = tokio::task::spawn_blocking(move || {
        for credential_ref in removed_refs {
            if let Err(source) = store.delete(&credential_ref) {
                tracing::warn!(
                    credential_ref = %credential_ref,
                    error = %source,
                    "failed to garbage-collect an orphaned credential-store record after an mcp_server edit"
                );
            }
        }
    })
    .await;
}

#[cfg(test)]
#[path = "settings_credentials_test.rs"]
mod tests;
