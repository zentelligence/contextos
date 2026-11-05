//! `GET /{{vault_name}}/signals/`: the Signals screen, three regions read from the vault's events with an inline
//! `base_query` definition, so a vault without `memory/events.base` still works:
//!
//! - **Needs attention**: warning and error events that nothing since has cleared ([`crate::signals_attention`]);
//! - **Feed**: events newest first, 50 to a page, filtered by kind, severity, and entity through HTMX partial swaps;
//! - **Background activity**: recorded kinds the product does not ship, grouped by kind.
//!
//! `GET /{{vault_name}}/signals/{feed,attention,background}` answer with one region as a fragment, for the filter
//! form, the paging links, and the attention region's refresh when the live stream says attention may have changed.
//! The screen reads and renders; it writes nothing, and it shows no note body.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::config;
use crate::mcp_client::{McpCallError, McpClient, McpClientSet};
use crate::rendering::page;
use crate::rendering::shell::{self, ActiveScreen};
use crate::routes::file_response::MalformedQuery;
use crate::routes::vault::{html_response, not_found, unreachable};
use crate::signals_load::{FeedRequest, load_attention, load_background, load_feed};
use crate::signals_query::{EventsApi, FeedFilters, ReadError};
use crate::signals_view::{AttentionView, BackgroundView, KindCatalogue, SignalsPage};
use crate::urls::VaultUrls;

#[derive(Clone)]
pub struct SignalsRoutesState {
    pub clients: Arc<McpClientSet>,
    pub web_config_path: Arc<PathBuf>,
}

impl From<super::CommonRoutesState> for SignalsRoutesState {
    fn from(value: super::CommonRoutesState) -> Self {
        Self {
            clients: value.clients,
            web_config_path: value.web_config_path,
        }
    }
}

/// What the screen and its regions read from the query string.
#[derive(Debug, Default, Deserialize)]
pub struct SignalsQuery {
    /// The entity switcher's selection.
    #[serde(default)]
    pub entity: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub severity: Option<String>,
    /// 1-based page of the feed.
    #[serde(default)]
    pub page: Option<usize>,
}

impl SignalsQuery {
    fn entity(&self) -> Option<&str> {
        self.entity.as_deref().filter(|entity| !entity.is_empty())
    }

    /// The feed this query asks for; a `severity` that is not `info`, `warning`, or `error` is malformed, never dropped.
    fn feed_request(&self) -> Result<FeedRequest, MalformedQuery> {
        let set = |value: &Option<String>| value.clone().filter(|value| !value.is_empty());
        let severity = set(&self.severity);
        if severity
            .as_deref()
            .is_some_and(|severity| crate::signals_event::Severity::try_from(severity).is_err())
        {
            return Err(MalformedQuery);
        }
        Ok(FeedRequest {
            filters: FeedFilters {
                kind: set(&self.kind),
                severity,
                entity: set(&self.entity),
            },
            page: self
                .page
                .unwrap_or(1)
                .clamp(1, crate::signals_query::MAX_ROWS / crate::signals_view::FEED_PAGE_SIZE),
        })
    }
}

async fn ensure_vault(client: &McpClient, vault_name: &str) -> Result<(), Box<Response>> {
    match crate::apps::vault_exists(client, vault_name).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(Box::new(not_found())),
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => Err(Box::new(unreachable())),
    }
}

async fn session(state: &SignalsRoutesState, vault_name: &str) -> Result<Arc<McpClient>, Box<Response>> {
    let client = crate::routes::primary_session(&state.clients)?;
    ensure_vault(&client, vault_name).await?;
    Ok(client)
}

/// The kinds the vault declares, read once for the request; none when they cannot be read, so every kind shows by its
/// own name and the server's own kinds show their known fields.
async fn labels_of(api: &EventsApi<'_>) -> KindCatalogue {
    api.kind_catalogue().await.unwrap_or_default()
}

fn region_failed(error: ReadError) -> Response {
    match error {
        ReadError::Unreachable | ReadError::Refused => unreachable(),
    }
}

