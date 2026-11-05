//! `/{vault}/settings/{category}/...`: the Vault Settings "Vault management" pages. Each category (Decisions,
//! Questions, Registry, Standards, Templates, Activity log) is its own addressable resource beneath the vault settings
//! page, sharing that page's sidebar ([`SettingsNav`]) so the two read as one screen.
//!
//! This module is the shell every category shares: it resolves the category, reads the vault's [`Catalogue`] once,
//! answers `404` for a category the vault has not provisioned, and composes the category's panel into the settings
//! layout, as a full page for a browser navigation or the `.vault-settings` fragment for an htmx request. A category
//! module handles everything beneath its own segment and answers with an [`Outcome`]: a panel to show, or, after a
//! successful write, the location of the resource the write produced. The shell then re-reads the catalogue (so the
//! sidebar's counts include the write), renders that location as an ordinary `GET` would, and tells htmx to push its
//! URL, so the address bar always names what is on screen.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use askama::Template;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::config;
use crate::mcp_client::{McpClient, McpClientSet};
use crate::rendering::page;
use crate::rendering::shell::{self, ActiveScreen};
use crate::routes::vault::{html_response, not_found, unreachable};
use crate::vault_catalogue::{Catalogue, ManagementCategory, load_catalogue};

use super::{vault_management_activity, vault_management_library, vault_management_registers};

#[derive(Clone)]
pub struct VaultManagementRoutesState {
    pub clients: Arc<McpClientSet>,
    pub web_config_path: Arc<PathBuf>,
    /// The operator's local time, for every date this screen writes into a vault.
    pub vault_time: Arc<crate::vault_time::VaultTime>,
}

impl From<super::CommonRoutesState> for VaultManagementRoutesState {
    fn from(value: super::CommonRoutesState) -> Self {
        Self {
            clients: value.clients,
            web_config_path: value.web_config_path,
            vault_time: value.vault_time,
        }
    }
}

/// One configuration pane on the vault settings page.
pub(crate) struct ConfigPane {
    pub(crate) name: &'static str,
    pub(crate) label: &'static str,
    pub(crate) icon: &'static str,
}

/// The configuration panes, in sidebar order. Vault health sits apart from both groups.
pub(crate) const CONFIG_PANES: &[ConfigPane] = &[
    ConfigPane {
        name: "tasks",
        label: "Tasks",
        icon: "list-checks",
    },
    ConfigPane {
        name: "inbox_outbox",
        label: "Inbox & Outbox",
        icon: "inbox",
    },
    ConfigPane {
        name: "journal",
        label: "Journal",
        icon: "book-open",
    },
    ConfigPane {
        name: "notes",
        label: "Notes",
        icon: "file-text",
    },
    ConfigPane {
        name: "wiki",
        label: "Wiki",
        icon: "book",
    },
    ConfigPane {
        name: "insights",
        label: "Insights",
        icon: "sparkles",
    },
    ConfigPane {
        name: "entity_docs",
        label: "Entity folders",
        icon: "folder",
    },
    ConfigPane {
        name: "profile",
        label: "Profile",
        icon: "user-round",
    },
];

/// One "Vault management" sidebar link.
pub(crate) struct ManagementLink {
    pub(crate) slug: &'static str,
    pub(crate) label: &'static str,
    pub(crate) icon: &'static str,
    pub(crate) count: usize,
    pub(crate) count_noun: &'static str,
    pub(crate) active: bool,
}

/// The vault settings sidebar's content for one response.
pub(crate) struct SettingsNav {
    pub(crate) vault_name: String,
    pub(crate) on_configuration_page: bool,
    pub(crate) config_panes: &'static [ConfigPane],
    pub(crate) management: Vec<ManagementLink>,
}

impl SettingsNav {
    fn build(vault_name: &str, catalogue: &Catalogue, active: Option<ManagementCategory>) -> Self {
        Self {
            vault_name: vault_name.to_owned(),
            on_configuration_page: active.is_none(),
            config_panes: CONFIG_PANES,
            management: catalogue
                .summaries()
                .into_iter()
                .map(|summary| ManagementLink {
                    slug: summary.category.slug(),
                    label: summary.category.label(),
                    icon: summary.category.icon(),
                    count: summary.count,
                    count_noun: summary.category.count_noun(),
                    active: Some(summary.category) == active,
                })
                .collect(),
        }
    }

