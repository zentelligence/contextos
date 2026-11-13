//! Combined per-vault search service.
//!
//! `VaultSearchService` wires one vault root's text index and link graph
//! behind a single `UpdatesSearch` consumer, so `contextos-mcp` can route
//! every completed mutation to whichever of the two derived indexes are
//! enabled for that vault, and expose one query, status, and rebuild surface
//! over both.
//!
//! Either index can be disabled independently through `[vault.search]`. A
//! disabled capability is represented internally as `None` rather than a
//! stub implementation, so callers observe the stable `index/disabled`
//! failure defined by [`SearchError`] instead of silently degraded
//! behaviour.
//!
//! The public query, status, and rebuild surface lives here; link-graph
//! synchronisation, semantic-queue draining, and the full-scan rebuild
//! passes are in [`maintenance`], the result types in [`report`], and the
//! filesystem walk helpers in [`scan`].

mod blended;
mod integrity;
mod maintenance;
mod paged;
mod reconcile;
mod report;
mod scan;
mod slot;
mod stores;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use contextos_fs::{HiddenPaths, HiddenPathsInput, default_hidden_patterns};

use contextos_core::{OperationEvent, OperationWarning, SystemClock, UpdatesSearch, VaultRootId};

use crate::chunk::Chunk;
use crate::store::{DEFAULT_WRITE_WAIT, DerivedStore, StoreFailure};
use crate::{
    EmbeddingWorker, EmbeddingWorkerConfig, EmbedsText, FilesystemChunkSource, FilesystemHitMetadata, FreshnessReport,
    GraphBackend, GraphDirection, GraphView, IndexesText, LinkGraph, LinkGraphConfig, SearchError, SimilarityQuery,
    SqliteVecConfig, StoreHealth, StoresVectors, TextHit, TextQuery, VectorStoreState,
};
use slot::Slot;
use stores::{GraphVault, Repopulate, TextStore, VectorSlot, graph_slot, text_slot, vector_slot};

pub use blended::{BlendedHit, BlendedQuery, BlendedResults, MatchMode, SearchMode};
pub use integrity::{IncomingLink, LinkIntegrity, UnlinkedNote, UnresolvedKind, UnresolvedLink};
pub use paged::{DescribedSemanticHit, DescribedTextHit, SemanticResults, TextResults};
pub use reconcile::GraphReconcileReport;
pub use report::{
    GraphIndexStatus, GraphRebuildReport, IndexStatusReport, RebuildProgress, RebuildReport, RebuildTarget,
    RecoveryOutcome, ScanFailure, SemanticHit, SemanticIndexStatus, SemanticQuery, SemanticRebuildReport,
    TextIndexStatus,
};

/// Construction input for one vault root's combined search service.
pub struct VaultSearchConfig {
    /// Identity of the vault root this service serves.
    pub root_id: VaultRootId,
    /// Resolved filesystem path of the vault root.
    pub root: PathBuf,
    /// Forward-slash relative path prefixes excluded from both indexes.
    pub excludes: Vec<String>,
    /// Forward-slash relative folders whose notes are not checked as link sources by the unresolved and unlinked
    /// reports. Links into them still resolve. The search `excludes` are always added to these, since a note outside
    /// the search corpus is not a graph node.
    pub link_excludes: Vec<String>,
    /// Forward-slash relative folders whose files are not checked as link targets by the unresolved report and the
    /// link integrity count: a link is ignored when the file it resolves to lies under one, or, when it resolves to no
    /// file, when the path it names does. Ambiguous links are reported whatever their candidates, and the link graph
    /// itself is unchanged. Nothing is added to these.
    pub link_ignore_targets: Vec<String>,
    /// The vault's `hidden` patterns, which every tool that lists the vault honours. A hidden file is no part of the
    /// link graph, so a link never reaches a file those tools hide. The folders the server keeps for itself (`.git`,
    /// `.contextos`) are always hidden, whatever the patterns say.
    pub hidden: Vec<String>,
    /// The vault's derived-state directory (its `.contextos` directory).
    pub state_directory: PathBuf,
    /// Whether the text index is enabled for this vault.
    pub text_enabled: bool,
    /// Whether the link graph is enabled for this vault.
    pub graph_enabled: bool,
    /// The link graph's persistence backend when enabled; ignored when
    /// `graph_enabled` is `false`.
    pub graph_backend: GraphBackend,
    /// Semantic search capability, or `None` when `[vault.search] semantic
    /// = false` for this vault.
    pub semantic: Option<SemanticConfig>,
}

