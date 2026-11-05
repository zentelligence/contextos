use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tracing_subscriber::fmt::MakeWriter;

use super::*;
use crate::graph::GraphNodeKind;

/// Captures tracing output to a plain byte buffer; mirrors
/// `contextos-web/tests/logging.rs`'s own established pattern for asserting
/// on log content rather than inventing a second one.
#[derive(Clone, Default)]
struct CapturingWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for CapturingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for CapturingWriter {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl CapturingWriter {
    fn contents(&self) -> Result<String, std::string::FromUtf8Error> {
        String::from_utf8(self.0.lock().unwrap_or_else(PoisonError::into_inner).clone())
    }
}

/// Proves the mutual-exclusion property the corruption fix depends on:
/// a second writer's critical section must not start until the first's has
/// fully finished, even though each runs on its own thread with its own
/// store handle, the same shape two independent OS processes would produce.
/// Confirmed live on real Windows hardware that `contextos-web`'s own
/// embedded `contextos` process and a separate `contextos` process writing
/// the same vault's `sqlite`-backed link graph concurrently produced genuine
/// on-disk damage (`PRAGMA integrity_check` reported B-tree damage), not
/// merely a transient busy error; this guards the mechanism added to prevent
/// that.
#[test]
fn write_guards_serialise_writers_across_independent_handles() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let directory = dir.path().to_path_buf();
    let first_store = SqliteGraphStore::open(&directory, crate::DEFAULT_WRITE_WAIT)?;
    let second_store = SqliteGraphStore::open(&directory, crate::DEFAULT_WRITE_WAIT)?;

    let events: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    // The second writer asks for the lock only once the first holds it, so the order is decided by the lock alone,
    // never by which thread the scheduler happens to run first.
    let (held, first_holds) = std::sync::mpsc::channel::<()>();

    let first_events = Arc::clone(&events);
    let first = std::thread::spawn(move || -> Result<(), SearchError> {
        let _guard = first_store.write_guard()?;
        first_events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push("first-start");
        let _ = held.send(());
        // Holds the lock long enough that a second writer the lock failed to stop would record its start in between.
        std::thread::sleep(Duration::from_millis(120));
        first_events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push("first-end");
        Ok(())
    });

    let second_events = Arc::clone(&events);
    let second = std::thread::spawn(move || -> Result<(), SearchError> {
        if first_holds.recv().is_err() {
            // The first writer ended without taking the lock; its own error is what the join below reports.
            return Ok(());
        }
        let _guard = second_store.write_guard()?;
        second_events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push("second-start");
        Ok(())
    });

    first.join().map_err(|_| "first thread panicked")??;
    second.join().map_err(|_| "second thread panicked")??;

    let recorded = events.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(
        recorded,
        vec!["first-start", "first-end", "second-start"],
        "the second writer's critical section must not begin until the first's has fully ended"
    );
    Ok(())
}

/// A failed write still releases the lock: a failed persist must not wedge
/// every later writer against the same store.
#[test]
fn a_write_guard_dropped_after_a_failed_write_lets_the_next_writer_in() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = SqliteGraphStore::open(dir.path(), Duration::from_millis(100))?;
    {
        let guard = store.write_guard()?;
        let oversized = GraphChange::RemoveEdge { store_id: u64::MAX };
        assert!(
            store
                .persist(&guard, &[oversized], 0, StoreCompleteness::Unchanged)
                .is_err()
        );
    }
    let _next = store.write_guard()?;
    Ok(())
}

