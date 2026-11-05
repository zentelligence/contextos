//! `serde`-backed [`super::StoresGraph`] implementation
//! (`GraphBackend::Serde`): a single JSON file, rewritten whole on every
//! persisted change.
//!
//! Intended for a small, starter, or experimental vault, not concurrent
//! multi-process use: a plain file has no locking of its own, unlike
//! `fjall` (an exclusive lock) or `sqlite` (WAL mode plus a busy timeout).
//! Kept as simple as possible in exchange for that narrower scope: no
//! incremental on-disk writes, no keyspaces, nothing beyond "read the whole
//! document in, write the whole document out."

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use super::{
    GraphChange, GraphDelta, GraphEdge, GraphNode, GraphRecords, GraphWriteGuard, JsonDecodeFailure, STORE_FORMAT,
    StoreCompleteness, StoreFailure, StoredGraphMetadata, StoredGraphState, StoresGraph, graph_storage_error,
};
use crate::SearchError;
use crate::store::DerivedStore;

const FILE_NAME: &str = "graph.json";

/// The whole on-disk shape of a `serde`-backed graph store: one JSON
/// document holding the format identifier, the edge-id allocator, and
/// every current node and edge record.
#[derive(Serialize, Deserialize)]
struct SerdeDocument {
    format: u32,
    next_edge_id: u64,
    /// Set while the store has only ever been written note by note since it was created; cleared by a rebuild. Absent
    /// in a document written before it existed, which reads as complete.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    incomplete: bool,
    nodes: Vec<GraphNode>,
    edges: Vec<SerdeEdgeRecord>,
}

#[derive(Serialize, Deserialize)]
struct SerdeEdgeRecord {
    store_id: u64,
    edge: GraphEdge,
}

/// Just the format identifier of a document, read before the rest so a document in another format is reported as
/// incompatible even when its other fields have a different shape.
#[derive(Deserialize)]
struct SerdeFormat {
    format: u32,
}

/// What the document on disk held when this store last read or wrote it.
enum SerdeLoad {
    /// No document has been written.
    Empty,
    /// The document is in another format.
    Incompatible,
    /// The mirror holds the document's complete content.
    Ready,
    /// The document could not be read or decoded.
    Unreadable(StoreFailure),
}

/// The in-memory mirror kept between `persist` calls, so a call carrying
/// only a delta (per [`GraphChange`]) can still emit a complete document.
struct SerdeState {
    nodes: HashMap<String, GraphNode>,
    edges: HashMap<u64, GraphEdge>,
    next_edge_id: u64,
    incomplete: bool,
    load: SerdeLoad,
}

pub(super) struct SerdeGraphStore {
    file_path: PathBuf,
    directory: PathBuf,
    state: Mutex<SerdeState>,
}

impl SerdeGraphStore {
    /// Opens (without creating) the JSON document at `directory/graph.json`, creating `directory` itself if absent so
    /// a later `persist` call has somewhere to write. The document is read once, here: a missing document is an empty
    /// store, one in another format is incompatible, and one that cannot be read or decoded is recorded as that
    /// failure, which every read then reports, rather than failing the open.
    ///
    /// # Errors
    ///
    /// Returns a storage error when `directory` does not exist and cannot be created.
    pub(super) fn open(directory: &Path) -> Result<Self, SearchError> {
        std::fs::create_dir_all(directory).map_err(|source| SearchError::GraphStorage {
            path: directory.display().to_string(),
            source,
        })?;
        let file_path = directory.join(FILE_NAME);
        let state = read_document(&file_path, directory);
        Ok(Self {
            file_path,
            directory: directory.to_path_buf(),
            state: Mutex::new(state),
        })
    }
}

/// Reads the document at `file_path` into a fresh mirror, recording what it found.
fn read_document(file_path: &Path, directory: &Path) -> SerdeState {
    let empty = |load| SerdeState {
        nodes: HashMap::new(),
        edges: HashMap::new(),
        next_edge_id: 0,
        incomplete: false,
        load,
    };
    let path = directory.display().to_string();
    let bytes = match std::fs::read(file_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return empty(SerdeLoad::Empty),
        Err(error) => {
            return empty(SerdeLoad::Unreadable(StoreFailure::io(
                DerivedStore::Graph,
                path,
                format!("reading {FILE_NAME}: {error}"),
            )));
        }
    };
    match serde_json::from_slice::<SerdeFormat>(&bytes) {
        Ok(found) if found.format != STORE_FORMAT => return empty(SerdeLoad::Incompatible),
        Ok(_) => {}
        Err(error) => {
            return empty(SerdeLoad::Unreadable(StoreFailure::undecodable(
                DerivedStore::Graph,
                path,
                format!("{FILE_NAME} {}", JsonDecodeFailure(&error)),
            )));
        }
    }
    match serde_json::from_slice::<SerdeDocument>(&bytes) {
        Ok(document) => SerdeState {
            nodes: document
                .nodes
                .into_iter()
                .map(|node| (node.path.clone(), node))
                .collect(),
            edges: document
                .edges
                .into_iter()
                .map(|record| (record.store_id, record.edge))
                .collect(),
            next_edge_id: document.next_edge_id,
            incomplete: document.incomplete,
            load: SerdeLoad::Ready,
        },
        Err(error) => empty(SerdeLoad::Unreadable(StoreFailure::undecodable(
            DerivedStore::Graph,
            path,
            format!("{FILE_NAME} {}", JsonDecodeFailure(&error)),
        ))),
    }
}

