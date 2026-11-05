//! `GET /{{vault_name}}/search`: the Command Centre search results view. The header search box
//! submits here with `q`; every other control (folder includes/excludes, file type, mode, page
//! size, sort, order, minimum relevance, the selected file) is carried as a URL query parameter
//! too, so the whole view is reproducible from its own URL alone.
//!
//! Selecting a result (`search.html`'s row link, distinct from the title link) re-requests this
//! same route with `selected` set, using `hx-select` to swap only the link-panels fragment while
//! `hx-push-url` still updates the address bar to the full page URL - one route serves both
//! the full page and that partial swap, rather than a second endpoint duplicating the query
//! parsing and MCP calls this one already does.

use std::path::PathBuf;
use std::sync::Arc;

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::mcp_client::{McpCallError, McpClient, McpClientSet};
use crate::rendering::page;
use crate::rendering::shell::{self, ActiveScreen};
use crate::routes::vault::{html_response, unreachable};
use crate::screen_routes::ScreenRoutes;
use crate::search::{
    self, DEFAULT_LIMIT, FolderAggregate, LinkTarget, LinkedNote, SearchError, SearchHit, SearchMode, SearchRequest,
    SearchResponse, SortKey, SortOrder,
};

#[derive(Clone)]
pub struct SearchRoutesState {
    pub clients: Arc<McpClientSet>,
    pub web_config_path: Arc<PathBuf>,
}

impl From<super::CommonRoutesState> for SearchRoutesState {
    fn from(value: super::CommonRoutesState) -> Self {
        Self {
            clients: value.clients,
            web_config_path: value.web_config_path,
        }
    }
}

impl SearchRoutesState {
    fn client(&self) -> Result<Arc<McpClient>, Box<Response>> {
        crate::routes::primary_session(&self.clients)
    }
}

async fn ensure_vault(client: &McpClient, vault_name: &str) -> Result<(), Box<Response>> {
    match crate::apps::vault_exists(client, vault_name).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(Box::new(crate::routes::vault::not_found())),
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => Err(Box::new(unreachable())),
    }
}

#[derive(Debug, Default, Deserialize, Clone)]
pub struct SearchQuery {
    #[serde(default)]
    pub q: Option<String>,
    /// Comma-separated folder paths to include.
    #[serde(default)]
    pub include: Option<String>,
    /// Comma-separated folder paths to exclude.
    #[serde(default)]
    pub exclude: Option<String>,
    #[serde(default)]
    pub note_type: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub order: Option<String>,
    #[serde(default)]
    pub min_relevance: Option<f64>,
    #[serde(default)]
    pub offset: Option<u64>,
    #[serde(default)]
    pub limit: Option<u64>,
    /// The result selected in Column 2, driving the two link panels.
    #[serde(default)]
    pub selected: Option<String>,
    /// The Command Centre entity switcher's own selection (`page_shell.html`'s `entity-select`,
    /// matching `routes::vault::VaultQuery::entity`'s identical field): absent means every
    /// entity. Acts as a folder filter here (`effective_include`), since a configured entity's
    /// slug is its vault's own real top-level directory (`routes::entity_docs`'s convention).
    #[serde(default)]
    pub entity: Option<String>,
}

/// The folder scope actually sent to the backend: an explicit Column 3 include (the operator
/// drilling into one folder from the aggregation panel, or typing one directly) always wins,
/// since it is the more specific choice; otherwise the entity switcher's own selection, when
/// set, scopes the search to that entity's top-level folder. Neither is shown as a removable
/// Column 3 chip when it comes from the entity switcher alone - `include_value` (the text field)
/// stays exactly what the operator typed.
fn effective_include(query: &SearchQuery) -> Vec<String> {
    let explicit = split_csv(query.include.as_deref());
    if !explicit.is_empty() {
        return explicit;
    }
    query.entity.iter().cloned().collect()
}

fn split_csv(value: Option<&str>) -> Vec<String> {
    value
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(str::to_owned)
        .collect()
}