/// A read against a genuinely corrupted store still returns `None`, per
/// `StoresGraph`'s documented all-or-nothing contract, but the real
/// `rusqlite` error is now logged at `warn` with the stable `index/storage`
/// code, instead of vanishing entirely the way it used to: previously the
/// only trace of a failure like this was `sync.rs`'s own generic,
/// synthesised message ("became unreadable ... having been readable
/// moments earlier"), which named no real cause. Corrupts the store by
/// overwriting a page deep inside the file with garbage, in place (the file
/// length, and therefore the header's own recorded page count, is
/// unchanged), so `SqliteGraphStore::open`'s bootstrap (which only touches
/// the header and schema, always page 1, first in the file) still succeeds
/// and only a later `SELECT` against the now-garbage page fails, the same
/// "open succeeds, read fails later" shape a real corrupted store has.
#[test]
fn a_read_failure_against_a_corrupted_store_is_logged_with_the_real_cause() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let directory = dir.path().to_path_buf();

    {
        let store = SqliteGraphStore::open(&directory, crate::DEFAULT_WRITE_WAIT)?;
        let changes: Vec<GraphChange> = (0..500)
            .map(|i| {
                GraphChange::UpsertNode(GraphNode {
                    path: format!("note-{i}-with-a-reasonably-long-path-to-grow-the-store-file.md"),
                    title: format!("Note {i} also has a reasonably long title, to grow the store file further still"),
                    kind: GraphNodeKind::Note,
                    candidates: Vec::new(),
                    unreadable: None,
                    base: None,
                    stamp: None,
                })
            })
            .collect();
        store.persist(&store.write_guard()?, &changes, 0, StoreCompleteness::Complete)?;
        // `store` drops here: its `Connection` closes, checkpointing WAL
        // content into the main file so corrupting that one file below is
        // enough (nothing needed is left only in `-wal`).
    }

    let file_path = directory.join(FILE_NAME);
    let original_len = std::fs::metadata(&file_path)?.len();
    assert!(
        original_len > 32768,
        "test fixture did not grow the store past a few pages; increase the row count"
    );
    {
        use std::io::{Seek, SeekFrom, Write};
        let mut file = std::fs::OpenOptions::new().write(true).open(&file_path)?;
        // Well past page 1 (header and schema), so `open()`'s own bootstrap
        // never touches this region; well before the end, so it lands
        // inside an actual data page rather than trailing free space.
        file.seek(SeekFrom::Start(original_len / 2))?;
        file.write_all(&[0xFFu8; 4096])?;
    }

    let writer = CapturingWriter::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(writer.clone())
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let store = SqliteGraphStore::open(&directory, crate::DEFAULT_WRITE_WAIT)?;
    let failure = store
        .load_all()
        .err()
        .ok_or("reading node/edge rows through the corrupted page must not be trusted")?;
    assert_eq!(failure.code(), "index/corrupt", "{failure}");

    let log = writer.contents()?;
    assert!(
        log.contains("sqlite graph store read failed"),
        "log missing the read-failure message: {log}"
    );
    assert!(
        log.contains("code=\"index/corrupt\""),
        "log missing the stable error code: {log}"
    );
    Ok(())
}

/// The failures no file fixture can produce on demand, classified from the result code `SQLite` itself reports.
#[test]
fn a_read_failure_is_classified_by_its_sqlite_result_code() {
    let directory = Path::new("graph");
    let classify = |code: i32| {
        let error = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None);
        StoreFailure::from(SqliteRead {
            directory,
            what: "reading",
            error: &error,
        })
        .code()
    };
    assert_eq!(classify(rusqlite::ffi::SQLITE_BUSY), "index/busy");
    assert_eq!(classify(rusqlite::ffi::SQLITE_LOCKED), "index/busy");
    assert_eq!(classify(rusqlite::ffi::SQLITE_CORRUPT), "index/corrupt");
    assert_eq!(classify(rusqlite::ffi::SQLITE_NOTADB), "index/corrupt");
    assert_eq!(classify(rusqlite::ffi::SQLITE_IOERR), "index/storage");
    assert_eq!(classify(rusqlite::ffi::SQLITE_CANTOPEN), "index/storage");

    let wrong_type = rusqlite::Error::InvalidColumnType(0, "value".to_owned(), rusqlite::types::Type::Text);
    let failure = StoreFailure::from(SqliteRead {
        directory,
        what: "reading",
        error: &wrong_type,
    });
    assert_eq!(failure.code(), "index/undecodable");
}
