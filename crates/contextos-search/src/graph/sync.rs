//! Cross-instance propagation for the `sqlite` backend: bringing this
//! instance's in-memory graph up to date with records a sibling
//! `contextos` process has persisted since this instance last checked,
//! via a partial delta where possible and a full reload when this
//! instance's last-seen generation has aged out of the store's history.

use petgraph::stable_graph::{EdgeIndex, NodeIndex};
use petgraph::visit::EdgeRef;

use crate::SearchError;

use super::{
    CatchUpKind, EdgeLink, EdgeRecord, GraphBackend, GraphChange, GraphNode, GraphNodeKind, LinkGraph, StoreFailure,
    StoreHealth, StoreSnapshot, load_snapshot, resolve::target_key,
};

impl LinkGraph {
    /// Brings this instance's in-memory graph up to date before a call. For a store that could not be read, retries
    /// the whole load, so a store another instance (or this one) has since rebuilt is picked up, and refuses the call
    /// with the failure while it still cannot be read. Otherwise, for `sqlite` only, applies any changes a sibling
    /// instance has persisted since this instance last checked: a no-op when nothing has changed (the cheap common
    /// case: one indexed point lookup). Called at the start of every public method except [`LinkGraph::rebuild`].
    ///
    /// # Errors
    ///
    /// Returns the read failure that keeps the store from being read.
    pub(super) fn catch_up(&mut self) -> Result<(), SearchError> {
        if matches!(self.health, StoreHealth::Unreadable(_)) {
            return self.retry_unreadable();
        }
        // A read that fails here leaves the store unreadable as far as this instance knows: recorded, so status
        // reports it and the next call retries the whole load rather than trusting the graph it last held.
        self.catch_up_with_siblings().map_err(|failure| {
            self.health = StoreHealth::Unreadable(failure.clone());
            SearchError::from(failure)
        })?;
        // A full reload leaves the graph without a listing, which the store never held, so the vault is listed again.
        self.ensure_listed()
    }

    /// Applies what sibling `sqlite` instances have persisted since this instance last checked.
    fn catch_up_with_siblings(&mut self) -> Result<(), StoreFailure> {
        if self.backend != GraphBackend::Sqlite {
            return Ok(());
        }
        let Some(current) = self.store.current_generation()? else {
            // Never persisted to yet: nothing a sibling could have written
            // either, so there is nothing to catch up to.
            return Ok(());
        };
        if current <= self.last_seen_generation {
            return Ok(());
        }

        if let Some(delta) = self.store.changes_since(self.last_seen_generation)? {
            self.apply_external_changes(delta.changes);
            self.last_seen_generation = delta.generation;
            // A delta carries records, not completeness: a graph not yet rebuilt stays so however many notes
            // siblings write, and only a rebuild (which forces the full reload below) makes it the vault's graph.
            self.last_catch_up = Some(CatchUpKind::Partial);
        } else {
            let snapshot = load_snapshot(self.store.as_ref(), &self.store_directory)?;
            self.adopt(snapshot);
            self.last_catch_up = Some(CatchUpKind::FullReload);
        }
        Ok(())
    }

    /// Retries loading a store that could not be read, adopting what it now holds, or recording and returning the
    /// failure that still keeps it from being read.
    fn retry_unreadable(&mut self) -> Result<(), SearchError> {
        match load_snapshot(self.store.as_ref(), &self.store_directory) {
            Ok(snapshot) => {
                self.adopt(snapshot);
                Ok(())
            }
            Err(failure) => {
                self.health = StoreHealth::Unreadable(failure.clone());
                Err(failure.into())
            }
        }
    }

    /// Replaces this instance's in-memory graph with what a full load found: the stored graph for a store in this
    /// build's format, or an empty graph for one never written or written in another format.
    pub(super) fn adopt(&mut self, snapshot: StoreSnapshot) {
        let (health, loaded) = match snapshot {
            StoreSnapshot::Ready(loaded) if loaded.complete => (StoreHealth::Ready, Some(loaded)),
            StoreSnapshot::Ready(loaded) => (StoreHealth::Empty, Some(loaded)),
            StoreSnapshot::Empty => (StoreHealth::Empty, None),
            StoreSnapshot::Incompatible => (StoreHealth::Incompatible, None),
        };
        if let Some(loaded) = loaded {
            self.graph = loaded.graph;
            self.index = loaded.index;
            self.edge_by_store_id = loaded.edge_by_store_id;
            self.next_edge_id = loaded.next_edge_id;
            self.last_seen_generation = loaded.generation;
            self.index_edges_by_name();
        } else {
            self.graph.clear();
            self.index.clear();
            self.edge_by_store_id.clear();
            self.edges_by_name.clear();
            self.next_edge_id = 0;
            self.last_seen_generation = 0;
        }
        self.health = health;
        // The store holds no listing, so a graph just replaced from it has none until the vault is listed again; the
        // base folders read for the graph it replaced may no longer be what the vault holds either.
        if self.lister.is_some() {
            self.files_listed = false;
        }
        self.base_records.clear();
    }

