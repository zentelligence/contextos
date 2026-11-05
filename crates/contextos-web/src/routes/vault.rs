//! Vault content routes:
//! `GET /{{vault_name}}/{{relative-path}}` and its directory form, plus the
//! `POST`/`PATCH`/`PUT`/`DELETE` mutation dispatch in
//! [`mutate`](super::vault_mutations). Also the bare HTTP server root's own
//! [`get_server_root`], a `contextos-web`-only convenience.

use std::path::PathBuf;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::config;
use crate::mcp_client::{McpCallError, McpClient, McpClientSet};
use crate::rendering::shell::{self, ActiveScreen};
use crate::rendering::{base, page};
use crate::screen_routes::{load_link_routes, route_for_note};

/// Shared state a vault content route needs: the connected MCP sessions,
/// the name of the `[[mcp_server]]` entry every vault operation is issued
/// against (`web.toml`'s first configured entry is always the local
/// `contextos-mcp` instance), and `web.toml`'s own path (read fresh per
/// request for the nav shell's `[server.ui]` appearance; this route never
/// writes it).
#[derive(Clone)]
pub struct VaultRoutesState {
    pub clients: Arc<McpClientSet>,
    pub web_config_path: Arc<PathBuf>,
    /// The operator's local date, for the Overview's Done today tile.
    pub vault_time: Arc<crate::vault_time::VaultTime>,
}

impl From<super::CommonRoutesState> for VaultRoutesState {
    fn from(value: super::CommonRoutesState) -> Self {
        Self {
            clients: value.clients,
            web_config_path: value.web_config_path,
            vault_time: value.vault_time,
        }
    }
}

impl VaultRoutesState {
    fn client(&self) -> Result<Arc<McpClient>, Box<Response>> {
        crate::routes::primary_session(&self.clients)
    }
}

pub(crate) fn server_not_configured() -> Response {
    error_response(StatusCode::INTERNAL_SERVER_ERROR, "mcp/server-not-configured")
}

pub(crate) fn error_response(status: StatusCode, code: &'static str) -> Response {
    #[derive(Serialize)]
    struct Body {
        error: &'static str,
    }
    (status, Json(Body { error: code })).into_response()
}

pub(crate) fn not_found() -> Response {
    error_response(StatusCode::NOT_FOUND, "route/not-found")
}

pub(crate) fn unreachable() -> Response {
    error_response(StatusCode::BAD_GATEWAY, "mcp/unreachable")
}

#[derive(Debug, Default, Deserialize)]
pub struct VaultQuery {
    #[serde(default)]
    pub raw: Option<String>,
    #[serde(default)]
    pub view: Option<String>,
    /// The Command Centre entity switcher's own selection: absent means
    /// "All entities", the always-available default.
    #[serde(default)]
    pub entity: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FileInfoResult {
    kind: String,
}

enum Kind {
    File,
    Dir,
}

async fn resolve_kind(client: &McpClient, vault_name: &str, trimmed: &str) -> Result<Kind, Box<Response>> {
    let target = if trimmed.is_empty() {
        format!("{vault_name}://.")
    } else {
        format!("{vault_name}://{trimmed}")
    };
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(target));
    match client.call_tool("fs_get_file_info".to_owned(), args).await {
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => Err(Box::new(unreachable())),
        Ok(result) if result.is_error == Some(true) => Err(Box::new(not_found())),
        Ok(result) => match result.into_typed::<FileInfoResult>() {
            Ok(info) if info.kind == "dir" => Ok(Kind::Dir),
            Ok(_) => Ok(Kind::File),
            Err(_) => Err(Box::new(not_found())),
        },
    }
}

/// The outcome of fetching a file's content via `fs_attach_file`: either
/// its MCP-resolved content type and body, still unconverted to a
/// [`Response`] so a caller can inspect the content type first (the
/// app-serving route's embed composition, `routes::apps`, needs to know
/// whether a fetched app entry is `text/html` before deciding how to
/// respond), or [`NotFound`](Attached::NotFound).
pub(crate) enum Attached {
    Found { content_type: String, body: AttachedBody },
    NotFound,
}

/// A fetched file's body, still typed as text or bytes per
/// `fs_attach_file`'s own detection, before either is turned into a
/// response body.
pub(crate) enum AttachedBody {
    Text(String),
    Bytes(Vec<u8>),
}