/// Construction input for one vault's semantic search capability: an
/// already-selected embedding provider (config-driven, see
/// `crate::EmbeddingProviderConfig`) and where this vault's vector store
/// lives. Provider construction happens in the composition root, which is
/// the only layer that sees both the vault's TOML configuration and the
/// concrete provider types (mirroring `EmbeddingProviderConfig`'s own
/// module-boundary rationale).
pub struct SemanticConfig {
    pub embedder: Box<dyn EmbedsText>,
    /// Filesystem path of this vault's `.contextos/vectors.db`.
    pub vector_store_path: PathBuf,
}

/// Concrete embedding worker type this service drives: the vector store is
/// always `SqliteVecStore`, held through its slot, the provider is whichever
/// `EmbedsText` the composition root selected from configuration (boxed, so
/// swapping providers is a value, not a type, change), content is read from
/// the real filesystem, and timestamps come from the real clock.
type SemanticWorker = EmbeddingWorker<VectorSlot, Box<dyn EmbedsText>, FilesystemChunkSource, SystemClock>;

/// Combined per-vault text index and link graph service.
pub struct VaultSearchService {
    root_id: VaultRootId,
    root: PathBuf,
    excludes: Vec<String>,
    link_excludes: Vec<String>,
    link_ignore_targets: Vec<String>,
    hidden: Arc<HiddenPaths>,
    state_directory: PathBuf,
    text: Option<Slot<TextStore>>,
    text_directory: Option<PathBuf>,
    graph: Option<Slot<Mutex<LinkGraph>>>,
    graph_store_directory: Option<PathBuf>,
    semantic: Option<SemanticWorker>,
    metadata: FilesystemHitMetadata,
}

impl std::fmt::Debug for VaultSearchService {
    /// Reports only the vault identity and which capabilities are enabled:
    /// the wrapped tantivy and link-graph internals do not implement
    /// `Debug`, so this is a hand-written summary rather than a derive.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VaultSearchService")
            .field("root_id", &self.root_id)
            .field("text_enabled", &self.text.is_some())
            .field("graph_enabled", &self.graph.is_some())
            .field("semantic_enabled", &self.semantic.is_some())
            .finish_non_exhaustive()
    }
}

impl TryFrom<VaultSearchConfig> for VaultSearchService {
    type Error = SearchError;

    /// Opens each enabled store. A store that cannot be opened leaves its capability reporting why rather than failing
    /// the whole service, and a store found damaged is moved aside and repopulated from the vault, provided no other
    /// instance holds it.
    ///
    /// # Errors
    ///
    /// Returns the embedding provider's error when semantic search is enabled and its dimension cannot be found, and
    /// a storage error when the vault root cannot be resolved.
    fn try_from(value: VaultSearchConfig) -> Result<Self, Self::Error> {
        let (text, text_directory) = if value.text_enabled {
            let slot = text_slot(value.root_id, &value.root, &value.excludes, &value.state_directory)?;
            (Some(slot), Some(value.state_directory.join("index")))
        } else {
            (None, None)
        };

        let hidden = Arc::new(
            HiddenPaths::try_from(HiddenPathsInput {
                root: &value.root,
                patterns: &default_hidden_patterns()
                    .into_iter()
                    .chain(value.hidden.iter().cloned())
                    .collect::<Vec<_>>(),
            })
            .map_err(|source| SearchError::HiddenPatternsInvalid { source })?,
        );

        let (graph, graph_store_directory) = if value.graph_enabled {
            let store_directory = value.state_directory.join("graph");
            let config = LinkGraphConfig {
                store_directory: store_directory.clone(),
                backend: value.graph_backend,
                write_wait: DEFAULT_WRITE_WAIT,
            };
            let vault = GraphVault {
                root: value.root.clone(),
                hidden: Arc::clone(&hidden),
            };
            match graph_slot(config, vault, &value.state_directory) {
                Ok(slot) => (Some(slot), Some(store_directory)),
                // Another process (a second server instance, or a concurrent
                // `contextos index` run) already holds this store's
                // exclusive lock: degrade to the same disabled
                // representation `graph_enabled = false` produces rather
                // than failing this vault's whole search service, so the
                // rest of its tools stay available.
                Err(SearchError::GraphLocked { path }) => {
                    tracing::warn!(
                        code = "index/locked",
                        path = %path,
                        "link graph store is held by another process; disabling the link graph \
                         for this vault"
                    );
                    (None, None)
                }
                Err(error) => return Err(error),
            }
        } else {
            (None, None)
        };

        let semantic = value
            .semantic
            .map(|config| build_semantic_worker(&value.root, &value.state_directory, config))
            .transpose()?;

        let metadata = FilesystemHitMetadata::from(value.root.clone());
        let service = Self {
            root_id: value.root_id,
            root: value.root,
            excludes: value.excludes,
            link_excludes: value.link_excludes,
            link_ignore_targets: value.link_ignore_targets,
            hidden,
            state_directory: value.state_directory,
            text,
            text_directory,
            graph,
            graph_store_directory,
            semantic,
            metadata,
        };
        for outcome in service.recover_damaged() {
            if let Err(error) = outcome.result {
                tracing::warn!(
                    store = outcome.store.name(),
                    code = error.code(),
                    %error,
                    "a damaged derived store could not be recovered when the vault opened; it is reported until it is"
                );
            }
        }
        Ok(service)
    }
}

