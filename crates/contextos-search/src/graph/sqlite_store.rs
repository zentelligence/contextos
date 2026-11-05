//! `SQLite`-backed [`super::StoresGraph`] implementation
//! (`GraphBackend::Sqlite`): WAL journal mode with an explicit busy
//! timeout, and a write lock every instance shares, so more than one
//! `contextos` process can hold the same vault's graph store open and write
//! it concurrently: the whole reason this backend exists.
//!
//! # Schema
//!
//! Three tables, one row per record, keyed to match `fjall`'s three
//! keyspaces: `nodes(path PRIMARY KEY, value)`, `edges(store_id PRIMARY
//! KEY, value)`, `metadata(key PRIMARY KEY, value)`. `value` holds the same
//! JSON encoding [`super::encode`] already produces for every other
//! backend, stored as `BLOB` rather than re-derived as native SQL columns:
//! this store's job is durable storage of the records `LinkGraph` hands
//! it, not a second schema for the same domain types.
//!
//! A fourth table, `changelog`, backs cross-instance propagation: one row
//! per [`super::GraphChange`] ever persisted, in application
//! order (`seq`, an autoincrementing rowid), tagged with the `generation`
//! it was persisted at. `nodes`/`edges` stay the materialised "current
//! state" `load_all` reads; `changelog` is the append-only history
//! `changes_since` replays for a sibling instance's partial catch-up.
//! Deliberately not a "tag `nodes`/`edges` rows with their own generation"
//! design: that would need a live schema migration for every pre-existing
//! `sqlite` store, and would still need a separate tombstone mechanism for
//! deletions (a plain `DELETE` leaves nothing to diff against). A single
//! changelog table needs neither: it is new (no migration, `CREATE TABLE
//! IF NOT EXISTS` on an already-populated `nodes`/`edges` pair is enough),
//! and a removal is naturally its own row (`kind = 'remove_node'`/
//! `'remove_edge'`, `value` left `NULL`), no separate tombstone table
//! required. Pruned by generation on every `persist` call
//! ([`RETENTION_GENERATIONS`]) so it cannot grow unbounded; a sibling whose
//! last-seen generation has aged out of what remains gets `None` from
//! `changes_since` and must fall back to a full [`super::StoresGraph::load_all`].

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use rusqlite::{Connection, ErrorCode, OptionalExtension, params};

use super::{
    GraphChange, GraphDelta, GraphEdge, GraphNode, GraphRecords, GraphWriteGuard, JsonDecodeFailure, STORE_FORMAT,
    StoreCompleteness, StoreFailure, StoreFailureKind, StoredGraphMetadata, StoredGraphState, StoresGraph, encode,
    graph_storage_error,
};
use crate::SearchError;
use crate::store::{DerivedStore, StoreHealth, StoreInspection, StoreWriteLock};

const FILE_NAME: &str = "graph.sqlite3";
/// The store's write lock file (see [`SqliteGraphStore::write_guard`]). Never opened by `SQLite` itself, so its own
/// retry and busy handling never interacts with it.
const LOCK_FILE_NAME: &str = "graph.sqlite3.lock";

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const FORMAT_KEY: &str = "format_version";
const NEXT_EDGE_ID_KEY: &str = "next_edge_id";
const GENERATION_KEY: &str = "generation";
/// Present (as `1`) while the store has only ever been written note by note since it was created: it then holds only
/// the notes written since, not the vault's graph. Removed by a rebuild. A store without it, including every store
/// written before it existed, reads as complete.
const INCOMPLETE_KEY: &str = "incomplete";
/// How many generations of `changelog` history a store retains before
/// pruning the oldest. A starting estimate, not a measurement: large
/// enough that a sibling checking in reasonably often
/// during an active Cowork session always finds a partial catch-up
/// available, small enough that the changelog itself never becomes the
/// dominant cost of this store. A sibling that falls further behind than
/// this simply pays a full reload instead of erroring.
const RETENTION_GENERATIONS: u64 = 10_000;

