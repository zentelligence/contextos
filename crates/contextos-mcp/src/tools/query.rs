//! `query_text`, `query_semantic`, `query_graph`, `query_index_status`,
//! and `query_index_rebuild`: the read-only search layer over a vault's
//! text, semantic, and link-graph indexes, plus the `search_service`
//! helper and streamed-progress plumbing shared by these tools.

use std::sync::Arc;

use contextos_core::{VaultPath, VaultPathInput};
use contextos_search::{
    DescribedSemanticHit, DescribedTextHit, GraphDirection, GraphEdge, GraphNode, GraphNodeKind, GraphRebuildReport,
    GraphView, RebuildProgress, RebuildReport, RebuildTarget, ResultControls, SearchError, SemanticQuery,
    SemanticRebuildReport, TextQuery, VaultSearchService,
};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{ProgressNotificationParam, ProgressToken};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, schemars, tool};
use serde::{Deserialize, Serialize};

use crate::resource_support::{fallible_output_schema_for, optional_u64_schema};
use crate::server::ContextOsServer;
use crate::tool_error::{ToolError, ToolFailure, evaluate, execute};
use crate::tools::index_status::{IndexesStatusToolResult, StoreRecoveryToolResult};
use crate::tools::query_controls::{ControlsArgs, HitMetadataToolResult, SortKeyInput, SortOrderInput};
use crate::tools::query_links::{
    LinkFormToolResult, LinkReportArgs, LinkReportRequest, LinkScope, LinkScopeInput, UnlinkedNoteToolResult,
    UnresolvedLinkToolResult,
};

