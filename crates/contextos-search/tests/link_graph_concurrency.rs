//! The `sqlite` link graph store shared by every `contextos` instance on a vault: a write that meets another
//! instance's write waits for it only up to a bound and then fails as `index/busy` having changed nothing, and a
//! rebuild never puts back an entry older than one another instance wrote while it ran.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use contextos_search::{GraphBackend, LinkGraph, LinkGraphConfig};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn store_directory(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("graph")
}

fn sqlite_graph(dir: &tempfile::TempDir, write_wait: Duration) -> Result<LinkGraph, Box<dyn std::error::Error>> {
    Ok(LinkGraph::try_from(LinkGraphConfig {
        store_directory: store_directory(dir),
        backend: GraphBackend::Sqlite,
        write_wait,
    })?)
}

#[test]
fn a_graph_write_blocked_past_its_wait_fails_as_busy_and_changes_nothing() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut graph = sqlite_graph(&dir, Duration::from_millis(100))?;
    graph.rebuild(&[("a.md".to_owned(), "A".to_owned(), Vec::new())])?;

    // Another instance in the middle of a long write holds the store's write lock.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(store_directory(&dir).join("graph.sqlite3.lock"))?;
    lock.lock()?;

    let (sender, receiver) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        let outcome = graph.upsert_note("b.md", "B", &[]).map_err(|error| error.code());
        let _ = sender.send(outcome);
        graph
    });
    let outcome = receiver
        .recv_timeout(Duration::from_secs(3))
        .map_err(|_| "a blocked graph write waited without bound")?;
    lock.unlock()?;
    let graph = writer.join().map_err(|_| "the writer thread panicked")?;

    assert_eq!(outcome, Err("index/busy"));
    let mut reopened = sqlite_graph(&dir, Duration::from_secs(5))?;
    assert!(
        reopened.backlinks("b.md").is_err(),
        "the blocked write must change nothing"
    );
    drop(graph);
    Ok(())
}
