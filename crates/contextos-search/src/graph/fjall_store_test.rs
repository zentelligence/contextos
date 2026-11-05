use super::*;
use contextos_obsidian::LinkForm;

use crate::graph::GraphNodeKind;

fn node(path: &str) -> GraphNode {
    GraphNode {
        path: path.to_owned(),
        title: path.to_owned(),
        kind: GraphNodeKind::Note,
        candidates: Vec::new(),
        unreadable: None,
        base: None,
        stamp: None,
    }
}

fn edge(from: &str, to: &str) -> GraphEdge {
    GraphEdge {
        from: from.to_owned(),
        to: to.to_owned(),
        form: LinkForm::Wikilink,
        target: to.to_owned(),
        line: 1,
        raw: format!("[[{to}]]"),
    }
}

fn seeded_store(directory: &Path) -> Result<FjallGraphStore, SearchError> {
    let store = FjallGraphStore::open(directory)?;
    store.persist(
        &GraphWriteGuard::unlocked(),
        &[
            GraphChange::UpsertNode(node("a.md")),
            GraphChange::UpsertNode(node("b.md")),
            GraphChange::UpsertEdge {
                store_id: 0,
                edge: edge("a.md", "b.md"),
            },
            GraphChange::UpsertEdge {
                store_id: 1,
                edge: edge("b.md", "a.md"),
            },
        ],
        2,
        StoreCompleteness::Complete,
    )?;
    Ok(store)
}

fn stored_node_paths(store: &FjallGraphStore) -> Vec<String> {
    let mut paths: Vec<String> = store
        .load_all()
        .map(|(nodes, _)| nodes.into_iter().map(|node| node.path).collect())
        .unwrap_or_default();
    paths.sort();
    paths
}

fn stored_edge_ids(store: &FjallGraphStore) -> Vec<u64> {
    let mut ids: Vec<u64> = store
        .load_all()
        .map(|(_, edges)| edges.into_iter().map(|(store_id, _)| store_id).collect())
        .unwrap_or_default();
    ids.sort_unstable();
    ids
}

/// The property that makes a rebuild atomic for this backend: discarding the
/// previous graph only queues removals in the caller's batch, so nothing is
/// removed until that same batch commits alongside the repopulation. A
/// discard that committed on its own (a separate write, or a keyspace clear)
/// would leave the store empty if the repopulation then failed.
#[test]
fn discarding_previous_records_takes_effect_only_when_the_callers_batch_commits()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = seeded_store(dir.path())?;

    let mut batch = store.database.batch();
    store.discard_records_not_upserted(&mut batch, &[GraphChange::Reset, GraphChange::UpsertNode(node("b.md"))])?;

    assert_eq!(
        stored_node_paths(&store),
        vec!["a.md".to_owned(), "b.md".to_owned()],
        "nothing is removed before the batch commits"
    );
    assert_eq!(stored_edge_ids(&store), vec![0, 1]);

    batch.commit()?;

    assert_eq!(
        stored_node_paths(&store),
        vec!["b.md".to_owned()],
        "the node the call did not upsert is discarded on commit"
    );
    assert!(
        stored_edge_ids(&store).is_empty(),
        "every edge the call did not upsert is discarded"
    );
    Ok(())
}

#[test]
fn a_reset_persist_replaces_every_previous_record_and_keeps_the_store_trustworthy()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = seeded_store(dir.path())?;

    store.persist(
        &GraphWriteGuard::unlocked(),
        &[
            GraphChange::Reset,
            GraphChange::UpsertNode(node("b.md")),
            GraphChange::UpsertNode(node("c.md")),
            GraphChange::UpsertEdge {
                store_id: 0,
                edge: edge("b.md", "c.md"),
            },
        ],
        1,
        StoreCompleteness::Complete,
    )?;

    assert_eq!(stored_node_paths(&store), vec!["b.md".to_owned(), "c.md".to_owned()]);
    assert_eq!(
        stored_edge_ids(&store),
        vec![0],
        "the overlapping edge id is replaced, the surplus one removed"
    );
    let StoredGraphState::Ready(metadata) = store.read_metadata()? else {
        return Err("metadata is valid after a reset persist".into());
    };
    assert_eq!(metadata.next_edge_id, 1);
    Ok(())
}

#[test]
fn a_reset_persist_with_nothing_after_it_leaves_a_valid_empty_store() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = seeded_store(dir.path())?;

    store.persist(
        &GraphWriteGuard::unlocked(),
        &[GraphChange::Reset],
        0,
        StoreCompleteness::Complete,
    )?;

    assert!(
        stored_node_paths(&store).is_empty(),
        "`stored_node_paths(&store)` should be empty, got {:?}",
        stored_node_paths(&store)
    );
    assert!(
        stored_edge_ids(&store).is_empty(),
        "`stored_edge_ids(&store)` should be empty, got {:?}",
        stored_edge_ids(&store)
    );
    assert!(
        matches!(store.read_metadata()?, StoredGraphState::Ready(_)),
        "an empty rebuild still records a trustworthy store"
    );
    Ok(())
}

/// The failures no file fixture can produce on demand, classified from the error `fjall` reports.
#[test]
fn a_read_failure_is_classified_by_the_error_fjall_reports() {
    let directory = Path::new("graph");
    let classify = |error: fjall::Error| {
        StoreFailure::from(FjallRead {
            directory,
            what: "reading",
            error: &error,
        })
        .code()
    };
    assert_eq!(
        classify(fjall::Error::Io(std::io::Error::other("disk"))),
        "index/storage"
    );
    assert_eq!(
        classify(fjall::Error::Storage(fjall::LsmError::Io(std::io::Error::other(
            "disk"
        )))),
        "index/storage"
    );
    assert_eq!(
        classify(fjall::Error::Storage(fjall::LsmError::InvalidTrailer)),
        "index/corrupt"
    );
    assert_eq!(classify(fjall::Error::Unrecoverable), "index/corrupt");
    assert_eq!(classify(fjall::Error::Locked), "index/busy");
}