pub(super) struct SqliteGraphStore {
    connection: Mutex<Connection>,
    directory: PathBuf,
    write_wait: Duration,
}

impl SqliteGraphStore {
    /// Opens (creating if absent) the `SQLite` database at
    /// `directory/graph.sqlite3`, switches it to WAL journal mode with an
    /// explicit busy timeout so a concurrent open from another process
    /// waits rather than failing outright, and ensures the `nodes`,
    /// `edges`, `metadata`, and `changelog` tables exist.
    ///
    /// # Errors
    ///
    /// Returns a storage error when `directory` cannot be created, or the
    /// database cannot be opened, configured, or migrated, and a busy failure
    /// when another instance holds the store's write lock past `write_wait`.
    pub(super) fn open(directory: &Path, write_wait: Duration) -> Result<Self, SearchError> {
        std::fs::create_dir_all(directory).map_err(|source| SearchError::GraphStorage {
            path: directory.display().to_string(),
            source,
        })?;
        let file_path = directory.join(FILE_NAME);
        // Opening switches the journal mode and creates the schema, which two instances opening a new store at once
        // would otherwise race on; the write lock orders them.
        let _lock = StoreWriteLock::acquire(DerivedStore::Graph, &directory.join(LOCK_FILE_NAME), write_wait)?;
        let connection = Self::open_connection(&file_path).map_err(|source| graph_storage_error(directory, source))?;

        Ok(Self {
            connection: Mutex::new(connection),
            directory: directory.to_path_buf(),
            write_wait,
        })
    }

