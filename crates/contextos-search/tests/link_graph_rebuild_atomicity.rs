//! A graph rebuild replaces the stored graph in one atomic write: no reader,
//! and no failure part way through, ever observes an emptied or half-filled
//! store presented as healthy.
//!
//! Failures are injected deterministically, through the store's own
//! mechanisms, rather than by racing threads against a clock.

use std::path::{Path, PathBuf};

use contextos_obsidian::{LinkCollection, ObsidianLink};
use contextos_search::{GraphBackend, LinkGraph, LinkGraphConfig};

type Notes = Vec<(String, String, Vec<ObsidianLink>)>;

fn open(store_directory: &Path, backend: GraphBackend) -> Result<LinkGraph, Box<dyn std::error::Error>> {
    Ok(LinkGraph::try_from(LinkGraphConfig {
        store_directory: store_directory.to_path_buf(),
        backend,
        write_wait: contextos_search::DEFAULT_WRITE_WAIT,
    })?)
}

fn note(path: &str, markdown: &str) -> Result<(String, String, Vec<ObsidianLink>), Box<dyn std::error::Error>> {
    let links = LinkCollection::try_from(markdown)?.outgoing().to_vec();
    Ok((path.to_owned(), path.to_owned(), links))
}

/// Three linked notes: the graph a rebuild is later asked to replace.
fn original_notes() -> Result<Notes, Box<dyn std::error::Error>> {
    Ok(vec![note("a.md", "[[b]]")?, note("b.md", "[[c]]")?, note("c.md", "")?])
}

/// A single, unrelated note: what the replacement graph holds.
fn replacement_notes() -> Result<Notes, Box<dyn std::error::Error>> {
    Ok(vec![note("d.md", "")?])
}

fn node_paths(graph: &mut LinkGraph) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut paths: Vec<String> = graph.full_view()?.nodes.into_iter().map(|node| node.path).collect();
    paths.sort();
    Ok(paths)
}

fn original_paths() -> Vec<String> {
    vec!["a.md".to_owned(), "b.md".to_owned(), "c.md".to_owned()]
}

/// A store whose graph was built from [`original_notes`], and an instance
/// over it that then attempted, and failed, to rebuild from
/// [`replacement_notes`].
struct AfterFailedRebuild {
    /// Keeps the temporary directory alive for the test's duration.
    _dir: tempfile::TempDir,
    store: PathBuf,
    backend: GraphBackend,
    graph: LinkGraph,
}

/// The backends whose persisting step a test can make fail from outside the
/// crate. The `fjall` backend has no such external handle: its atomicity is
/// proven at the store level, in `fjall_store_test.rs`.
#[derive(Clone, Copy, Debug)]
enum Injectable {
    Sqlite,
    Serde,
}

impl From<Injectable> for GraphBackend {
    fn from(value: Injectable) -> Self {
        match value {
            Injectable::Sqlite => Self::Sqlite,
            Injectable::Serde => Self::Serde,
        }
    }
}

impl AfterFailedRebuild {
    /// Injects a failure into the persisting step of the chosen backend's
    /// store, attempts a rebuild that must therefore fail, then removes the
    /// injection so later assertions read the store normally.
    ///
    /// The injections deliberately name the stores' own file, table, and
    /// temporary-file names; renaming any of them makes this fixture fail
    /// loudly rather than silently stop injecting.
    fn new(injectable: Injectable) -> Result<Self, Box<dyn std::error::Error>> {
        let backend = GraphBackend::from(injectable);
        let dir = tempfile::tempdir()?;
        let store = dir.path().join("graph");
        let mut graph = open(&store, backend)?;
        graph.rebuild(&original_notes()?)?;

        match injectable {
            Injectable::Sqlite => {
                // Aborts every node insert, so the repopulation fails after
                // whatever the rebuild did first has already been applied.
                let injector = rusqlite::Connection::open(store.join("graph.sqlite3"))?;
                injector.execute_batch(
                    "CREATE TRIGGER reject_node_inserts BEFORE INSERT ON nodes \
                     BEGIN SELECT RAISE(ABORT, 'injected node insert failure'); END;",
                )?;
                assert!(
                    graph.rebuild(&replacement_notes()?).is_err(),
                    "the injected failure surfaces"
                );
                injector.execute_batch("DROP TRIGGER reject_node_inserts;")?;
            }
            Injectable::Serde => {
                // The document is written through a sibling temporary file; a
                // directory occupying that name makes the write fail.
                std::fs::create_dir(store.join("graph.json.tmp"))?;
                assert!(
                    graph.rebuild(&replacement_notes()?).is_err(),
                    "the injected failure surfaces"
                );
                std::fs::remove_dir(store.join("graph.json.tmp"))?;
            }
        }

        Ok(Self {
            _dir: dir,
            store,
            backend,
            graph,
        })
    }

    /// A freshly opened second instance over the same store.
    fn reopened(&self) -> Result<LinkGraph, Box<dyn std::error::Error>> {
        open(&self.store, self.backend)
    }
}

#[test]
fn a_failed_sqlite_rebuild_leaves_the_stored_graph_intact() -> Result<(), Box<dyn std::error::Error>> {
    let failed = AfterFailedRebuild::new(Injectable::Sqlite)?;

    let mut reopened = failed.reopened()?;
    assert!(
        !reopened.needs_rebuild(),
        "the store still reports itself trustworthy after the failed rebuild"
    );
    assert_eq!(
        node_paths(&mut reopened)?,
        original_paths(),
        "a fresh open sees the previous graph, never an emptied one"
    );
    Ok(())
}