fn join_csv(values: &[String]) -> String {
    values.join(",")
}

impl SearchQuery {
    fn mode(&self) -> SearchMode {
        SearchMode::from_param(self.mode.as_deref())
    }

    fn sort(&self) -> SortKey {
        SortKey::from_param(self.sort.as_deref())
    }

    fn order(&self) -> SortOrder {
        SortOrder::from_param(self.order.as_deref())
    }

    /// The raw requested page size, defaulting only when absent - never clamped here. An
    /// out-of-range value is sent through to the backend unchanged, so its own paging rule
    /// rejects it with a stable error rather than this layer silently repairing it.
    fn limit(&self) -> u64 {
        self.limit.unwrap_or(DEFAULT_LIMIT)
    }

    fn offset(&self) -> u64 {
        self.offset.unwrap_or(0)
    }
}

/// Every current parameter as `key=value` pairs, `url_escape`-encoded, in a fixed order so the
/// URL this view builds for itself is stable across renders.
fn query_pairs(vault_name: &str, query: &SearchQuery) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    if let Some(q) = query.q.as_deref().filter(|value| !value.is_empty()) {
        pairs.push(("q".to_owned(), q.to_owned()));
    }
    if let Some(include) = query.include.as_deref().filter(|value| !value.is_empty()) {
        pairs.push(("include".to_owned(), include.to_owned()));
    }
    if let Some(exclude) = query.exclude.as_deref().filter(|value| !value.is_empty()) {
        pairs.push(("exclude".to_owned(), exclude.to_owned()));
    }
    if let Some(note_type) = query.note_type.as_deref().filter(|value| !value.is_empty()) {
        pairs.push(("note_type".to_owned(), note_type.to_owned()));
    }
    if query.mode() != SearchMode::default() {
        pairs.push(("mode".to_owned(), query.mode().as_param().to_owned()));
    }
    if query.sort() != SortKey::default() {
        pairs.push(("sort".to_owned(), query.sort().as_param().to_owned()));
    }
    if query.order() != SortOrder::default() {
        pairs.push(("order".to_owned(), query.order().as_param().to_owned()));
    }
    if let Some(min_relevance) = query.min_relevance {
        pairs.push(("min_relevance".to_owned(), min_relevance.to_string()));
    }
    if query.offset() != 0 {
        pairs.push(("offset".to_owned(), query.offset().to_string()));
    }
    if query.limit() != DEFAULT_LIMIT {
        pairs.push(("limit".to_owned(), query.limit().to_string()));
    }
    if let Some(selected) = query.selected.as_deref().filter(|value| !value.is_empty()) {
        pairs.push(("selected".to_owned(), selected.to_owned()));
    }
    if let Some(entity) = query.entity.as_deref().filter(|value| !value.is_empty()) {
        pairs.push(("entity".to_owned(), entity.to_owned()));
    }
    let _ = vault_name;
    pairs
}

fn encode_query_value(value: &str) -> String {
    use std::fmt::Write as _;
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => encoded.push(byte as char),
            _ => drop(write!(encoded, "%{byte:02X}")),
        }
    }
    encoded
}

fn search_href(vault_name: &str, pairs: &[(String, String)]) -> String {
    if pairs.is_empty() {
        return crate::urls::VaultUrls::from(vault_name).search();
    }
    let joined = pairs
        .iter()
        .map(|(key, value)| format!("{key}={}", encode_query_value(value)))
        .collect::<Vec<_>>()
        .join("&");
    crate::urls::VaultUrls::from(vault_name).search_with(&joined)
}

/// The current URL with `key` replaced (or removed, if `value` is `None`) - the building block
/// every Column 3 control link and the pagination links use.
fn href_with(vault_name: &str, query: &SearchQuery, key: &str, value: Option<&str>) -> String {
    let mut pairs: Vec<(String, String)> = query_pairs(vault_name, query)
        .into_iter()
        .filter(|(k, _)| k != key)
        .collect();
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        pairs.push((key.to_owned(), value.to_owned()));
    }
    search_href(vault_name, &pairs)
}

