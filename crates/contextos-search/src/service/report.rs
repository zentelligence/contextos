//! Status and rebuild result types for [`VaultSearchService`].
//!
//! [`VaultSearchService`]: super::VaultSearchService

use std::path::PathBuf;

use serde::Serialize;
use time::OffsetDateTime;

use crate::store::{DerivedStore, StoreRecovery};
use crate::{CatchUpKind, FreshnessReport, SearchError, StoreHealth};

/// Per-index document counts, staleness, and last-build time.
///
/// `state_directory` is the vault's derived-state directory itself (the
/// parent of the text index, link-graph cache, and semantic vector store),
/// surfaced so an operator can confirm a separately invoked `contextos
/// index` and a running server resolve to the same on-disk store rather
/// than two silently diverged ones.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct IndexStatusReport {
    pub state_directory: PathBuf,
    pub text: TextIndexStatus,
    pub graph: GraphIndexStatus,
    pub semantic: SemanticIndexStatus,
    /// Why the vault or its state directory could not be scanned for staleness or build times, when they could not;
    /// each store's own health is reported regardless.
    pub scan_failure: Option<ScanFailure>,
}

/// A failure to scan the vault or its state directory while reporting status, with its stable code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanFailure {
    pub code: &'static str,
    pub message: String,
    pub remediation: &'static str,
}

impl From<&SearchError> for ScanFailure {
    fn from(value: &SearchError) -> Self {
        Self {
            code: value.code(),
            message: value.to_string(),
            remediation: value.remediation(),
        }
    }
}

/// Status of the text index.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TextIndexStatus {
    pub enabled: bool,
    /// Whether the index can be read: `Ready`, or `Unreadable` with the failure, in which case no documents are
    /// counted. `None` when text search is disabled.
    pub health: Option<StoreHealth>,
    /// The most recent time this instance moved a damaged index aside and recreated it.
    pub last_recovery: Option<StoreRecovery>,
    pub documents: usize,
    /// A read-only estimate of files missing from the index or whose
    /// modification time no longer matches the stored entry. Computed
    /// without reindexing; call [`VaultSearchService::rebuild`] to
    /// reconcile.
    ///
    /// [`VaultSearchService::rebuild`]: super::VaultSearchService::rebuild
    pub stale_estimate: usize,
    pub last_build: Option<OffsetDateTime>,
}

/// Status of the link graph.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GraphIndexStatus {
    pub enabled: bool,
    pub nodes: usize,
    pub edges: usize,
    pub needs_rebuild: bool,
    /// What the graph found in its store, or `None` when the graph is disabled. An unreadable store reports its
    /// failure here, with no nodes or edges, rather than failing the status call.
    pub health: Option<StoreHealth>,
    pub last_build: Option<OffsetDateTime>,
    /// The most recent time this instance moved a damaged graph store aside and rebuilt it.
    pub last_recovery: Option<StoreRecovery>,
    /// This vault's link graph's current generation and most recent
    /// cross-instance catch-up kind, or `None` when
    /// the configured `graph_backend` does not track one (`fjall`,
    /// `serde`).
    pub generation: Option<u64>,
    pub last_catch_up: Option<CatchUpKind>,
}

/// Status of the semantic index: `documents` and `chunks`
/// count currently stored vectors; `stale_estimate` is the embedding
/// worker's pending-queue length (paths signalled but not yet processed),
/// matching [`TextIndexStatus::stale_estimate`]'s read-only-estimate
/// contract without a second full-vault scan; `last_build` is when the
/// worker last finished processing a path.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SemanticIndexStatus {
    pub enabled: bool,
    /// Whether the vector store can be read: `Ready`, or `Unreadable` with the failure, in which case no vectors are
    /// counted. `None` when semantic search is disabled.
    pub health: Option<StoreHealth>,
    /// The most recent time this instance moved a damaged vector store aside and queued the vault to be embedded
    /// again.
    pub last_recovery: Option<StoreRecovery>,
    pub documents: usize,
    pub chunks: usize,
    pub stale_estimate: usize,
    pub last_build: Option<OffsetDateTime>,
}

/// Selects which index [`VaultSearchService::rebuild`] repopulates.
///
/// [`VaultSearchService::rebuild`]: super::VaultSearchService::rebuild
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RebuildTarget {
    Text,
    Graph,
    Semantic,
    All,
}

/// One progress update from [`VaultSearchService::rebuild_with_progress`].
///
/// `Text`/`Graph` only ever emit their `Started`/`Complete` pair (each
/// phase is fast enough that finer granularity isn't useful); `Semantic`
/// emits one `Progress` update per document as it completes, since local
/// CPU embedding of a large vault is the phase long enough that a caller
/// needs visibility into it while it runs.
///
/// [`VaultSearchService::rebuild_with_progress`]: super::VaultSearchService::rebuild_with_progress
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RebuildProgress {
    TextStarted,
    TextComplete,
    GraphStarted,
    GraphComplete,
    SemanticProgress { completed: usize, total: usize },
}

/// Outcome of a full graph rebuild from a vault scan.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct GraphRebuildReport {
    pub notes_scanned: usize,
    pub nodes: usize,
    pub edges: usize,
}

/// Outcome of a full semantic rebuild from a vault scan.
/// `embedded` and `skipped` count individual chunks (a chunk is skipped
/// when its content hash is unchanged since the last build); `failed`
/// counts whole paths whose processing failed. `remaining` is the number of
/// paths still queued once this call returned: nonzero only when a budget
/// (see [`VaultSearchService::rebuild_with_budget`]) cut the rebuild short,
/// and the signal a caller uses to decide whether to call again.
///
/// [`VaultSearchService::rebuild_with_budget`]: super::VaultSearchService::rebuild_with_budget
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct SemanticRebuildReport {
    pub paths_scanned: usize,
    pub embedded: usize,
    pub skipped: usize,
    pub failed: usize,
    pub remaining: usize,
}

/// Combined outcome of one [`VaultSearchService::rebuild`] call.
///
/// [`VaultSearchService::rebuild`]: super::VaultSearchService::rebuild
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RebuildReport {
    pub text: Option<FreshnessReport>,
    pub graph: Option<GraphRebuildReport>,
    pub semantic: Option<SemanticRebuildReport>,
    /// Every store this call found damaged, moved aside, and recreated before rebuilding it.
    pub recovered: Vec<StoreRecovery>,
}

/// One attempt to recover a damaged store: the store, and where its damaged files went or why they could not be moved.
#[derive(Debug)]
pub struct RecoveryOutcome {
    pub store: DerivedStore,
    pub result: Result<StoreRecovery, SearchError>,
}

/// One `query_semantic` request: `limit` of `0` returns no hits,
/// without error, without embedding the query or reading the store,
/// matching [`crate::vector_store::SimilarityQuery::k`]'s own `0`
/// convention.
#[derive(Clone, Copy, Debug)]
pub struct SemanticQuery<'a> {
    pub query: &'a str,
    pub limit: usize,
    pub path_prefix: Option<&'a str>,
    /// See [`crate::vector_store::SimilarityQuery::exclude_paths`].
    pub exclude_paths: &'a [String],
}

/// One ranked `query_semantic` hit. `chunk` is the chunk's prose,
/// re-read from its source document at query time: the vector store never
/// holds chunk text itself (see the `vector_store` module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticHit {
    pub path: String,
    pub chunk: String,
    /// Cosine similarity, from -1 to 1.
    pub score: f32,
    /// The similarity clamped to 0 to 1, so it reads as a fraction of a
    /// perfect match and can be compared with a minimum relevance threshold.
    pub relevance: f32,
    pub heading_context: Vec<String>,
}