#[test]
fn a_failed_sqlite_rebuild_leaves_the_rebuilding_instance_intact() -> Result<(), Box<dyn std::error::Error>> {
    let mut failed = AfterFailedRebuild::new(Injectable::Sqlite)?;

    assert_eq!(node_paths(&mut failed.graph)?, original_paths());
    Ok(())
}

#[test]
fn a_failed_serde_rebuild_leaves_the_stored_graph_intact() -> Result<(), Box<dyn std::error::Error>> {
    // A guard rather than a discriminator: the serde document on disk was
    // already untouched by a failed rebuild. What this backend gained is the
    // in-memory guarantee, asserted by the neighbouring instance test.
    let failed = AfterFailedRebuild::new(Injectable::Serde)?;

    let mut reopened = failed.reopened()?;
    assert!(
        !reopened.needs_rebuild(),
        "the store still reports itself trustworthy after the failed rebuild"
    );
    assert_eq!(node_paths(&mut reopened)?, original_paths());
    Ok(())
}

#[test]
fn a_failed_serde_rebuild_leaves_the_rebuilding_instance_intact() -> Result<(), Box<dyn std::error::Error>> {
    let mut failed = AfterFailedRebuild::new(Injectable::Serde)?;

    assert_eq!(node_paths(&mut failed.graph)?, original_paths());
    Ok(())
}

#[test]
fn a_failed_rebuild_does_not_corrupt_later_incremental_writes() -> Result<(), Box<dyn std::error::Error>> {
    for backend in [Injectable::Serde, Injectable::Sqlite] {
        let mut failed = AfterFailedRebuild::new(backend)?;

        // A later single-note write must extend the surviving graph, not an
        // emptied or half-replaced one a failed rebuild would have left.
        failed.graph.upsert_note("e.md", "e.md", &[])?;

        let mut expected = original_paths();
        expected.push("e.md".to_owned());
        assert_eq!(node_paths(&mut failed.graph)?, expected, "{backend:?} instance");
        let mut reopened = failed.reopened()?;
        assert_eq!(node_paths(&mut reopened)?, expected, "{backend:?} reopened store");
    }
    Ok(())
}

#[test]
fn a_rebuild_replaces_earlier_content_completely_for_every_backend() -> Result<(), Box<dyn std::error::Error>> {
    for backend in [GraphBackend::Serde, GraphBackend::Fjall, GraphBackend::Sqlite] {
        let dir = tempfile::tempdir()?;
        let store: PathBuf = dir.path().join("graph");
        let mut graph = open(&store, backend)?;
        graph.rebuild(&original_notes()?)?;
        graph.rebuild(&replacement_notes()?)?;

        assert_eq!(node_paths(&mut graph)?, vec!["d.md".to_owned()], "{backend:?} instance");
        drop(graph);

        let mut reopened = open(&store, backend)?;
        assert!(!reopened.needs_rebuild(), "{backend:?} reopened store is trustworthy");
        assert_eq!(
            node_paths(&mut reopened)?,
            vec!["d.md".to_owned()],
            "{backend:?} reopened store holds only the replacement graph"
        );
        assert!(
            reopened.full_view()?.edges.is_empty(),
            "{backend:?} kept no edge from the replaced graph"
        );
    }
    Ok(())
}

#[test]
fn a_rebuild_over_overlapping_content_keeps_exactly_the_new_graph_for_every_backend()
-> Result<(), Box<dyn std::error::Error>> {
    for backend in [GraphBackend::Serde, GraphBackend::Fjall, GraphBackend::Sqlite] {
        let dir = tempfile::tempdir()?;
        let store: PathBuf = dir.path().join("graph");
        let mut graph = open(&store, backend)?;
        graph.rebuild(&original_notes()?)?;

        // `b.md` and `c.md` survive, `a.md` and its link vanish, `e.md` is new.
        let overlapping = vec![note("b.md", "[[c]] [[e]]")?, note("c.md", "")?, note("e.md", "")?];
        graph.rebuild(&overlapping)?;
        drop(graph);

        let mut reopened = open(&store, backend)?;
        assert_eq!(
            node_paths(&mut reopened)?,
            vec!["b.md".to_owned(), "c.md".to_owned(), "e.md".to_owned()],
            "{backend:?} nodes"
        );
        let mut edges: Vec<(String, String)> = reopened
            .full_view()?
            .edges
            .into_iter()
            .map(|edge| (edge.from, edge.to))
            .collect();
        edges.sort();
        assert_eq!(
            edges,
            vec![
                ("b.md".to_owned(), "c.md".to_owned()),
                ("b.md".to_owned(), "e.md".to_owned())
            ],
            "{backend:?} edges"
        );
    }
    Ok(())
}

#[test]
fn rebuilding_an_empty_vault_produces_a_trustworthy_empty_graph_for_every_backend()
-> Result<(), Box<dyn std::error::Error>> {
    for backend in [GraphBackend::Serde, GraphBackend::Fjall, GraphBackend::Sqlite] {
        let dir = tempfile::tempdir()?;
        let store: PathBuf = dir.path().join("graph");
        let mut graph = open(&store, backend)?;
        graph.rebuild(&original_notes()?)?;
        graph.rebuild(&Vec::new())?;
        drop(graph);

        let mut reopened = open(&store, backend)?;
        assert!(
            !reopened.needs_rebuild(),
            "{backend:?} empty rebuild is still trustworthy"
        );
        assert!(node_paths(&mut reopened)?.is_empty(), "{backend:?} holds no nodes");
    }
    Ok(())
}