/// Builds the standard passthrough response for a fetched file: its
/// resolved content type and body, verbatim. The one response shape every
/// caller that does not need to inspect the content first already wants.
pub(crate) fn attached_response(content_type: String, body: AttachedBody) -> Response {
    match body {
        AttachedBody::Text(text) => (StatusCode::OK, [("content-type", content_type)], text).into_response(),
        AttachedBody::Bytes(bytes) => (StatusCode::OK, [("content-type", content_type)], bytes).into_response(),
    }
}

/// Calls `fs_attach_file` on `vault_path` and converts the result into its
/// MCP-resolved content type and body, reusing `fs_attach_file`'s existing
/// binary/text detection rather than this crate inventing a second one.
/// Shared by `routes::file_response` (a file answered as it is stored) and the app-serving route (`routes::apps`), which
/// fetches a registered app's own bundle files the identical way, since
/// both are "serve this vault file's bytes with its real content type"
/// (still an MCP tool call, never a direct filesystem read).
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the MCP transport itself
/// fails.
pub(crate) async fn fetch_attached(client: &McpClient, vault_path: String) -> Result<Attached, McpCallError> {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(vault_path));
    let result = client.call_tool("fs_attach_file".to_owned(), args).await?;
    if result.is_error == Some(true) {
        return Ok(Attached::NotFound);
    }
    for block in &result.content {
        let rmcp::model::ContentBlock::Resource(embedded) = block else {
            continue;
        };
        match &embedded.resource {
            rmcp::model::ResourceContents::TextResourceContents { mime_type, text, .. } => {
                let content_type = mime_type
                    .clone()
                    .unwrap_or_else(|| "text/plain; charset=utf-8".to_owned());
                return Ok(Attached::Found {
                    content_type,
                    body: AttachedBody::Text(text.clone()),
                });
            }
            rmcp::model::ResourceContents::BlobResourceContents { mime_type, blob, .. } => {
                use base64::Engine;
                if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(blob) {
                    let content_type = mime_type
                        .clone()
                        .unwrap_or_else(|| "application/octet-stream".to_owned());
                    return Ok(Attached::Found {
                        content_type,
                        body: AttachedBody::Bytes(bytes),
                    });
                }
            }
            // `ResourceContents` is `#[non_exhaustive]`: `fs_attach_file`
            // only ever returns the two variants matched above, but a
            // future third variant must fail closed here (fall through to
            // `NotFound` below), never panic.
            _ => {}
        }
    }
    Ok(Attached::NotFound)
}

pub(crate) fn extension_of(relative_path: &str) -> &str {
    relative_path
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .map_or("", |(_, ext)| ext)
}

pub(crate) fn html_response(html: String) -> Response {
    (StatusCode::OK, [("content-type", "text/html; charset=utf-8")], html).into_response()
}

/// The content types a browser can run script from when one is opened directly.
fn can_run_script(content_type: &str) -> bool {
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim()
        .to_ascii_lowercase();
    // HTML, and every XML document type (`*/xml`, any `+xml` suffix, and the XSL types), which a browser renders as
    // XML and so runs XHTML-namespaced script in.
    essence == "text/html"
        || essence.ends_with("/xml")
        || essence.ends_with("+xml")
        || matches!(essence.as_str(), "text/xsl" | "application/xsl")
}

/// A vault file's bytes, served so that nothing in the file can act in the interface's origin: the browser never
/// guesses another type for it, and a type that can run script (SVG, HTML, XML) is sandboxed with no script and no
/// network. A vault file opened directly can come from anywhere (a clipping, a synced folder, an agent), and the
/// interface's origin can call every MCP tool. Apps are served by their own routes and are not affected.
pub(crate) fn asset_response(content_type: String, body: AttachedBody) -> Response {
    let sandboxed = can_run_script(&content_type);
    let mut response = attached_response(content_type, body);
    let headers = response.headers_mut();
    headers.insert(
        axum::http::header::X_CONTENT_TYPE_OPTIONS,
        axum::http::HeaderValue::from_static("nosniff"),
    );
    if sandboxed {
        headers.insert(
            axum::http::header::CONTENT_SECURITY_POLICY,
            axum::http::HeaderValue::from_static(
                "sandbox; default-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src data:",
            ),
        );
    }
    response
}