    /// Applies a sequence of externally-persisted [`GraphChange`] records
    /// (from a sibling instance's write, via
    /// [`StoresGraph::changes_since`]) to this instance's in-memory graph,
    /// without re-persisting them: they are already durable in the shared
    /// store. Every node/edge referenced by an upsert is resolved by its
    /// exact path (never the fuzzy bare-name matching
    /// [`LinkGraph::resolve_target`] does for a raw wikilink target,
    /// because these paths are already fully resolved by whichever
    /// instance originated the change): find-or-create, never a bare-name
    /// search. Idempotent: reapplying an already-applied change (possible
    /// if `self.store.current_generation()` was read slightly before
    /// `load_all()` completed during this instance's own initial open) is
    /// always safe, never doubles a node or drops an edge.
    ///
    /// [`GraphChange::Reset`] never appears here: [`LinkGraph::catch_up`]
    /// only reaches this method via the `Some(delta)` branch, and
    /// [`StoresGraph::changes_since`] itself guarantees a `Reset` anywhere
    /// in the requested range yields `None` instead, forcing the full
    /// reload branch. `next_edge_id` is advanced past every `store_id`
    /// this delta references: without this, a later *local* mutation on
    /// this instance could allocate a `store_id` a sibling has already
    /// used, silently colliding two unrelated edges into one stored row.
    ///
    /// [`StoresGraph::changes_since`]: super::StoresGraph::changes_since
    fn apply_external_changes(&mut self, changes: Vec<GraphChange>) {
        for change in changes {
            match change {
                GraphChange::Reset => {}
                GraphChange::RemoveNode(path) => {
                    if let Some(idx) = self.index.remove(&path) {
                        // Removing a node removes the edges on it, which this instance's lookups must forget too.
                        let incident: Vec<EdgeIndex> = self.graph.edges(idx).map(|edge| edge.id()).collect();
                        for edge in incident {
                            if let Some(record) = self.graph.remove_edge(edge) {
                                self.forget_edge(&record);
                            }
                        }
                        self.graph.remove_node(idx);
                    }
                }
                GraphChange::FileAdded(path) => {
                    self.files.insert(&path);
                    self.base_records.remove(&path);
                }
                GraphChange::FileRemoved { path, folder } => {
                    self.files.remove(&path);
                    self.base_records.remove(&path);
                    if folder {
                        for removed in self.files.remove_under(&path) {
                            self.base_records.remove(&removed);
                        }
                    }
                }
                GraphChange::UpsertNode(node) => {
                    // A note, or a file a link reaches, exists, so it is one of the vault's files.
                    if matches!(node.kind, GraphNodeKind::Note | GraphNodeKind::File) {
                        self.files.insert(&node.path);
                    }
                    let idx = self.ensure_node_by_exact_path(&node.path);
                    if let Some(weight) = self.graph.node_weight_mut(idx) {
                        *weight = node;
                    }
                }
                GraphChange::RemoveEdge { store_id } => {
                    if let Some(idx) = self.edge_by_store_id.get(&store_id).copied() {
                        if let Some(record) = self.graph.remove_edge(idx) {
                            self.forget_edge(&record);
                        } else {
                            self.edge_by_store_id.remove(&store_id);
                        }
                    }
                }
                GraphChange::UpsertEdge { store_id, edge } => {
                    self.next_edge_id = self.next_edge_id.max(store_id.saturating_add(1));
                    if let Some(existing) = self.edge_by_store_id.get(&store_id).copied() {
                        // Never actually reached today (a `store_id` is
                        // never reused by `wire_links`'s originating
                        // instance either), kept for genuine idempotency
                        // rather than assuming that invariant holds
                        // forever.
                        if let Some(record) = self.graph.remove_edge(existing) {
                            self.forget_edge(&record);
                        }
                    }
                    let from = self.ensure_node_by_exact_path(&edge.from);
                    let to = self.ensure_node_by_exact_path(&edge.to);
                    self.edges_by_name
                        .entry(target_key(&edge.target))
                        .or_default()
                        .insert(store_id);
                    let edge_idx = self.graph.add_edge(
                        from,
                        to,
                        EdgeRecord {
                            store_id,
                            link: EdgeLink {
                                form: edge.form,
                                target: edge.target,
                                line: edge.line,
                                raw: edge.raw,
                            },
                        },
                    );
                    self.edge_by_store_id.insert(store_id, edge_idx);
                }
            }
        }
    }

    /// Finds the node at exactly `path`, or creates a new phantom node
    /// there: the resolution [`LinkGraph::apply_external_changes`] needs
    /// for an edge's `from`/`to` endpoint, already a fully resolved path
    /// (never a raw wikilink target), so no bare-name fallback applies.
    /// Every [`GraphChange::UpsertEdge`] delta record is preceded by its
    /// endpoints' own [`GraphChange::UpsertNode`] records within the same
    /// `persist` call (`persist_changes` always orders node changes before
    /// edge changes), so this is expected to find an existing node in
    /// practice; the phantom fallback exists for genuine defensiveness, not
    /// because it is a normal path.
    fn ensure_node_by_exact_path(&mut self, path: &str) -> NodeIndex {
        if let Some(&idx) = self.index.get(path) {
            return idx;
        }
        let idx = self.graph.add_node(GraphNode::stand_in(path, GraphNodeKind::NotFound));
        self.index.insert(path.to_owned(), idx);
        idx
    }
}
