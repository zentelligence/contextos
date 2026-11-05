//! App registry and app-serving routes: `GET
//! /{{vault_name}}/apps/`, `GET /{{vault_name}}/apps/{{slug}}/...`, and
//! the operator-initiated rescan `POST /{{vault_name}}/apps/rescan`.
//!
//! Discovery ([`crate::apps::discover_apps`]) runs on a vault's first
//! registry-route request and is cached in [`AppRoutesState`] thereafter,
//! mirroring the vault content routes' own established pattern of
//! resolving a vault's identity per request through an MCP tool call
//! rather than this crate maintaining a second, locally pre-loaded vault
//! registry; there is exactly one discovery code path, exercised
//! identically by a real request and by this module's own tests.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Redirect, Response};

use axum::http::{HeaderMap, StatusCode};

use crate::apps::{self, AppKind, AppStatus, RegisteredApp};
use crate::apps_favourites::AppsFavouritesState;
use crate::config;
use crate::mcp_client::{McpCallError, McpClient, McpClientSet};
use crate::rendering::page;
use crate::rendering::shell::{self, ActiveScreen};
use crate::rendering::title_action::{self, TitleAction};
use crate::routes::EntityQuery;
use crate::routes::vault::{
    Attached, AttachedBody, attached_response, fetch_attached, html_response, not_found, server_not_configured,
    unreachable,
};
use crate::view_favourites;

/// Shared state an app route needs: the connected MCP sessions, the
/// `[[mcp_server]]` name every vault operation is issued against, the
/// full configured `[[mcp_server]]` name list (a manifest's own
/// `mcp_servers` allow-list is validated against this), the per-vault
/// discovered-app cache, and `web.toml`'s own path (read fresh per
/// request for the nav shell's `[server.ui]` appearance; this route
/// never writes it).
#[derive(Clone)]
pub struct AppRoutesState {
    clients: Arc<McpClientSet>,
    /// Each vault's discovered apps, with the configured server names they were validated against: a list validated
    /// against different names is discovered again, so adding or removing a server in `/settings/` counts at once.
    registry: Arc<RwLock<HashMap<String, DiscoveredApps>>>,
    web_config_path: Arc<PathBuf>,
}

/// One vault's discovered apps and the configured server names they were validated against.
type DiscoveredApps = (Vec<String>, Vec<RegisteredApp>);

#[derive(Clone, Copy)]
enum RegistryError {
    ServerNotConfigured,
    Unreachable,
}

impl From<McpCallError> for RegistryError {
    fn from(_source: McpCallError) -> Self {
        Self::Unreachable
    }
}

impl AppRoutesState {
    #[must_use]
    pub fn new(clients: Arc<McpClientSet>, web_config_path: Arc<PathBuf>) -> Self {
        Self {
            clients,
            registry: Arc::new(RwLock::new(HashMap::new())),
            web_config_path,
        }
    }

    fn client(&self) -> Result<Arc<McpClient>, RegistryError> {
        self.clients.primary().ok_or(RegistryError::ServerNotConfigured)
    }

    fn cached(&self, vault_name: &str, server_names: &[String]) -> Option<Vec<RegisteredApp>> {
        let registry = self.registry.read().ok()?;
        let (validated_against, apps) = registry.get(vault_name)?;
        (validated_against.as_slice() == server_names).then(|| apps.clone())
    }

    fn store(&self, vault_name: &str, server_names: Vec<String>, apps: Vec<RegisteredApp>) {
        if let Ok(mut guard) = self.registry.write() {
            guard.insert(vault_name.to_owned(), (server_names, apps));
        }
    }

    /// Returns `vault_name`'s app list, discovering it first if this is
    /// the first request for that vault since startup or the last
    /// [`rescan`](Self::rescan).
    async fn apps_for(&self, vault_name: &str) -> Result<Vec<RegisteredApp>, RegistryError> {
        if let Some(cached) = self.cached(vault_name, &self.clients.names()) {
            return Ok(cached);
        }
        self.rescan(vault_name).await
    }

