//! How the vault search service opens its derived stores, uses them so a damaged one is released, and recovers one
//! that is damaged: moving it aside and repopulating a fresh store from the vault.
//!
//! Recovery runs where the cost of a rebuild is expected, never inside a vault write: when the service opens (a fresh
//! process finding a damaged store), before a query or rebuild needs a store found damaged, and when an operator or
//! agent asks for it (`doctor_resolve`). A write that finds a store damaged only reports it as a warning, so the host
//! write lock is never held for a rebuild.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use contextos_core::{ContentHash, SystemClock, VaultRootId};
use contextos_fs::HiddenPaths;
use contextos_obsidian::BaseRecords;

use super::scan::walk_files;
use super::slot::{Held, Recovered, Slot, SlotConfig};
use super::{RecoveryOutcome, VaultSearchService};
use crate::store::{DEFAULT_WRITE_WAIT, DerivedStore, StoreFailureKind, StoreHealth, StoreRecovery};
use crate::{
    FileLister, LinkGraph, LinkGraphConfig, PathCommit, PathCommitOutcome, ReadsBaseFolders, SearchError,
    SimilarityHit, SimilarityQuery, SqliteVecConfig, SqliteVecStore, StoresVectors, TantivyIndex, TextIndexConfig,
    TextSearchService, TextSyncConfig, VectorRecord, VectorStoreStats,
};

/// Whether recovering a store also repopulates it from the vault, or leaves that to a rebuild the caller runs next.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Repopulate {
    Yes,
    No,
}

/// The text index as the service holds it.
pub(super) type TextStore = TextSearchService<TantivyIndex>;

/// The vector store as the embedding worker uses it: through its slot, so a damaged store is released and a closed
/// one reopened on the next use.
pub(super) struct VectorSlot {
    slot: Slot<SqliteVecStore>,
}

impl VectorSlot {
    /// The slot behind the store.
    pub(super) const fn slot(&self) -> &Slot<SqliteVecStore> {
        &self.slot
    }

    fn with<R>(&self, call: impl FnOnce(&SqliteVecStore) -> Result<R, SearchError>) -> Result<R, SearchError> {
        let store = self.slot.get()?;
        call(&store).inspect_err(|error| self.slot.note(error))
    }
}

