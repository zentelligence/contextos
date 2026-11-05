// `unsafe_code` is `deny`, not `forbid`, only in this crate (see
// `Cargo.toml`'s `[lints]` comment): `vector_store::register_sqlite_vec`
// carries the one `unsafe` block in this crate, behind a narrow, documented
// `#[allow(unsafe_code)]`. Every other line in this crate remains subject
// to the deny, so a second `unsafe` occurrence anywhere else still fails
// the build.
#![deny(unsafe_code)]

mod chunk;
mod controls;
mod document;
mod embedding;
mod embedding_worker;
mod error;
mod fake_embedder;
#[cfg(feature = "semantic-local")]
mod fastembed_embedder;
mod fusion;
mod graph;
mod hit_metadata;
mod openai_embedder;
mod service;
mod store;
mod sync;
mod text;
mod vector_store;

pub use chunk::{Chunk, ChunkSource, chunk_document, estimate_tokens};
pub use controls::{CANDIDATE_WINDOW, MAX_PAGE_LIMIT, MinRelevance, ResultControls, SortKey, SortOrder, SortSpec};
pub use document::{DocumentSource, IndexedDocument};
pub use embedding::{EmbeddingProviderConfig, EmbedsText};
pub use embedding_worker::{
    EmbeddingWorker, EmbeddingWorkerConfig, EmbeddingWorkerStatus, FilesystemChunkSource, PathEmbeddingOutcome,
    ReadsChunkSource,
};
pub use error::SearchError;
pub use fake_embedder::FakeEmbedder;
#[cfg(feature = "semantic-local")]
pub use fastembed_embedder::{FastembedLocal, REQUIRED_MODEL_FILES};
pub use fusion::{FUSION_RANK_CONSTANT, FusedFile, fuse_by_rank};
pub use graph::{
    CatchUpKind, FileEntry, FileLister, GraphBackend, GraphDirection, GraphEdge, GraphKnowledge, GraphNode,
    GraphNodeKind, GraphView, LinkGraph, LinkGraphConfig, NoteEntry, NoteLinks, NoteSource, NoteStamp,
    ReadsBaseFolders, SyncStatus, UnreadableNote, VaultScan, inspect_graph_store,
};
pub use hit_metadata::{FilesystemHitMetadata, HitMetadata, METADATA_READ_LIMIT, ReadsHitMetadata};
pub use openai_embedder::{ApiKeySource, OpenAiCompatible, OpenAiCompatibleConfig};
pub use service::{
    BlendedHit, BlendedQuery, BlendedResults, DescribedSemanticHit, DescribedTextHit, GraphIndexStatus,
    GraphRebuildReport, GraphReconcileReport, IncomingLink, IndexStatusReport, LinkIntegrity, MatchMode,
    RebuildProgress, RebuildReport, RebuildTarget, RecoveryOutcome, ScanFailure, SearchMode, SemanticConfig,
    SemanticHealth, SemanticHit, SemanticIndexStatus, SemanticQuery, SemanticRebuildReport, SemanticResults,
    TextIndexStatus, TextResults, UnlinkedNote, UnresolvedKind, UnresolvedLink, VaultSearchConfig, VaultSearchService,
};
pub use store::{
    DEFAULT_WRITE_WAIT, DerivedStore, HolderCensus, HolderRegistry, StoreFailure, StoreFailureKind, StoreHealth,
    StoreInspection, StoreRecovery,
};
pub use sync::{FreshnessReport, TextSearchService, TextSyncConfig, is_markdown};
pub use text::{
    IndexEntry, IndexesText, TantivyIndex, TextBatch, TextHit, TextIndexConfig, TextPage, TextQuery, inspect_text_index,
};
pub use vector_store::{
    PathCommit, PathCommitOutcome, SimilarityHit, SimilarityQuery, SqliteVecConfig, SqliteVecStore, StoresVectors,
    VectorRecord, VectorStoreState, VectorStoreStats, inspect_vector_store,
};