    /// Opens and initialises one connection, retrying the whole sequence on
    /// `SQLITE_BUSY`/`SQLITE_LOCKED` for up to [`BUSY_TIMEOUT`]'s own
    /// budget.
    ///
    /// `busy_timeout` (set immediately after `Connection::open`, before the
    /// `journal_mode` pragma that itself briefly needs a lock) covers
    /// ordinary lock waits on an already-open connection, but does not
    /// reliably cover the specific race of two connections switching a
    /// brand-new database file to WAL mode and creating its schema at the
    /// same instant: confirmed empirically by
    /// `benches/graph_persistence.rs`'s concurrent-write contention test,
    /// which still hit `DatabaseBusy` here even with `busy_timeout` ordered
    /// first. Retrying the whole bootstrap sequence, not just a single
    /// statement, is the general-purpose fix for that race: the failed
    /// attempt's connection is dropped and a fresh one opened each time,
    /// since a connection that failed partway through WAL setup should not
    /// be reused.
    fn open_connection(file_path: &Path) -> rusqlite::Result<Connection> {
        let deadline = Instant::now() + BUSY_TIMEOUT;
        loop {
            match Self::try_open_connection(file_path) {
                Ok(connection) => return Ok(connection),
                Err(error) if is_transient_busy(&error) && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn try_open_connection(file_path: &Path) -> rusqlite::Result<Connection> {
        let connection = Connection::open(file_path)?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS nodes (path TEXT PRIMARY KEY, value BLOB NOT NULL);\
             CREATE TABLE IF NOT EXISTS edges (store_id INTEGER PRIMARY KEY, value BLOB NOT NULL);\
             CREATE TABLE IF NOT EXISTS metadata (key TEXT PRIMARY KEY, value INTEGER NOT NULL);\
             CREATE TABLE IF NOT EXISTS changelog (\
                 seq INTEGER PRIMARY KEY AUTOINCREMENT,\
                 generation INTEGER NOT NULL,\
                 kind TEXT NOT NULL,\
                 key TEXT NOT NULL,\
                 value BLOB\
             );\
             CREATE INDEX IF NOT EXISTS changelog_generation_idx ON changelog(generation);",
        )?;
        Ok(connection)
    }

    /// Applies one [`GraphChange`] to the live `nodes`/`edges` tables and
    /// records its own `changelog` row at `generation_value`, within `tx`.
    fn apply_one_change(
        &self,
        tx: &rusqlite::Transaction<'_>,
        generation_value: i64,
        change: &GraphChange,
    ) -> Result<(), SearchError> {
        match change {
            GraphChange::Reset => {
                // Inside the caller's transaction, so the discard commits
                // (or rolls back) together with the repopulation that
                // follows it; a reader never sees the emptied tables.
                tx.execute_batch("DELETE FROM nodes; DELETE FROM edges;")
                    .map_err(|source| graph_storage_error(&self.directory, source))?;
                self.record_change(tx, generation_value, "reset", "", None)?;
            }
            GraphChange::RemoveNode(path) => {
                tx.execute("DELETE FROM nodes WHERE path = ?1", params![path])
                    .map_err(|source| graph_storage_error(&self.directory, source))?;
                self.record_change(tx, generation_value, "remove_node", path, None)?;
            }
            GraphChange::UpsertNode(node) => {
                let value = encode(node, &self.directory)?;
                tx.execute(
                    "INSERT INTO nodes (path, value) VALUES (?1, ?2) \
                     ON CONFLICT(path) DO UPDATE SET value = excluded.value",
                    params![node.path, value],
                )
                .map_err(|source| graph_storage_error(&self.directory, source))?;
                self.record_change(tx, generation_value, "upsert_node", &node.path, Some(&value))?;
            }
            GraphChange::RemoveEdge { store_id } => {
                let store_id_value =
                    i64::try_from(*store_id).map_err(|source| graph_storage_error(&self.directory, source))?;
                tx.execute("DELETE FROM edges WHERE store_id = ?1", params![store_id_value])
                    .map_err(|source| graph_storage_error(&self.directory, source))?;
                self.record_change(tx, generation_value, "remove_edge", &store_id.to_string(), None)?;
            }
            GraphChange::FileAdded(path) => {
                self.record_change(tx, generation_value, "file_added", path, None)?;
            }
            GraphChange::FileRemoved { path, folder } => {
                let kind = if *folder { "folder_removed" } else { "file_removed" };
                self.record_change(tx, generation_value, kind, path, None)?;
            }
            GraphChange::UpsertEdge { store_id, edge } => {
                let value = encode(edge, &self.directory)?;
                let store_id_value =
                    i64::try_from(*store_id).map_err(|source| graph_storage_error(&self.directory, source))?;
                tx.execute(
                    "INSERT INTO edges (store_id, value) VALUES (?1, ?2) \
                     ON CONFLICT(store_id) DO UPDATE SET value = excluded.value",
                    params![store_id_value, value],
                )
                .map_err(|source| graph_storage_error(&self.directory, source))?;
                self.record_change(tx, generation_value, "upsert_edge", &store_id.to_string(), Some(&value))?;
            }
        }
        Ok(())
    }

    /// Inserts one `changelog` row.
    fn record_change(
        &self,
        tx: &rusqlite::Transaction<'_>,
        generation_value: i64,
        kind: &str,
        key: &str,
        value: Option<&[u8]>,
    ) -> Result<(), SearchError> {
        tx.execute(
            "INSERT INTO changelog (generation, kind, key, value) VALUES (?1, ?2, ?3, ?4)",
            params![generation_value, kind, key, value],
        )
        .map_err(|source| graph_storage_error(&self.directory, source))?;
        Ok(())
    }

    /// Unconditionally (re)writes `format_version`, `next_edge_id`, and
    /// `generation` in one call, matching [`StoresGraph::persist`]'s own
    /// documented contract, and sets or clears the incomplete mark as `completeness` says.
    fn write_metadata(
        &self,
        tx: &rusqlite::Transaction<'_>,
        next_edge_id: u64,
        generation_value: i64,
        completeness: StoreCompleteness,
    ) -> Result<(), SearchError> {
        match completeness {
            StoreCompleteness::Complete => {
                tx.execute("DELETE FROM metadata WHERE key = ?1", params![INCOMPLETE_KEY])
                    .map_err(|source| graph_storage_error(&self.directory, source))?;
            }
            StoreCompleteness::Incomplete => {
                tx.execute(
                    "INSERT INTO metadata (key, value) VALUES (?1, 1) \
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![INCOMPLETE_KEY],
                )
                .map_err(|source| graph_storage_error(&self.directory, source))?;
            }
            StoreCompleteness::Unchanged => {}
        }
        let next_edge_id_value =
            i64::try_from(next_edge_id).map_err(|source| graph_storage_error(&self.directory, source))?;
        for (key, value) in [
            (FORMAT_KEY, i64::from(STORE_FORMAT)),
            (NEXT_EDGE_ID_KEY, next_edge_id_value),
            (GENERATION_KEY, generation_value),
        ] {
            tx.execute(
                "INSERT INTO metadata (key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map_err(|source| graph_storage_error(&self.directory, source))?;
        }
        Ok(())
    }

    /// Deletes `changelog` rows older than [`RETENTION_GENERATIONS`].
    fn prune_changelog(&self, tx: &rusqlite::Transaction<'_>, generation: u64) -> Result<(), SearchError> {
        let retain_after = i64::try_from(generation.saturating_sub(RETENTION_GENERATIONS))
            .map_err(|source| graph_storage_error(&self.directory, source))?;
        tx.execute("DELETE FROM changelog WHERE generation <= ?1", params![retain_after])
            .map_err(|source| graph_storage_error(&self.directory, source))?;
        Ok(())
    }

    fn persist_once(
        &self,
        changes: &[GraphChange],
        next_edge_id: u64,
        completeness: StoreCompleteness,
    ) -> Result<(), SearchError> {
        let mut connection = self.connection.lock().unwrap_or_else(PoisonError::into_inner);
        let tx = connection
            .transaction()
            .map_err(|source| graph_storage_error(&self.directory, source))?;

        let previous_generation = read_metadata_u64(&tx, GENERATION_KEY)
            .map_err(|source| graph_storage_error(&self.directory, source))?
            .unwrap_or(0);
        let generation = previous_generation.saturating_add(1);
        let generation_value =
            i64::try_from(generation).map_err(|source| graph_storage_error(&self.directory, source))?;

        for change in changes {
            self.apply_one_change(&tx, generation_value, change)?;
        }
        self.write_metadata(&tx, next_edge_id, generation_value, completeness)?;
        self.prune_changelog(&tx, generation)?;

        tx.commit()
            .map_err(|source| graph_storage_error(&self.directory, source))
    }
}

/// Whether `error` is the transient `SQLITE_BUSY`/`SQLITE_LOCKED` this
/// module retries opening through, rather than a genuine failure (a
/// permissions problem, corrupted file, or malformed schema) that retrying
/// cannot fix.
fn is_transient_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(inner, _)
            if matches!(inner.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

/// The [`is_transient_busy`] check, applied through the `rusqlite::Error`
/// [`graph_storage_error`] boxes inside [`SearchError::GraphStorage`]'s
/// `std::io::Error::other` source.
fn is_transient_busy_search_error(error: &SearchError) -> bool {
    match error {
        SearchError::GraphStorage { source, .. } => source
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<rusqlite::Error>())
            .is_some_and(is_transient_busy),
        _ => false,
    }
}

/// Reads one `INTEGER` metadata row as stored, or `None` when the key is absent (never written yet).
fn read_metadata_value(connection: &Connection, key: &str) -> Result<Option<i64>, rusqlite::Error> {
    connection
        .query_row("SELECT value FROM metadata WHERE key = ?1", params![key], |row| {
            row.get(0)
        })
        .optional()
}

/// Reads one `INTEGER` metadata row as a `u64`, or `None` when the key is absent; for writes, where a value that does
/// not fit is treated as absent and rewritten.
fn read_metadata_u64(connection: &Connection, key: &str) -> Result<Option<u64>, rusqlite::Error> {
    Ok(read_metadata_value(connection, key)?.and_then(|value| u64::try_from(value).ok()))
}

/// One `rusqlite` failure while reading the store, with what was being read.
struct SqliteRead<'a> {
    directory: &'a Path,
    what: &'static str,
    error: &'a rusqlite::Error,
}

impl From<SqliteRead<'_>> for StoreFailure {
    /// Classifies a read failure by `SQLite`'s own result code: busy or locked past the busy timeout, a file `SQLite`
    /// reports as malformed or not a database, a value of the wrong type or range in a row, or (anything else) a
    /// failure to read the file.
    fn from(value: SqliteRead<'_>) -> Self {
        let path = value.directory.display().to_string();
        let detail = format!("{}: {}", value.what, value.error);
        match value.error {
            rusqlite::Error::SqliteFailure(inner, _) => match inner.code {
                ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => Self::busy(DerivedStore::Graph, path, detail),
                ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase => {
                    Self::corrupt(DerivedStore::Graph, path, detail)
                }
                _ => Self::io(DerivedStore::Graph, path, detail),
            },
            rusqlite::Error::FromSqlConversionFailure(..)
            | rusqlite::Error::IntegralValueOutOfRange(..)
            | rusqlite::Error::InvalidColumnType(..)
            | rusqlite::Error::Utf8Error(..) => Self::undecodable(DerivedStore::Graph, path, detail),
            _ => Self::io(DerivedStore::Graph, path, detail),
        }
    }
}

impl StoresGraph for SqliteGraphStore {
    fn read_metadata(&self) -> Result<StoredGraphState, StoreFailure> {
        let connection = self.connection.lock().unwrap_or_else(PoisonError::into_inner);

        let Some(format) = self.metadata(&connection, FORMAT_KEY, "format_version metadata")? else {
            return Ok(StoredGraphState::Empty);
        };
        if u32::try_from(format).ok() != Some(STORE_FORMAT) {
            return Ok(StoredGraphState::Incompatible);
        }
        let next_edge_id = self
            .metadata(&connection, NEXT_EDGE_ID_KEY, "next_edge_id metadata")?
            .ok_or_else(|| self.undecodable("next_edge_id metadata is missing from a written store"))?;
        // A `generation` key absent entirely means this store predates the
        // changelog table's introduction: treated leniently as `0`, the
        // oldest possible generation, an in-place additive capability
        // rather than a format mismatch forcing a full rebuild (see this
        // module's own doc comment on why no schema migration is needed for
        // `nodes`/`edges` at all).
        let generation = self
            .metadata(&connection, GENERATION_KEY, "generation metadata")?
            .unwrap_or(0);

        let complete = self
            .metadata(&connection, INCOMPLETE_KEY, "incomplete metadata")?
            .is_none();

        Ok(StoredGraphState::Ready(StoredGraphMetadata {
            complete,
            next_edge_id,
            generation,
        }))
    }

