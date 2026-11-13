//! A damaged derived store recovers: it is moved aside, kept where the status reports it, recreated, and repopulated
//! from the vault, by a fresh instance opening the vault or by a rebuild, but only when no other instance holds it,
//! and a store that is merely busy is never mistaken for a damaged one.

use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use contextos_core::VaultRootId;
use contextos_search::{
    FakeEmbedder, GraphBackend, RebuildTarget, SemanticConfig, StoreHealth, VaultSearchConfig, VaultSearchService,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Stores {
    text: bool,
    graph: bool,
    semantic: bool,
}

const GRAPH_ONLY: Stores = Stores {
    text: false,
    graph: true,
    semantic: false,
};

fn vault_with_notes(count: usize) -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    for index in 0..count {
        let next = (index + 1) % count;
        std::fs::write(
            vault
                .path()
                .join(format!("note-{index}-with-a-reasonably-long-name-to-grow-the-store.md")),
            format!(
                "# Note {index} with a long title to grow the store\n\n[[note-{next}-with-a-reasonably-long-name-to-grow-the-store]]\n"
            ),
        )?;
    }
    Ok(vault)
}

fn state(vault: &tempfile::TempDir) -> PathBuf {
    vault.path().join(".contextos")
}

fn open(vault: &tempfile::TempDir, stores: &Stores) -> Result<VaultSearchService, Box<dyn std::error::Error>> {
    Ok(VaultSearchService::try_from(VaultSearchConfig {
        root_id: VaultRootId::try_from(0_usize)?,
        root: vault.path().to_path_buf(),
        excludes: vec![".contextos".to_owned()],
        link_excludes: Vec::new(),
        link_ignore_targets: Vec::new(),
        hidden: Vec::new(),
        state_directory: state(vault),
        text_enabled: stores.text,
        graph_enabled: stores.graph,
        graph_backend: GraphBackend::Sqlite,
        semantic: stores.semantic.then(|| SemanticConfig {
            embedder: Box::new(FakeEmbedder::new(4)),
            vector_store_path: state(vault).join("vectors.db"),
        }),
    })?)
}

/// Overwrites the middle half of `file` with garbage, in place, so the store opens (its first page is intact) but its
/// data pages cannot be read, whichever of the store's tables the pages in the middle belong to.
fn damage_middle_of(file: &Path) -> TestResult {
    let length = std::fs::metadata(file)?.len();
    assert!(length > 32_768, "the fixture must span several pages");
    let mut handle = OpenOptions::new().write(true).open(file)?;
    handle.seek(SeekFrom::Start(length / 4))?;
    handle.write_all(&vec![0xFF_u8; usize::try_from(length / 2)?])?;
    Ok(())
}

/// A vault with a built, then damaged, `sqlite` link graph store.
fn vault_with_damaged_graph() -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    let vault = vault_with_notes(400)?;
    open(&vault, &GRAPH_ONLY)?.rebuild(RebuildTarget::Graph)?;
    damage_middle_of(&state(&vault).join("graph").join("graph.sqlite3"))?;
    Ok(vault)
}

/// Registers a holder of `store` for a process other than this one, as another live instance would.
fn another_instance_holding(
    vault: &tempfile::TempDir,
    store: &str,
) -> Result<std::fs::File, Box<dyn std::error::Error>> {
    let holders = state(vault).join("holders").join(store);
    std::fs::create_dir_all(&holders)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(holders.join(format!("{}-0.holder", std::process::id().wrapping_add(1))))?;
    file.lock()?;
    Ok(file)
}

fn quarantined(vault: &tempfile::TempDir) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let directory = state(vault).join("quarantine");
    if !directory.exists() {
        return Ok(Vec::new());
    }
    Ok(std::fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<_, _>>()?)
}

#[test]
fn a_fresh_instance_moves_a_damaged_graph_store_aside_and_rebuilds_it() -> TestResult {
    let vault = vault_with_damaged_graph()?;

    let service = open(&vault, &GRAPH_ONLY)?;

    let graph = service.status().graph;
    assert_eq!(graph.health, Some(StoreHealth::Ready), "{graph:?}");
    assert_eq!(graph.nodes, 400, "the recovered graph must be rebuilt from the vault");
    let recovery = graph.last_recovery.ok_or("the recovery must be reported")?;
    assert!(
        recovery.moved_to.join("graph").join("graph.sqlite3").is_file(),
        "the damaged store must be kept where the status says: {}",
        recovery.moved_to.display()
    );
    assert_eq!(quarantined(&vault)?, vec![recovery.moved_to]);
    Ok(())
}

