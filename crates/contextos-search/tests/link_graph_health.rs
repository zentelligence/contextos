//! What a link graph reports when its store cannot be trusted: an empty store is empty, a store written in another
//! format is incompatible, and a store whose content cannot be read opens flagged unreadable with the specific
//! failure (busy, corrupt, undecodable, or an I/O failure), refusing queries and edits with that failure's code until
//! a rebuild or another instance repairs it, instead of answering as though the vault had no links.

use std::path::{Path, PathBuf};

use contextos_search::{GraphBackend, LinkGraph, LinkGraphConfig, SearchError, StoreHealth};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn store_directory(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join(".contextos").join("graph")
}

fn graph_at(dir: &tempfile::TempDir, backend: GraphBackend) -> Result<LinkGraph, Box<dyn std::error::Error>> {
    Ok(LinkGraph::try_from(LinkGraphConfig {
        store_directory: store_directory(dir),
        backend,
        write_wait: contextos_search::DEFAULT_WRITE_WAIT,
    })?)
}

fn one_note() -> Vec<(String, String, Vec<contextos_obsidian::ObsidianLink>)> {
    vec![("a.md".to_owned(), "A".to_owned(), Vec::new())]
}

/// Asserts `graph` opened flagged unreadable with `code`, and refuses both a query and an edit with that code.
fn assert_unreadable(graph: &mut LinkGraph, code: &str) -> TestResult {
    let rebuild_helps = match graph.health() {
        StoreHealth::Unreadable(failure) => {
            assert_eq!(failure.code(), code, "{failure}");
            failure.kind().rebuild_helps()
        }
        other => return Err(format!("expected an unreadable graph ({code}), found {other:?}").into()),
    };
    assert_eq!(graph.needs_rebuild(), rebuild_helps, "{code}");
    let query = graph
        .orphans()
        .err()
        .ok_or("a query against an unreadable store must be refused")?;
    assert_eq!(query.code(), code, "{query}");
    let edit = graph
        .upsert_note("b.md", "B", &[])
        .err()
        .ok_or("an edit must not be written onto an unreadable store")?;
    assert_eq!(edit.code(), code, "{edit}");
    Ok(())
}

fn sqlite_connection(dir: &tempfile::TempDir) -> rusqlite::Result<rusqlite::Connection> {
    rusqlite::Connection::open(store_directory(dir).join("graph.sqlite3"))
}

/// A `sqlite` store with enough rows that a damaged page mid-file lands inside node data, not the schema.
fn populated_sqlite_store(dir: &tempfile::TempDir) -> TestResult {
    let mut graph = graph_at(dir, GraphBackend::Sqlite)?;
    let notes = (0..500)
        .map(|i| {
            (
                format!("note-{i}-with-a-reasonably-long-path-to-grow-the-store-file.md"),
                format!("Note {i} also has a reasonably long title, to grow the store file further still"),
                Vec::new(),
            )
        })
        .collect::<Vec<_>>();
    graph.rebuild(&notes)?;
    Ok(())
}

fn damage_middle_of(file: &Path) -> TestResult {
    use std::io::{Seek, SeekFrom, Write};
    let length = std::fs::metadata(file)?.len();
    assert!(length > 32_768, "the fixture must span several pages");
    let mut handle = std::fs::OpenOptions::new().write(true).open(file)?;
    handle.seek(SeekFrom::Start(length / 4))?;
    handle.write_all(&vec![0xFF_u8; usize::try_from(length / 2)?])?;
    Ok(())
}

#[test]
fn a_store_never_written_reports_empty_and_answers_queries() -> TestResult {
    for backend in [GraphBackend::Sqlite, GraphBackend::Fjall, GraphBackend::Serde] {
        let dir = tempfile::tempdir()?;
        let mut graph = graph_at(&dir, backend)?;
        assert_eq!(graph.health(), &StoreHealth::Empty, "{backend:?}");
        assert!(graph.needs_rebuild());
        assert!(
            graph.orphans()?.nodes.is_empty(),
            "an empty store answers with no nodes ({backend:?})"
        );
    }
    Ok(())
}