    /// Forces a fresh discovery pass for `vault_name`, replacing whatever was cached. Not
    /// `pub`: only this module's own route handlers call it.
    async fn rescan(&self, vault_name: &str) -> Result<Vec<RegisteredApp>, RegistryError> {
        let client = self.client()?;
        let server_names = self.clients.names();
        let discovered = apps::discover_apps(&client, vault_name, &server_names).await?;
        self.store(vault_name, server_names, discovered.clone());
        Ok(discovered)
    }
}

fn not_yet_supported(slug: &str) -> Response {
    #[derive(serde::Serialize)]
    struct Body<'a> {
        error: &'static str,
        slug: &'a str,
    }
    (
        StatusCode::NOT_FOUND,
        axum::Json(Body {
            error: "app/not-yet-supported",
            slug,
        }),
    )
        .into_response()
}

async fn ensure_vault(client: &McpClient, vault_name: &str) -> Result<(), Box<Response>> {
    match apps::vault_exists(client, vault_name).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(Box::new(not_found())),
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => Err(Box::new(unreachable())),
    }
}

fn registry_error_response(error: RegistryError) -> Response {
    match error {
        RegistryError::ServerNotConfigured => server_not_configured(),
        RegistryError::Unreachable => unreachable(),
    }
}

struct AppListEntry {
    name: String,
    slug: String,
    kind_label: &'static str,
    /// The card's own icon-avatar mask name, derived from `kind` alone (never a `manifest.toml` field: the
    /// decision recorded above `list_entry`'s own body).
    kind_icon: &'static str,
    servable: bool,
    /// `None` for an embed (`Js`/`Htmx`) app: the "Open" link is a normal
    /// same-tab navigation, so no `target` attribute is rendered at all.
    target_attr: Option<&'static str>,
    /// The mock's `<dl>` "Opens" row (`outbox/2026-09-04-contextos-web-
    /// mock.html`'s `tpl-apps`): a human label for `target_attr`, kept
    /// distinct from it since `target_attr` is an HTML attribute value and
    /// this is prose.
    opens_label: &'static str,
    /// The star toggle's own rendered markup ([`crate::rendering::favourite_toggle::render`]), pre-built here
    /// rather than composed in the template: it is identical across the list card and (via
    /// [`set_favourite`]'s own HTMX response) the button's freshly swapped-in replacement, so both render it
    /// through this one function.
    favourite_toggle_html: String,
}

#[derive(Template)]
#[template(path = "apps_list.html")]
struct AppsListTemplate {
    /// Every URL the template links to within the vault.
    urls: crate::urls::VaultUrls,
    apps: Vec<AppListEntry>,
}

fn list_entry(vault_name: &str, app: &RegisteredApp, favourites: &AppsFavouritesState) -> AppListEntry {
    let favourite = favourites.is_favourite(&app.slug);
    let favourite_toggle_html = crate::rendering::favourite_toggle::render(
        &crate::urls::VaultUrls::from(vault_name).app(&app.slug),
        &app.name,
        favourite,
    );
    AppListEntry {
        name: app.name.clone(),
        slug: app.slug.clone(),
        kind_label: match app.kind {
            AppKind::Js => "js",
            AppKind::Spa => "spa",
            AppKind::Htmx => "htmx",
        },
        // No dedicated icon per kind exists in the shipped icon set; both derivations reuse an already-defined
        // icon rather than adding a new one purely for this distinction. `Spa` reuses the "opens externally"
        // icon already carried by the "Open" link's own new-tab semantics; `Js`/`Htmx` reuse the Apps group's
        // own heading icon, matching their in-shell embed semantics.
        kind_icon: match app.kind {
            AppKind::Js | AppKind::Htmx => "apps",
            AppKind::Spa => "arrow-up-right",
        },
        servable: app.status == AppStatus::Supported,
        // Serving mode is derived from `kind` alone: `Js`/`Htmx` compose
        // within the shell (no `target` attribute needed at all, since
        // the link is a normal same-tab navigation to a shell-composed
        // page); `Spa` still opens in a new tab.
        target_attr: match app.kind {
            AppKind::Js | AppKind::Htmx => None,
            AppKind::Spa => Some("_blank"),
        },
        opens_label: match app.kind {
            AppKind::Js | AppKind::Htmx => "inline",
            AppKind::Spa => "new tab",
        },
        favourite_toggle_html,
    }
}

