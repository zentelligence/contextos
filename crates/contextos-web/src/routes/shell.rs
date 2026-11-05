//! `POST /shell/sidebar-toggle`: flips the sidebar's own wide/narrow cookie and redirects back to whichever page
//! the toggle control was submitted from. Vault-independent and MCP-independent (no `AppState` of any kind): the
//! sidebar's wide/narrow state is chrome, not vault content, so this is the one route in this crate that touches
//! neither a vault nor a configured `[[mcp_server]]` session.
//!
//! `GET /shell/favourites`: the header Favourites panel's own data, lazy-loaded by `page_shell.html`'s `<details>`
//! on its own first open (`hx-trigger="toggle once"`) rather than fetched on every page render, since the panel
//! is usually closed. Also shell-level chrome, but this one route does need an MCP session (to list the current
//! vault's own favourited apps), so it carries `ShellFavouritesRoutesState` rather than staying state-free like
//! [`toggle`].

use std::sync::Arc;

use askama::Template;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;

use super::sidebar_wide_from_headers;
use crate::mcp_client::{McpClient, McpClientSet};
use crate::recently_accessed;
use crate::rendering::shell;
use crate::routes::SIDEBAR_COOKIE;
use crate::routes::vault::html_response;
use crate::scheduler;

/// A `Referer`-relative path is trusted only when it is a same-origin,
/// root-relative path (`starts_with('/')`, no scheme/host of its own):
/// anything else (a missing header, an absolute URL, a `javascript:` or
/// other non-path scheme) falls back to the vault-independent root rather
/// than redirecting off-site or into a malformed target.
fn redirect_target(headers: &HeaderMap) -> String {
    headers
        .get(header::REFERER)
        .and_then(|value| value.to_str().ok())
        .filter(|referer| referer.starts_with('/') && !referer.starts_with("//"))
        .unwrap_or("/")
        .to_owned()
}

/// `POST /shell/sidebar-toggle`: a plain HTML form submission (no
/// client-side script required), matching this crate's established
/// "Rescan apps" precedent (`crate::routes::apps::rescan`).
pub async fn toggle(headers: HeaderMap) -> Response {
    let currently_wide = sidebar_wide_from_headers(&headers);
    let next_value = if currently_wide { "narrow" } else { "wide" };
    let mut response = Redirect::to(&redirect_target(&headers)).into_response();
    if let Ok(cookie) = HeaderValue::from_str(&format!("{SIDEBAR_COOKIE}={next_value}; Path=/; SameSite=Lax")) {
        response.headers_mut().insert(header::SET_COOKIE, cookie);
    }
    *response.status_mut() = StatusCode::SEE_OTHER;
    response
}

/// `GET /shell/status`: the header badge and the footer's MCP servers item, freshly probed, each marked to replace
/// its counterpart on the page. The shell polls it, so a server that stops answering after a page loaded is shown
/// without a reload. It reads `tools/list` from every configured session once per call, the same probe a full page
/// render makes.
pub async fn status(State(state): State<ShellFavouritesRoutesState>) -> Response {
    html_response(crate::rendering::system_status::render_fragment(
        shell::mcp_status(&state.clients).await,
    ))
}

#[derive(Clone)]
pub struct ShellFavouritesRoutesState {
    pub clients: Arc<McpClientSet>,
}

impl From<super::CommonRoutesState> for ShellFavouritesRoutesState {
    fn from(value: super::CommonRoutesState) -> Self {
        Self { clients: value.clients }
    }
}

impl ShellFavouritesRoutesState {
    /// `None` when the primary server is not configured: the panel degrades to an apps-less render rather than
    /// an error page inside what is, from the operator's own perspective, a dropdown, not a navigation.
    fn client(&self) -> Option<Arc<McpClient>> {
        self.clients.primary()
    }
}

/// The `?vault=` query parameter [`favourites_panel`] takes: the vault whose own favourited apps to list, the
/// same [`crate::rendering::page::NavData::nav_target_vault`] fallback every other header control already
/// targets from a vault-independent page. Absent (or naming an unconfigured vault) renders the "Favourite apps"
/// section empty rather than erroring, matching every vault-scoped section's own degrade-gracefully contract.
#[derive(Debug, Default, Deserialize)]
pub struct FavouritesQuery {
    #[serde(default)]
    pub vault: Option<String>,
}

/// One row in either populated section of the panel: a plain `<a>`, `target` only ever set for a `spa`-kind
/// favourited app (mirroring [`crate::routes::apps::list_entry`]'s identical rule), always `None` for a
/// recently-accessed entry.
struct FavouriteRow {
    label: String,
    href: String,
    target: Option<&'static str>,
}

#[derive(Template)]
#[template(path = "favourites_panel.html")]
struct FavouritesPanelTemplate {
    views: Vec<FavouriteRow>,
    pages: Vec<FavouriteRow>,
    apps: Vec<FavouriteRow>,
    jobs: Vec<FavouriteRow>,
    recent: Vec<FavouriteRow>,
}