    fn load_all(&self) -> Result<GraphRecords, StoreFailure> {
        let connection = self.connection.lock().unwrap_or_else(PoisonError::into_inner);

        let mut nodes_stmt = connection
            .prepare("SELECT value FROM nodes")
            .map_err(|error| self.read_failure("preparing the nodes query", &error))?;
        let nodes = nodes_stmt
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .map_err(|error| self.read_failure("running the nodes query", &error))?
            .map(|value| {
                let value = value.map_err(|error| self.read_failure("reading a node row", &error))?;
                serde_json::from_slice::<GraphNode>(&value)
                    .map_err(|error| self.decode_failure("a node row", &JsonDecodeFailure(&error)))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let mut edges_stmt = connection
            .prepare("SELECT store_id, value FROM edges")
            .map_err(|error| self.read_failure("preparing the edges query", &error))?;
        let edges = edges_stmt
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)))
            .map_err(|error| self.read_failure("running the edges query", &error))?
            .map(|row| {
                let (store_id, value) = row.map_err(|error| self.read_failure("reading an edge row", &error))?;
                let store_id =
                    u64::try_from(store_id).map_err(|error| self.decode_failure("an edge row's store id", &error))?;
                let edge = serde_json::from_slice::<GraphEdge>(&value)
                    .map_err(|error| self.decode_failure("an edge row", &JsonDecodeFailure(&error)))?;
                Ok((store_id, edge))
            })
            .collect::<Result<Vec<_>, StoreFailure>>()?;