fn render_list(vault_name: &str, registered: &[RegisteredApp], favourites: &AppsFavouritesState) -> String {
    let template = AppsListTemplate {
        urls: crate::urls::VaultUrls::from(vault_name),
        apps: registered
            .iter()
            .map(|app| list_entry(vault_name, app, favourites))
            .collect(),
    };
    template
        .render()
        .unwrap_or_else(|_| "<p>The app registry could not be rendered.</p>".to_owned())
}

/// `GET /{{vault_name}}/apps/`: lists every registered app.
/// Follows the identical `HX-Request` full-page/fragment split every other
/// full-page route in this crate uses
/// (`docs/http-routing-response-contract.md`).
pub async fn list(
    State(state): State<AppRoutesState>,
    Path(vault_name): Path<String>,
    Query(query): Query<EntityQuery>,
    headers: HeaderMap,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(error) => return registry_error_response(error),
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    match state.apps_for(&vault_name).await {
        Ok(registered) => {
            let favourites = crate::apps_favourites::load_apps_favourites(&client, &vault_name).await;
            let fragment = render_list(&vault_name, &registered, &favourites);
            if headers.contains_key("hx-request") {
                html_response(fragment)
            } else {
                // The breadcrumb suffix here is a label, not a path, so the title bar is chosen explicitly.
                let entity_base_path = crate::urls::VaultUrls::from(&*vault_name).apps();
                let (mut nav, page_title) = tokio::join!(
                    nav_with_appearance(
                        &state,
                        &client,
                        &vault_name,
                        "Apps",
                        query.entity.as_deref(),
                        &entity_base_path,
                        crate::routes::sidebar_wide_from_headers(&headers),
                    ),
                    shell::page_title(&client, &vault_name, &view_favourites::APPS),
                );
                nav.page_title = Some(page_title);
                nav.set_title_actions(title_action::render(&TitleAction::RescanApps(&vault_name)));
                html_response(page::render_page(&nav, "Apps", &fragment))
            }
        }
        Err(error) => registry_error_response(error),
    }
}

/// The nav shell and `web.toml`'s appearance, composed together: the two
/// reads are independent (the appearance load is also a blocking
/// filesystem read and parse, so it runs on `spawn_blocking` rather than
/// the async executor thread), issued concurrently with the nav shell's
/// own MCP round trips rather than after them. Shared by every full-page
/// app route ([`list`], [`compose_embed`]) that wraps its own content in
/// the shell.
async fn nav_with_appearance(
    state: &AppRoutesState,
    client: &McpClient,
    vault_name: &str,
    breadcrumb_suffix: &str,
    entity: Option<&str>,
    entity_base_path: &str,
    sidebar_wide: bool,
) -> page::NavData {
    let web_config_path = Arc::clone(&state.web_config_path);
    let server_names = state.clients.names();
    let (mut nav, appearance, mcp_status) = tokio::join!(
        Box::pin(shell::build_nav(
            client,
            ActiveScreen::Apps,
            Some(vault_name),
            Some(breadcrumb_suffix),
            entity,
            Some(entity_base_path),
            &server_names,
        )),
        async move {
            tokio::task::spawn_blocking(move || config::current_appearance(&web_config_path))
                .await
                .unwrap_or_default()
        },
        shell::mcp_status(&state.clients),
    );
    nav.appearance = appearance;
    nav.sidebar_wide = sidebar_wide;
    nav.mcp_status = mcp_status;
    nav
}

