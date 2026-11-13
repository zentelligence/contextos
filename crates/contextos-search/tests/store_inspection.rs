//! Inspecting a derived store for diagnostics never changes it: a store never built reads as such and is not created,
//! a healthy store reports what it holds, and a damaged store is reported damaged with the stable code a status call
//! and a recovery use for it.

use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use contextos_core::VaultRootId;
use contextos_search::{
    FakeEmbedder, GraphBackend, RebuildTarget, SemanticConfig, StoreHealth, StoreInspection, VaultSearchConfig,
    VaultSearchService, inspect_graph_store, inspect_text_index, inspect_vector_store,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn vault() -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    for index in 0..300 {
        std::fs::write(
            vault.path().join(format!(
                "note-{index}-with-a-reasonably-long-name-to-grow-every-store.md"
            )),
            format!("# Note {index} with a long title to grow every store\n\nProse about note {index}.\n"),
        )?;
    }
    Ok(vault)
}

/// Builds every store for `vault`, then closes them.
fn build_every_store(vault: &tempfile::TempDir) -> TestResult {
    let service = VaultSearchService::try_from(VaultSearchConfig {
        root_id: VaultRootId::try_from(0_usize)?,
        root: vault.path().to_path_buf(),
        excludes: vec![".contextos".to_owned()],
        link_excludes: Vec::new(),
        link_ignore_targets: Vec::new(),
        hidden: Vec::new(),
        state_directory: vault.path().join(".contextos"),
        text_enabled: true,
        graph_enabled: true,
        graph_backend: GraphBackend::Sqlite,
        semantic: Some(SemanticConfig {
            embedder: Box::new(FakeEmbedder::new(4)),
            vector_store_path: vault.path().join(".contextos/vectors.db"),
        }),
    })?;
    service.rebuild(RebuildTarget::All)?;
    Ok(())
}

fn inspect_all(state: &Path) -> [StoreInspection; 3] {
    [
        inspect_text_index(&state.join("index")),
        inspect_graph_store(&state.join("graph"), GraphBackend::Sqlite),
        inspect_vector_store(&state.join("vectors.db")),
    ]
}

fn assert_code(inspection: &StoreInspection, code: &str) -> TestResult {
    match &inspection.health {
        Some(StoreHealth::Unreadable(failure)) => {
            assert_eq!(failure.code(), code, "{failure}");
            Ok(())
        }
        other => Err(format!("expected {code} for {:?}, found {other:?}", inspection.store).into()),
    }
}

fn damage_middle_of(file: &Path) -> TestResult {
    let length = std::fs::metadata(file)?.len();
    assert!(length > 32_768, "the fixture must span several pages");
    let mut handle = OpenOptions::new().write(true).open(file)?;
    handle.seek(SeekFrom::Start(length / 4))?;
    handle.write_all(&vec![0xFF_u8; usize::try_from(length / 2)?])?;
    Ok(())
}

/// Every file under `directory`, by path relative to it, with its length.
fn listing(directory: &Path) -> Result<Vec<(String, u64)>, Box<dyn std::error::Error>> {
    let mut files = Vec::new();
    for entry in walk(directory)? {
        let relative = entry.strip_prefix(directory)?.display().to_string();
        files.push((relative, std::fs::metadata(&entry)?.len()));
    }
    files.sort();
    Ok(files)
}

fn walk(directory: &Path) -> std::io::Result<Vec<std::path::PathBuf>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            files.extend(walk(&path)?);
        } else {
            files.push(path);
        }
    }
    Ok(files)
}

#[test]
fn a_store_never_built_reads_as_not_built_and_is_not_created() -> TestResult {
    let dir = tempfile::tempdir()?;
    let state = dir.path().join(".contextos");
    for inspection in inspect_all(&state) {
        assert_eq!(inspection.health, None, "{:?}", inspection.store);
    }
    assert!(!state.exists(), "inspecting must not create the state directory");
    Ok(())
}