/// Text embedded once at construction, only for providers whose
/// [`EmbedsText::dimension`] is not known from configuration alone (an
/// arbitrary openai-compatible endpoint): the resulting vector's length
/// becomes the vector store's fixed dimension. Mirrors
/// `FastembedLocal`'s own internal dimension probe, generalised here so
/// every provider works uniformly regardless of whether it already knows
/// its dimension.
const DIMENSION_PROBE_TEXT: &str = "contextos-dimension-probe";

/// The length of the vectors `embedder` produces: from its configuration when it knows it, else by embedding
/// [`DIMENSION_PROBE_TEXT`] once.
fn embedding_dimension(embedder: &dyn EmbedsText) -> Result<usize, SearchError> {
    if let Some(dimension) = embedder.dimension() {
        return Ok(dimension);
    }
    let probe = Chunk::query(DIMENSION_PROBE_TEXT);
    let vectors = embedder.embed(std::slice::from_ref(&probe))?;
    vectors
        .first()
        .map(Vec::len)
        .ok_or_else(|| SearchError::EmbeddingShapeMismatch {
            reason: "embedding provider returned no vector for the dimension probe".to_owned(),
        })
}

/// A vault's semantic index checked without writing: the provider answers as the embedding worker would need it to,
/// and the vector store is read, never created, so a diagnostic can report both without changing derived state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticHealth {
    /// The length of the vectors the provider produces.
    pub dimension: usize,
    /// What the vault's vector store holds, or that it has not been built.
    pub store: VectorStoreState,
}

impl TryFrom<SemanticConfig> for SemanticHealth {
    type Error = SearchError;

    /// Asks the provider for its dimension (embedding one probe text when its configuration does not say), then
    /// reads the vector store read-only.
    ///
    /// # Errors
    ///
    /// Returns the provider's error when it cannot embed, or a storage error when an existing store cannot be read.
    fn try_from(value: SemanticConfig) -> Result<Self, Self::Error> {
        let dimension = embedding_dimension(value.embedder.as_ref())?;
        let store = VectorStoreState::try_from(value.vector_store_path.as_path())?;
        Ok(Self { dimension, store })
    }
}

fn build_semantic_worker(
    root: &Path,
    state_directory: &Path,
    config: SemanticConfig,
) -> Result<SemanticWorker, SearchError> {
    let SemanticConfig {
        embedder,
        vector_store_path,
    } = config;
    let dimension = embedding_dimension(embedder.as_ref())?;
    let store = vector_slot(
        SqliteVecConfig {
            path: vector_store_path,
            dimension,
            write_wait: DEFAULT_WRITE_WAIT,
        },
        state_directory,
    )?;
    EmbeddingWorker::try_from(EmbeddingWorkerConfig {
        root: root.to_path_buf(),
        store,
        embedder,
        content: FilesystemChunkSource::new(root.to_path_buf()),
        clock: SystemClock,
    })
}

