//! `query_blended`: full-text and semantic search fused into one ranked,
//! file-level list, with the same paging, sorting, relevance, and note type
//! controls as the single-index query tools.

use contextos_search::{BlendedHit, BlendedQuery, MatchMode, ResultControls, SearchMode};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{schemars, tool};
use serde::{Deserialize, Serialize};

use crate::resource_support::fallible_output_schema_for;
use crate::server::{ContextOsServer, WarningMessages};
use crate::tool_error::{ToolFailure, execute};
use crate::tools::query::IndexFreshnessToolResult;
use crate::tools::query_controls::{
    ControlsArgs, HitMetadataToolResult, SearchModeInput, SortKeyInput, SortOrderInput,
};

#[rmcp::tool_router(router = query_blended_tool_router, vis = "pub(crate)")]
impl ContextOsServer {
    #[tool(
        name = "query_blended",
        description = "Blended search: full-text and semantic results fused by reciprocal rank fusion into one file-level ranking, so notes found by both indexes tend to rank above those found by one; mode limits it to text or semantic only; if one index is disabled or failing, or the text side cannot parse the query, results from the other index are returned with a warning instead of failing; pages with offset and limit (a page must end within the first 200 results), sorts by relevance, title, or modified, drops results below a minimum relevance (0 to 1), and keeps only notes of a frontmatter note_type; each hit reports which indexes matched it, a snippet, and the note's frontmatter metadata; path_prefix and exclude_paths scope both indexes",
        output_schema = fallible_output_schema_for::<QueryBlendedToolResult>()
    )]
    async fn query_blended(
        &self,
        Parameters(input): Parameters<QueryBlendedInput>,
    ) -> Result<Json<QueryBlendedToolResult>, ToolFailure> {
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
            let request = BlendedQuery {
                query: &input.query,
                limit,
                path_prefix: input.path_prefix.as_deref(),
                exclude_paths: &input.exclude_paths,
                mode: input.mode.map_or(SearchMode::Blended, SearchMode::from),
            };
            let results = service.query_blended(&request, &controls)?;
            let WarningMessages(warnings) = WarningMessages::from(results.warnings);
            Ok(QueryBlendedToolResult {
                hits: results.hits.into_iter().map(BlendedHitToolResult::from).collect(),
                total: results.total,
                offset: controls.offset,
                total_capped: results.total_capped,
                warnings,
                index_freshness: results.freshness.map(IndexFreshnessToolResult::from),
            })
        })
        .await
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct QueryBlendedInput {
    query: String,
    /// The page size, at most 100. A page must end within the first 200
    /// results, so `offset + limit` cannot exceed 200. `0` returns only the
    /// total.
    #[serde(default = "default_query_blended_limit")]
    limit: u64,
    /// Number of ranked results to skip.
    #[serde(default)]
    offset: u64,
    mode: Option<SearchModeInput>,
    sort: Option<SortKeyInput>,
    order: Option<SortOrderInput>,
    /// Drops hits whose relevance is below this value, from 0 to 1. In
    /// blended mode relevance is the fused rank score as a fraction of the
    /// best a file could score; in text mode it is the score as a fraction
    /// of the best hit's; in semantic mode it is cosine similarity.
    min_relevance: Option<f64>,
    /// Keeps only notes whose frontmatter `type` equals this value.
    note_type: Option<String>,
    path_prefix: Option<String>,
    /// Forward-slash relative path prefixes to exclude from results,
    /// matching whole path segments like `path_prefix`.
    #[serde(default)]
    exclude_paths: Vec<String>,
    /// The vault to search: `{name}://.` to select a specific configured
    /// vault by name (a bare vault name alone is not accepted
    /// for this parameter); omit to use the sole configured vault.
    vault: Option<String>,
}

const fn default_query_blended_limit() -> u64 {
    20
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct QueryBlendedToolResult {
    hits: Vec<BlendedHitToolResult>,
    /// Files passing the controls: exact in text mode, otherwise counted
    /// over the candidate pools.
    total: usize,
    offset: usize,
    /// True when a candidate pool was truncated, so more matches exist than
    /// `total` counts.
    total_capped: bool,
    /// Notes on degraded behaviour, each `code: message`, such as an index
    /// that could not be consulted.
    warnings: Vec<String>,
    /// Present when the text index was consulted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    index_freshness: Option<IndexFreshnessToolResult>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct BlendedHitToolResult {
    path: String,
    title: String,
    /// Fraction of a perfect match, from 0 to 1.
    relevance: f32,
    snippet: String,
    /// The indexes that matched this file: `text`, `semantic`, or both.
    matched_by: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::resource_support::optional_string_schema")]
    modified: Option<String>,
    metadata: HitMetadataToolResult,
}

impl From<BlendedHit> for BlendedHitToolResult {
    fn from(value: BlendedHit) -> Self {
        Self {
            metadata: HitMetadataToolResult::from(&value.metadata),
            modified: value.modified.map(|modified| modified.to_string()),
            path: value.path,
            title: value.title,
            relevance: value.relevance,
            snippet: value.snippet,
            matched_by: value
                .matched_by
                .into_iter()
                .map(|mode| match mode {
                    MatchMode::Text => "text".to_owned(),
                    MatchMode::Semantic => "semantic".to_owned(),
                })
                .collect(),
        }
    }
}