        Ok((nodes, edges))
    }

    /// Persists `changes`, retrying the whole transaction on
    /// `SQLITE_BUSY`/`SQLITE_LOCKED` for up to [`BUSY_TIMEOUT`]'s own
    /// budget, the same defence [`Self::open_connection`] already applies
    /// to opening: this connection's own `busy_timeout` pragma covers an
    /// ordinary lock wait, but a sibling instance's concurrent writer
    /// transaction can still surface as an immediate `DatabaseBusy` here
    /// rather than a retried wait, particularly under CI-runner disk
    /// contention. A failed attempt's transaction is dropped (rolling back)
    /// before the next attempt begins a fresh one.
    ///
    /// A write that still fails is reported by the same classification as a read where `SQLite` says why: a damaged
    /// store as `index/corrupt`, so a rebuild against it is told to delete the store first rather than to rebuild
    /// again, and a store still busy past the budget as `index/busy`.
    /// Takes the store's write lock, which every instance shares: `SQLite`'s own write-ahead-log locking was observed
    /// on real Windows hardware, with two `contextos` processes writing the same vault's store at once, to end in real
    /// on-disk damage (`PRAGMA integrity_check` reported B-tree damage, not a busy error), so writers do not rely on it
    /// alone. Reads take no lock: in write-ahead-log mode a reader never waits for a writer.
    fn write_guard(&self) -> Result<GraphWriteGuard, SearchError> {
        let lock = StoreWriteLock::acquire(
            DerivedStore::Graph,
            &self.directory.join(LOCK_FILE_NAME),
            self.write_wait,
        )?;
        Ok(GraphWriteGuard::locked(lock))
    }