impl StoresGraph for SerdeGraphStore {
    /// A document that could not be read is read again here, so a failure that has since cleared (a file another
    /// instance rewrote, or a passing I/O fault) is picked up by the graph's retry rather than held until a restart.
    fn read_metadata(&self) -> Result<StoredGraphState, StoreFailure> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if matches!(state.load, SerdeLoad::Unreadable(_)) {
            *state = read_document(&self.file_path, &self.directory);
        }
        match &state.load {
            SerdeLoad::Empty => Ok(StoredGraphState::Empty),
            SerdeLoad::Incompatible => Ok(StoredGraphState::Incompatible),
            SerdeLoad::Ready => Ok(StoredGraphState::Ready(StoredGraphMetadata {
                complete: !state.incomplete,
                next_edge_id: state.next_edge_id,
                generation: 0,
            })),
            SerdeLoad::Unreadable(failure) => Err(failure.clone()),
        }
    }

    fn load_all(&self) -> Result<GraphRecords, StoreFailure> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let SerdeLoad::Unreadable(failure) = &state.load {
            return Err(failure.clone());
        }
        let nodes = state.nodes.values().cloned().collect();
        let edges = state
            .edges
            .iter()
            .map(|(&store_id, edge)| (store_id, edge.clone()))
            .collect();
        Ok((nodes, edges))
    }

    /// Applies `changes` to a copy of the in-memory mirror, writes the whole
    /// document, and only then adopts the copy: a failed write leaves both
    /// the on-disk document and the mirror exactly as they were, so a later
    /// call never builds on a half-applied change set.
    /// A single-instance store needs no cross-process lock: this instance's own calls are already serialised by the
    /// `&mut` access every [`super::LinkGraph`] mutation takes.
    fn write_guard(&self) -> Result<GraphWriteGuard, SearchError> {
        Ok(GraphWriteGuard::unlocked())
    }

    fn persist(
        &self,
        _guard: &GraphWriteGuard,
        changes: &[GraphChange],
        next_edge_id: u64,
        completeness: StoreCompleteness,
    ) -> Result<(), SearchError> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut nodes = state.nodes.clone();
        let mut edges = state.edges.clone();
        for change in changes {
            match change {
                // `serde` already loses data outright under concurrent
                // instances (see `current_generation` above), so there is
                // no sibling for the marker to signal; it discards the
                // previous graph in the same write as everything after it.
                GraphChange::Reset => {
                    nodes.clear();
                    edges.clear();
                }
                GraphChange::RemoveNode(path) => {
                    nodes.remove(path);
                }
                GraphChange::UpsertNode(node) => {
                    nodes.insert(node.path.clone(), node.clone());
                }
                GraphChange::RemoveEdge { store_id } => {
                    edges.remove(store_id);
                }
                // The store holds no listing of files, and no other instance can share it.
                GraphChange::FileAdded(_) | GraphChange::FileRemoved { .. } => {}
                GraphChange::UpsertEdge { store_id, edge } => {
                    edges.insert(*store_id, edge.clone());
                }
            }
        }

        let incomplete = match completeness {
            StoreCompleteness::Complete => false,
            StoreCompleteness::Incomplete => true,
            StoreCompleteness::Unchanged => state.incomplete,
        };
        let document = SerdeDocument {
            format: STORE_FORMAT,
            next_edge_id,
            incomplete,
            nodes: nodes.values().cloned().collect(),
            edges: edges
                .iter()
                .map(|(&store_id, edge)| SerdeEdgeRecord {
                    store_id,
                    edge: edge.clone(),
                })
                .collect(),
        };
        write_atomically(&self.file_path, &document)?;

        state.nodes = nodes;
        state.edges = edges;
        state.next_edge_id = next_edge_id;
        state.incomplete = incomplete;
        state.load = SerdeLoad::Ready;
        Ok(())
    }

    /// Always `None`: `serde` already loses data outright under concurrent
    /// instances (its own module documentation above), so cross-instance
    /// propagation would be solving the wrong layer for this backend, not
    /// merely unimplemented.
    fn current_generation(&self) -> Result<Option<u64>, StoreFailure> {
        Ok(None)
    }

    /// Always `None`, for the same reason as [`SerdeGraphStore::current_generation`].
    fn changes_since(&self, _since: u64) -> Result<Option<GraphDelta>, StoreFailure> {
        Ok(None)
    }
}

/// Serialises `document` and writes it to `path` via a same-directory
/// temporary file plus an atomic rename, so a reader never observes a
/// partially written document, matching this project's write-atomicity
/// convention (`.claude/rules/architecture.md`) even though this store's
/// content is disposable, rebuildable derived state.
fn write_atomically(path: &Path, document: &SerdeDocument) -> Result<(), SearchError> {
    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let bytes = serde_json::to_vec(document).map_err(|source| graph_storage_error(directory, source))?;

    let temp_path = path.with_extension("json.tmp");
    std::fs::write(&temp_path, &bytes).map_err(|source| SearchError::GraphStorage {
        path: directory.display().to_string(),
        source,
    })?;
    std::fs::rename(&temp_path, path).map_err(|source| SearchError::GraphStorage {
        path: directory.display().to_string(),
        source,
    })
}