impl UpdatesSearch for VaultSearchService {
    /// Applies the event to whichever of the text index and link graph are
    /// enabled. Both are attempted regardless of whether the first fails;
    /// the first failure becomes the returned warning.
    fn update(&self, event: &OperationEvent) -> Result<(), OperationWarning> {
        let text_result = self.update_text(std::slice::from_ref(event));
        let graph_result = self.update_graph(event);
        self.enqueue_semantic(event);
        text_result.and(graph_result)
    }

    /// Applies a batch in order. The text index commits once per run of consecutive documents; the link graph and
    /// the embedding queue are applied event by event, since neither commits per call. Every event is attempted and
    /// the first failure becomes the returned warning, as for [`Self::update`].
    fn update_all(&self, events: &[OperationEvent]) -> Result<(), OperationWarning> {
        let text_result = self.update_text(events);
        let mut graph_result = Ok(());
        for event in events {
            if let Err(warning) = self.update_graph(event) {
                graph_result = graph_result.and(Err(warning));
            }
            self.enqueue_semantic(event);
        }
        text_result.and(graph_result)
    }
}

impl VaultSearchService {
    /// Applies a write to the text index when it is enabled. A text index that cannot be opened is reported as the
    /// write's warning and never recovered here: recovery repopulates the index, which must not hold up a vault write.
    /// A write that finds the index damaged releases it, so it can be recovered once no instance holds it.
    fn update_text(&self, events: &[OperationEvent]) -> Result<(), OperationWarning> {
        let Some(slot) = &self.text else {
            return Ok(());
        };
        let text = slot.get().map_err(OperationWarning::from)?;
        text.apply_all(events).map_err(|error| {
            slot.note(&error);
            OperationWarning::from(error)
        })
    }

    /// Embeds `text` as a search query with the vault's configured provider.
    fn embed_query(semantic: &SemanticWorker, text: &str) -> Result<Vec<f32>, SearchError> {
        let probe = Chunk::query(text);
        let vectors = semantic.embedder().embed(std::slice::from_ref(&probe))?;
        vectors
            .into_iter()
            .next()
            .ok_or_else(|| SearchError::EmbeddingShapeMismatch {
                reason: "embedding provider returned no vector for the query text".to_owned(),
            })
    }

    fn locked(graph: &Mutex<LinkGraph>) -> std::sync::MutexGuard<'_, LinkGraph> {
        graph.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs a full-text query after reconciling the text index against the
    /// current filesystem state.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::TextDisabled`] when text search is disabled
    /// for this vault, an invalid-query error for unparsable syntax or
    /// filter values, and a storage error when the index cannot be read.
    pub fn query_text(&self, request: &TextQuery<'_>) -> Result<(Vec<TextHit>, FreshnessReport), SearchError> {
        self.with_text(|text| {
            let freshness = text.refresh()?;
            let hits = text.index().query(request)?;
            Ok((hits, freshness))
        })
    }

    /// Returns nodes and edges reachable from `from` within `depth` hops.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::GraphDisabled`] when the link graph is
    /// disabled for this vault, [`SearchError::InvalidDepth`] when `depth`
    /// is outside `1..=4`, and [`SearchError::UnknownNote`] when `from` is
    /// not in the graph.
    pub fn graph_neighbours(
        &self,
        from: &str,
        depth: u32,
        direction: GraphDirection,
    ) -> Result<GraphView, SearchError> {
        self.reconcile_before_query();
        self.with_graph(|graph| graph.neighbours(from, depth, direction))
    }

    /// Returns the notes and edges that link or embed directly to `from`.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::GraphDisabled`] when the link graph is
    /// disabled for this vault, and [`SearchError::UnknownNote`] when
    /// `from` is not in the graph.
    pub fn graph_backlinks(&self, from: &str) -> Result<GraphView, SearchError> {
        self.reconcile_before_query();
        self.with_graph(|graph| graph.backlinks(from))
    }

    /// Returns the shortest path between `from` and `to`.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::GraphDisabled`] when the link graph is
    /// disabled for this vault, and [`SearchError::UnknownNote`] when
    /// `from` or `to` is not in the graph.
    pub fn graph_path(&self, from: &str, to: &str, direction: GraphDirection) -> Result<GraphView, SearchError> {
        self.reconcile_before_query();
        self.with_graph(|graph| graph.path_between(from, to, direction))
    }