/// `GET /{{vault_name}}/signals/`.
pub async fn page(
    State(state): State<SignalsRoutesState>,
    Path(vault_name): Path<String>,
    Query(query): Query<SignalsQuery>,
    headers: HeaderMap,
) -> Response {
    let request = match query.feed_request() {
        Ok(request) => request,
        Err(malformed) => return malformed.into_response(),
    };
    let client = match session(&state, &vault_name).await {
        Ok(client) => client,
        Err(response) => return *response,
    };
    let urls = VaultUrls::from(vault_name.as_str());
    let api = EventsApi {
        client: &client,
        vault_name: &vault_name,
    };
    let labels = labels_of(&api).await;
    let (feed, attention, background) = tokio::join!(
        load_feed(&api, &urls, &labels, &request),
        load_attention(&api, &urls, &labels, query.entity(), time::OffsetDateTime::now_utc()),
        load_background(&api, &urls, &labels, query.entity()),
    );
    let feed = match feed {
        Ok(feed) => feed,
        Err(error) => return region_failed(error),
    };
    let attention = attention.unwrap_or_else(|_| AttentionView {
        urls: urls.clone(),
        refresh_href: urls.signals_region("attention", ""),
        lines: Vec::new(),
        entity: None,
        more: 0,
        unreadable: true,
    });
    let background = background.unwrap_or_else(|_| BackgroundView {
        urls: urls.clone(),
        groups: Vec::new(),
        entity: None,
        unreadable: true,
        refresh_href: urls.signals_region("background", ""),
    });
    let body = crate::signals_view::render_page(&SignalsPage {
        urls: urls.clone(),
        attention: crate::signals_view::render_attention(&attention),
        feed: crate::signals_view::render_feed(&feed),
        background: crate::signals_view::render_background(&background),
    });
    if headers.contains_key("hx-request") {
        return html_response(body);
    }
    let web_config_path = Arc::clone(&state.web_config_path);
    let known_mcp_servers = state.clients.names();
    let entity_base_path = urls.signals();
    let (mut nav, appearance, mcp_status) = tokio::join!(
        Box::pin(shell::build_nav(
            &client,
            ActiveScreen::Vault,
            Some(&vault_name),
            Some("signals"),
            query.entity.as_deref(),
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
    nav.sidebar_wide = crate::routes::sidebar_wide_from_headers(&headers);
    nav.mcp_status = mcp_status;
    nav.events_stream_href = Some(stream_href(&urls, query.entity(), feed.newest_cursor.as_deref()));
    html_response(page::render_page(&nav, "Signals", &body)).into_response()
}

/// The stream address for a page that is showing events up to `after`.
fn stream_href(urls: &VaultUrls, entity: Option<&str>, after: Option<&str>) -> String {
    let mut pairs = Vec::new();
    if let Some(entity) = entity {
        pairs.push(format!("entity={}", crate::inbox_outbox::encode_query_value(entity)));
    }
    if let Some(after) = after {
        pairs.push(format!("after={}", crate::inbox_outbox::encode_query_value(after)));
    }
    urls.events_stream(&pairs.join("&"))
}

/// `GET /{{vault_name}}/signals/feed`: the feed region.
pub async fn feed(
    State(state): State<SignalsRoutesState>,
    Path(vault_name): Path<String>,
    Query(query): Query<SignalsQuery>,
) -> Response {
    let request = match query.feed_request() {
        Ok(request) => request,
        Err(malformed) => return malformed.into_response(),
    };
    let client = match session(&state, &vault_name).await {
        Ok(client) => client,
        Err(response) => return *response,
    };
    let urls = VaultUrls::from(vault_name.as_str());
    let api = EventsApi {
        client: &client,
        vault_name: &vault_name,
    };
    let labels = labels_of(&api).await;
    match load_feed(&api, &urls, &labels, &request).await {
        Ok(view) => html_response(crate::signals_view::render_feed(&view)),
        Err(error) => region_failed(error),
    }
}

/// `GET /{{vault_name}}/signals/attention`: the attention region.
pub async fn attention(
    State(state): State<SignalsRoutesState>,
    Path(vault_name): Path<String>,
    Query(query): Query<SignalsQuery>,
) -> Response {
    let client = match session(&state, &vault_name).await {
        Ok(client) => client,
        Err(response) => return *response,
    };
    let urls = VaultUrls::from(vault_name.as_str());
    let api = EventsApi {
        client: &client,
        vault_name: &vault_name,
    };
    let labels = labels_of(&api).await;
    match load_attention(&api, &urls, &labels, query.entity(), time::OffsetDateTime::now_utc()).await {
        Ok(view) => html_response(crate::signals_view::render_attention(&view)),
        Err(error) => region_failed(error),
    }
}

/// `GET /{{vault_name}}/signals/background`: the background activity region.
pub async fn background(
    State(state): State<SignalsRoutesState>,
    Path(vault_name): Path<String>,
    Query(query): Query<SignalsQuery>,
) -> Response {
    let client = match session(&state, &vault_name).await {
        Ok(client) => client,
        Err(response) => return *response,
    };
    let urls = VaultUrls::from(vault_name.as_str());
    let api = EventsApi {
        client: &client,
        vault_name: &vault_name,
    };
    let labels = labels_of(&api).await;
    match load_background(&api, &urls, &labels, query.entity()).await {
        Ok(view) => html_response(crate::signals_view::render_background(&view)),
        Err(error) => region_failed(error),
    }
}