/// A full-navigation `Redirect` for a plain browser `GET`, or the HTMX-native `HX-Redirect` header on a `200` for
/// an `hx-request`, matching `routes::journal`'s and `routes::entity_docs`'s own identical local helpers (this
/// crate's own per-module convention rather than one shared function).
fn redirect_response(target: &str, is_hx_request: bool) -> Response {
    if is_hx_request {
        (StatusCode::OK, [("HX-Redirect", target.to_owned())], String::new()).into_response()
    } else {
        Redirect::to(target).into_response()
    }
}

/// `GET /` (the bare HTTP server root): there is no page at this literal
/// path, since every vault-content and app route always
/// carries `vault_name` as its own first segment, so this redirects to
/// somewhere that does resolve, mirroring the nav shell's own "no page-
/// specific vault yet, fall back to the first configured one" behaviour
/// ([`shell::build_nav`]'s `nav_target_vault`): the first configured
/// vault's own root, or `/settings/` when no vault is configured at all (or
/// the primary MCP session cannot be reached to ask).
pub async fn get_server_root(State(state): State<VaultRoutesState>) -> Response {
    let Ok(client) = state.client() else {
        return Redirect::to("/settings/").into_response();
    };
    let target = shell::configured_vault_names(&client)
        .await
        .unwrap_or_default()
        .into_iter()
        .next()
        .map_or_else(
            || "/settings/".to_owned(),
            |name| crate::urls::VaultUrls::from(&*name).root(),
        );
    Redirect::to(&target).into_response()
}

/// `GET /{{vault_name}}/` (root directory).
pub async fn get_root(
    State(state): State<VaultRoutesState>,
    Path(vault_name): Path<String>,
    Query(query): Query<VaultQuery>,
    headers: HeaderMap,
) -> Response {
    Box::pin(get_dispatch(state, vault_name, String::new(), query, headers)).await
}

/// `GET /{{vault_name}}/{{relative-path}}`.
pub async fn get_path(
    State(state): State<VaultRoutesState>,
    Path((vault_name, relative_path)): Path<(String, String)>,
    Query(query): Query<VaultQuery>,
    headers: HeaderMap,
) -> Response {
    Box::pin(get_dispatch(state, vault_name, relative_path, query, headers)).await
}

/// Everything a rendered vault page's own shell assembly needs, bundled so
/// the render functions below stay within a reasonable argument count as
/// the shell grows more per-page chrome (appearance, the sidebar's own
/// wide/narrow state, the footer's MCP-servers status) beyond the original
/// `client`/`vault_name` pair.
struct ShellContext<'a> {
    client: &'a McpClient,
    clients: &'a McpClientSet,
    vault_name: &'a str,
    appearance: &'a config::Appearance,
    sidebar_wide: bool,
    vault_time: &'a crate::vault_time::VaultTime,
}

async fn get_dispatch(
    state: VaultRoutesState,
    vault_name: String,
    relative_path: String,
    query: VaultQuery,
    headers: HeaderMap,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    let trimmed = relative_path.trim_end_matches('/');
    // `web.toml`'s appearance read is a small blocking filesystem read and
    // parse, and does not depend on `resolve_kind`'s MCP round trip (or vice
    // versa): moving it off the async executor thread via `spawn_blocking`
    // and running it concurrently with `resolve_kind` avoids both blocking
    // the runtime and paying the two costs back to back.
    let web_config_path = Arc::clone(&state.web_config_path);
    let (appearance, kind) = tokio::join!(
        async move {
            tokio::task::spawn_blocking(move || config::current_appearance(&web_config_path))
                .await
                .unwrap_or_default()
        },
        Box::pin(resolve_kind(&client, &vault_name, trimmed)),
    );
    let kind = match kind {
        Ok(kind) => kind,
        Err(response) => return *response,
    };
    let ctx = ShellContext {
        client: &client,
        clients: &state.clients,
        vault_name: &vault_name,
        appearance: &appearance,
        sidebar_wide: crate::routes::sidebar_wide_from_headers(&headers),
        vault_time: &state.vault_time,
    };
    let is_hx_request = headers.contains_key("hx-request");
    match kind {
        Kind::Dir if trimmed.is_empty() => render_directory(&ctx, query.entity.as_deref()).await,
        // A folder is never a page of its own here: the section that owns it shows it.
        Kind::Dir => {
            let routes = load_link_routes(&client, &vault_name).await;
            match routes.folder_route(trimmed) {
                Some(target) => redirect_response(&with_entity(target, query.entity.as_deref()), is_hx_request),
                None => missed(&vault_name, trimmed, &headers),
            }
        }
        Kind::File => render_file(&ctx, trimmed, &query, &headers).await,
    }
}