    /// Returns every real note with neither incoming nor outgoing edges.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::GraphDisabled`] when the link graph is
    /// disabled for this vault.
    pub fn graph_orphans(&self) -> Result<GraphView, SearchError> {
        self.reconcile_before_query();
        self.with_graph(LinkGraph::orphans)
    }

    /// Returns the top `request.limit` chunks by cosine similarity to
    /// `request.query`'s embedding, optionally scoped to
    /// `request.path_prefix` and with `request.exclude_paths` removed.
    ///
    /// A hit whose source document changed since it was last embedded (so
    /// re-chunking it no longer produces a chunk at the stored ordinal) is
    /// silently skipped rather than returned with stale or missing text;
    /// this staleness is visible through
    /// [`VaultSearchService::status`]'s semantic `stale_estimate` instead.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::SemanticUnavailable`] when semantic search is
    /// disabled for this vault, and a typed embedding or storage error when
    /// the query cannot be embedded or the vector store cannot be read.
    pub fn query_semantic(&self, request: &SemanticQuery<'_>) -> Result<Vec<SemanticHit>, SearchError> {
        let semantic = self.semantic_store()?;
        if request.limit == 0 {
            return Ok(Vec::new());
        }

        let vector = Self::embed_query(semantic, request.query)?;

        let hits = semantic.store().similar(&SimilarityQuery {
            vector: &vector,
            k: request.limit,
            path_prefix: request.path_prefix,
            exclude_paths: request.exclude_paths,
        })?;

        let mut results = Vec::with_capacity(hits.len());
        for hit in hits {
            if let Some(text) = self.reread_chunk_text(semantic, &hit.path, hit.ordinal)? {
                results.push(SemanticHit {
                    path: hit.path,
                    chunk: text,
                    score: hit.score,
                    relevance: hit.score.clamp(0.0, 1.0),
                    heading_context: hit.heading_context,
                });
            }
        }
        Ok(results)
    }

    /// Returns per-index document counts, staleness, last-build time, and health.
    ///
    /// `stale_estimate` is computed with a read-only scan: it never
    /// reindexes. Call [`VaultSearchService::rebuild`] to reconcile. A store
    /// that cannot be opened or read is reported as unreadable, naming why,
    /// and a vault or state directory that cannot be scanned is reported in
    /// `scan_failure`, so every store's status stays visible whatever fails;
    /// this call never recovers a store.
    #[must_use]
    pub fn status(&self) -> IndexStatusReport {
        let mut scan_failure = None;
        let text = match &self.text {
            Some(slot) => self.text_status(slot, &mut scan_failure),
            None => TextIndexStatus::default(),
        };
        let graph = match &self.graph {
            Some(slot) => self.graph_status(slot, &mut scan_failure),
            None => GraphIndexStatus::default(),
        };
        let semantic = match &self.semantic {
            Some(semantic) => Self::semantic_status(semantic),
            None => SemanticIndexStatus::default(),
        };
        IndexStatusReport {
            state_directory: self.state_directory.clone(),
            text,
            graph,
            semantic,
            scan_failure,
        }
    }

    /// The newest modification time under `directory`, or `None` when nothing is there; a failure to scan it is
    /// recorded in `scan_failure`.
    fn last_build(directory: Option<&Path>, scan_failure: &mut Option<ScanFailure>) -> Option<time::OffsetDateTime> {
        match directory.map(scan::newest_mtime_under).transpose() {
            Ok(found) => found.flatten(),
            Err(error) => {
                scan_failure.get_or_insert_with(|| ScanFailure::from(&error));
                None
            }
        }
    }

    fn text_status(&self, slot: &Slot<TextStore>, scan_failure: &mut Option<ScanFailure>) -> TextIndexStatus {
        let last_build = Self::last_build(self.text_directory.as_deref(), scan_failure);
        let entries = slot
            .get()
            .and_then(|text| text.index().entries().inspect_err(|error| slot.note(error)));
        match entries {
            Ok(entries) => TextIndexStatus {
                enabled: true,
                health: Some(StoreHealth::Ready),
                documents: entries.len(),
                stale_estimate: self.text_stale_estimate(&entries).unwrap_or_else(|error| {
                    scan_failure.get_or_insert_with(|| ScanFailure::from(&error));
                    0
                }),
                last_build,
                last_recovery: slot.last_recovery(),
            },
            Err(error) => TextIndexStatus {
                enabled: true,
                health: Some(StoreHealth::Unreadable(slot.failure_of(&error))),
                last_build,
                last_recovery: slot.last_recovery(),
                ..TextIndexStatus::default()
            },
        }
    }