impl StoresVectors for VectorSlot {
    fn upsert(&self, records: &[VectorRecord<'_>]) -> Result<(), SearchError> {
        self.with(|store| store.upsert(records))
    }

    fn delete(&self, path: &str) -> Result<(), SearchError> {
        self.with(|store| store.delete(path))
    }

    fn similar(&self, request: &SimilarityQuery<'_>) -> Result<Vec<SimilarityHit>, SearchError> {
        self.with(|store| store.similar(request))
    }

    fn existing_hash(&self, path: &str, ordinal: usize) -> Result<Option<ContentHash>, SearchError> {
        self.with(|store| store.existing_hash(path, ordinal))
    }

    fn prune_ordinals_at_or_beyond(&self, path: &str, keep_below: usize) -> Result<(), SearchError> {
        self.with(|store| store.prune_ordinals_at_or_beyond(path, keep_below))
    }

    fn commit_path(&self, commit: &PathCommit<'_>) -> Result<PathCommitOutcome, SearchError> {
        self.with(|store| store.commit_path(commit))
    }

    fn stats(&self) -> Result<VectorStoreStats, SearchError> {
        self.with(StoresVectors::stats)
    }
}

/// Opens the text index kept in `state_directory/index` for the vault `root_id` at `root`.
pub(super) fn text_slot(
    root_id: VaultRootId,
    root: &Path,
    excludes: &[String],
    state_directory: &Path,
) -> Result<Slot<TextStore>, SearchError> {
    let directory = state_directory.join("index");
    let (root, excludes, opened) = (root.to_path_buf(), excludes.to_vec(), directory.clone());
    Slot::try_open(SlotConfig {
        store: DerivedStore::Text,
        state_directory: state_directory.to_path_buf(),
        members: vec![directory],
        wait: DEFAULT_WRITE_WAIT,
        open: Box::new(move || {
            let index = TantivyIndex::try_from(TextIndexConfig {
                directory: opened.clone(),
                writer_lock_wait: TextIndexConfig::DEFAULT_WRITER_LOCK_WAIT,
            })?;
            Ok(TextSearchService::from(TextSyncConfig {
                root_id,
                root: root.clone(),
                excludes: excludes.clone(),
                index,
            }))
        }),
    })
}

/// What the link graph needs of the vault it serves: where it is and what it hides.
pub(super) struct GraphVault {
    pub(super) root: PathBuf,
    pub(super) hidden: Arc<HiddenPaths>,
}

/// Reads the folder a base file keeps its records in, from the base file on disk.
struct DiskBases {
    root: PathBuf,
}

impl ReadsBaseFolders for DiskBases {
    fn records_folder(&self, base: &str) -> Option<BaseRecords> {
        let text = contextos_fs::read_regular_text(&self.root.join(base)).ok()?;
        BaseRecords::try_from(text.as_str()).ok()
    }
}

/// Opens the link graph kept in `state_directory/graph`. A graph whose store opens but is found damaged when loaded
/// fails to open, so the slot can recover it; any other unreadable store opens flagged with its failure, as the graph
/// reports it.
///
/// The vault is listed as the graph opens, here and not in a write: a graph opened from its store knows only the notes
/// written since, and the files a link resolves against are all of the vault's. A vault that cannot be listed then is
/// listed again before the graph's next change.
pub(super) fn graph_slot(
    config: LinkGraphConfig,
    vault: GraphVault,
    state_directory: &Path,
) -> Result<Slot<Mutex<LinkGraph>>, SearchError> {
    let GraphVault { root, hidden } = vault;
    let lister: FileLister = {
        let (root, hidden) = (root.clone(), Arc::clone(&hidden));
        Arc::new(move || {
            let mut files = Vec::new();
            walk_files(&root, &root, &hidden, &mut files)?;
            Ok(files)
        })
    };
    let bases: Arc<dyn ReadsBaseFolders> = Arc::new(DiskBases { root });
    let members = vec![config.store_directory.clone()];
    Slot::try_open(SlotConfig {
        store: DerivedStore::Graph,
        state_directory: state_directory.to_path_buf(),
        members,
        wait: DEFAULT_WRITE_WAIT,
        open: Box::new(move || {
            let mut graph = LinkGraph::try_from(config.clone())?;
            if let StoreHealth::Unreadable(failure) = graph.health()
                && failure.kind() == StoreFailureKind::Corrupt
            {
                return Err(failure.clone().into());
            }
            graph.read_bases_with(Arc::clone(&bases));
            if let Err(error) = graph.list_files_with(Arc::clone(&lister)) {
                tracing::warn!(
                    code = error.code(),
                    "the vault could not be listed when the link graph opened; it is listed before the graph's next change"
                );
            }
            Ok(Mutex::new(graph))
        }),
    })
}

/// Opens the vector store `config` describes, kept in `state_directory`.
pub(super) fn vector_slot(config: SqliteVecConfig, state_directory: &Path) -> Result<VectorSlot, SearchError> {
    // Side files first and the database last, so a move that fails part-way never leaves a log beside the wrong
    // database; `-journal` covers a store an older build wrote in rollback-journal mode.
    let members = ["-wal", "-shm", "-journal", ""]
        .iter()
        .map(|suffix| {
            let mut name = config.path.as_os_str().to_os_string();
            name.push(suffix);
            name.into()
        })
        .collect();
    let slot = Slot::try_open(SlotConfig {
        store: DerivedStore::Vectors,
        state_directory: state_directory.to_path_buf(),
        members,
        wait: DEFAULT_WRITE_WAIT,
        open: Box::new(move || SqliteVecStore::try_from(config.clone())),
    })?;
    Ok(VectorSlot { slot })
}

impl VaultSearchService {
    /// The text index, recovering it first when it is closed because it is damaged.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::TextDisabled`] when text search is disabled, and the failure that keeps the index from
    /// opening or being recovered otherwise.
    pub(super) fn text_store(&self) -> Result<Arc<Held<TextStore>>, SearchError> {
        let slot = self.text.as_ref().ok_or(SearchError::TextDisabled)?;
        if slot.needs_recovery() {
            self.recover(DerivedStore::Text)?;
        }
        slot.get()
    }

    /// The embedding worker, recovering its vector store first when it is closed because it is damaged.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::SemanticUnavailable`] when semantic search is disabled, and the failure to recover the
    /// store otherwise.
    pub(super) fn semantic_store(&self) -> Result<&super::SemanticWorker, SearchError> {
        let semantic = self.semantic.as_ref().ok_or(SearchError::SemanticUnavailable)?;
        if semantic.store().slot().needs_recovery() {
            self.recover(DerivedStore::Vectors)?;
        }
        Ok(semantic)
    }

    /// Runs `call` against the text index, releasing it when the call finds it damaged.
    pub(super) fn with_text<R>(
        &self,
        call: impl FnOnce(&TextStore) -> Result<R, SearchError>,
    ) -> Result<R, SearchError> {
        let text = self.text_store()?;
        call(&text).inspect_err(|error| {
            if let Some(slot) = &self.text {
                slot.note(error);
            }
        })
    }