#[test]
fn a_rebuild_recovers_a_damaged_graph_store_in_one_call_once_no_other_instance_holds_it() -> TestResult {
    let vault = vault_with_damaged_graph()?;
    let other = another_instance_holding(&vault, "graph")?;
    let service = open(&vault, &GRAPH_ONLY)?;
    drop(other);

    let report = service.rebuild(RebuildTarget::Graph)?;

    assert_eq!(report.graph.map(|graph| graph.nodes), Some(400));
    let recovery = report.recovered.first().ok_or("the rebuild must report the recovery")?;
    assert!(recovery.moved_to.join("graph").join("graph.sqlite3").is_file());
    assert_eq!(service.status().graph.health, Some(StoreHealth::Ready));
    Ok(())
}

#[test]
fn a_damaged_store_another_instance_holds_is_reported_and_left_in_place() -> TestResult {
    let vault = vault_with_damaged_graph()?;
    let _other = another_instance_holding(&vault, "graph")?;

    let service = open(&vault, &GRAPH_ONLY)?;

    let graph = service.status().graph;
    let Some(StoreHealth::Unreadable(failure)) = &graph.health else {
        return Err(format!("expected an unreadable graph, found {graph:?}").into());
    };
    assert_eq!(failure.code(), "index/corrupt");
    let refused = service
        .rebuild(RebuildTarget::Graph)
        .err()
        .ok_or("the rebuild must not proceed while another instance holds the store")?;
    assert_eq!(refused.code(), "index/in-use", "{refused}");
    assert!(
        quarantined(&vault)?.is_empty(),
        "nothing may be moved while another instance holds the store"
    );
    assert!(state(&vault).join("graph").join("graph.sqlite3").is_file());
    Ok(())
}

#[test]
fn a_busy_store_is_never_mistaken_for_a_damaged_one() -> TestResult {
    let vault = vault_with_notes(5)?;
    let service = open(&vault, &GRAPH_ONLY)?;
    service.rebuild(RebuildTarget::Graph)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(state(&vault).join("graph").join("graph.sqlite3.lock"))?;
    lock.lock()?;

    let refused = service
        .rebuild(RebuildTarget::Graph)
        .err()
        .ok_or("a rebuild must not take a store another instance is writing")?;

    assert_eq!(refused.code(), "index/busy", "{refused}");
    assert!(quarantined(&vault)?.is_empty(), "a busy store must not be moved aside");
    lock.unlock()?;
    assert_eq!(service.status().graph.health, Some(StoreHealth::Ready));
    Ok(())
}

#[test]
fn a_fresh_instance_moves_a_damaged_text_index_aside_and_refills_it_on_the_next_search() -> TestResult {
    let vault = vault_with_notes(3)?;
    let text_only = Stores {
        text: true,
        graph: false,
        semantic: false,
    };
    open(&vault, &text_only)?.rebuild(RebuildTarget::Text)?;
    std::fs::write(state(&vault).join("index").join("meta.json"), b"{ not json")?;

    let service = open(&vault, &text_only)?;

    let text = service.status().text;
    assert_eq!(text.health, Some(StoreHealth::Ready), "{text:?}");
    let recovery = text.last_recovery.ok_or("the recovery must be reported")?;
    assert!(recovery.moved_to.join("index").join("meta.json").is_file());
    assert_eq!(
        text.stale_estimate, 3,
        "the fresh index is refilled by its next reconciliation"
    );
    service.rebuild(RebuildTarget::Text)?;
    assert_eq!(service.status().text.documents, 3);
    Ok(())
}

#[test]
fn a_fresh_instance_moves_a_damaged_vector_store_aside_and_queues_the_vault_to_embed_again() -> TestResult {
    let vault = vault_with_notes(3)?;
    std::fs::create_dir_all(state(&vault))?;
    std::fs::write(state(&vault).join("vectors.db"), vec![0xFF_u8; 8192])?;
    let semantic_only = Stores {
        text: false,
        graph: false,
        semantic: true,
    };

    let service = open(&vault, &semantic_only)?;

    let semantic = service.status().semantic;
    assert_eq!(semantic.health, Some(StoreHealth::Ready), "{semantic:?}");
    assert_eq!(
        semantic.stale_estimate, 3,
        "every note must be queued to be embedded again"
    );
    let recovery = semantic.last_recovery.ok_or("the recovery must be reported")?;
    assert_eq!(
        std::fs::read(recovery.moved_to.join("vectors.db"))?,
        vec![0xFF_u8; 8192]
    );
    Ok(())
}