/// `POST /{{vault_name}}/apps/rescan`: re-discovers
/// `vault_name`'s `registry/apps/` on demand (the refresh icon on
/// [`list`]'s page), then redirects back to the listing so a plain HTML
/// form submission (no client-side script required) re-renders the
/// refreshed list.
pub async fn rescan(State(state): State<AppRoutesState>, Path(vault_name): Path<String>) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(error) => return registry_error_response(error),
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    match state.rescan(&vault_name).await {
        Ok(_apps) => Redirect::to(&crate::urls::VaultUrls::from(&*vault_name).apps()).into_response(),
        Err(error) => registry_error_response(error),
    }
}

/// `PATCH /{{vault_name}}/apps/{{slug}}`: sets `slug`'s own favourite flag in `.contextos/apps.toml` to the
/// request body's own explicit `favourite` value and re-renders the star toggle (an HTMX `hx-patch` targeting
/// itself, per `favourite_toggle.html`'s own doc comment). There is no plain-form fallback: HTML forms cannot
/// submit PATCH natively (`docs/http-routing-response-contract.md`'s own HTMX-specific constraint), so this
/// action depends on JavaScript, matching every other PATCH action in this crate.
pub async fn set_favourite(
    State(state): State<AppRoutesState>,
    Path((vault_name, slug)): Path<(String, String)>,
    axum::Json(update): axum::Json<crate::rendering::favourite_toggle::FavouriteUpdate>,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(error) => return registry_error_response(error),
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let registered = match state.apps_for(&vault_name).await {
        Ok(registered) => registered,
        Err(error) => return registry_error_response(error),
    };
    let Some(app) = registered.iter().find(|app| app.slug == slug) else {
        return not_found();
    };
    if let Err(error) = crate::apps_favourites::set_app_favourite(&client, &vault_name, &slug, update.favourite).await {
        tracing::warn!(vault = %vault_name, app = %slug, %error, "failed to persist an app favourite update");
        return unreachable();
    }
    html_response(crate::rendering::favourite_toggle::render(
        &crate::urls::VaultUrls::from(&*vault_name).app(&slug),
        &app.name,
        update.favourite,
    ))
}