/// `GET /shell/favourites`: see this module's own doc comment.
///
pub async fn favourites_panel(
    State(state): State<ShellFavouritesRoutesState>,
    Query(query): Query<FavouritesQuery>,
    headers: HeaderMap,
) -> Response {
    let (apps, jobs) = match (query.vault.as_deref(), state.client()) {
        (Some(vault_name), Some(client)) => {
            let known_mcp_servers = state.clients.names();
            let (app_links, favourited_jobs) = tokio::join!(
                shell::favourited_app_links(&client, vault_name, &known_mcp_servers),
                scheduler::favourited_jobs(&client, vault_name, &known_mcp_servers),
            );
            let apps = app_links
                .into_iter()
                .map(|link| FavouriteRow {
                    label: link.label,
                    href: link.href,
                    target: link.target,
                })
                .collect();
            let jobs = favourited_jobs
                .into_iter()
                .map(|job| FavouriteRow {
                    label: job.name,
                    href: crate::urls::VaultUrls::from(&*vault_name).job(&job.package, &job.workflow_slug, &job.slug),
                    target: None,
                })
                .collect();
            (apps, jobs)
        }
        _ => (Vec::new(), Vec::new()),
    };
    let recent = recently_accessed::read_from_headers(&headers)
        .into_iter()
        .map(|entry| FavouriteRow {
            label: entry.label,
            href: entry.href,
            target: None,
        })
        .collect();
    let (views, pages) = if let (Some(vault), Some(client)) = (query.vault.as_deref(), state.client()) {
        let state = crate::view_favourites::load(&client, vault).await;
        let views = crate::view_favourites::VIEWS
            .iter()
            .filter(|view| state.is_favourite(view.id))
            .map(|view| FavouriteRow {
                label: view.label.to_owned(),
                href: crate::urls::VaultUrls::from(&*vault).page(view.path),
                target: None,
            })
            .collect();
        (views, favourite_page_rows(&client, vault, &state).await)
    } else {
        (Vec::new(), Vec::new())
    };
    let template = FavouritesPanelTemplate {
        views,
        pages,
        apps,
        jobs,
        recent,
    };
    html_response(
        template
            .render()
            .unwrap_or_else(|_| "<p>The favourites panel could not be rendered.</p>".to_owned()),
    )
}

#[cfg(test)]
#[path = "shell_test.rs"]
mod tests;

/// Updates one fixed view's explicit vault-wide favourite flag.
pub async fn set_view_favourite(
    State(state): State<ShellFavouritesRoutesState>,
    axum::extract::Path((vault, id)): axum::extract::Path<(String, String)>,
    axum::Json(update): axum::Json<crate::rendering::favourite_toggle::FavouriteUpdate>,
) -> Response {
    let Some(view) = crate::view_favourites::VIEWS.iter().find(|view| view.id == id) else {
        return super::vault::not_found();
    };
    let Some(client) = state.client() else {
        return super::vault::server_not_configured();
    };
    match crate::apps::vault_exists(&client, &vault).await {
        Ok(true) => {}
        Ok(false) => return super::vault::not_found(),
        Err(_) => return super::vault::unreachable(),
    }
    if let Err(error) = crate::view_favourites::set(&client, &vault, &id, update.favourite).await {
        return favourite_failure(&error);
    }
    crate::rendering::favourite_toggle::response(
        &crate::urls::VaultUrls::from(&*vault).view(&id),
        view.label,
        update.favourite,
    )
}

/// One row per favourite file, opening it on the screen that shows it. The route table is read only when there is a
/// favourite file to route.
async fn favourite_page_rows(
    client: &McpClient,
    vault: &str,
    state: &crate::view_favourites::ViewsState,
) -> Vec<FavouriteRow> {
    let paths: Vec<&str> = state.favourite_pages().collect();
    if paths.is_empty() {
        return Vec::new();
    }
    let routes = crate::screen_routes::ScreenRoutes::load(client, vault, paths.iter().copied()).await;
    paths
        .into_iter()
        .map(|path| {
            let name = path.rsplit('/').next().unwrap_or(path);
            FavouriteRow {
                label: name.strip_suffix(".md").unwrap_or(name).to_owned(),
                href: routes
                    .route(path)
                    .unwrap_or_else(|| crate::urls::VaultUrls::from(vault).file(path)),
                target: None,
            }
        })
        .collect()
}

/// Updates one file's explicit vault-wide favourite flag. Only a file inside the vault that exists can be made a
/// favourite; one that no longer exists can still be removed.
pub async fn set_page_favourite(
    State(state): State<ShellFavouritesRoutesState>,
    axum::extract::Path((vault, path)): axum::extract::Path<(String, String)>,
    axum::Json(update): axum::Json<crate::rendering::favourite_toggle::FavouriteUpdate>,
) -> Response {
    let Some(path) = crate::inbox_outbox::sanitised_relative_path(&path)
        .ok()
        .filter(|segments| !segments.is_empty())
        .map(|segments| segments.join("/"))
    else {
        return super::vault::not_found();
    };
    let Some(client) = state.client() else {
        return super::vault::server_not_configured();
    };
    match crate::apps::vault_exists(&client, &vault).await {
        Ok(true) => {}
        Ok(false) => return super::vault::not_found(),
        Err(_) => return super::vault::unreachable(),
    }
    if update.favourite {
        match super::file_response::is_file(&client, &vault, &path).await {
            Ok(true) => {}
            Ok(false) => return super::vault::not_found(),
            Err(_) => return super::vault::unreachable(),
        }
    }
    if let Err(error) = crate::view_favourites::set_page(&client, &vault, &path, update.favourite).await {
        return favourite_failure(&error);
    }
    let name = path.rsplit('/').next().unwrap_or(&path);
    crate::rendering::favourite_toggle::response(
        &crate::urls::VaultUrls::from(&*vault).favourite(&path),
        name,
        update.favourite,
    )
}

/// A favourite that could not be saved: the failure's own status, and an alert naming what to do, carrying the
/// failure's stable code.
fn favourite_failure(error: &crate::vault_state::FavouriteError) -> Response {
    (
        StatusCode::from(error),
        axum::response::Html(format!(
            "<p role=\"alert\" data-code=\"{code}\">{error}</p>",
            code = error.code()
        )),
    )
        .into_response()
}
