//! Search domain logic: `query_blended`/`query_text`/`query_semantic` calls, the selected
//! file's outbound (`links_read`) and inbound (`query_graph` backlinks) links, and the
//! nested-folder aggregation over one result set.
//!
//! `query_blended`'s own `snippet` is Tantivy's pre-escaped, `<b>`-highlighted HTML when a text
//! match contributed it, but raw untrusted chunk text when only the semantic index matched
//! (`contextos-search::service::blended::snippet_of`). [`SearchHit::snippet_is_html`] is exactly
//! that distinction, carried through so the template renders the right one safely.

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::mcp_client::{McpCallError, McpClient};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SearchMode {
    #[default]
    Blended,
    Text,
    Semantic,
}

impl SearchMode {
    #[must_use]
    pub fn as_param(self) -> &'static str {
        match self {
            Self::Blended => "blended",
            Self::Text => "text",
            Self::Semantic => "semantic",
        }
    }

    #[must_use]
    pub fn from_param(raw: Option<&str>) -> Self {
        match raw {
            Some("text") => Self::Text,
            Some("semantic") => Self::Semantic,
            _ => Self::Blended,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SortKey {
    #[default]
    Relevance,
    Title,
    Modified,
}

impl SortKey {
    #[must_use]
    pub fn as_param(self) -> &'static str {
        match self {
            Self::Relevance => "relevance",
            Self::Title => "title",
            Self::Modified => "modified",
        }
    }

    #[must_use]
    pub fn from_param(raw: Option<&str>) -> Self {
        match raw {
            Some("title") => Self::Title,
            Some("modified") => Self::Modified,
            _ => Self::Relevance,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SortOrder {
    #[default]
    Desc,
    Asc,
}

impl SortOrder {
    #[must_use]
    pub fn as_param(self) -> &'static str {
        match self {
            Self::Desc => "desc",
            Self::Asc => "asc",
        }
    }

    #[must_use]
    pub fn from_param(raw: Option<&str>) -> Self {
        match raw {
            Some("asc") => Self::Asc,
            _ => Self::Desc,
        }
    }
}

/// The default page size when a fresh search sets none.
pub const DEFAULT_LIMIT: u64 = 20;

pub struct SearchRequest<'a> {
    pub query: &'a str,
    pub mode: SearchMode,
    pub include: &'a [String],
    pub exclude: &'a [String],
    pub note_type: Option<&'a str>,
    pub offset: u64,
    pub limit: u64,
    pub sort: SortKey,
    pub order: SortOrder,
    pub min_relevance: Option<f64>,
}

#[derive(Clone, Debug, Default)]
pub struct HitMetadata {
    pub description: Option<String>,
    pub summary: Option<String>,
    pub excerpt: Option<String>,
    pub status: Option<String>,
    pub updated: Option<String>,
    pub note_type: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SearchHit {
    pub path: String,
    pub title: String,
    pub relevance: f32,
    pub snippet: String,
    /// `true` when `snippet` is Tantivy's own pre-escaped, `<b>`-highlighted HTML (a text-index
    /// match contributed it); `false` when it is untrusted plain text (a semantic chunk excerpt,
    /// or no snippet at all) that must still be HTML-escaped on render.
    pub snippet_is_html: bool,
    pub matched_by: Vec<String>,
    pub modified: Option<String>,
    pub metadata: HitMetadata,
}

#[derive(Debug, Default)]
pub struct SearchResponse {
    pub hits: Vec<SearchHit>,
    pub total: usize,
    pub offset: usize,
    pub total_capped: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error(transparent)]
    Call(#[from] McpCallError),
    #[error("{message}")]
    Tool { code: String, message: String },
}

#[derive(Debug, Default, Deserialize)]
struct ToolFailureView {
    #[serde(default)]
    code: String,
    #[serde(default)]
    message: String,
}

fn tool_error(result: rmcp::model::CallToolResult) -> SearchError {
    let view: ToolFailureView = result.into_typed().unwrap_or_default();
    let code = if view.code.is_empty() {
        "search/failed".to_owned()
    } else {
        view.code
    };
    let message = if view.message.is_empty() {
        "the search request failed".to_owned()
    } else {
        view.message
    };
    SearchError::Tool { code, message }
}

fn common_args(vault_name: &str, request: &SearchRequest<'_>, args: &mut Map<String, Value>) {
    args.insert("query".to_owned(), Value::String(request.query.to_owned()));
    args.insert("vault".to_owned(), Value::String(format!("{vault_name}://.")));
    args.insert("offset".to_owned(), Value::Number(request.offset.into()));
    args.insert("limit".to_owned(), Value::Number(request.limit.into()));
    args.insert("sort".to_owned(), Value::String(request.sort.as_param().to_owned()));
    args.insert("order".to_owned(), Value::String(request.order.as_param().to_owned()));
    if let Some(min_relevance) = request.min_relevance
        && let Some(number) = serde_json::Number::from_f64(min_relevance)
    {
        args.insert("min_relevance".to_owned(), Value::Number(number));
    }
    if let Some(note_type) = request.note_type.filter(|value| !value.is_empty()) {
        args.insert("note_type".to_owned(), Value::String(note_type.to_owned()));
    }
    if let Some(path_prefix) = request.include.first() {
        args.insert("path_prefix".to_owned(), Value::String(path_prefix.clone()));
    }
    if !request.exclude.is_empty() {
        args.insert(
            "exclude_paths".to_owned(),
            Value::Array(request.exclude.iter().cloned().map(Value::String).collect()),
        );
    }
}

#[derive(Debug, Default, Deserialize)]
struct HitMetadataResult {
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    excerpt: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    updated: Option<String>,
    #[serde(default)]
    note_type: Option<String>,
}

impl From<HitMetadataResult> for HitMetadata {
    fn from(value: HitMetadataResult) -> Self {
        Self {
            description: value.description,
            summary: value.summary,
            excerpt: value.excerpt,
            status: value.status,
            updated: value.updated,
            note_type: value.note_type,
        }
    }
}

#[derive(Debug, Deserialize)]
struct BlendedHitResult {
    path: String,
    title: String,
    relevance: f32,
    snippet: String,
    matched_by: Vec<String>,
    #[serde(default)]
    modified: Option<String>,
    #[serde(default)]
    metadata: HitMetadataResult,
}

#[derive(Debug, Deserialize)]
struct QueryBlendedResult {
    hits: Vec<BlendedHitResult>,
    total: usize,
    offset: usize,
    #[serde(default)]
    total_capped: bool,
    #[serde(default)]
    warnings: Vec<String>,
}

impl From<QueryBlendedResult> for SearchResponse {
    fn from(value: QueryBlendedResult) -> Self {
        Self {
            hits: value
                .hits
                .into_iter()
                .map(|hit| {
                    let snippet_is_html = hit.matched_by.iter().any(|mode| mode == "text");
                    SearchHit {
                        path: hit.path,
                        title: hit.title,
                        relevance: hit.relevance,
                        snippet: hit.snippet,
                        snippet_is_html,
                        matched_by: hit.matched_by,
                        modified: hit.modified,
                        metadata: hit.metadata.into(),
                    }
                })
                .collect(),
            total: value.total,
            offset: value.offset,
            total_capped: value.total_capped,
            warnings: value.warnings,
        }
    }
}

#[derive(Debug, Deserialize)]
struct TextHitResult {
    path: String,
    relevance: f32,
    title: String,
    snippet: String,
    modified: String,
    #[serde(default)]
    metadata: HitMetadataResult,
}

#[derive(Debug, Deserialize)]
struct QueryTextResult {
    hits: Vec<TextHitResult>,
    total: usize,
    offset: usize,
}

impl From<QueryTextResult> for SearchResponse {
    fn from(value: QueryTextResult) -> Self {
        Self {
            hits: value
                .hits
                .into_iter()
                .map(|hit| SearchHit {
                    path: hit.path,
                    title: hit.title,
                    relevance: hit.relevance,
                    snippet: hit.snippet,
                    snippet_is_html: true,
                    matched_by: vec!["text".to_owned()],
                    modified: Some(hit.modified),
                    metadata: hit.metadata.into(),
                })
                .collect(),
            total: value.total,
            offset: value.offset,
            total_capped: false,
            warnings: Vec::new(),
        }
    }
}

/// The longest a semantic chunk's own excerpt is shown as a result snippet, in characters.
const SEMANTIC_SNIPPET_MAX_CHARS: usize = 220;

fn truncate_chars(value: &str, max_chars: usize) -> String {
    match value.char_indices().nth(max_chars) {
        Some((byte_index, _)) => format!("{}…", &value[..byte_index]),
        None => value.to_owned(),
    }
}

#[derive(Debug, Deserialize)]
struct SemanticHitResult {
    path: String,
    chunk: String,
    relevance: f32,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    modified: Option<String>,
    #[serde(default)]
    metadata: HitMetadataResult,
}

#[derive(Debug, Deserialize)]
struct QuerySemanticResult {
    hits: Vec<SemanticHitResult>,
    total: usize,
    offset: usize,
    #[serde(default)]
    total_capped: bool,
}

impl From<QuerySemanticResult> for SearchResponse {
    fn from(value: QuerySemanticResult) -> Self {
        Self {
            hits: value
                .hits
                .into_iter()
                .map(|hit| SearchHit {
                    title: hit.title.clone().unwrap_or_else(|| hit.path.clone()),
                    snippet: truncate_chars(&hit.chunk, SEMANTIC_SNIPPET_MAX_CHARS),
                    snippet_is_html: false,
                    matched_by: vec!["semantic".to_owned()],
                    path: hit.path,
                    relevance: hit.relevance,
                    modified: hit.modified,
                    metadata: hit.metadata.into(),
                })
                .collect(),
            total: value.total,
            offset: value.offset,
            total_capped: value.total_capped,
            warnings: Vec::new(),
        }
    }
}

/// Runs `request` against the mode-appropriate tool (`query_blended` for [`SearchMode::Blended`],
/// otherwise the single-index tool directly, so text/semantic-only mode never pays for a fused
/// call it does not need).
///
/// # Errors
///
/// Returns [`SearchError::Call`] when the MCP transport itself fails, and [`SearchError::Tool`]
/// for a tool-level error result (an out-of-range page, an unparsable query, a disabled index).
pub async fn search(
    client: &McpClient,
    vault_name: &str,
    request: &SearchRequest<'_>,
) -> Result<SearchResponse, SearchError> {
    let mut args = Map::new();
    common_args(vault_name, request, &mut args);
    let (tool_name, response) = match request.mode {
        SearchMode::Blended => {
            args.insert("mode".to_owned(), Value::String("blended".to_owned()));
            (
                "query_blended",
                client.call_tool("query_blended".to_owned(), args).await?,
            )
        }
        SearchMode::Text => ("query_text", client.call_tool("query_text".to_owned(), args).await?),
        SearchMode::Semantic => (
            "query_semantic",
            client.call_tool("query_semantic".to_owned(), args).await?,
        ),
    };
    if response.is_error == Some(true) {
        return Err(tool_error(response));
    }
    let mut response = match tool_name {
        "query_blended" => response
            .into_typed::<QueryBlendedResult>()
            .map(SearchResponse::from)
            .map_err(|_| SearchError::Tool {
                code: "search/malformed-response".to_owned(),
                message: "the search backend returned an unexpected response".to_owned(),
            })?,
        "query_text" => response
            .into_typed::<QueryTextResult>()
            .map(SearchResponse::from)
            .map_err(|_| SearchError::Tool {
                code: "search/malformed-response".to_owned(),
                message: "the search backend returned an unexpected response".to_owned(),
            })?,
        _ => response
            .into_typed::<QuerySemanticResult>()
            .map(SearchResponse::from)
            .map_err(|_| SearchError::Tool {
                code: "search/malformed-response".to_owned(),
                message: "the search backend returned an unexpected response".to_owned(),
            })?,
    };
    // A folder's managed `index.md` is never shown in the interface, so it is never a result either.
    let before = response.hits.len();
    response
        .hits
        .retain(|hit| !crate::vault_links::is_managed_index(&hit.path));
    response.total = response.total.saturating_sub(before - response.hits.len());
    Ok(response)
}

// ---------------------------------------------------------------------
// Folder aggregation over one result set
// ---------------------------------------------------------------------

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FolderAggregate {
    pub label: String,
    /// The full folder path from the vault root, used as an `include` filter value when this
    /// aggregate is selected.
    pub relative_dir: String,
    /// Hits at or below this folder.
    pub count: usize,
    pub children: Vec<FolderAggregate>,
}

struct AggregateNode {
    count: usize,
    children: std::collections::BTreeMap<String, AggregateNode>,
}

impl AggregateNode {
    fn empty() -> Self {
        Self {
            count: 0,
            children: std::collections::BTreeMap::new(),
        }
    }

    fn insert(&mut self, segments: &[&str]) {
        self.count += 1;
        if let [first, rest @ ..] = segments {
            self.children
                .entry((*first).to_owned())
                .or_insert_with(AggregateNode::empty)
                .insert(rest);
        }
    }

    fn into_aggregates(self, parent_dir: &str) -> Vec<FolderAggregate> {
        self.children
            .into_iter()
            .map(|(label, node)| {
                let relative_dir = if parent_dir.is_empty() {
                    label.clone()
                } else {
                    format!("{parent_dir}/{label}")
                };
                let count = node.count;
                let children = node.into_aggregates(&relative_dir);
                FolderAggregate {
                    label,
                    relative_dir,
                    count,
                    children,
                }
            })
            .collect()
    }
}

/// Builds a nested folder aggregation over `hits`' own paths, following the Knowledge wiki
/// landing view's nested-folder aggregation, applied to a result set instead of a fixed folder
/// A hit's own file segment is dropped; only its containing directories are counted.
#[must_use]
pub fn aggregate_by_folder(hits: &[SearchHit]) -> Vec<FolderAggregate> {
    let mut root = AggregateNode::empty();
    for hit in hits {
        let mut segments: Vec<&str> = hit.path.split('/').collect();
        segments.pop();
        if !segments.is_empty() {
            root.insert(&segments);
        }
    }
    root.into_aggregates("")
}

// ---------------------------------------------------------------------
// Selected result's link panels
// ---------------------------------------------------------------------

/// What a link in a panel reaches.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LinkTarget {
    /// A Markdown note.
    Note,
    /// An existing file that is not a Markdown note: an image, a PDF, a canvas, a base.
    File,
    /// The link names no file.
    NotFound,
    /// The link names several files, none nearer the linking note than the others.
    Ambiguous(Vec<String>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinkedNote {
    pub path: String,
    pub title: String,
    pub is_embed: bool,
    pub target: LinkTarget,
}

fn note_title_from_path(path: &str) -> String {
    let filename = path.rsplit('/').next().unwrap_or(path);
    filename.strip_suffix(".md").unwrap_or(filename).to_owned()
}

#[derive(Debug, Deserialize)]
struct ObsidianLinkResult {
    target: String,
    #[serde(default)]
    embed: bool,
    #[serde(default)]
    resolved: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UnresolvedLinkResult {
    target: String,
    kind: String,
    #[serde(default)]
    candidates: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct LinksReadResult {
    outgoing: Vec<ObsidianLinkResult>,
    #[serde(default)]
    unresolved: Vec<UnresolvedLinkResult>,
}

/// The panel's links out of a note, one per distinct target: a link that resolves shows the file it reaches (a note or
/// another kind of file), and one that does not shows its target as written and why. Links to a folder's managed
/// `index.md` are left out, as they are in every other link list.
impl From<LinksReadResult> for Vec<LinkedNote> {
    fn from(value: LinksReadResult) -> Self {
        let mut seen = std::collections::HashSet::new();
        let mut panel = Vec::new();
        for link in value.outgoing {
            let (path, target) = match link.resolved {
                Some(resolved) if is_markdown_path(&resolved) => (resolved, LinkTarget::Note),
                Some(resolved) => (resolved, LinkTarget::File),
                None => match value.unresolved.iter().find(|entry| entry.target == link.target) {
                    Some(entry) if entry.kind == "ambiguous" => {
                        (link.target, LinkTarget::Ambiguous(entry.candidates.clone()))
                    }
                    Some(_) => (link.target, LinkTarget::NotFound),
                    // A template placeholder: neither resolved nor broken, so there is nothing to show.
                    None => continue,
                },
            };
            if crate::vault_links::is_managed_index(&path) || !seen.insert((path.clone(), link.embed)) {
                continue;
            }
            panel.push(LinkedNote {
                title: note_title_from_path(&path),
                path,
                is_embed: link.embed,
                target,
            });
        }
        panel
    }
}

fn is_markdown_path(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

/// What this file links out to (`links_read`, `direction: out`, the default): notes and other files that resolve, and
/// links that name no file or several, shown as such rather than left out. Embeds are kept, since "outbound links" is
/// not restricted to non-embed links.
///
/// # Errors
///
/// See [`search`].
pub async fn outbound_links(client: &McpClient, vault_name: &str, path: &str) -> Result<Vec<LinkedNote>, SearchError> {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{path}")));
    let result = client.call_tool("links_read".to_owned(), args).await?;
    if result.is_error == Some(true) {
        return Err(tool_error(result));
    }
    let parsed: LinksReadResult = result.into_typed().map_err(|_| SearchError::Tool {
        code: "search/malformed-response".to_owned(),
        message: "the link panel could not be read".to_owned(),
    })?;
    Ok(Vec::from(parsed))
}

#[derive(Debug, Deserialize)]
struct GraphNodeResult {
    path: String,
    title: String,
    #[serde(default)]
    phantom: bool,
}

#[derive(Debug, Deserialize)]
struct GraphEdgeResult {
    from: String,
    to: String,
    #[serde(default)]
    kind: String,
}

#[derive(Debug, Deserialize)]
struct GraphViewResult {
    nodes: Vec<GraphNodeResult>,
    edges: Vec<GraphEdgeResult>,
}

/// Notes that resolve and link (or embed) directly to this file (`query_graph`,
/// `operation: backlinks`). `links_read` with `direction: in` reports the same links, but as bare sources, lines, and
/// forms, whereas `backlinks` also carries each linking note's title, which the panel shows. Phantom (unresolved-target)
/// nodes are excluded, matching the resolved-links-only scope.
///
/// Unlike every other tool this module calls, `query_graph`'s `from`/`to` are bare
/// portable-relative paths, not `{vault}://{path}` (`vault` selects the vault as its own,
/// separate parameter instead - confirmed against `insights::load_backlinks`'s identical,
/// already-working call shape).
///
/// Degrades to an empty list, never an error, whenever the graph has nothing to say for this
/// path: disabled, stale enough that this note is not yet a known node, or the note genuinely
/// has no backlinks - all three are "nothing to show" from an operator's point of view, matching
/// `insights::load_backlinks`'s identical reasoning. Only the transport itself failing is a real
/// error here.
///
/// # Errors
///
/// Returns [`SearchError::Call`] when the MCP transport itself fails.
pub async fn inbound_links(client: &McpClient, vault_name: &str, path: &str) -> Result<Vec<LinkedNote>, SearchError> {
    let mut args = Map::new();
    args.insert("operation".to_owned(), Value::String("backlinks".to_owned()));
    args.insert("from".to_owned(), Value::String(path.to_owned()));
    args.insert("vault".to_owned(), Value::String(format!("{vault_name}://.")));
    let result = client.call_tool("query_graph".to_owned(), args).await?;
    if result.is_error == Some(true) {
        return Ok(Vec::new());
    }
    let Ok(parsed) = result.into_typed::<GraphViewResult>() else {
        return Ok(Vec::new());
    };
    let titles: std::collections::HashMap<&str, (&str, bool)> = parsed
        .nodes
        .iter()
        .map(|node| (node.path.as_str(), (node.title.as_str(), node.phantom)))
        .collect();
    let mut seen = std::collections::HashSet::new();
    Ok(parsed
        .edges
        .into_iter()
        .filter(|edge| edge.to == path)
        .filter(|edge| !crate::vault_links::is_managed_index(&edge.from))
        .filter_map(|edge| {
            let (title, phantom) = titles.get(edge.from.as_str())?;
            if *phantom || !seen.insert(edge.from.clone()) {
                return None;
            }
            Some(LinkedNote {
                is_embed: edge.kind.ends_with("embed"),
                path: edge.from,
                title: (*title).to_owned(),
                target: LinkTarget::Note,
            })
        })
        .collect())
}

#[cfg(test)]
#[path = "search_test.rs"]
mod tests;
