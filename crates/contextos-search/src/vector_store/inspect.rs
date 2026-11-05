//! Reading a vault's vector store without changing it, for diagnostics.

use std::path::Path;

use super::{VectorStoreStats, register_sqlite_vec, storage_error};
use crate::SearchError;
use crate::store::{DerivedStore, StoreFailure, StoreHealth, StoreInspection};

/// What a vector store holds, read without writing, for diagnostics that must not change a vault's derived state:
/// reading never creates the database, its directory, or its schema, so a store that was never built reads as
/// [`VectorStoreState::Absent`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VectorStoreState {
    /// No store exists at the path yet: nothing has been embedded for this vault.
    Absent,
    /// The store exists and holds these counts.
    Present(VectorStoreStats),
}

impl TryFrom<&Path> for VectorStoreState {
    type Error = SearchError;

    /// Reads the store at `value` through a read-only connection.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the path exists but cannot be opened read-only or read as a vector store.
    fn try_from(value: &Path) -> Result<Self, Self::Error> {
        let db_path = value.display().to_string();
        match std::fs::metadata(value) {
            Ok(_) => {}
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Self::Absent),
            Err(source) => {
                return Err(SearchError::IndexDirectory { path: db_path, source });
            }
        }
        register_sqlite_vec();
        let connection =
            crate::store::open_sqlite_for_reading(value).map_err(|source| storage_error(&db_path, source))?;
        let (documents, chunks): (i64, i64) = connection
            .query_row("SELECT COUNT(DISTINCT path), COUNT(*) FROM chunks", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .map_err(|source| storage_error(&db_path, source))?;
        Ok(Self::Present(VectorStoreStats {
            documents: usize::try_from(documents).unwrap_or(0),
            chunks: usize::try_from(chunks).unwrap_or(0),
        }))
    }
}

/// Inspects the vector store at `path` without writing: whether it has been built, whether `SQLite` finds it intact and
/// readable, and how many chunks it holds.
#[must_use]
pub fn inspect_vector_store(path: &Path) -> StoreInspection {
    let inspection = |health, records| StoreInspection {
        store: DerivedStore::Vectors,
        location: path.to_path_buf(),
        health,
        records,
    };
    let db_path = path.display().to_string();
    let failure = |error: SearchError| match error {
        SearchError::Store(failure) => failure,
        other => StoreFailure::io(DerivedStore::Vectors, db_path.clone(), other.to_string()),
    };
    match std::fs::metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return inspection(None, 0),
        Err(error) => {
            return inspection(
                Some(StoreHealth::Unreadable(StoreFailure::io(
                    DerivedStore::Vectors,
                    db_path,
                    error.to_string(),
                ))),
                0,
            );
        }
        Ok(_) => {}
    }
    register_sqlite_vec();
    let checked = crate::store::open_sqlite_for_reading(path).and_then(|connection| {
        let damage = crate::store::sqlite_damage(&connection)?;
        Ok((connection, damage))
    });
    let connection = match checked {
        Ok((_, Some(damage))) => {
            return inspection(
                Some(StoreHealth::Unreadable(StoreFailure::corrupt(
                    DerivedStore::Vectors,
                    db_path,
                    damage,
                ))),
                0,
            );
        }
        Ok((connection, None)) => connection,
        Err(error) => {
            return inspection(
                Some(StoreHealth::Unreadable(failure(storage_error(&db_path, error)))),
                0,
            );
        }
    };
    match connection.query_row("SELECT COUNT(*) FROM chunks", [], |row| row.get::<_, i64>(0)) {
        Ok(chunks) => inspection(Some(StoreHealth::Ready), usize::try_from(chunks).unwrap_or(0)),
        Err(error) => inspection(
            Some(StoreHealth::Unreadable(failure(storage_error(&db_path, error)))),
            0,
        ),
    }
}