#[rmcp::tool_router(router = query_tool_router, vis = "pub(crate)")]
impl ContextOsServer {
    #[tool(
        name = "query_text",
        description = "Ranked full-text search across vault markdown with path, tag, frontmatter, and note type filters; pages results with offset and limit (a page must end within the first 200 results), sorts by relevance, title, or modified, and drops results below a minimum relevance (0 to 1, the fraction of the best hit); each hit carries the note's frontmatter description, summary, excerpt, status, updated, and type; exclude_paths omits given path prefixes (e.g. a superseded version) from results",
        output_schema = fallible_output_schema_for::<QueryTextToolResult>()
    )]
    async fn query_text(
        &self,
        Parameters(input): Parameters<QueryTextInput>,
    ) -> Result<Json<QueryTextToolResult>, ToolFailure> {
        let (service, _index) = self.search_service(input.vault.as_deref()).await?;
        execute(move || {
            let limit = usize::try_from(input.limit).unwrap_or(usize::MAX);
            let controls = ResultControls::try_from(ControlsArgs {
                offset: input.offset,
                sort: input.sort,
                order: input.order,
                min_relevance: input.min_relevance,
                note_type: input.note_type.as_deref(),
            })?;
            let request = TextQuery {
                query: &input.query,
                path_prefix: input.path_prefix.as_deref(),
                exclude_paths: &input.exclude_paths,
                tags: &input.tags,
                fields: &input.fields,
                limit,
            };
            let results = service.query_text_page(&request, &controls)?;
            Ok(QueryTextToolResult {
                hits: results.hits.into_iter().map(TextHitToolResult::from).collect(),
                total: results.total,
                offset: controls.offset,
                index_freshness: IndexFreshnessToolResult::from(results.freshness),
            })
        })
        .await
    }

    #[tool(
        name = "query_semantic",
        description = "Vector similarity search over chunked note content; requires [vault.search] semantic = true; ranks the 200 most similar chunks, so a page must end within them, and pages with offset and limit, sorts by relevance, title, or modified, and drops results below a minimum relevance (cosine similarity from 0 to 1); note_type keeps only notes of that frontmatter type; each hit carries the note's frontmatter metadata; exclude_paths omits given path prefixes (e.g. a superseded version) from results",
        output_schema = fallible_output_schema_for::<QuerySemanticToolResult>()
    )]
    async fn query_semantic(
        &self,
        Parameters(input): Parameters<QuerySemanticInput>,
    ) -> Result<Json<QuerySemanticToolResult>, ToolFailure> {
        let (service, _index) = self.search_service(input.vault.as_deref()).await?;
        execute(move || {
            let limit = usize::try_from(input.limit).unwrap_or(usize::MAX);
            let controls = ResultControls::try_from(ControlsArgs {
                offset: input.offset,
                sort: input.sort,
                order: input.order,
                min_relevance: input.min_relevance,
                note_type: input.note_type.as_deref(),
            })?;
            let request = SemanticQuery {
                query: &input.query,
                limit,
                path_prefix: input.path_prefix.as_deref(),
                exclude_paths: &input.exclude_paths,
            };
            let results = service.query_semantic_page(&request, &controls)?;
            Ok(QuerySemanticToolResult {
                hits: results.hits.into_iter().map(SemanticHitToolResult::from).collect(),
                total: results.total,
                offset: controls.offset,
                total_capped: results.total_capped,
            })
        })
        .await
    }

    #[tool(
        name = "query_graph",
        description = "Traverse the link graph (wikilinks and Markdown links) or check its integrity. Traversal: neighbours, backlinks, shortest path, or orphans (notes with no links in or out). Integrity, complete and paged with offset and limit (default 200, at most 1000) and limited to a folder by path: unresolved lists every link that finds no file (not-found), finds several (ambiguous, with candidates), or sits in a note whose links cannot be read (unreadable), each with its source, line, text, target, and form; unlinked lists the notes no other note links to. Both skip the folders `[vault.links] exclude` names as link sources, unresolved also leaves out a link whose target lies in a folder `[vault.links] ignore_targets` names (an ambiguous link is still listed), and the graph is brought up to date with outside edits before it answers",
        output_schema = fallible_output_schema_for::<GraphViewToolResult>()
    )]
    async fn query_graph(
        &self,
        Parameters(input): Parameters<QueryGraphInput>,
    ) -> Result<Json<GraphViewToolResult>, ToolFailure> {
        let (service, index) = self.search_service(input.vault.as_deref()).await?;
        let roots = Arc::clone(&self.roots);
        let filesystem = Arc::clone(&self.filesystem);
        execute(move || {
            let operation = input.operation;
            let integrity = matches!(
                operation,
                GraphOperationInput::Unresolved | GraphOperationInput::Unlinked
            );
            if integrity {
                let name = operation.name();
                if input.from.is_some() || input.to.is_some() || input.depth.is_some() || input.direction.is_some() {
                    return Err(ToolError::from(SearchError::InvalidLinkReport {
                        operation: name,
                        reason: "from, to, depth, and direction belong to the traversal operations".to_owned(),
                    }));
                }
                let request = LinkReportRequest::try_from(LinkReportArgs {
                    operation: name,
                    offset: input.offset,
                    limit: input.limit,
                })?;
                let scope = input
                    .path
                    .as_deref()
                    .map(|raw| {
                        LinkScope::try_from(LinkScopeInput {
                            operation: name,
                            raw,
                            roots: &roots,
                            vault: index,
                            filesystem: &filesystem,
                        })
                    })
                    .transpose()?;
                let scope = scope.as_ref().map(LinkScope::path);
                return Ok(if matches!(operation, GraphOperationInput::Unresolved) {
                    let (links, total) = request.page(service.unresolved_links(scope)?);
                    GraphViewToolResult::links(links, total, request.offset)
                } else {
                    let (notes, total) = request.page(service.unlinked_notes(scope)?);
                    GraphViewToolResult::notes(notes, total, request.offset)
                });
            }
            if input.path.is_some() || input.offset.is_some() || input.limit.is_some() {
                return Err(ToolError::from(SearchError::InvalidLinkReport {
                    operation: operation.name(),
                    reason: "path, offset, and limit belong to the unresolved and unlinked operations".to_owned(),
                }));
            }
            let direction = GraphDirection::from(input.direction.unwrap_or_default());
            let depth = input.depth.unwrap_or(1);
            let view = match operation {
                GraphOperationInput::Neighbours => {
                    let from = input
                        .from
                        .ok_or(ToolError::Invalid("query_graph 'neighbours' requires 'from'"))?;
                    service.graph_neighbours(&from, depth, direction)?
                }
                GraphOperationInput::Backlinks => {
                    let from = input
                        .from
                        .ok_or(ToolError::Invalid("query_graph 'backlinks' requires 'from'"))?;
                    service.graph_backlinks(&from)?
                }
                GraphOperationInput::Path => {
                    let from = input
                        .from
                        .ok_or(ToolError::Invalid("query_graph 'path' requires 'from'"))?;
                    let to = input.to.ok_or(ToolError::Invalid("query_graph 'path' requires 'to'"))?;
                    service.graph_path(&from, &to, direction)?
                }
                GraphOperationInput::Orphans => service.graph_orphans()?,
                GraphOperationInput::Unresolved | GraphOperationInput::Unlinked => {
                    return Err(ToolError::Invalid(
                        "query_graph integrity operations are answered above",
                    ));
                }
            };
            Ok(GraphViewToolResult::from(view))
        })
        .await
    }

    #[tool(
        name = "query_index_status",
        description = "Report per-index document counts, staleness estimate, last build time, and health, naming why any index cannot be read and where a damaged one was moved aside",
        output_schema = fallible_output_schema_for::<IndexesStatusToolResult>()
    )]
    async fn query_index_status(
        &self,
        Parameters(input): Parameters<QueryVaultInput>,
    ) -> Result<Json<IndexesStatusToolResult>, ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let search = Arc::clone(&self.search);
        let raw = input.vault.unwrap_or_else(|| ".".to_owned());
        let path = evaluate(move || {
            VaultPath::try_from(VaultPathInput {
                roots: &roots,
                raw: &raw,
            })
            .map_err(ToolError::from)
        })
        .await?;
        let index = usize::try_from(path.root_id()).map_err(ToolError::from)?;
        let service = search.get(index).cloned().flatten();
        // An index that cannot be read, or a vault that cannot be scanned, is reported in the result rather than as a
        // failed call, the same way `vault_info` reports it: status never fails.
        execute(move || {
            Ok(match service {
                Some(service) => IndexesStatusToolResult::from(service.status()),
                None => IndexesStatusToolResult::default(),
            })
        })
        .await
    }

    /// Rebuilds the requested indexes. Text and graph are each a single
    /// fast pass and always run to completion; the semantic phase (local
    /// CPU embedding of a vault, which can run far longer than a typical
    /// request timeout) is budgeted (`budget_seconds`, defaulting to this
    /// vault's configured `[vault.search] rebuild_budget_seconds`) and
    /// returns early with partial progress once the budget elapses, leaving
    /// the rest queued. The
    /// result's `semantic.remaining` is the continuation signal: nonzero
    /// means call this tool again (same `index`) to keep going, zero means
    /// the semantic index is fully caught up. This is the primary mechanism
    /// for a long semantic rebuild, since it works even for a client (such
    /// as Claude Cowork) that never sends `_meta.progressToken` and so
    /// cannot receive the MCP progress notifications also streamed here for
    /// each phase boundary and, during the semantic phase, each completed
    /// document.
    #[tool(
        name = "query_index_rebuild",
        description = "Rebuild the text index, link graph, and/or semantic index from a full vault scan. The semantic phase returns early after `budget_seconds` (default from [vault.search] rebuild_budget_seconds, 25 if unset) with partial progress rather than blocking until finished: call again with the same `index` while the result's `semantic.remaining` is nonzero.",
        output_schema = fallible_output_schema_for::<QueryIndexRebuildToolResult>()
    )]
    async fn query_index_rebuild(
        &self,
        context: RequestContext<RoleServer>,
        Parameters(input): Parameters<QueryIndexRebuildInput>,
    ) -> Result<Json<QueryIndexRebuildToolResult>, ToolFailure> {
        let (service, index) = self.search_service(input.vault.as_deref()).await?;
        let progress_token = context.meta.get_progress_token();
        let peer = context.peer;
        let configured_default = self
            .rebuild_budget_seconds
            .get(index)
            .copied()
            .unwrap_or(FALLBACK_REBUILD_BUDGET_SECONDS);
        let budget = resolve_rebuild_budget(input.budget_seconds, configured_default);

        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel::<RebuildProgress>();
        let forwarder = tokio::spawn(async move {
            let mut sequence = 0.0_f64;
            while let Some(update) = progress_rx.recv().await {
                sequence += 1.0;
                if let Some(token) = &progress_token {
                    let _ = peer
                        .notify_progress(rebuild_progress_notification(token.clone(), sequence, update))
                        .await;
                }
            }
        });

        let result = execute(move || {
            let report =
                service.rebuild_with_budget(RebuildTarget::from(input.index), Some(budget), &mut |update| {
                    let _ = progress_tx.send(update);
                })?;
            Ok(QueryIndexRebuildToolResult::from(report))
        })
        .await;

        let _ = forwarder.await;
        result
    }
}

