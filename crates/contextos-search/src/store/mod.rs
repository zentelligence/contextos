//! What every derived store a vault keeps in its state directory shares: which store it is, how a failure to read or
//! write it is reported, and its health.

mod failure;
mod holders;
mod lock;
mod quarantine;

use std::path::Path;

use rusqlite::{Connection, OpenFlags};

pub use failure::{StoreFailure, StoreFailureKind, StoreHealth};
pub use holders::{HolderCensus, HolderRegistry, StoreHolder};
pub use lock::{DEFAULT_WRITE_WAIT, StoreWriteLock, lock_path_beside};
pub use quarantine::{StoreRecovery, quarantine, quarantined_count};

/// What a read-only inspection of a derived store found, for diagnostics that must not change a vault's derived state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreInspection {
    pub store: DerivedStore,
    /// Where the store lives.
    pub location: std::path::PathBuf,
    /// What the store holds, or `None` when it has never been built.
    pub health: Option<StoreHealth>,
    /// How many documents (text), real and phantom notes (graph), or chunks (vectors) the store holds; `0` when it
    /// cannot be read.
    pub records: usize,
}

/// Opens an existing `SQLite` store for reading only, leaving nothing behind: without `SQLITE_OPEN_CREATE`, so a store
/// removed since it was found fails to open rather than being made, and with `query_only`, so nothing this connection
/// runs can write. It is opened read-write nonetheless, because a read-only connection to a store in write-ahead-log
/// mode creates the log's side files and cannot remove them when it closes, where the last read-write connection to
/// close removes them.
pub(crate) fn open_sqlite_for_reading(path: &Path) -> rusqlite::Result<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(DEFAULT_WRITE_WAIT)?;
    connection.pragma_update(None, "query_only", true)?;
    Ok(connection)
}

/// Runs `SQLite`'s quick integrity check over `connection`, returning the first problem it reports, or `None` when the
/// store is intact.
///
/// # Errors
///
/// Returns the `rusqlite` error when the check itself cannot run.
pub(crate) fn sqlite_damage(connection: &Connection) -> rusqlite::Result<Option<String>> {
    let first: String = connection.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
    Ok((first != "ok").then_some(first))
}

/// One of the derived stores a vault keeps in its state directory, each rebuildable from the vault.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DerivedStore {
    /// The tantivy full-text index (`index/`).
    Text,
    /// The link graph store (`graph/`).
    Graph,
    /// The semantic vector store (`vectors.db`).
    Vectors,
}

impl DerivedStore {
    /// How the store is named in messages.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Text => "text index",
            Self::Graph => "link graph store",
            Self::Vectors => "vector store",
        }
    }

    /// The stable lowercase name the store goes by on the wire and on disk.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Graph => "graph",
            Self::Vectors => "vectors",
        }
    }
}