struct HitView {
    path: String,
    /// The screen that shows the hit's note ([`ScreenRoutes`]); `None` when no screen does.
    href: Option<String>,
    title: String,
    description: String,
    description_is_html: bool,
    updated_date: Option<String>,
    status: Option<String>,
    select_href: String,
    is_selected: bool,
}

/// `updated`'s own date portion (before the first `T` or space), or the whole value when it
/// carries no time component.
fn date_only(updated: &str) -> String {
    updated.split(['T', ' ']).next().unwrap_or(updated).to_owned()
}

fn hit_view(hit: &SearchHit, vault_name: &str, query: &SearchQuery, screens: &ScreenRoutes) -> HitView {
    let (description, description_is_html) = hit
        .metadata
        .description
        .clone()
        .or_else(|| hit.metadata.summary.clone())
        .or_else(|| hit.metadata.excerpt.clone())
        .map_or_else(|| (hit.snippet.clone(), hit.snippet_is_html), |text| (text, false));
    HitView {
        select_href: href_with(vault_name, query, "selected", Some(&hit.path)),
        is_selected: query.selected.as_deref() == Some(hit.path.as_str()),
        href: screens.route(&hit.path),
        path: hit.path.clone(),
        title: if hit.title.is_empty() {
            hit.path.rsplit('/').next().unwrap_or(&hit.path).to_owned()
        } else {
            hit.title.clone()
        },
        description,
        description_is_html,
        updated_date: hit.metadata.updated.as_deref().map(date_only),
        status: hit.metadata.status.clone(),
    }
}

/// One row of the folder aggregation panel, pre-order-flattened (Askama forbids a macro calling
/// itself, so a tree of unknown depth renders from a flat sequence with a `depth` for CSS
/// indentation, never native template recursion - matching `tree_view::flatten`'s identical
/// reason, though this panel has no open/closed accordion state of its own to track).
struct FolderRow {
    depth: usize,
    label: String,
    count: usize,
    href: String,
}

fn flatten_folders(
    aggregates: &[FolderAggregate],
    vault_name: &str,
    query: &SearchQuery,
    depth: usize,
    out: &mut Vec<FolderRow>,
) {
    for aggregate in aggregates {
        let mut include = split_csv(query.include.as_deref());
        if !include.iter().any(|value| value == &aggregate.relative_dir) {
            include.push(aggregate.relative_dir.clone());
        }
        out.push(FolderRow {
            depth,
            label: aggregate.label.clone(),
            count: aggregate.count,
            href: href_with(vault_name, query, "include", Some(&join_csv(&include))),
        });
        flatten_folders(&aggregate.children, vault_name, query, depth + 1, out);
    }
}

#[derive(Template)]
#[template(path = "search.html")]
struct SearchTemplate {
    /// Every URL the template links to within the vault.
    urls: crate::urls::VaultUrls,
    q: String,
    has_query: bool,
    error: Option<String>,
    warnings: Vec<String>,
    hits: Vec<HitView>,
    top_results: Vec<HitView>,
    folders: Vec<FolderRow>,
    total: usize,
    total_capped: bool,
    page_start: u64,
    page_end: u64,
    prev_href: Option<String>,
    next_href: Option<String>,
    include_value: String,
    exclude_value: String,
    note_type_value: String,
    mode: SearchMode,
    sort: SortKey,
    order: SortOrder,
    limit_value: u64,
    min_relevance_value: String,
    selected: Option<String>,
    inbound: Vec<LinkedNoteView>,
    outbound: Vec<LinkedNoteView>,
    link_panels_error: Option<String>,
}

/// One note of a link panel, with the screen that shows it; `href` is `None` when no screen does.
struct LinkedNoteView {
    title: String,
    is_embed: bool,
    href: Option<String>,
    /// How the link fails, when it does: it names no file, or several. Such a link has no screen to go to.
    problem: Option<String>,
    /// Whether the link reaches a file that is not a Markdown note, such as an image or a PDF.
    is_file: bool,
}

