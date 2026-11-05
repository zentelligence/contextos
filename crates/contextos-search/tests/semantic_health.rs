//! Reading a vault's semantic index for diagnostics without writing: the vector store is never created, migrated, or
//! modified by being inspected, and an absent store reads as absent rather than being made.

use std::error::Error;

use contextos_core::ContentHash;
use contextos_search::{
    FakeEmbedder, SemanticConfig, SemanticHealth, SqliteVecConfig, SqliteVecStore, StoresVectors, VectorRecord,
    VectorStoreState, VectorStoreStats,
};
use tempfile::tempdir;

#[test]
fn a_store_that_does_not_exist_reads_as_absent_without_being_created() -> Result<(), Box<dyn Error>> {
    let directory = tempdir()?;
    let state = directory.path().join("state");
    let path = state.join("vectors.db");

    let read = VectorStoreState::try_from(path.as_path())?;

    assert_eq!(read, VectorStoreState::Absent);
    assert!(!state.exists(), "reading the store created its directory");
    Ok(())
}

#[test]
fn an_existing_store_reads_its_counts_without_being_modified() -> Result<(), Box<dyn Error>> {
    let directory = tempdir()?;
    let path = directory.path().join("vectors.db");
    {
        let store = SqliteVecStore::try_from(SqliteVecConfig {
            path: path.clone(),
            dimension: 4,
            write_wait: contextos_search::DEFAULT_WRITE_WAIT,
        })?;
        store.upsert(&[VectorRecord {
            path: "a/one.md",
            ordinal: 0,
            heading_context: &["Intro".to_owned()],
            content_hash: &ContentHash::from([1; 32]),
            vector: &[1.0, 0.0, 0.0, 0.0],
        }])?;
    }
    let before = std::fs::read(&path)?;
    let entries_before = std::fs::read_dir(directory.path())?.count();

    let read = VectorStoreState::try_from(path.as_path())?;

    assert_eq!(
        read,
        VectorStoreState::Present(VectorStoreStats {
            documents: 1,
            chunks: 1
        })
    );
    assert_eq!(std::fs::read(&path)?, before, "reading the store changed it");
    assert_eq!(
        std::fs::read_dir(directory.path())?.count(),
        entries_before,
        "reading the store left a journal or other file beside it"
    );
    Ok(())
}

#[test]
fn semantic_health_reports_the_providers_dimension_and_an_absent_store_without_creating_it()
-> Result<(), Box<dyn Error>> {
    let directory = tempdir()?;
    let state = directory.path().join("state");

    let health = SemanticHealth::try_from(SemanticConfig {
        embedder: Box::new(FakeEmbedder::new(8)),
        vector_store_path: state.join("vectors.db"),
    })?;

    assert_eq!(health.dimension, 8);
    assert_eq!(health.store, VectorStoreState::Absent);
    assert!(!state.exists(), "checking semantic health created the state directory");
    Ok(())
}
