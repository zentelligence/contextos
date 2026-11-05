//! Inspecting a vault's link graph store without changing it, for diagnostics.

use std::path::Path;

use super::{GraphBackend, LinkGraph, LinkGraphConfig, sqlite_store};
use crate::SearchError;
use crate::store::{DEFAULT_WRITE_WAIT, DerivedStore, StoreFailure, StoreHealth, StoreInspection};

/// Inspects the link graph store kept in `store_directory` by `backend`, without writing: whether it has been built,
/// whether it can be read, what it holds, and how many notes. A `sqlite` store is read through a read-only connection
/// and checked for damage; a single-instance store is opened as an instance would open it, and one another process
/// already holds open is reported busy rather than opened.
#[must_use]
pub fn inspect_graph_store(store_directory: &Path, backend: GraphBackend) -> StoreInspection {
    if backend == GraphBackend::Sqlite {
        return sqlite_store::inspect(store_directory);
    }
    let inspection = |health, records| StoreInspection {
        store: DerivedStore::Graph,
        location: store_directory.to_path_buf(),
        health,
        records,
    };
    let built = match backend {
        GraphBackend::Serde => store_directory.join("graph.json").exists(),
        GraphBackend::Fjall | GraphBackend::Sqlite => store_directory.exists(),
    };
    if !built {
        return inspection(None, 0);
    }
    let opened = LinkGraph::try_from(LinkGraphConfig {
        store_directory: store_directory.to_path_buf(),
        backend,
        write_wait: DEFAULT_WRITE_WAIT,
    });
    match opened {
        Ok(mut graph) => {
            let health = graph.health().clone();
            let nodes = graph.full_view().map_or(0, |view| view.nodes.len());
            inspection(Some(health), nodes)
        }
        Err(SearchError::GraphLocked { path }) => inspection(
            Some(StoreHealth::Unreadable(StoreFailure::busy(
                DerivedStore::Graph,
                path,
                "another contextos process has this single-instance store open; it reports the store's health itself",
            ))),
            0,
        ),
        Err(SearchError::Store(failure)) => inspection(Some(StoreHealth::Unreadable(failure)), 0),
        Err(error) => inspection(
            Some(StoreHealth::Unreadable(StoreFailure::io(
                DerivedStore::Graph,
                store_directory.display().to_string(),
                error.to_string(),
            ))),
            0,
        ),
    }
}