    fn graph_status(&self, slot: &Slot<Mutex<LinkGraph>>, scan_failure: &mut Option<ScanFailure>) -> GraphIndexStatus {
        let last_build = Self::last_build(self.graph_store_directory.as_deref(), scan_failure);
        let unreadable = |failure: StoreFailure| GraphIndexStatus {
            enabled: true,
            needs_rebuild: failure.kind().rebuild_helps(),
            health: Some(StoreHealth::Unreadable(failure)),
            last_build,
            last_recovery: slot.last_recovery(),
            ..GraphIndexStatus::default()
        };
        let graph = match slot.get() {
            Ok(graph) => graph,
            Err(error) => return unreadable(slot.failure_of(&error)),
        };
        let mut locked = Self::locked(&graph);
        // A store that cannot be read is reported, not raised: the status call still answers, naming the failure, so
        // the rest of the vault's status stays visible.
        let view = match locked.full_view() {
            Ok(view) => view,
            Err(error) => {
                drop(locked);
                slot.note(&error);
                return unreadable(slot.failure_of(&error));
            }
        };
        let needs_rebuild = locked.needs_rebuild();
        let health = locked.health().clone();
        let sync_status = locked.sync_status();
        drop(locked);
        if let StoreHealth::Unreadable(failure) = &health {
            slot.note(&failure.clone().into());
        }
        GraphIndexStatus {
            enabled: true,
            nodes: view.nodes.len(),
            edges: view.edges.len(),
            needs_rebuild,
            health: Some(health),
            last_build,
            last_recovery: slot.last_recovery(),
            generation: sync_status.map(|status| status.generation),
            last_catch_up: sync_status.and_then(|status| status.last_catch_up),
        }
    }

    fn semantic_status(semantic: &SemanticWorker) -> SemanticIndexStatus {
        let slot = semantic.store().slot();
        let worker_status = semantic.status();
        let last_recovery = slot.last_recovery();
        match semantic.store().stats() {
            Ok(stats) => SemanticIndexStatus {
                enabled: true,
                health: Some(StoreHealth::Ready),
                documents: stats.documents,
                chunks: stats.chunks,
                stale_estimate: worker_status.pending,
                last_build: worker_status.last_processed_at,
                last_recovery,
            },
            Err(error) => SemanticIndexStatus {
                enabled: true,
                health: Some(StoreHealth::Unreadable(slot.failure_of(&error))),
                stale_estimate: worker_status.pending,
                last_build: worker_status.last_processed_at,
                last_recovery,
                ..SemanticIndexStatus::default()
            },
        }
    }

    /// Recovers `store` whether or not this instance has found it damaged (a diagnostic may have, reading it afresh):
    /// it is moved aside, kept for diagnosis, recreated, and repopulated from the vault, provided no other instance
    /// holds it. The text index is refilled by its next reconciliation or rebuild.
    ///
    /// # Errors
    ///
    /// Returns the store's disabled error when it is not enabled, [`SearchError::StoreInUse`] when another instance
    /// still holds it, and the failure to move it aside, open it, or repopulate it otherwise.
    pub fn recover_store(&self, store: DerivedStore) -> Result<crate::StoreRecovery, SearchError> {
        self.recover(store)
    }

    /// Recovers every enabled store found damaged: each is moved aside, kept for diagnosis, recreated, and
    /// repopulated from the vault, provided no other instance holds it. A store that is not damaged is left alone.
    #[must_use]
    pub fn recover_damaged_stores(&self) -> Vec<RecoveryOutcome> {
        self.recover_damaged()
    }

    /// This vault's derived-state directory: the parent of
    /// the text index, link-graph cache, and semantic vector store, so an
    /// operator can confirm a separately invoked `contextos index` and this
    /// running server resolve to the same on-disk store rather than two
    /// silently diverged ones.
    #[must_use]
    pub fn state_directory(&self) -> &Path {
        &self.state_directory
    }