impl LinkedNoteView {
    fn new(note: LinkedNote, screens: &ScreenRoutes) -> Self {
        let (problem, is_file) = match &note.target {
            LinkTarget::Note => (None, false),
            LinkTarget::File => (None, true),
            LinkTarget::NotFound => (Some("not found: no file answers to this link".to_owned()), false),
            LinkTarget::Ambiguous(candidates) => (
                Some(format!(
                    "ambiguous: more than one file answers to this link ({})",
                    candidates.join(", ")
                )),
                false,
            ),
        };
        Self {
            href: if problem.is_some() {
                None
            } else {
                screens.route(&note.path)
            },
            title: note.title,
            is_embed: note.is_embed,
            problem,
            is_file,
        }
    }
}

/// The top N of an already-fetched page, shown independent of the active sort order: since the
/// backend only ever sorts the requested page, Top Results only orders by relevance within it
/// when the page itself was not already sorted by relevance.
const TOP_RESULTS_COUNT: usize = 5;

fn top_results(hits: &[SearchHit], sort: SortKey) -> Vec<SearchHit> {
    let mut ranked = hits.to_vec();
    if sort != SortKey::Relevance {
        ranked.sort_by(|a, b| b.relevance.total_cmp(&a.relevance).then_with(|| a.path.cmp(&b.path)));
    }
    ranked.truncate(TOP_RESULTS_COUNT);
    ranked
}

async fn run_search(
    client: &McpClient,
    vault_name: &str,
    query: &SearchQuery,
    q: &str,
) -> (Option<String>, SearchResponse) {
    if q.trim().is_empty() {
        return (None, SearchResponse::default());
    }
    let request = SearchRequest {
        query: q,
        mode: query.mode(),
        include: &effective_include(query),
        exclude: &split_csv(query.exclude.as_deref()),
        note_type: query.note_type.as_deref(),
        offset: query.offset(),
        limit: query.limit(),
        sort: query.sort(),
        order: query.order(),
        min_relevance: query.min_relevance,
    };
    match search::search(client, vault_name, &request).await {
        Ok(response) => (None, response),
        Err(SearchError::Tool { message, .. }) => (Some(message), SearchResponse::default()),
        Err(SearchError::Call(_)) => (
            Some("the search backend could not be reached".to_owned()),
            SearchResponse::default(),
        ),
    }
}

async fn resolve_link_panels(
    client: &McpClient,
    vault_name: &str,
    query: &SearchQuery,
    search_failed: bool,
) -> (Vec<LinkedNote>, Vec<LinkedNote>, Option<String>) {
    let Some(path) = query.selected.as_deref().filter(|path| !path.is_empty()) else {
        return (Vec::new(), Vec::new(), None);
    };
    if search_failed {
        return (Vec::new(), Vec::new(), None);
    }
    let (inbound, outbound) = tokio::join!(
        search::inbound_links(client, vault_name, path),
        search::outbound_links(client, vault_name, path),
    );
    match (inbound, outbound) {
        (Ok(inbound), Ok(outbound)) => (inbound, outbound, None),
        _ => (
            Vec::new(),
            Vec::new(),
            Some("the link panels could not be read".to_owned()),
        ),
    }
}

struct Pagination {
    page_start: u64,
    page_end: u64,
    prev_href: Option<String>,
    next_href: Option<String>,
}

fn paginate(vault_name: &str, query: &SearchQuery, response: &SearchResponse) -> Pagination {
    let limit = query.limit();
    let offset = query.offset();
    let page_start = if response.hits.is_empty() { 0 } else { offset + 1 };
    let page_end = offset + u64::try_from(response.hits.len()).unwrap_or(0);
    let prev_href = (offset > 0).then(|| {
        let prev_offset = offset.saturating_sub(limit);
        href_with(
            vault_name,
            query,
            "offset",
            (prev_offset > 0).then_some(prev_offset.to_string()).as_deref(),
        )
    });
    let next_href = (page_end < response.total as u64 && !response.hits.is_empty())
        .then(|| href_with(vault_name, query, "offset", Some(&(offset + limit).to_string())));
    Pagination {
        page_start,
        page_end,
        prev_href,
        next_href,
    }
}