/// `target` carrying the request's `?entity=` selection, when it had one.
fn with_entity(mut target: String, entity: Option<&str>) -> String {
    if let Some(entity) = entity {
        target.push_str(if target.contains('?') { "&entity=" } else { "?entity=" });
        target.push_str(&crate::inbox_outbox::encode_query_value(entity));
    }
    target
}

/// A note or folder that reached this route although no section shows it: every link the interface builds goes
/// through the route table, so this means a link was missed somewhere. Logged, with the page it came from, so the
/// missed link can be found, and answered as not found rather than shown outside every section.
fn missed(vault_name: &str, path: &str, headers: &HeaderMap) -> Response {
    let referer = headers
        .get(axum::http::header::REFERER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("none");
    tracing::warn!(
        vault = vault_name,
        path,
        referer,
        "a page or folder no section shows was requested through the generic vault route; a link was missed"
    );
    not_found()
}

/// The current directory page's own path, carrying no `?entity=` query
/// parameter, for the entity switcher's own href composition
/// ([`shell::build_nav`]'s `entity_base_path`): always trailing-slashed,
/// matching every other directory link this crate builds (`shell.rs`'s own
/// `directory_entries`).
fn directory_entity_base_path(vault_name: &str, trimmed: &str) -> String {
    crate::urls::VaultUrls::from(vault_name).folder(trimmed)
}

/// The vault root: the Command Centre Overview dashboard. No other folder is rendered here.
async fn render_directory(ctx: &ShellContext<'_>, entity: Option<&str>) -> Response {
    let vault_name = ctx.vault_name;
    let trimmed = "";
    let entity_base_path = directory_entity_base_path(vault_name, trimmed);
    let known_mcp_servers = ctx.clients.names();
    let nav = shell::build_nav(
        ctx.client,
        ActiveScreen::Vault,
        Some(vault_name),
        Some(trimmed),
        entity,
        Some(&entity_base_path),
        &known_mcp_servers,
    );
    // The vault root's own discrete Overview dashboard (`crate::routes::overview::render_html`'s own doc comment).
    let overview = super::overview::render_html(ctx.client, vault_name, entity, ctx.vault_time);
    let (mut nav, mcp_status, overview_html) =
        tokio::join!(Box::pin(nav), shell::mcp_status(ctx.clients), Box::pin(overview));
    nav.appearance = ctx.appearance.clone();
    nav.sidebar_wide = ctx.sidebar_wide;
    nav.mcp_status = mcp_status;
    html_response(page::render_page(&nav, trimmed, &overview_html))
}

async fn render_file(ctx: &ShellContext<'_>, relative_path: &str, query: &VaultQuery, headers: &HeaderMap) -> Response {
    let is_hx_request = headers.contains_key("hx-request");
    let vault_name = ctx.vault_name;
    let extension = extension_of(relative_path).to_ascii_lowercase();
    let Ok(raw) = super::file_response::Raw::try_from(query.raw.as_deref()) else {
        return super::file_response::malformed_query();
    };
    if raw.0 {
        return super::file_response::stored(ctx.client, vault_name, relative_path, raw).await;
    }

    match extension.as_str() {
        // A base's view tabs switch it in place, wherever the base is shown: an htmx request gets the view alone.
        "base" if is_hx_request => {
            match base::render_view(ctx.client, vault_name, relative_path, query.view.as_deref()).await {
                Ok(fragment) => html_response(fragment),
                Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => unreachable(),
            }
        }
        // A page (a note, a base, a canvas, a diagram) is never shown here: the section that owns it shows it, and a
        // page no section owns, a folder's managed `index.md` included, means a link was missed somewhere.
        "md" | "base" | "canvas" | "mermaid" => match route_for_note(ctx.client, vault_name, relative_path).await {
            Some(target) => redirect_response(&with_entity(target, query.entity.as_deref()), is_hx_request),
            None => missed(vault_name, relative_path, headers),
        },
        _ => super::file_response::stored(ctx.client, vault_name, relative_path, raw).await,
    }
}

#[cfg(test)]
#[path = "vault_test.rs"]
mod tests;