    /// The sidebar as the configuration page shows it.
    pub(crate) fn configuration(vault_name: &str, catalogue: &Catalogue) -> Self {
        Self::build(vault_name, catalogue, None)
    }
}

/// What a category module shows in the settings content area.
pub(crate) struct Panel {
    /// The page `<title>` suffix, after "Vault settings".
    pub(crate) title: String,
    pub(crate) html: String,
}

/// A category module's answer to one request.
pub(crate) enum Outcome {
    /// Show this panel.
    Show(Panel),
    /// A write succeeded; show the resource at this path beneath the category (empty for the category's own root)
    /// and push its URL.
    Wrote { location: String },
    /// Answer with this response as it is (a `404`, an unreachable server, a malformed request).
    Respond(Box<Response>),
}

impl From<Response> for Outcome {
    fn from(response: Response) -> Self {
        Self::Respond(Box::new(response))
    }
}

/// Everything a category module needs to answer a request.
pub(crate) struct ManagementContext<'a> {
    pub(crate) client: &'a McpClient,
    pub(crate) vault_name: &'a str,
    pub(crate) catalogue: &'a Catalogue,
    /// The request's query parameters (a list's type and status filters, a new record's type).
    pub(crate) query: &'a BTreeMap<String, String>,
    /// The instant this request is answered at, at the operator's local offset.
    pub(crate) now: time::OffsetDateTime,
}

impl ManagementContext<'_> {
    /// The URL of `location` beneath `category`, percent-encoded for an `href` or a pushed URL.
    pub(crate) fn href(&self, category: ManagementCategory, location: &str) -> String {
        category_href(self.vault_name, category, location)
    }
}

pub(crate) fn category_href(vault_name: &str, category: ManagementCategory, location: &str) -> String {
    crate::urls::VaultUrls::from(vault_name).settings_category(category.slug(), location)
}

/// A request body this screen's forms could not have produced: answered with an inline error fragment and nothing
/// changed.
pub(crate) fn malformed() -> Outcome {
    Outcome::from(
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            [("content-type", "text/html; charset=utf-8")],
            "<p class=\"hint error\" role=\"alert\">This request could not be understood, so nothing was changed.</p>",
        )
            .into_response(),
    )
}

#[derive(Template)]
#[template(path = "vault_management.html")]
struct VaultManagementTemplate {
    nav: SettingsNav,
    panel_html: String,
}

async fn dispatch(
    context: &ManagementContext<'_>,
    category: ManagementCategory,
    rest: &str,
    method: &Method,
    body: &Bytes,
) -> Outcome {
    match category {
        ManagementCategory::Decisions => {
            Box::pin(vault_management_registers::handle(context, rest, method, body)).await
        }
        ManagementCategory::Registry | ManagementCategory::Standards | ManagementCategory::Templates => {
            Box::pin(vault_management_library::handle(context, category, rest, method, body)).await
        }
        ManagementCategory::Activity => {
            if method == Method::GET {
                Box::pin(vault_management_activity::handle(context, rest)).await
            } else {
                Outcome::from(StatusCode::METHOD_NOT_ALLOWED.into_response())
            }
        }
    }
}

/// One request beneath a vault management category, as the routes received it.
struct ManagementRequest<'a> {
    vault_name: String,
    category_slug: &'a str,
    rest: &'a str,
    method: &'a Method,
    headers: &'a HeaderMap,
    query: &'a BTreeMap<String, String>,
    body: &'a Bytes,
}