async fn render(client: &McpClient, vault_name: &str, query: &SearchQuery) -> String {
    let q = query.q.clone().unwrap_or_default();
    let has_query = !q.trim().is_empty();
    let (error, response) = run_search(client, vault_name, query, &q).await;
    let (inbound, outbound, link_panels_error) = resolve_link_panels(client, vault_name, query, error.is_some()).await;
    let top = top_results(&response.hits, query.sort());
    let pagination = paginate(vault_name, query, &response);
    let listed = response
        .hits
        .iter()
        .map(|hit| hit.path.as_str())
        .chain(inbound.iter().chain(&outbound).map(|note| note.path.as_str()));
    let screens = ScreenRoutes::load(client, vault_name, listed).await;

    let template = SearchTemplate {
        urls: crate::urls::VaultUrls::from(vault_name),
        has_query,
        error,
        warnings: response.warnings.clone(),
        hits: response
            .hits
            .iter()
            .map(|hit| hit_view(hit, vault_name, query, &screens))
            .collect(),
        top_results: top
            .iter()
            .map(|hit| hit_view(hit, vault_name, query, &screens))
            .collect(),
        folders: {
            let mut rows = Vec::new();
            flatten_folders(
                &search::aggregate_by_folder(&response.hits),
                vault_name,
                query,
                0,
                &mut rows,
            );
            rows
        },
        total: response.total,
        total_capped: response.total_capped,
        page_start: pagination.page_start,
        page_end: pagination.page_end,
        prev_href: pagination.prev_href,
        next_href: pagination.next_href,
        include_value: query.include.clone().unwrap_or_default(),
        exclude_value: query.exclude.clone().unwrap_or_default(),
        note_type_value: query.note_type.clone().unwrap_or_default(),
        mode: query.mode(),
        sort: query.sort(),
        order: query.order(),
        limit_value: query.limit(),
        min_relevance_value: query.min_relevance.map(|value| value.to_string()).unwrap_or_default(),
        selected: query.selected.clone(),
        inbound: inbound
            .into_iter()
            .map(|note| LinkedNoteView::new(note, &screens))
            .collect(),
        outbound: outbound
            .into_iter()
            .map(|note| LinkedNoteView::new(note, &screens))
            .collect(),
        link_panels_error,
        q,
    };
    template
        .render()
        .unwrap_or_else(|_| "<p>The search results could not be rendered.</p>".to_owned())
}

pub async fn get(
    State(state): State<SearchRoutesState>,
    Path(vault_name): Path<String>,
    Query(query): Query<SearchQuery>,
    headers: HeaderMap,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(response) => return *response,
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let fragment = render(&client, &vault_name, &query).await;
    if headers.contains_key("hx-request") {
        html_response(fragment)
    } else {
        let web_config_path = Arc::clone(&state.web_config_path);
        let entity_base_path = crate::urls::VaultUrls::from(&*vault_name).search();
        let known_mcp_servers = state.clients.names();
        let (mut nav, appearance, mcp_status) = tokio::join!(
            Box::pin(shell::build_nav(
                &client,
                ActiveScreen::Vault,
                Some(&vault_name),
                Some("search"),
                query.entity.as_deref(),
                Some(&entity_base_path),
                &known_mcp_servers,
            )),
            async move {
                tokio::task::spawn_blocking(move || crate::config::current_appearance(&web_config_path))
                    .await
                    .unwrap_or_default()
            },
            shell::mcp_status(&state.clients),
        );
        nav.appearance = appearance;
        nav.sidebar_wide = crate::routes::sidebar_wide_from_headers(&headers);
        nav.mcp_status = mcp_status;
        html_response(page::render_page(&nav, "Search", &fragment))
    }
    .into_response()
}