    fn persist(
        &self,
        _guard: &GraphWriteGuard,
        changes: &[GraphChange],
        next_edge_id: u64,
        completeness: StoreCompleteness,
    ) -> Result<(), SearchError> {
        let deadline = Instant::now() + BUSY_TIMEOUT;
        loop {
            match self.persist_once(changes, next_edge_id, completeness) {
                Ok(()) => return Ok(()),
                Err(error) if is_transient_busy_search_error(&error) && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => return Err(self.classified_write_failure(error)),
            }
        }
    }

    fn current_generation(&self) -> Result<Option<u64>, StoreFailure> {
        let connection = self.connection.lock().unwrap_or_else(PoisonError::into_inner);
        self.metadata(&connection, GENERATION_KEY, "generation metadata")
    }

    fn changes_since(&self, since: u64) -> Result<Option<GraphDelta>, StoreFailure> {
        let connection = self.connection.lock().unwrap_or_else(PoisonError::into_inner);

        let Some(current) = self.metadata(&connection, GENERATION_KEY, "generation metadata")? else {
            // Never written: nothing to catch up to, and no history to answer from.
            return Ok(None);
        };
        if since >= current {
            return Ok(Some(GraphDelta {
                changes: Vec::new(),
                generation: current,
            }));
        }
        // Anything at or before this generation may already be pruned:
        // completeness cannot be guaranteed, so the caller must fall back
        // to a full reload rather than risk an incomplete partial delta.
        let oldest_safe = current.saturating_sub(RETENTION_GENERATIONS);
        if since < oldest_safe {
            return Ok(None);
        }
        // `since < current`, and `current` came from an `i64` column, so it fits.
        let since_value =
            i64::try_from(since).map_err(|error| self.decode_failure("the requested generation", &error))?;

        // A `rebuild` replaced the whole graph somewhere in this range: the
        // ordinary upsert/remove vocabulary cannot represent that, so a
        // partial merge here would silently keep records the rebuild
        // actually dropped. Force the full-reload fallback instead of
        // returning an incomplete delta.
        let reset_in_range: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM changelog WHERE generation > ?1 AND kind = 'reset')",
                params![since_value],
                |row| row.get(0),
            )
            .map_err(|error| self.read_failure("checking the changelog for a reset", &error))?;
        if reset_in_range {
            return Ok(None);
        }

        let mut stmt = connection
            .prepare(
                "SELECT kind, key, value FROM changelog \
                 WHERE generation > ?1 ORDER BY seq",
            )
            .map_err(|error| self.read_failure("preparing the changelog query", &error))?;
        let changes = stmt
            .query_map(params![since_value], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<Vec<u8>>>(2)?,
                ))
            })
            .map_err(|error| self.read_failure("running the changelog query", &error))?
            .map(|row| {
                let (kind, key, value) = row.map_err(|error| self.read_failure("reading a changelog row", &error))?;
                decode_changelog_row(&kind, &key, value).ok_or_else(|| {
                    self.undecodable(
                        "a changelog row has an unrecognised kind, a non-numeric edge key, or an undecodable value",
                    )
                })
            })
            .collect::<Result<Vec<GraphChange>, StoreFailure>>()?;

        Ok(Some(GraphDelta {
            changes,
            generation: current,
        }))
    }
}