    /// The link graph, recovering it first when it is closed because it is damaged.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::GraphDisabled`] when the link graph is disabled, and the failure that keeps it from
    /// opening or being recovered otherwise.
    pub(super) fn graph_store(&self) -> Result<Arc<Held<Mutex<LinkGraph>>>, SearchError> {
        let slot = self.graph.as_ref().ok_or(SearchError::GraphDisabled)?;
        if slot.needs_recovery() {
            self.recover(DerivedStore::Graph)?;
        }
        slot.get()
    }

    /// Runs `call` against the link graph, releasing it when the call finds it damaged.
    pub(super) fn with_graph<R>(
        &self,
        call: impl FnOnce(&mut LinkGraph) -> Result<R, SearchError>,
    ) -> Result<R, SearchError> {
        let graph = self.graph_store()?;
        let mut locked = Self::locked(&graph);
        call(&mut locked).inspect_err(|error| {
            if let Some(slot) = &self.graph {
                slot.note(error);
            }
        })
    }

    /// Recovers `store`: moves it aside when damaged, opens a fresh one, and repopulates it from the vault (the link
    /// graph rebuilt at once, the vault queued for embedding, and the text index refilled by its next reconciliation).
    ///
    /// # Errors
    ///
    /// Returns the store's disabled error when it is not enabled, [`SearchError::StoreInUse`] when another instance
    /// still holds it, and the failure to move it aside, open it, or repopulate it otherwise.
    pub(super) fn recover(&self, store: DerivedStore) -> Result<StoreRecovery, SearchError> {
        self.recover_then(store, Repopulate::Yes)
    }

    /// As [`Self::recover`], leaving the fresh store empty when `repopulate` says the caller is about to rebuild it.
    pub(super) fn recover_then(
        &self,
        store: DerivedStore,
        repopulate: Repopulate,
    ) -> Result<StoreRecovery, SearchError> {
        match store {
            DerivedStore::Text => {
                let slot = self.text.as_ref().ok_or(SearchError::TextDisabled)?;
                moved(slot.recover(&SystemClock)?, slot)
            }
            DerivedStore::Graph => {
                let slot = self.graph.as_ref().ok_or(SearchError::GraphDisabled)?;
                let recovery = moved(slot.recover(&SystemClock)?, slot)?;
                if repopulate == Repopulate::Yes {
                    let graph = slot.get()?;
                    let scanned = self.scan_graph_notes(&graph)?;
                    self.commit_graph_rebuild(&graph, scanned)?;
                }
                Ok(recovery)
            }
            DerivedStore::Vectors => {
                let semantic = self.semantic.as_ref().ok_or(SearchError::SemanticUnavailable)?;
                let slot = semantic.store().slot();
                let recovery = moved(slot.recover(&SystemClock)?, slot)?;
                if repopulate == Repopulate::Yes {
                    for (relative, _absolute) in self.collect_markdown_files()? {
                        semantic.enqueue(relative);
                    }
                }
                Ok(recovery)
            }
        }
    }

    /// Recovers every enabled store that is closed because it is damaged, reporting each attempt.
    pub(super) fn recover_damaged(&self) -> Vec<RecoveryOutcome> {
        self.damaged_stores()
            .into_iter()
            .map(|store| RecoveryOutcome {
                store,
                result: self.recover(store),
            })
            .collect()
    }

    /// Every enabled store that is closed because it is damaged.
    pub(super) fn damaged_stores(&self) -> Vec<DerivedStore> {
        let text = self.text.as_ref().filter(|slot| slot.needs_recovery()).map(Slot::store);
        let graph = self
            .graph
            .as_ref()
            .filter(|slot| slot.needs_recovery())
            .map(Slot::store);
        let vectors = self
            .semantic
            .as_ref()
            .filter(|semantic| semantic.store().slot().needs_recovery())
            .map(|semantic| semantic.store().slot().store());
        [text, graph, vectors].into_iter().flatten().collect()
    }
}

/// The recovery `recovered` stands for: this slot's own move, or, when another instance had already replaced the
/// store, the move this slot last recorded.
fn moved<T>(recovered: Recovered, slot: &Slot<T>) -> Result<StoreRecovery, SearchError> {
    match recovered {
        Recovered::MovedAside(recovery) => Ok(recovery),
        Recovered::AlreadyReplaced => slot.last_recovery().ok_or_else(|| SearchError::StoreInUse {
            store: slot.store(),
            path: String::new(),
            other_processes: 0,
            this_process: 0,
        }),
    }
}
