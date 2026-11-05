//! `fjall`-backed [`super::StoresGraph`] implementation (`GraphBackend::Fjall`):
//! an embedded key-value store, single-process-exclusive.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use fjall::{Database, Keyspace, KeyspaceCreateOptions, PersistMode, Readable};

use super::{
    GraphChange, GraphDelta, GraphEdge, GraphNode, GraphRecords, GraphWriteGuard, JsonDecodeFailure, STORE_FORMAT,
    StoreCompleteness, StoreFailure, StoredGraphMetadata, StoredGraphState, StoresGraph, encode, graph_storage_error,
};
use crate::SearchError;
use crate::store::DerivedStore;

const META_FORMAT_KEY: &[u8] = b"format_version";
const META_NEXT_EDGE_ID_KEY: &[u8] = b"next_edge_id";
/// Present while the store has only ever been written note by note since it was created; removed by a rebuild. A
/// store without it reads as complete.
const META_INCOMPLETE_KEY: &[u8] = b"incomplete";
const NODES_KEYSPACE: &str = "nodes";
const EDGES_KEYSPACE: &str = "edges";
const METADATA_KEYSPACE: &str = "metadata";

/// The three `fjall` keyspaces backing one link graph's persisted state,
/// plus the database handle they share.
pub(super) struct FjallGraphStore {
    database: Database,
    nodes: Keyspace,
    edges: Keyspace,
    metadata: Keyspace,
    directory: PathBuf,
}

impl FjallGraphStore {
    /// Opens (creating if absent) the `fjall` database at `directory` and
    /// its three keyspaces.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the database or any keyspace cannot be
    /// opened, for example due to a permissions problem or genuinely
    /// corrupted on-disk state. This is a narrower failure than the former
    /// JSON cache ever raised (a missing or malformed *file* was always
    /// trivially recoverable by treating it as absent), and deliberately
    /// aligns `LinkGraph`'s fallibility with this crate's other derived
    /// stores (`TantivyIndex`, `SqliteVecStore`), both of which already
    /// surface a genuine storage-open failure rather than masking it.
    ///
    /// A store already held open by another process (`fjall::Error::Locked`,
    /// its exclusive lock file could not be acquired) is reported as
    /// [`SearchError::GraphLocked`] rather than the generic
    /// [`SearchError::GraphStorage`], so `VaultSearchService::try_from` can
    /// tell contention on a live store apart from genuine corruption and
    /// degrade only the former.
    pub(super) fn open(directory: &Path) -> Result<Self, SearchError> {
        let database = Database::builder(directory).open().map_err(|source| {
            if matches!(source, fjall::Error::Locked) {
                SearchError::GraphLocked {
                    path: directory.display().to_string(),
                }
            } else {
                graph_storage_error(directory, source)
            }
        })?;
        let nodes = database
            .keyspace(NODES_KEYSPACE, KeyspaceCreateOptions::default)
            .map_err(|source| graph_storage_error(directory, source))?;
        let edges = database
            .keyspace(EDGES_KEYSPACE, KeyspaceCreateOptions::default)
            .map_err(|source| graph_storage_error(directory, source))?;
        let metadata = database
            .keyspace(METADATA_KEYSPACE, KeyspaceCreateOptions::default)
            .map_err(|source| graph_storage_error(directory, source))?;
        Ok(Self {
            database,
            nodes,
            edges,
            metadata,
            directory: directory.to_path_buf(),
        })
    }

    /// Queues the removal of every stored node and edge that `changes` does
    /// not itself upsert, so a [`GraphChange::Reset`] discards the previous
    /// graph inside the caller's batch rather than in a separate, earlier
    /// write. Keys the batch re-inserts are left alone, so the outcome never
    /// depends on how a batch orders a removal and an insert of one key.
    fn discard_records_not_upserted(
        &self,
        batch: &mut fjall::OwnedWriteBatch,
        changes: &[GraphChange],
    ) -> Result<(), SearchError> {
        let upserted_nodes: HashSet<&[u8]> = changes
            .iter()
            .filter_map(|change| match change {
                GraphChange::UpsertNode(node) => Some(node.path.as_bytes()),
                _ => None,
            })
            .collect();
        let upserted_edges: HashSet<u64> = changes
            .iter()
            .filter_map(|change| match change {
                GraphChange::UpsertEdge { store_id, .. } => Some(*store_id),
                _ => None,
            })
            .collect();

        let snapshot = self.database.snapshot();
        for item in snapshot.iter(&self.nodes) {
            let (key, _) = item
                .into_inner()
                .map_err(|source| graph_storage_error(&self.directory, source))?;
            if !upserted_nodes.contains(&*key) {
                batch.remove(&self.nodes, key);
            }
        }
        for item in snapshot.iter(&self.edges) {
            let (key, _) = item
                .into_inner()
                .map_err(|source| graph_storage_error(&self.directory, source))?;
            let is_upserted = decode_edge_key(&key).is_some_and(|store_id| upserted_edges.contains(&store_id));
            if !is_upserted {
                batch.remove(&self.edges, key);
            }
        }
        Ok(())
    }
}