#[test]
fn a_store_written_and_reopened_reports_ready() -> TestResult {
    for backend in [GraphBackend::Sqlite, GraphBackend::Fjall, GraphBackend::Serde] {
        let dir = tempfile::tempdir()?;
        graph_at(&dir, backend)?.rebuild(&one_note())?;
        let graph = graph_at(&dir, backend)?;
        assert_eq!(graph.health(), &StoreHealth::Ready, "{backend:?}");
        assert!(!graph.needs_rebuild());
    }
    Ok(())
}

#[test]
fn a_sqlite_store_with_a_damaged_page_opens_unreadable_as_corrupt() -> TestResult {
    let dir = tempfile::tempdir()?;
    populated_sqlite_store(&dir)?;
    damage_middle_of(&store_directory(&dir).join("graph.sqlite3"))?;
    let mut graph = graph_at(&dir, GraphBackend::Sqlite)?;
    assert_unreadable(&mut graph, "index/corrupt")
}

#[test]
fn a_sqlite_store_holding_a_row_that_does_not_decode_opens_unreadable_and_a_rebuild_recovers_it() -> TestResult {
    let dir = tempfile::tempdir()?;
    graph_at(&dir, GraphBackend::Sqlite)?.rebuild(&one_note())?;
    sqlite_connection(&dir)?.execute("INSERT INTO nodes (path, value) VALUES ('bad.md', x'00ff')", [])?;

    let mut graph = graph_at(&dir, GraphBackend::Sqlite)?;
    assert_unreadable(&mut graph, "index/undecodable")?;

    graph.rebuild(&one_note())?;
    assert_eq!(graph.health(), &StoreHealth::Ready);
    assert_eq!(graph.orphans()?.nodes.len(), 1);
    Ok(())
}

#[test]
fn a_sqlite_store_written_in_another_format_reports_incompatible() -> TestResult {
    let dir = tempfile::tempdir()?;
    graph_at(&dir, GraphBackend::Sqlite)?.rebuild(&one_note())?;
    sqlite_connection(&dir)?.execute("UPDATE metadata SET value = 999 WHERE key = 'format_version'", [])?;

    let mut graph = graph_at(&dir, GraphBackend::Sqlite)?;
    assert_eq!(graph.health(), &StoreHealth::Incompatible);
    assert!(graph.needs_rebuild());
    assert!(
        graph.orphans()?.nodes.is_empty(),
        "an incompatible store answers with no nodes until rebuilt"
    );
    Ok(())
}

/// The format before edges recorded their link form and target: a graph in it has Markdown links missing and its
/// links resolved by an older rule, so it must be rebuilt once rather than read.
#[test]
fn a_sqlite_store_written_in_the_previous_format_is_rebuilt_not_read() -> TestResult {
    let dir = tempfile::tempdir()?;
    graph_at(&dir, GraphBackend::Sqlite)?.rebuild(&one_note())?;
    sqlite_connection(&dir)?.execute("UPDATE metadata SET value = 1 WHERE key = 'format_version'", [])?;

    let graph = graph_at(&dir, GraphBackend::Sqlite)?;

    assert_eq!(graph.health(), &StoreHealth::Incompatible);
    assert!(graph.needs_rebuild());
    Ok(())
}

#[test]
fn a_serde_store_written_in_the_previous_format_is_rebuilt_not_read() -> TestResult {
    let dir = tempfile::tempdir()?;
    std::fs::create_dir_all(store_directory(&dir))?;
    std::fs::write(
        store_directory(&dir).join("graph.json"),
        br#"{"format":1,"next_edge_id":1,"nodes":[{"path":"a.md","title":"a","phantom":false}],"edges":[]}"#,
    )?;

    let graph = graph_at(&dir, GraphBackend::Serde)?;

    assert_eq!(graph.health(), &StoreHealth::Incompatible);
    assert!(graph.needs_rebuild());
    Ok(())
}