/// `GET /{{vault_name}}/apps/{{slug}}/`.
pub async fn serve_root(
    State(state): State<AppRoutesState>,
    Path((vault_name, slug)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    serve(state, vault_name, slug, String::new(), headers).await
}

/// `GET /{{vault_name}}/apps/{{slug}}/{{*sub_path}}`.
pub async fn serve_path(
    State(state): State<AppRoutesState>,
    Path((vault_name, slug, sub_path)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    serve(state, vault_name, slug, sub_path, headers).await
}

async fn serve(
    state: AppRoutesState,
    vault_name: String,
    slug: String,
    sub_path: String,
    headers: HeaderMap,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(error) => return registry_error_response(error),
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let registered = match state.apps_for(&vault_name).await {
        Ok(registered) => registered,
        Err(error) => return registry_error_response(error),
    };
    let Some(app) = registered.into_iter().find(|app| app.slug == slug) else {
        return not_found();
    };
    if app.status != AppStatus::Supported {
        return not_yet_supported(&slug);
    }

    let trimmed = sub_path.trim_start_matches('/');
    let target = if trimmed.is_empty() {
        app.entry.clone()
    } else {
        trimmed.to_owned()
    };
    let bundle_path = format!("{vault_name}://registry/apps/{slug}/{target}");
    match fetch_attached(&client, bundle_path).await {
        Ok(Attached::Found { content_type, body }) => {
            respond_with_app_content(&state, &client, &vault_name, &app, content_type, body, &headers).await
        }
        Ok(Attached::NotFound) => match app.kind {
            // Only a true SPA is assumed to own a client-side router that
            // an unmatched path should be handed to; `Js` has no such
            // assumption, so an unmatched path is a genuine 404, not
            // silently masked by the entry document. `Htmx` is unreachable
            // here: its `NotYetSupported` status already returned above.
            AppKind::Spa => serve_entry_fallback(&client, &vault_name, &slug, &app.entry).await,
            AppKind::Js | AppKind::Htmx => not_found(),
        },
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => unreachable(),
    }
}

/// Standard SPA fallback: an unmatched sub-path
/// serves the bundle's own entry file, letting the SPA's own client-side
/// router handle it. `Spa`-only; see [`serve`].
async fn serve_entry_fallback(client: &McpClient, vault_name: &str, slug: &str, entry: &str) -> Response {
    let entry_path = format!("{vault_name}://registry/apps/{slug}/{entry}");
    match fetch_attached(client, entry_path).await {
        Ok(Attached::Found { content_type, body }) => attached_response(content_type, body),
        Ok(Attached::NotFound) => not_found(),
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => unreachable(),
    }
}

/// Dispatches a fetched app response: a `kind = "js"` app's HTML entry
/// document composes into the shell via the exact mechanism [`list`]
/// already uses (`render_page`'s `body|safe`); everything else
/// (a `js` app's own non-HTML assets, and every `spa` response) passes
/// through raw and unwrapped, unchanged from today.
async fn respond_with_app_content(
    state: &AppRoutesState,
    client: &McpClient,
    vault_name: &str,
    app: &RegisteredApp,
    content_type: String,
    body: AttachedBody,
    headers: &HeaderMap,
) -> Response {
    if app.kind == AppKind::Js
        && is_html_content_type(&content_type)
        && let AttachedBody::Text(html) = body
    {
        return compose_embed(state, client, vault_name, &app.name, &html, headers).await;
    }
    attached_response(content_type, body)
}

/// `text/html`, ignoring any `; charset=...` parameter.
fn is_html_content_type(content_type: &str) -> bool {
    content_type
        .split(';')
        .next()
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/html"))
}

/// Composes `body_html` (an embed app's own fetched entry document) into
/// the shell's main content region, the identical `build_nav`/
/// `render_page` pattern [`list`] already uses for its own full-page
/// response.
///
/// Wrapped in `app-embed` (`contextos-web.css`'s `.content .app-embed`
/// exemption) so the app's own layout gets the content region's full
/// width rather than the `.content > *` prose-reading cap every other
/// direct child of the shell's content region carries: an app is
/// arbitrary application UI, not vault-note markdown, and (unlike the
/// shell's own `card-grid`/`base-view`/`apps-list` content types) has no
/// other way to opt out of that cap.
///
/// Renders the entity switcher with no selection carried over from the
/// apps list (`serve`/`serve_root` do not read `?entity=`, deliberately: an
/// embedded app's own sub-path has no defined entity-scoping meaning, since
/// entity scoping is a registry-listing concern, not a specific app's own
/// UI); switching entity from here navigates to the vault-scoped apps
/// list, not back into this same embedded app.
async fn compose_embed(
    state: &AppRoutesState,
    client: &McpClient,
    vault_name: &str,
    title: &str,
    body_html: &str,
    headers: &HeaderMap,
) -> Response {
    let sidebar_wide = crate::routes::sidebar_wide_from_headers(headers);
    let nav = nav_with_appearance(
        state,
        client,
        vault_name,
        title,
        None,
        &crate::urls::VaultUrls::from(vault_name).apps(),
        sidebar_wide,
    )
    .await;
    let wrapped = format!("<div class=\"app-embed\">{body_html}</div>");
    html_response(page::render_page(&nav, title, &wrapped))
}

#[cfg(test)]
#[path = "apps_test.rs"]
mod tests;