async fn respond(state: &VaultManagementRoutesState, request: ManagementRequest<'_>) -> Response {
    let ManagementRequest {
        vault_name,
        category_slug,
        rest,
        method,
        headers,
        query,
        body,
    } = request;
    let client = match crate::routes::primary_session(&state.clients) {
        Ok(client) => client,
        Err(response) => return *response,
    };
    match crate::apps::vault_exists(&client, &vault_name).await {
        Ok(true) => {}
        Ok(false) => return not_found(),
        Err(_) => return unreachable(),
    }
    let Ok(category) = ManagementCategory::try_from(category_slug) else {
        return not_found();
    };
    let now = state.vault_time.instant().await;
    let mut catalogue = Box::pin(load_catalogue(&client, &vault_name)).await;
    if !catalogue.is_present(category) {
        return not_found();
    }
    let outcome = {
        let context = ManagementContext {
            client: &client,
            vault_name: &vault_name,
            catalogue: &catalogue,
            query,
            now,
        };
        dispatch(&context, category, rest, method, body).await
    };
    let (panel, pushed_url) = match outcome {
        Outcome::Show(panel) => (panel, None),
        Outcome::Respond(response) => return *response,
        Outcome::Wrote { location } => {
            catalogue = Box::pin(load_catalogue(&client, &vault_name)).await;
            let no_query = BTreeMap::new();
            let context = ManagementContext {
                client: &client,
                vault_name: &vault_name,
                catalogue: &catalogue,
                query: &no_query,
                now,
            };
            match dispatch(&context, category, &location, &Method::GET, &Bytes::new()).await {
                Outcome::Show(panel) => (panel, Some(context.href(category, &location))),
                Outcome::Respond(response) => return *response,
                Outcome::Wrote { .. } => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            }
        }
    };
    let fragment = VaultManagementTemplate {
        nav: SettingsNav::build(&vault_name, &catalogue, Some(category)),
        panel_html: panel.html,
    }
    .render()
    .unwrap_or_else(|_| "<p class=\"hint error\">Vault settings could not be rendered.</p>".to_owned());
    if headers.contains_key("hx-request") {
        let mut response = html_response(fragment);
        if let Some(url) = pushed_url.and_then(|url| HeaderValue::from_str(&url).ok()) {
            response.headers_mut().insert("HX-Push-Url", url);
        }
        return response;
    }
    full_page(
        state,
        &client,
        &vault_name,
        &format!("Vault settings · {}", panel.title),
        &fragment,
        headers,
    )
    .await
}

async fn full_page(
    state: &VaultManagementRoutesState,
    client: &McpClient,
    vault_name: &str,
    title: &str,
    fragment: &str,
    headers: &HeaderMap,
) -> Response {
    let web_config_path = Arc::clone(&state.web_config_path);
    let breadcrumb = "settings".to_owned();
    let entity_base_path = crate::urls::VaultUrls::from(vault_name).settings();
    let known_mcp_servers = state.clients.names();
    let (mut nav, appearance, mcp_status) = tokio::join!(
        Box::pin(shell::build_nav(
            client,
            ActiveScreen::Vault,
            Some(vault_name),
            Some(&breadcrumb),
            None,
            Some(&entity_base_path),
            &known_mcp_servers,
        )),
        async move {
            tokio::task::spawn_blocking(move || config::current_appearance(&web_config_path))
                .await
                .unwrap_or_default()
        },
        shell::mcp_status(&state.clients),
    );
    nav.appearance = appearance;
    nav.sidebar_wide = crate::routes::sidebar_wide_from_headers(headers);
    nav.mcp_status = mcp_status;
    html_response(page::render_page(&nav, title, fragment))
}

/// `/{vault}/settings/{category}` and `/{vault}/settings/{category}/`, every method.
pub async fn category_root(
    State(state): State<VaultManagementRoutesState>,
    Path((vault_name, category)): Path<(String, String)>,
    Query(query): Query<BTreeMap<String, String>>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request = ManagementRequest {
        vault_name,
        category_slug: &category,
        rest: "",
        method: &method,
        headers: &headers,
        query: &query,
        body: &body,
    };
    respond(&state, request).await
}

/// `/{vault}/settings/{category}/{*rest}`, every method.
pub async fn category_path(
    State(state): State<VaultManagementRoutesState>,
    Path((vault_name, category, rest)): Path<(String, String, String)>,
    Query(query): Query<BTreeMap<String, String>>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request = ManagementRequest {
        vault_name,
        category_slug: &category,
        rest: &rest,
        method: &method,
        headers: &headers,
        query: &query,
        body: &body,
    };
    respond(&state, request).await
}

#[cfg(test)]
#[path = "vault_management_test.rs"]
mod tests;