#[test]
fn a_fjall_store_written_in_the_previous_format_is_rebuilt_not_read() -> TestResult {
    let dir = tempfile::tempdir()?;
    graph_at(&dir, GraphBackend::Fjall)?.rebuild(&one_note())?;
    {
        let database = fjall::Database::builder(store_directory(&dir)).open()?;
        let metadata = database.keyspace("metadata", fjall::KeyspaceCreateOptions::default)?;
        let mut batch = database.batch().durability(Some(fjall::PersistMode::SyncAll));
        batch.insert(&metadata, b"format_version", 1_u32.to_be_bytes());
        batch.commit()?;
    }

    let graph = graph_at(&dir, GraphBackend::Fjall)?;

    assert_eq!(graph.health(), &StoreHealth::Incompatible);
    assert!(graph.needs_rebuild());
    Ok(())
}

#[test]
fn a_sqlite_store_whose_metadata_does_not_decode_opens_unreadable() -> TestResult {
    let dir = tempfile::tempdir()?;
    graph_at(&dir, GraphBackend::Sqlite)?.rebuild(&one_note())?;
    sqlite_connection(&dir)?.execute("UPDATE metadata SET value = -1 WHERE key = 'next_edge_id'", [])?;

    let mut graph = graph_at(&dir, GraphBackend::Sqlite)?;
    assert_unreadable(&mut graph, "index/undecodable")
}

#[test]
fn a_fjall_store_holding_a_record_that_does_not_decode_opens_unreadable() -> TestResult {
    let dir = tempfile::tempdir()?;
    graph_at(&dir, GraphBackend::Fjall)?.rebuild(&one_note())?;
    {
        let database = fjall::Database::builder(store_directory(&dir)).open()?;
        let nodes = database.keyspace("nodes", fjall::KeyspaceCreateOptions::default)?;
        let mut batch = database.batch().durability(Some(fjall::PersistMode::SyncAll));
        batch.insert(&nodes, b"bad.md", b"\x00\xff");
        batch.commit()?;
    }

    let mut graph = graph_at(&dir, GraphBackend::Fjall)?;
    assert_unreadable(&mut graph, "index/undecodable")
}

#[test]
fn a_serde_store_that_is_not_json_opens_unreadable() -> TestResult {
    let dir = tempfile::tempdir()?;
    std::fs::create_dir_all(store_directory(&dir))?;
    std::fs::write(store_directory(&dir).join("graph.json"), b"{ not json")?;

    let mut graph = graph_at(&dir, GraphBackend::Serde)?;
    assert_unreadable(&mut graph, "index/undecodable")
}

#[test]
fn a_serde_store_written_in_another_format_reports_incompatible() -> TestResult {
    let dir = tempfile::tempdir()?;
    std::fs::create_dir_all(store_directory(&dir))?;
    std::fs::write(
        store_directory(&dir).join("graph.json"),
        br#"{"format":999,"next_edge_id":0,"nodes":[],"edges":[]}"#,
    )?;

    let graph = graph_at(&dir, GraphBackend::Serde)?;
    assert_eq!(graph.health(), &StoreHealth::Incompatible);
    Ok(())
}

#[test]
fn a_serde_store_that_cannot_be_read_from_disk_opens_unreadable_as_an_io_failure() -> TestResult {
    let dir = tempfile::tempdir()?;
    // A directory where the document should be: reading it fails as I/O on every platform.
    std::fs::create_dir_all(store_directory(&dir).join("graph.json"))?;

    let mut graph = graph_at(&dir, GraphBackend::Serde)?;
    assert_unreadable(&mut graph, "index/storage")
}