impl StoresGraph for FjallGraphStore {
    fn read_metadata(&self) -> Result<StoredGraphState, StoreFailure> {
        let snapshot = self.database.snapshot();
        let Some(format) = self.read_bytes::<4>(&snapshot, META_FORMAT_KEY, "format_version metadata")? else {
            return Ok(StoredGraphState::Empty);
        };
        if u32::from_be_bytes(format) != STORE_FORMAT {
            return Ok(StoredGraphState::Incompatible);
        }
        let next_edge_id = self
            .read_bytes::<8>(&snapshot, META_NEXT_EDGE_ID_KEY, "next_edge_id metadata")?
            .map(u64::from_be_bytes)
            .ok_or_else(|| self.undecodable("next_edge_id metadata is missing from a written store"))?;
        let complete = snapshot
            .get(&self.metadata, META_INCOMPLETE_KEY)
            .map_err(|error| self.read_failure("incomplete metadata", &error))?
            .is_none();
        Ok(StoredGraphState::Ready(StoredGraphMetadata {
            complete,
            next_edge_id,
            generation: 0,
        }))
    }

    fn load_all(&self) -> Result<GraphRecords, StoreFailure> {
        let snapshot = self.database.snapshot();

        let mut nodes = Vec::new();
        for item in snapshot.iter(&self.nodes) {
            let (_, value) = item
                .into_inner()
                .map_err(|error| self.read_failure("reading a node", &error))?;
            let node: GraphNode = serde_json::from_slice(&value)
                .map_err(|error| self.undecodable(&format!("a node record {}", JsonDecodeFailure(&error))))?;
            nodes.push(node);
        }

        let mut edges = Vec::new();
        for item in snapshot.iter(&self.edges) {
            let (key, value) = item
                .into_inner()
                .map_err(|error| self.read_failure("reading an edge", &error))?;
            let store_id = decode_edge_key(&key).ok_or_else(|| self.undecodable("an edge key is not an 8-byte id"))?;
            let edge: GraphEdge = serde_json::from_slice(&value)
                .map_err(|error| self.undecodable(&format!("an edge record {}", JsonDecodeFailure(&error))))?;
            edges.push((store_id, edge));
        }

        Ok((nodes, edges))
    }

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
        let mut batch = self.database.batch().durability(Some(PersistMode::SyncAll));

        if changes.iter().any(|change| matches!(change, GraphChange::Reset)) {
            self.discard_records_not_upserted(&mut batch, changes)?;
        }

        for change in changes {
            match change {
                // Handled above, in the same batch as everything else: the
                // discard commits (or is abandoned) with the repopulation.
                // `fjall`'s exclusive lock means a sibling can never be
                // concurrently open, so the marker has nothing further to
                // record for propagation.
                //
                // The store also holds no listing of files, which is only ever recorded for other instances sharing a
                // store, and none can share this one.
                GraphChange::Reset | GraphChange::FileAdded(_) | GraphChange::FileRemoved { .. } => {}
                GraphChange::RemoveNode(path) => {
                    batch.remove(&self.nodes, path.as_bytes());
                }
                GraphChange::UpsertNode(node) => {
                    let value = encode(node, &self.directory)?;
                    batch.insert(&self.nodes, node.path.as_bytes(), value);
                }
                GraphChange::RemoveEdge { store_id } => {
                    batch.remove(&self.edges, store_id.to_be_bytes());
                }
                GraphChange::UpsertEdge { store_id, edge } => {
                    let value = encode(edge, &self.directory)?;
                    batch.insert(&self.edges, store_id.to_be_bytes(), value);
                }
            }
        }