impl SqliteGraphStore {
    /// Reads one metadata value as a `u64`, or `None` when the key is absent; a stored value that is not a valid
    /// `u64` is undecodable rather than treated as absent.
    fn metadata(&self, connection: &Connection, key: &str, what: &'static str) -> Result<Option<u64>, StoreFailure> {
        read_metadata_value(connection, key)
            .map_err(|error| self.read_failure(what, &error))?
            .map(|value| u64::try_from(value).map_err(|error| self.decode_failure(what, &error)))
            .transpose()
    }

    /// Classifies a `rusqlite` read failure, logging its real cause: the store's own path and the error, never vault
    /// content.
    fn read_failure(&self, what: &'static str, error: &rusqlite::Error) -> StoreFailure {
        let failure = StoreFailure::from(SqliteRead {
            directory: &self.directory,
            what,
            error,
        });
        self.log_read_failure(&failure);
        failure
    }

    /// Classifies a write that failed with a `rusqlite` error `SQLite` attributes to a damaged or busy store, reporting
    /// it as that read failure; every other write failure is returned as it was.
    fn classified_write_failure(&self, error: SearchError) -> SearchError {
        let SearchError::GraphStorage { source, .. } = &error else {
            return error;
        };
        let Some(sqlite) = source
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<rusqlite::Error>())
        else {
            return error;
        };
        let failure = StoreFailure::from(SqliteRead {
            directory: &self.directory,
            what: "writing",
            error: sqlite,
        });
        match failure.kind() {
            StoreFailureKind::Corrupt | StoreFailureKind::Busy => {
                self.log_read_failure(&failure);
                SearchError::from(failure)
            }
            StoreFailureKind::Undecodable | StoreFailureKind::Io => error,
        }
    }

    /// An undecodable failure for a stored value that could not be decoded.
    fn decode_failure<E: std::fmt::Display>(&self, what: &str, error: &E) -> StoreFailure {
        self.undecodable(&format!("{what}: {error}"))
    }

    /// An undecodable failure described by `detail`.
    fn undecodable(&self, detail: &str) -> StoreFailure {
        let failure = StoreFailure::undecodable(DerivedStore::Graph, self.directory.display().to_string(), detail);
        self.log_read_failure(&failure);
        failure
    }

    /// Logs a read failure at `warn` with its own stable code, so an operator with tracing enabled sees every failed
    /// read, including one a caller handles without surfacing it.
    fn log_read_failure(&self, failure: &StoreFailure) {
        tracing::warn!(
            code = failure.code(),
            path = %self.directory.display(),
            %failure,
            "sqlite graph store read failed"
        );
    }
}