#[test]
fn each_failure_kind_has_its_own_code_and_a_remediation_that_does_not_name_a_failing_call() {
    for (error, code) in [
        (
            SearchError::from(contextos_search::StoreFailure::busy(
                contextos_search::DerivedStore::Graph,
                "graph",
                "reading the nodes",
            )),
            "index/busy",
        ),
        (
            SearchError::from(contextos_search::StoreFailure::corrupt(
                contextos_search::DerivedStore::Graph,
                "graph",
                "database disk image is malformed",
            )),
            "index/corrupt",
        ),
        (
            SearchError::from(contextos_search::StoreFailure::undecodable(
                contextos_search::DerivedStore::Graph,
                "graph",
                "a node row",
            )),
            "index/undecodable",
        ),
    ] {
        assert_eq!(error.code(), code);
        assert!(!error.remediation().is_empty(), "{code} needs a remediation");
    }
    let corrupt = SearchError::from(contextos_search::StoreFailure::corrupt(
        contextos_search::DerivedStore::Graph,
        "graph",
        "malformed",
    ));
    assert!(
        corrupt.remediation().contains("moves the damaged files aside"),
        "a damaged store cannot be rebuilt in place, so the remediation must name the rebuild that first moves it \
         aside: {}",
        corrupt.remediation()
    );
}

#[test]
fn an_instance_holding_an_unreadable_store_recovers_once_another_instance_rebuilds_it() -> TestResult {
    let dir = tempfile::tempdir()?;
    graph_at(&dir, GraphBackend::Sqlite)?.rebuild(&one_note())?;
    sqlite_connection(&dir)?.execute("INSERT INTO nodes (path, value) VALUES ('bad.md', x'00ff')", [])?;
    let mut waiting = graph_at(&dir, GraphBackend::Sqlite)?;
    assert!(matches!(waiting.health(), StoreHealth::Unreadable(_)));

    graph_at(&dir, GraphBackend::Sqlite)?.rebuild(&one_note())?;

    assert_eq!(waiting.orphans()?.nodes.len(), 1);
    assert_eq!(waiting.health(), &StoreHealth::Ready);
    Ok(())
}

#[test]
fn notes_written_before_any_rebuild_leave_the_graph_needing_one_after_a_reopen() -> TestResult {
    for backend in [GraphBackend::Sqlite, GraphBackend::Fjall, GraphBackend::Serde] {
        let dir = tempfile::tempdir()?;
        {
            let mut graph = graph_at(&dir, backend)?;
            graph.upsert_note("a.md", "A", &[])?;
            assert_eq!(graph.health(), &StoreHealth::Empty, "{backend:?}");
        }
        let reopened = graph_at(&dir, backend)?;
        assert_eq!(
            reopened.health(),
            &StoreHealth::Empty,
            "a graph only ever written note by note is not the vault's graph ({backend:?})"
        );
        assert!(reopened.needs_rebuild());
    }
    Ok(())
}

#[test]
fn a_sibling_that_catches_up_on_note_writes_to_an_unbuilt_store_does_not_report_it_ready() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut sibling = graph_at(&dir, GraphBackend::Sqlite)?;
    let mut writer = graph_at(&dir, GraphBackend::Sqlite)?;
    writer.upsert_note("a.md", "A", &[])?;

    assert_eq!(sibling.orphans()?.nodes.len(), 1, "the sibling still sees the write");
    assert_eq!(sibling.health(), &StoreHealth::Empty);
    Ok(())
}

#[test]
fn a_note_write_never_stamps_a_store_written_in_another_format() -> TestResult {
    let dir = tempfile::tempdir()?;
    graph_at(&dir, GraphBackend::Sqlite)?.rebuild(&one_note())?;
    sqlite_connection(&dir)?.execute("UPDATE metadata SET value = 999 WHERE key = 'format_version'", [])?;
    {
        let mut graph = graph_at(&dir, GraphBackend::Sqlite)?;
        graph.upsert_note("b.md", "B", &[])?;
    }
    let reopened = graph_at(&dir, GraphBackend::Sqlite)?;
    assert_eq!(reopened.health(), &StoreHealth::Incompatible);
    Ok(())
}