        batch.insert(&self.metadata, META_FORMAT_KEY, STORE_FORMAT.to_be_bytes());
        batch.insert(&self.metadata, META_NEXT_EDGE_ID_KEY, next_edge_id.to_be_bytes());
        match completeness {
            StoreCompleteness::Complete => batch.remove(&self.metadata, META_INCOMPLETE_KEY),
            StoreCompleteness::Incomplete => batch.insert(&self.metadata, META_INCOMPLETE_KEY, [1_u8]),
            StoreCompleteness::Unchanged => {}
        }

        batch
            .commit()
            .map_err(|source| graph_storage_error(&self.directory, source))
    }

    /// Always `None`: `fjall`'s exclusive lock means a sibling instance can
    /// never be concurrently open against the same store to begin with, so
    /// cross-instance propagation is structurally unnecessary for this
    /// backend, not merely unimplemented.
    fn current_generation(&self) -> Result<Option<u64>, StoreFailure> {
        Ok(None)
    }

    /// Always `None`, for the same reason as [`FjallGraphStore::current_generation`].
    fn changes_since(&self, _since: u64) -> Result<Option<GraphDelta>, StoreFailure> {
        Ok(None)
    }
}

impl FjallGraphStore {
    /// Reads one metadata value of exactly `N` bytes, or `None` when the key is absent; a value of any other length is
    /// undecodable.
    fn read_bytes<const N: usize>(
        &self,
        snapshot: &fjall::Snapshot,
        key: &[u8],
        what: &'static str,
    ) -> Result<Option<[u8; N]>, StoreFailure> {
        let Some(bytes) = snapshot
            .get(&self.metadata, key)
            .map_err(|error| self.read_failure(what, &error))?
        else {
            return Ok(None);
        };
        let bytes: [u8; N] = bytes
            .as_ref()
            .try_into()
            .map_err(|_| self.undecodable(&format!("{what} is not {N} bytes")))?;
        Ok(Some(bytes))
    }

    fn read_failure(&self, what: &'static str, error: &fjall::Error) -> StoreFailure {
        StoreFailure::from(FjallRead {
            directory: &self.directory,
            what,
            error,
        })
    }

    fn undecodable(&self, detail: &str) -> StoreFailure {
        StoreFailure::undecodable(DerivedStore::Graph, self.directory.display().to_string(), detail)
    }
}

/// One `fjall` failure while reading the store, with what was being read.
struct FjallRead<'a> {
    directory: &'a Path,
    what: &'static str,
    error: &'a fjall::Error,
}

impl From<FjallRead<'_>> for StoreFailure {
    /// Classifies a read failure: a damaged file the engine detects (a checksum, header, tag, trailer, compression,
    /// version, or journal fault, or an engine that gave up), the lock, or (anything else) a failure to read from disk.
    fn from(value: FjallRead<'_>) -> Self {
        let path = value.directory.display().to_string();
        let detail = format!("{}: {}", value.what, value.error);
        match value.error {
            // `Poisoned`: an earlier write failed and the engine refuses further work. The files are not known to be
            // damaged, and a restart, not a deletion, is the remedy.
            fjall::Error::Io(_)
            | fjall::Error::Storage(fjall::LsmError::Io(_))
            | fjall::Error::KeyspaceDeleted
            | fjall::Error::Poisoned => Self::io(DerivedStore::Graph, path, detail),
            fjall::Error::Locked => Self::busy(DerivedStore::Graph, path, detail),
            fjall::Error::Storage(_)
            | fjall::Error::JournalRecovery(_)
            | fjall::Error::InvalidVersion(_)
            | fjall::Error::Decompress(_)
            | fjall::Error::InvalidTrailer
            | fjall::Error::InvalidTag(_)
            | fjall::Error::Unrecoverable => Self::corrupt(DerivedStore::Graph, path, detail),
            // `fjall::Error` is non-exhaustive: a kind added by a later release is reported as a failure to read,
            // the one classification that claims nothing about the store's state.
            _ => Self::io(DerivedStore::Graph, path, detail),
        }
    }
}

/// Decodes an `edges` keyspace key back into the `u64` store id it encodes.
fn decode_edge_key(key: &[u8]) -> Option<u64> {
    let bytes: [u8; 8] = key.try_into().ok()?;
    Some(u64::from_be_bytes(bytes))
}

#[cfg(test)]
#[path = "fjall_store_test.rs"]
mod tests;
