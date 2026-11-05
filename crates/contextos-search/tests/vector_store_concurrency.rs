//! The vector store shared by every `contextos` instance on a vault: writers from different instances are serialised
//! with a bounded wait rather than failing on contention, a write still blocked when the wait runs out fails as
//! `index/busy` having changed nothing, and every instance reads what the others have committed.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::Duration;

use contextos_core::ContentHash;
use contextos_search::{DEFAULT_WRITE_WAIT, SqliteVecConfig, SqliteVecStore, StoresVectors, VectorRecord};

type TestResult = Result<(), Box<dyn Error>>;

fn open(path: &Path, write_wait: Duration) -> Result<SqliteVecStore, Box<dyn Error>> {
    Ok(SqliteVecStore::try_from(SqliteVecConfig {
        path: path.to_path_buf(),
        dimension: 4,
        write_wait,
    })?)
}

fn store_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("vectors.db")
}

fn record_for<'a>(path: &'a str, ordinal: usize, hash: &'a ContentHash) -> VectorRecord<'a> {
    VectorRecord {
        path,
        ordinal,
        heading_context: &[],
        content_hash: hash,
        vector: &[1.0, 0.0, 0.0, 0.0],
    }
}

#[test]
fn the_store_uses_a_write_ahead_log_so_readers_never_wait_for_another_instances_writer() -> TestResult {
    let dir = tempfile::tempdir()?;
    let _store = open(&store_path(&dir), DEFAULT_WRITE_WAIT)?;

    let probe = rusqlite::Connection::open(store_path(&dir))?;
    let mode: String = probe.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;

    assert_eq!(mode.to_ascii_lowercase(), "wal");
    Ok(())
}

#[test]
fn two_instances_write_concurrently_without_failing_and_each_sees_the_others_rows() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = store_path(&dir);
    let first = open(&path, DEFAULT_WRITE_WAIT)?;
    let second = open(&path, DEFAULT_WRITE_WAIT)?;
    let hash = ContentHash::from([7_u8; 32]);

    std::thread::scope(|scope| -> TestResult {
        let writers = [("first", &first), ("second", &second)].map(|(name, store)| {
            let hash = &hash;
            scope.spawn(move || -> Result<(), String> {
                for index in 0..40 {
                    let note = format!("{name}/note-{index}.md");
                    store
                        .upsert(&[record_for(&note, 0, hash)])
                        .map_err(|error| format!("{name} write {index} failed: {error}"))?;
                }
                Ok(())
            })
        });
        for writer in writers {
            writer.join().map_err(|_| "a writer thread panicked")??;
        }
        Ok(())
    })?;

    assert_eq!(first.stats()?.documents, 80, "the first instance must see every row");
    assert_eq!(second.stats()?.documents, 80, "the second instance must see every row");
    assert_eq!(first.existing_hash("second/note-39.md", 0)?, Some(hash.clone()));
    assert_eq!(second.existing_hash("first/note-0.md", 0)?, Some(hash));
    Ok(())
}

#[test]
fn a_write_blocked_past_its_wait_fails_as_busy_and_changes_nothing() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = store_path(&dir);
    let store = open(&path, Duration::from_millis(100))?;
    let hash = ContentHash::from([1_u8; 32]);
    store.upsert(&[record_for("kept.md", 0, &hash)])?;

    // Another instance in the middle of a long write holds the store's write lock.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.path().join("vectors.db.lock"))?;
    lock.lock()?;

    let upsert = store.upsert(&[record_for("blocked.md", 0, &hash)]);
    let delete = store.delete("kept.md");

    for (what, outcome) in [("upsert", upsert), ("delete", delete)] {
        let error = outcome.err().ok_or(format!("a blocked {what} must fail"))?;
        assert_eq!(error.code(), "index/busy", "{what}: {error}");
    }
    lock.unlock()?;
    assert_eq!(
        store.existing_hash("blocked.md", 0)?,
        None,
        "the blocked upsert must change nothing"
    );
    assert_eq!(
        store.existing_hash("kept.md", 0)?,
        Some(hash),
        "the blocked delete must change nothing"
    );
    Ok(())
}