#[test]
fn a_ready_graph_whose_store_stops_decoding_reports_itself_unreadable() -> TestResult {
    let dir = tempfile::tempdir()?;
    graph_at(&dir, GraphBackend::Sqlite)?.rebuild(&one_note())?;
    let mut graph = graph_at(&dir, GraphBackend::Sqlite)?;
    assert_eq!(graph.health(), &StoreHealth::Ready);
    // Another instance's write the catch-up cannot decode.
    let connection = sqlite_connection(&dir)?;
    connection.execute(
        "INSERT INTO changelog (generation, kind, key, value) VALUES (2, 'not-a-change', 'x', NULL)",
        [],
    )?;
    connection.execute("UPDATE metadata SET value = 2 WHERE key = 'generation'", [])?;

    let error = graph.orphans().err().ok_or("the catch-up's failure must be reported")?;
    assert_eq!(error.code(), "index/undecodable");
    assert!(
        matches!(graph.health(), StoreHealth::Unreadable(failure) if failure.code() == "index/undecodable"),
        "{:?}",
        graph.health()
    );
    Ok(())
}

#[test]
fn rebuilding_a_damaged_sqlite_store_reports_it_as_corrupt_not_as_a_storage_failure_to_rebuild() -> TestResult {
    let dir = tempfile::tempdir()?;
    populated_sqlite_store(&dir)?;
    damage_middle_of(&store_directory(&dir).join("graph.sqlite3"))?;
    let mut graph = graph_at(&dir, GraphBackend::Sqlite)?;

    let error = graph
        .rebuild(&one_note())
        .err()
        .ok_or("a damaged store cannot take the rebuild's write")?;

    assert_eq!(error.code(), "index/corrupt", "{error}");
    Ok(())
}

#[test]
fn only_a_store_a_rebuild_can_fix_asks_for_one() -> TestResult {
    let dir = tempfile::tempdir()?;
    std::fs::create_dir_all(store_directory(&dir).join("graph.json"))?;
    let graph = graph_at(&dir, GraphBackend::Serde)?;
    assert!(matches!(graph.health(), StoreHealth::Unreadable(_)));
    assert!(
        !graph.needs_rebuild(),
        "a store that cannot be read from disk is not fixed by a rebuild"
    );
    Ok(())
}

#[test]
fn a_ready_graph_that_reloads_a_store_another_build_rewrote_reports_incompatible_and_leaves_it_alone() -> TestResult {
    let dir = tempfile::tempdir()?;
    graph_at(&dir, GraphBackend::Sqlite)?.rebuild(&one_note())?;
    let mut graph = graph_at(&dir, GraphBackend::Sqlite)?;
    assert_eq!(graph.health(), &StoreHealth::Ready);
    // Another build rebuilt the store in its own format: a reset at a newer generation forces a full reload.
    let connection = sqlite_connection(&dir)?;
    connection.execute(
        "INSERT INTO changelog (generation, kind, key, value) VALUES (2, 'reset', '', NULL)",
        [],
    )?;
    connection.execute("UPDATE metadata SET value = 2 WHERE key = 'generation'", [])?;
    connection.execute("UPDATE metadata SET value = 999 WHERE key = 'format_version'", [])?;

    assert!(graph.orphans()?.nodes.is_empty(), "nothing in another format is read");
    assert_eq!(graph.health(), &StoreHealth::Incompatible);
    graph.upsert_note("b.md", "B", &[])?;

    let format: i64 = connection.query_row("SELECT value FROM metadata WHERE key = 'format_version'", [], |row| {
        row.get(0)
    })?;
    assert_eq!(format, 999, "a note write must not stamp another build's store");
    Ok(())
}