impl ContextOsServer {
    /// Resolves the vault search service for `vault` (or the sole
    /// configured vault when `None`), matching the Git tools' `vault`
    /// parameter convention: default to the sole vault, else required.
    /// Also returns the resolved vault's index, for callers (currently only
    /// `query_index_rebuild`) that need it to look up other per-vault
    /// configuration alongside the service itself.
    pub(crate) async fn search_service(
        &self,
        vault: Option<&str>,
    ) -> Result<(Arc<VaultSearchService>, usize), ToolFailure> {
        let roots = Arc::clone(&self.roots);
        let raw = vault.unwrap_or(".").to_owned();
        let path = evaluate(move || {
            VaultPath::try_from(VaultPathInput {
                roots: &roots,
                raw: &raw,
            })
            .map_err(ToolError::from)
        })
        .await?;
        let index = usize::try_from(path.root_id()).map_err(ToolError::from)?;
        let service = self
            .search
            .get(index)
            .cloned()
            .flatten()
            .ok_or_else(|| ToolFailure::from(ToolError::SearchDisabled))?;
        Ok((service, index))
    }
}

/// Renders one [`RebuildProgress`] update as an MCP progress notification.
/// `progress` is a monotonically increasing sequence number (every update
/// counts, per the MCP spec's "this should increase every time progress is
/// made"); `total`/`message` carry the actually meaningful detail, which is
/// only precise for the semantic phase's per-document count.
fn rebuild_progress_notification(
    token: ProgressToken,
    sequence: f64,
    update: RebuildProgress,
) -> ProgressNotificationParam {
    let message = match update {
        RebuildProgress::TextStarted => "rebuilding text index".to_owned(),
        RebuildProgress::TextComplete => "text index rebuilt".to_owned(),
        RebuildProgress::GraphStarted => "rebuilding link graph".to_owned(),
        RebuildProgress::GraphComplete => "link graph rebuilt".to_owned(),
        RebuildProgress::SemanticProgress { completed, total } => {
            format!("embedded {completed} of {total} documents")
        }
    };
    let param = ProgressNotificationParam::new(token, sequence).with_message(message);
    match update {
        RebuildProgress::SemanticProgress { total, .. } => {
            #[allow(clippy::cast_precision_loss)]
            let total = total as f64;
            param.with_total(total)
        }
        _ => param,
    }
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct QueryVaultInput {
    /// The vault to report on: `{name}://.` to select a specific
    /// configured vault by name (a bare vault name alone is not
    /// accepted for this parameter); omit to use the sole configured
    /// vault.
    vault: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct QueryTextInput {
    query: String,
    path_prefix: Option<String>,
    /// Forward-slash relative path prefixes to exclude from results,
    /// matching whole path segments like `path_prefix`: an entry `"old"`
    /// excludes `"old"` and everything under `"old/"`, never
    /// `"oldstuff.md"`. Useful for excluding a superseded version of
    /// something (the previous draft, an archived copy) so it cannot bias
    /// or be mistaken for the current one while rewriting or researching.
    #[serde(default)]
    exclude_paths: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    fields: serde_json::Map<String, serde_json::Value>,
    /// The page size, at most 100. A page must end within the first 200
    /// results, so `offset + limit` cannot exceed 200. `0` returns only the
    /// total.
    #[serde(default = "default_query_text_limit")]
    limit: u64,
    /// Number of ranked results to skip.
    #[serde(default)]
    offset: u64,
    sort: Option<SortKeyInput>,
    order: Option<SortOrderInput>,
    /// Drops hits whose relevance (the score as a fraction of the best
    /// hit's, from 0 to 1) is below this value.
    min_relevance: Option<f64>,
    /// Keeps only notes whose frontmatter `type` equals this value.
    note_type: Option<String>,
    /// The vault to search: `{name}://.` to select a specific configured
    /// vault by name (a bare vault name alone is not accepted
    /// for this parameter); omit to use the sole configured vault.
    vault: Option<String>,
}

const fn default_query_text_limit() -> u64 {
    20
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct QueryTextToolResult {
    hits: Vec<TextHitToolResult>,
    /// Every note matching the query and controls, not only this page.
    total: usize,
    offset: usize,
    index_freshness: IndexFreshnessToolResult,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct TextHitToolResult {
    path: String,
    /// The raw full-text score, comparable only within one result set.
    score: f32,
    /// The score as a fraction of the best hit's, from 0 to 1.
    relevance: f32,
    title: String,
    snippet: String,
    modified: String,
    metadata: HitMetadataToolResult,
}

impl From<DescribedTextHit> for TextHitToolResult {
    fn from(value: DescribedTextHit) -> Self {
        Self {
            metadata: HitMetadataToolResult::from(&value.metadata),
            path: value.hit.path,
            score: value.hit.score,
            relevance: value.hit.relevance,
            title: value.hit.title,
            snippet: value.hit.snippet,
            modified: value.hit.modified.to_string(),
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
pub(super) struct IndexFreshnessToolResult {
    scanned: usize,
    reindexed: usize,
    removed: usize,
}

impl From<contextos_search::FreshnessReport> for IndexFreshnessToolResult {
    fn from(value: contextos_search::FreshnessReport) -> Self {
        Self {
            scanned: value.scanned,
            reindexed: value.reindexed,
            removed: value.removed,
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct QuerySemanticInput {
    query: String,
    /// The page size, at most 100. A page must end within the 200 most
    /// similar chunks, so `offset + limit` cannot exceed 200. `0` returns
    /// only the total.
    #[serde(default = "default_query_semantic_limit")]
    limit: u64,
    /// Number of ranked results to skip.
    #[serde(default)]
    offset: u64,
    path_prefix: Option<String>,
    /// Forward-slash relative path prefixes to exclude from results,
    /// matching whole path segments like `path_prefix`: an entry `"old"`
    /// excludes `"old"` and everything under `"old/"`, never
    /// `"oldstuff.md"`. Useful for excluding a superseded version of
    /// something (the previous draft, an archived copy) so it cannot bias
    /// or be mistaken for the current one while rewriting or researching.
    #[serde(default)]
    exclude_paths: Vec<String>,
    sort: Option<SortKeyInput>,
    order: Option<SortOrderInput>,
    /// Drops hits whose cosine similarity, clamped to 0 to 1, is below this
    /// value.
    min_relevance: Option<f64>,
    /// Keeps only notes whose frontmatter `type` equals this value.
    note_type: Option<String>,
    /// The vault to search: `{name}://.` to select a specific configured
    /// vault by name (a bare vault name alone is not accepted
    /// for this parameter); omit to use the sole configured vault.
    vault: Option<String>,
}

const fn default_query_semantic_limit() -> u64 {
    10
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct QuerySemanticToolResult {
    hits: Vec<SemanticHitToolResult>,
    /// Chunks among the 200 most similar that pass the controls.
    total: usize,
    offset: usize,
    /// True when the 200-chunk candidate pool was full, so more matches
    /// exist than `total` counts.
    total_capped: bool,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct SemanticHitToolResult {
    path: String,
    chunk: String,
    /// Cosine similarity, from -1 to 1.
    score: f32,
    /// The similarity clamped to 0 to 1.
    relevance: f32,
    heading_context: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    modified: Option<String>,
    metadata: HitMetadataToolResult,
}

impl From<DescribedSemanticHit> for SemanticHitToolResult {
    fn from(value: DescribedSemanticHit) -> Self {
        Self {
            metadata: HitMetadataToolResult::from(&value.metadata),
            title: value.metadata.title,
            modified: value.metadata.modified.map(|modified| modified.to_string()),
            path: value.hit.path,
            chunk: value.hit.chunk,
            score: value.hit.score,
            relevance: value.hit.relevance,
            heading_context: value.hit.heading_context,
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct QueryGraphInput {
    operation: GraphOperationInput,
    from: Option<String>,
    to: Option<String>,
    /// Traversal operations only: how many hops to follow, 1 to 4. Default 1.
    depth: Option<u32>,
    /// Traversal operations only: `out`, `in`, or `both` (default).
    direction: Option<GraphDirectionInput>,
    /// `unresolved` and `unlinked` only: an existing folder of the selected vault, such as `memory/wiki`, to limit the
    /// report to. It is resolved like any other `path` parameter (a `{name}://` prefix is accepted when it names the
    /// selected vault), and a folder that does not exist is refused.
    path: Option<String>,
    /// `unresolved` and `unlinked` only: how many entries to skip. Default 0.
    offset: Option<i64>,
    /// `unresolved` and `unlinked` only: how many entries to return, at most 1000. Default 200.
    limit: Option<i64>,
    /// The vault to query: `{name}://.` to select a specific configured
    /// vault by name (a bare vault name alone is not accepted
    /// for this parameter); omit to use the sole configured vault.
    vault: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum GraphOperationInput {
    Neighbours,
    Backlinks,
    Path,
    Orphans,
    Unresolved,
    Unlinked,
}

impl GraphOperationInput {
    /// The operation's name as the caller wrote it.
    const fn name(self) -> &'static str {
        match self {
            Self::Neighbours => "neighbours",
            Self::Backlinks => "backlinks",
            Self::Path => "path",
            Self::Orphans => "orphans",
            Self::Unresolved => "unresolved",
            Self::Unlinked => "unlinked",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum GraphDirectionInput {
    Out,
    In,
    #[default]
    Both,
}

impl From<GraphDirectionInput> for GraphDirection {
    fn from(value: GraphDirectionInput) -> Self {
        match value {
            GraphDirectionInput::Out => Self::Out,
            GraphDirectionInput::In => Self::In,
            GraphDirectionInput::Both => Self::Both,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct GraphViewToolResult {
    /// Traversal operations: the nodes touched. Absent for `unresolved` and `unlinked`.
    #[serde(skip_serializing_if = "Option::is_none")]
    nodes: Option<Vec<GraphNodeToolResult>>,
    /// Traversal operations: the links between them. Absent for `unresolved` and `unlinked`.
    #[serde(skip_serializing_if = "Option::is_none")]
    edges: Option<Vec<GraphEdgeToolResult>>,
    /// `unresolved` only: the requested page of links that do not resolve, sorted by source then line.
    #[serde(skip_serializing_if = "Option::is_none")]
    links: Option<Vec<UnresolvedLinkToolResult>>,
    /// `unlinked` only: the requested page of notes no other note links to, sorted by path.
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<Vec<UnlinkedNoteToolResult>>,
    /// `unresolved` and `unlinked` only: how many entries there are in all, whatever the page.
    #[serde(skip_serializing_if = "Option::is_none")]
    total: Option<usize>,
    /// `unresolved` and `unlinked` only: how many entries the page skipped.
    #[serde(skip_serializing_if = "Option::is_none")]
    offset: Option<usize>,
}

impl GraphViewToolResult {
    fn links(links: Vec<contextos_search::UnresolvedLink>, total: usize, offset: usize) -> Self {
        Self {
            nodes: None,
            edges: None,
            links: Some(links.into_iter().map(UnresolvedLinkToolResult::from).collect()),
            notes: None,
            total: Some(total),
            offset: Some(offset),
        }
    }

    fn notes(notes: Vec<contextos_search::UnlinkedNote>, total: usize, offset: usize) -> Self {
        Self {
            nodes: None,
            edges: None,
            links: None,
            notes: Some(notes.into_iter().map(UnlinkedNoteToolResult::from).collect()),
            total: Some(total),
            offset: Some(offset),
        }
    }
}

impl From<GraphView> for GraphViewToolResult {
    fn from(value: GraphView) -> Self {
        Self {
            nodes: Some(value.nodes.into_iter().map(GraphNodeToolResult::from).collect()),
            edges: Some(value.edges.into_iter().map(GraphEdgeToolResult::from).collect()),
            links: None,
            notes: None,
            total: None,
            offset: None,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct GraphNodeToolResult {
    path: String,
    title: String,
    /// `true` for a link target that resolved to no file or to several; `kind` says which.
    phantom: bool,
    /// `note`, `file` (an existing file that is not an indexed note), `not-found`, or `ambiguous`.
    kind: GraphNodeKindToolResult,
    /// For an `ambiguous` node, every file the link target answers to, sorted; absent for any other kind.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    candidates: Vec<String>,
    /// For a note whose links could not be read, why: it names a line and a fault, never the note's text. Such a note
    /// has no outgoing links in the graph. Absent for every other node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    unreadable: Option<String>,
}

impl From<GraphNode> for GraphNodeToolResult {
    fn from(value: GraphNode) -> Self {
        let phantom = value.phantom();
        Self {
            path: value.path,
            title: value.title,
            phantom,
            kind: GraphNodeKindToolResult::from(value.kind),
            candidates: value.candidates,
            unreadable: value.unreadable,
        }
    }
}

/// What a graph node stands for. The variants carry no doc comments: they would turn the schema into a `oneOf`, which
/// the tool registry of some hosts rejects.
#[derive(Clone, Copy, Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
enum GraphNodeKindToolResult {
    Note,
    File,
    NotFound,
    Ambiguous,
}

impl From<GraphNodeKind> for GraphNodeKindToolResult {
    fn from(value: GraphNodeKind) -> Self {
        match value {
            GraphNodeKind::Note => Self::Note,
            GraphNodeKind::File => Self::File,
            GraphNodeKind::NotFound => Self::NotFound,
            GraphNodeKind::Ambiguous => Self::Ambiguous,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct GraphEdgeToolResult {
    from: String,
    to: String,
    /// How the link was written: `wikilink`, `wikilink-embed`, `markdown`, or `markdown-embed`.
    kind: LinkFormToolResult,
}

impl From<GraphEdge> for GraphEdgeToolResult {
    fn from(value: GraphEdge) -> Self {
        Self {
            from: value.from,
            to: value.to,
            kind: LinkFormToolResult::from(value.form),
        }
    }
}

/// Safety-net fallback for [`ContextOsServer::rebuild_budget_seconds`] if a
/// vault index is ever out of range against that lookup table (should not
/// happen in practice: it is built with exactly one entry per configured
/// vault). Same value as `[vault.search] rebuild_budget_seconds`'s own
/// config default, so this is never a user-visible behaviour change.
const FALLBACK_REBUILD_BUDGET_SECONDS: u64 = 25;

/// Resolves the effective semantic-rebuild budget: an explicit per-call
/// `requested` value always wins; otherwise falls back to
/// `configured_default` (this vault's `[vault.search]
/// rebuild_budget_seconds`). A `requested`/`configured_default` value too
/// large to represent as `i64` seconds falls back to `i64::MAX` seconds
/// (effectively unbounded) rather than panicking.
fn resolve_rebuild_budget(requested: Option<u64>, configured_default: u64) -> time::Duration {
    let seconds = requested.unwrap_or(configured_default);
    time::Duration::seconds(i64::try_from(seconds).unwrap_or(i64::MAX))
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct QueryIndexRebuildInput {
    #[serde(default)]
    index: RebuildIndexInput,
    /// The vault to rebuild: `{name}://.` to select a specific configured
    /// vault by name (a bare vault name alone is not accepted
    /// for this parameter); omit to use the sole configured vault.
    vault: Option<String>,
    /// Maximum seconds the semantic phase may spend embedding documents
    /// before returning early with partial progress; ignored by the text
    /// and graph phases, which always run to completion. Defaults to this
    /// vault's configured `[vault.search] rebuild_budget_seconds` when
    /// omitted.
    // `#[serde(default)]` is required alongside `schema_with` here: the
    // schema-generation macro derives whether a field counts as "optional"
    // (and thus absent from the schema's `required` array) from the same
    // overridden schema type `schema_with` supplies, and a bare function
    // override's wrapper type is not itself recognised as an `Option`.
    // Without this, budget_seconds would be incorrectly marked required.
    #[serde(default)]
    #[schemars(schema_with = "optional_u64_schema")]
    budget_seconds: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum RebuildIndexInput {
    Text,
    Graph,
    Semantic,
    #[default]
    All,
}

impl From<RebuildIndexInput> for RebuildTarget {
    fn from(value: RebuildIndexInput) -> Self {
        match value {
            RebuildIndexInput::Text => Self::Text,
            RebuildIndexInput::Graph => Self::Graph,
            RebuildIndexInput::Semantic => Self::Semantic,
            RebuildIndexInput::All => Self::All,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct QueryIndexRebuildToolResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<IndexFreshnessToolResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    graph: Option<GraphRebuildToolResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    semantic: Option<SemanticRebuildToolResult>,
    /// Every store this call found damaged and moved aside before rebuilding it, with where the damaged files were
    /// kept; omitted when there were none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    recovered: Vec<StoreRecoveryToolResult>,
}

impl From<RebuildReport> for QueryIndexRebuildToolResult {
    fn from(value: RebuildReport) -> Self {
        Self {
            text: value.text.map(IndexFreshnessToolResult::from),
            graph: value.graph.map(GraphRebuildToolResult::from),
            semantic: value.semantic.map(SemanticRebuildToolResult::from),
            recovered: value.recovered.iter().map(StoreRecoveryToolResult::from).collect(),
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct GraphRebuildToolResult {
    notes_scanned: usize,
    nodes: usize,
    edges: usize,
}

impl From<GraphRebuildReport> for GraphRebuildToolResult {
    fn from(value: GraphRebuildReport) -> Self {
        Self {
            notes_scanned: value.notes_scanned,
            nodes: value.nodes,
            edges: value.edges,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct SemanticRebuildToolResult {
    paths_scanned: usize,
    embedded: usize,
    skipped: usize,
    failed: usize,
    /// Documents still queued for embedding once this call returned.
    /// Nonzero means the rebuild budget was reached before the queue was
    /// empty: call `query_index_rebuild` again with the same `index` to
    /// continue. Zero means the semantic index is fully caught up.
    remaining: usize,
    /// Human-readable summary of `remaining`, meant for a tool-calling
    /// agent deciding whether to call this tool again.
    message: String,
}

impl From<SemanticRebuildReport> for SemanticRebuildToolResult {
    fn from(value: SemanticRebuildReport) -> Self {
        let message = if value.remaining > 0 {
            format!(
                "Embedded {} document(s) this pass; {} still pending. Call query_index_rebuild again with the same index target to continue.",
                value.paths_scanned, value.remaining
            )
        } else {
            "Semantic index is fully up to date.".to_owned()
        };
        Self {
            paths_scanned: value.paths_scanned,
            embedded: value.embedded,
            remaining: value.remaining,
            message,
            skipped: value.skipped,
            failed: value.failed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_rebuild_budget;

    #[test]
    fn resolve_rebuild_budget_falls_back_to_the_configured_vault_default_when_omitted() {
        assert_eq!(resolve_rebuild_budget(None, 60), time::Duration::seconds(60));
    }

    #[test]
    fn resolve_rebuild_budget_prefers_an_explicit_per_call_override() {
        assert_eq!(resolve_rebuild_budget(Some(5), 60), time::Duration::seconds(5));
    }

    #[test]
    fn resolve_rebuild_budget_never_panics_on_an_unrepresentable_second_count() {
        assert_eq!(
            resolve_rebuild_budget(None, u64::MAX),
            time::Duration::seconds(i64::MAX)
        );
    }
}