/// Inspects the `sqlite` store in `directory` without writing: whether it has been built, whether `SQLite` finds it
/// intact, whether its metadata reads as a complete graph in this build's format with a changelog no newer than its
/// generation, and how many notes it holds.
pub(super) fn inspect(directory: &Path) -> StoreInspection {
    let file = directory.join(FILE_NAME);
    let location = directory.display().to_string();
    let inspection = |health, records| StoreInspection {
        store: DerivedStore::Graph,
        location: directory.to_path_buf(),
        health,
        records,
    };
    let unreadable = |failure: StoreFailure| inspection(Some(StoreHealth::Unreadable(failure)), 0);
    let read_failure =
        |what: &'static str, error: &rusqlite::Error| StoreFailure::from(SqliteRead { directory, what, error });
    if !file.exists() {
        return inspection(None, 0);
    }
    let connection = match crate::store::open_sqlite_for_reading(&file) {
        Ok(connection) => connection,
        Err(error) => return unreadable(read_failure("opening the store", &error)),
    };
    match crate::store::sqlite_damage(&connection) {
        Ok(None) => {}
        Ok(Some(damage)) => return unreadable(StoreFailure::corrupt(DerivedStore::Graph, location, damage)),
        Err(error) => return unreadable(read_failure("checking the store's integrity", &error)),
    }
    let metadata = |key: &str| read_metadata_value(&connection, key);
    let state = (|| -> Result<StoreHealth, StoreFailure> {
        let Some(format) = metadata(FORMAT_KEY).map_err(|error| read_failure("format_version metadata", &error))?
        else {
            return Ok(StoreHealth::Empty);
        };
        if u32::try_from(format).ok() != Some(STORE_FORMAT) {
            return Ok(StoreHealth::Incompatible);
        }
        if metadata(INCOMPLETE_KEY)
            .map_err(|error| read_failure("incomplete metadata", &error))?
            .is_some()
        {
            return Ok(StoreHealth::Empty);
        }
        let generation = metadata(GENERATION_KEY)
            .map_err(|error| read_failure("generation metadata", &error))?
            .unwrap_or(0);
        let newest: Option<i64> = connection
            .query_row("SELECT MAX(generation) FROM changelog", [], |row| row.get(0))
            .map_err(|error| read_failure("the changelog's newest generation", &error))?;
        if newest.is_some_and(|newest| newest > generation) {
            return Ok(StoreHealth::Unreadable(StoreFailure::undecodable(
                DerivedStore::Graph,
                location.clone(),
                format!("the changelog reaches generation {newest:?}, past the store's own generation {generation}"),
            )));
        }
        Ok(StoreHealth::Ready)
    })();
    let health = match state {
        Ok(health) => health,
        Err(failure) => return unreadable(failure),
    };
    let nodes = connection
        .query_row("SELECT COUNT(*) FROM nodes", [], |row| row.get::<_, i64>(0))
        .map_err(|error| read_failure("counting the nodes", &error));
    match nodes {
        Ok(nodes) => inspection(Some(health), usize::try_from(nodes).unwrap_or(0)),
        Err(failure) => unreadable(failure),
    }
}

/// Decodes one `changelog` row back into the [`GraphChange`] it recorded,
/// or `None` on any malformed row (unrecognised `kind`, non-numeric edge
/// `key`, or undecodable JSON `value`): the same all-or-nothing contract
/// every other read in this module shares, aborting the whole
/// `changes_since` call rather than admitting a partially reconstructed
/// delta.
fn decode_changelog_row(kind: &str, key: &str, value: Option<Vec<u8>>) -> Option<GraphChange> {
    match kind {
        "remove_node" => Some(GraphChange::RemoveNode(key.to_owned())),
        "upsert_node" => {
            let node: GraphNode = serde_json::from_slice(&value?).ok()?;
            Some(GraphChange::UpsertNode(node))
        }
        "remove_edge" => {
            let store_id: u64 = key.parse().ok()?;
            Some(GraphChange::RemoveEdge { store_id })
        }
        "file_added" => Some(GraphChange::FileAdded(key.to_owned())),
        "file_removed" | "folder_removed" => Some(GraphChange::FileRemoved {
            path: key.to_owned(),
            folder: kind == "folder_removed",
        }),
        "upsert_edge" => {
            let store_id: u64 = key.parse().ok()?;
            let edge: GraphEdge = serde_json::from_slice(&value?).ok()?;
            Some(GraphChange::UpsertEdge { store_id, edge })
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "sqlite_store_test.rs"]
mod tests;
