use std::cell::Cell;

use super::*;
use crate::StoreFailure;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn graph_service(vault: &tempfile::TempDir) -> Result<VaultSearchService, Box<dyn std::error::Error>> {
    Ok(VaultSearchService::try_from(VaultSearchConfig {
        root_id: VaultRootId::try_from(0_usize)?,
        root: vault.path().to_path_buf(),
        excludes: vec![".contextos".to_owned()],
        link_excludes: Vec::new(),
        hidden: Vec::new(),
        state_directory: vault.path().join(".contextos"),
        text_enabled: false,
        graph_enabled: true,
        graph_backend: GraphBackend::Sqlite,
        semantic: None,
    })?)
}

#[test]
fn a_rebuild_that_finds_its_store_damaged_recovers_it_and_rebuilds_once_more() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("a.md"), "# A\n")?;
    let service = graph_service(&vault)?;
    let attempts = Cell::new(0);
    let mut recovered = Vec::new();

    let outcome = service.recovering(DerivedStore::Graph, &mut recovered, |service| {
        attempts.set(attempts.get() + 1);
        if attempts.get() == 1 {
            // The rebuild's own write finds the store damaged, as a persist into damaged pages reports it.
            let damage: SearchError = StoreFailure::corrupt(DerivedStore::Graph, "graph", "malformed").into();
            if let Some(slot) = &service.graph {
                slot.note(&damage);
            }
            return Err(damage);
        }
        Ok("rebuilt")
    })?;

    assert_eq!(outcome, "rebuilt");
    assert_eq!(attempts.get(), 2, "the rebuild runs once more after recovering");
    assert_eq!(recovered.len(), 1);
    Ok(())
}

#[test]
fn a_rebuild_that_fails_for_any_other_reason_is_not_retried_or_recovered() -> TestResult {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let service = graph_service(&vault)?;
    let attempts = Cell::new(0);
    let mut recovered = Vec::new();

    let outcome: Result<(), SearchError> = service.recovering(DerivedStore::Graph, &mut recovered, |_| {
        attempts.set(attempts.get() + 1);
        Err(StoreFailure::busy(DerivedStore::Graph, "graph", "contended").into())
    });

    assert_eq!(outcome.err().map(|error| error.code()), Some("index/busy"));
    assert_eq!(attempts.get(), 1);
    assert_eq!(recovered.len(), 0, "nothing is recovered");
    Ok(())
}

#[test]
fn a_text_index_released_as_damaged_stays_released_through_a_write_and_the_next_query_recovers_it() -> TestResult {
    use contextos_core::{OpKind, Origin, VaultPath, VaultPathInput, VaultRoot, VaultRootInput, VaultSet};

    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    std::fs::write(vault.path().join("a.md"), "# A\n\nsearchable prose\n")?;
    let service = VaultSearchService::try_from(VaultSearchConfig {
        root_id: VaultRootId::try_from(0_usize)?,
        root: vault.path().to_path_buf(),
        excludes: vec![".contextos".to_owned()],
        link_excludes: Vec::new(),
        hidden: Vec::new(),
        state_directory: vault.path().join(".contextos"),
        text_enabled: true,
        graph_enabled: false,
        graph_backend: GraphBackend::Sqlite,
        semantic: None,
    })?;
    let slot = service.text.as_ref().ok_or("text is enabled")?;
    slot.note(&StoreFailure::corrupt(DerivedStore::Text, "index", "a segment is damaged").into());

    let roots = VaultSet::try_from(vec![VaultRoot::try_from(VaultRootInput {
        path: vault.path().to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?])?;
    let event = OperationEvent {
        kind: OpKind::Modify,
        paths: vec![VaultPath::try_from(VaultPathInput {
            roots: &roots,
            raw: "a.md",
        })?],
        origin: Origin::Tool("fs_write_file".to_owned()),
        summary: "test".to_owned(),
        at: time::OffsetDateTime::UNIX_EPOCH,
    };
    let warning = service
        .update(&event)
        .err()
        .ok_or("the write must report the damaged index")?;
    assert_eq!(warning.code, "index/corrupt");
    assert!(
        slot.needs_recovery(),
        "a write must not quietly reopen the damaged index"
    );

    let fields = serde_json::Map::new();
    let (hits, _) = service.query_text(&TextQuery {
        query: "searchable",
        path_prefix: None,
        exclude_paths: &[],
        tags: &[],
        fields: &fields,
        limit: 10,
    })?;

    assert_eq!(hits.len(), 1, "the query recovers the index and refills it");
    let status = service.status();
    assert!(status.text.last_recovery.is_some());
    Ok(())
}