#[test]
fn healthy_stores_report_what_they_hold_and_inspecting_changes_nothing() -> TestResult {
    let vault = vault()?;
    build_every_store(&vault)?;
    let state = vault.path().join(".contextos");
    let before = listing(&state)?;

    let [text, graph, vectors] = inspect_all(&state);

    assert_eq!((text.health, text.records), (Some(StoreHealth::Ready), 300));
    assert_eq!((graph.health, graph.records), (Some(StoreHealth::Ready), 300));
    assert_eq!((vectors.health, vectors.records), (Some(StoreHealth::Ready), 300));
    assert_eq!(listing(&state)?, before, "inspecting must leave every file as it was");
    Ok(())
}

#[test]
fn a_damaged_graph_store_is_reported_corrupt() -> TestResult {
    let vault = vault()?;
    build_every_store(&vault)?;
    let state = vault.path().join(".contextos");
    damage_middle_of(&state.join("graph").join("graph.sqlite3"))?;

    assert_code(
        &inspect_graph_store(&state.join("graph"), GraphBackend::Sqlite),
        "index/corrupt",
    )
}

#[test]
fn a_graph_store_whose_changelog_runs_past_its_generation_is_reported_undecodable() -> TestResult {
    let vault = vault()?;
    build_every_store(&vault)?;
    let state = vault.path().join(".contextos");
    let connection = rusqlite::Connection::open(state.join("graph").join("graph.sqlite3"))?;
    connection.execute(
        "INSERT INTO changelog (generation, kind, key, value) VALUES (999999, 'remove_node', 'x.md', NULL)",
        [],
    )?;
    drop(connection);

    assert_code(
        &inspect_graph_store(&state.join("graph"), GraphBackend::Sqlite),
        "index/undecodable",
    )
}

#[test]
fn a_graph_store_never_rebuilt_reads_as_empty() -> TestResult {
    let vault = vault()?;
    let state = vault.path().join(".contextos");
    let mut graph = contextos_search::LinkGraph::try_from(contextos_search::LinkGraphConfig {
        store_directory: state.join("graph"),
        backend: GraphBackend::Sqlite,
        write_wait: contextos_search::DEFAULT_WRITE_WAIT,
    })?;
    graph.upsert_note("a.md", "A", &[])?;
    drop(graph);

    let inspection = inspect_graph_store(&state.join("graph"), GraphBackend::Sqlite);
    assert_eq!(inspection.health, Some(StoreHealth::Empty));
    Ok(())
}

#[test]
fn a_text_index_whose_metadata_is_damaged_is_reported_corrupt() -> TestResult {
    let vault = vault()?;
    build_every_store(&vault)?;
    let index = vault.path().join(".contextos").join("index");
    std::fs::write(index.join("meta.json"), b"{ not json")?;

    assert_code(&inspect_text_index(&index), "index/corrupt")
}

#[test]
fn a_text_index_with_a_segment_file_failing_its_checksum_is_reported_corrupt() -> TestResult {
    let vault = vault()?;
    build_every_store(&vault)?;
    let index = vault.path().join(".contextos").join("index");
    let segment = walk(&index)?
        .into_iter()
        .filter(|path| path.extension().is_some_and(|extension| extension == "store"))
        .max_by_key(|path| std::fs::metadata(path).map_or(0, |metadata| metadata.len()))
        .ok_or("the index must hold a stored-fields file")?;
    let length = std::fs::metadata(&segment)?.len();
    let mut handle = OpenOptions::new().write(true).open(&segment)?;
    handle.seek(SeekFrom::Start(length / 2))?;
    handle.write_all(&[0xA5_u8; 16])?;
    drop(handle);

    assert_code(&inspect_text_index(&index), "index/corrupt")
}

#[test]
fn a_vector_store_that_is_not_a_database_is_reported_corrupt() -> TestResult {
    let state = tempfile::tempdir()?;
    let path = state.path().join("vectors.db");
    std::fs::write(&path, vec![0xFF_u8; 8192])?;

    assert_code(&inspect_vector_store(&path), "index/corrupt")
}