    /// Whether semantic search is enabled for this vault: the
    /// cheap, in-memory check a caller uses to decide whether there is
    /// anything to drain, without the I/O `status`'s live store query
    /// costs.
    #[must_use]
    pub fn semantic_enabled(&self) -> bool {
        self.semantic.is_some()
    }

    /// Rebuilds the selected index (or both) from a full vault scan.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::TextDisabled`] or [`SearchError::GraphDisabled`]
    /// when the requested index is disabled for this vault,
    /// [`SearchError::SemanticUnavailable`] for [`RebuildTarget::Semantic`],
    /// and a storage error when the vault cannot be scanned or an index
    /// cannot be written.
    pub fn rebuild(&self, target: RebuildTarget) -> Result<RebuildReport, SearchError> {
        self.rebuild_with_progress(target, &mut |_| {})
    }

    /// As [`Self::rebuild`], but invokes `on_progress` as each phase starts,
    /// completes, or (for the semantic phase) processes one more document,
    /// so a caller driving a long-running rebuild can surface live activity
    /// instead of a single silent wait.
    ///
    /// # Errors
    ///
    /// Same as [`Self::rebuild`].
    pub fn rebuild_with_progress(
        &self,
        target: RebuildTarget,
        on_progress: &mut dyn FnMut(RebuildProgress),
    ) -> Result<RebuildReport, SearchError> {
        self.rebuild_with_budget(target, None, on_progress)
    }

    /// As [`Self::rebuild_with_progress`], but the semantic phase (the only
    /// phase costly enough for this to matter, text and graph are each a
    /// single fast pass) stops once `budget` has elapsed since it began,
    /// leaving anything still queued for a later call to pick up rather
    /// than blocking past a caller's own request timeout. `None` drains
    /// exhaustively, matching [`Self::rebuild_with_progress`]. Text and
    /// graph rebuilds always run to completion regardless of `budget`.
    ///
    /// # Errors
    ///
    /// Same as [`Self::rebuild`].
    pub fn rebuild_with_budget(
        &self,
        target: RebuildTarget,
        budget: Option<time::Duration>,
        on_progress: &mut dyn FnMut(RebuildProgress),
    ) -> Result<RebuildReport, SearchError> {
        let mut report = RebuildReport::default();
        let rebuilds_text = matches!(target, RebuildTarget::Text | RebuildTarget::All);
        let rebuilds_graph = matches!(target, RebuildTarget::Graph | RebuildTarget::All);
        let rebuilds_semantic =
            matches!(target, RebuildTarget::Semantic) || (target == RebuildTarget::All && self.semantic.is_some());
        if rebuilds_text {
            report.text = Some(self.recovering(DerivedStore::Text, &mut report.recovered, |service| {
                service.rebuild_text_reporting(on_progress)
            })?);
        }
        if rebuilds_graph {
            report.graph = Some(self.recovering(DerivedStore::Graph, &mut report.recovered, |service| {
                service.rebuild_graph_reporting(on_progress)
            })?);
        }
        if rebuilds_semantic {
            report.semantic = Some(
                self.recovering(DerivedStore::Vectors, &mut report.recovered, |service| {
                    service.rebuild_semantic(budget, on_progress)
                })?,
            );
        }
        Ok(report)
    }

    /// Runs one store's rebuild, first recovering the store when it is closed because it is damaged, and recovering
    /// it and running the rebuild once more when the rebuild itself finds the store damaged: a damaged store cannot be
    /// rebuilt in place, so the rebuild never reports the damage as its own failure while it can still be recovered.
    /// Each recovery is recorded in `recovered`.
    fn recovering<R>(
        &self,
        store: DerivedStore,
        recovered: &mut Vec<crate::store::StoreRecovery>,
        mut rebuild: impl FnMut(&Self) -> Result<R, SearchError>,
    ) -> Result<R, SearchError> {
        if self.damaged_stores().contains(&store) {
            recovered.push(self.recover_then(store, Repopulate::No)?);
        }
        match rebuild(self) {
            Err(error) if self.damaged_stores().contains(&store) => {
                tracing::warn!(
                    store = store.name(),
                    code = error.code(),
                    "a rebuild found its store damaged; recovering it"
                );
                recovered.push(self.recover_then(store, Repopulate::No)?);
                rebuild(self)
            }
            outcome => outcome,
        }
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
