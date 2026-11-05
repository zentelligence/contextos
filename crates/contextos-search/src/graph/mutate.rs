//! Per-file and full-scan mutation of the in-memory link graph:
//! [`LinkGraph::upsert_note`], [`LinkGraph::remove_note`], and
//! [`LinkGraph::rebuild`], plus the single batched
//! [`persist_changes`](LinkGraph::persist_changes) write that carries each
//! call's changed records to the configured store. Resolving links, and
//! resolving them again as files come and go, is in `resolve`.

use std::collections::BTreeSet;

use contextos_obsidian::{ObsidianLink, VaultFiles};
use petgraph::Direction;
use petgraph::stable_graph::{EdgeIndex, NodeIndex};
use petgraph::visit::EdgeRef;

use crate::SearchError;

use super::{
    EdgeLink, FileEntry, GraphBackend, GraphChange, GraphEdge, GraphNode, GraphNodeKind, GraphWriteGuard, LinkGraph,
    NoteEntry, NoteLinks, NoteSource, PendingChanges, StoreCompleteness, StoreHealth, UnreadableNote, VaultScan,
};

/// What a pass over a note read of it besides its links.
#[derive(Clone, Copy)]
pub(super) struct NoteFacts<'a> {
    /// Why its links could not be read, when they could not.
    pub(super) unreadable: Option<&'a str>,
    /// The base it declares.
    pub(super) base: Option<&'a str>,
    /// Its file as it was when read.
    pub(super) stamp: Option<super::NoteStamp>,
}

impl LinkGraph {
    /// Reports whether repopulating the graph from a full vault scan is part of getting it right: its store has not
    /// been rebuilt, was written in another format, holds a record that cannot be decoded, or is damaged (once
    /// deleted). Not for a store that is busy or cannot be read from disk, which a rebuild does not fix.
    #[must_use]
    pub const fn needs_rebuild(&self) -> bool {
        match &self.health {
            StoreHealth::Ready => false,
            StoreHealth::Empty | StoreHealth::Incompatible => true,
            StoreHealth::Unreadable(failure) => failure.kind().rebuild_helps(),
        }
    }

    /// What this graph found when it last loaded its store.
    #[must_use]
    pub const fn health(&self) -> &StoreHealth {
        &self.health
    }

    /// Marks the graph as reflecting a complete, trustworthy scan of the vault.
    pub fn mark_rebuilt(&mut self) {
        self.health = StoreHealth::Ready;
    }

    /// Upserts a note into the graph, replacing its previously recorded outgoing links.
    ///
    /// An existing phantom or file node at `path` is upgraded to a note in place, so edges other notes already hold to
    /// it keep pointing at the same node. Every prior outgoing edge of this note is dropped before the supplied `links`
    /// are wired, each resolved with the one link resolver against the vault's files (see [`Self::seed_files`]), so a
    /// link to an image, a canvas, or a note outside the index reaches that file instead of a phantom. The links in
    /// other notes that this note's name can change the meaning of are resolved again, whether or not the listing
    /// already held it. Self-links are permitted.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the updated graph cannot be persisted
    /// to its store.
    pub fn upsert_note(&mut self, path: &str, title: &str, links: &[ObsidianLink]) -> Result<(), SearchError> {
        self.apply_entries(&[FileEntry::Note(NoteEntry {
            path: path.to_owned(),
            title: title.to_owned(),
            links: Ok(links.to_vec()),
            base: None,
            stamp: None,
        })])
    }

    /// Records the note at `path` as one whose links cannot be read, for `reason`. Its previously recorded outgoing
    /// links are dropped, because what it links to is no longer known, and the node keeps the reason until the note is
    /// written again.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the updated graph cannot be persisted to its store.
    pub fn upsert_unreadable_note(&mut self, path: &str, title: &str, reason: &str) -> Result<(), SearchError> {
        self.apply_entries(&[FileEntry::Note(NoteEntry {
            path: path.to_owned(),
            title: title.to_owned(),
            links: Err(reason.to_owned()),
            base: None,
            stamp: None,
        })])
    }

    /// Applies a batch of notes and other files that now exist, in one lock, one catch-up, and one write to the store,
    /// so a folder moved into the vault costs one transaction and not one per file. Each is handled as
    /// [`Self::upsert_note`] and [`Self::file_added`] handle one.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the updated graph cannot be persisted to its store.
    pub fn apply_entries(&mut self, entries: &[FileEntry]) -> Result<(), SearchError> {
        if entries.is_empty() {
            return Ok(());
        }
        let guard = self.store.write_guard()?;
        self.catch_up()?;
        let mut changes = PendingChanges::default();
        for entry in entries {
            match entry {
                FileEntry::Note(note) => self.apply_note(note, &mut changes),
                FileEntry::File(path) => self.add_file(path, &mut changes),
                FileEntry::Removed { path, folder } => self.remove_inner(path, *folder, &mut changes),
            }
        }
        self.prune_unreferenced(&mut changes);
        self.persist_changes(&guard, &changes, false)
    }

    /// Records `note` in the graph: its node, its links, and the links elsewhere that its name can change the meaning
    /// of.
    fn apply_note(&mut self, note: &NoteEntry, changes: &mut PendingChanges) {
        self.files.insert(&note.path);
        let (links, reason) = match &note.links {
            Ok(links) => (Some(links.as_slice()), None),
            Err(reason) => (None, Some(reason.as_str())),
        };
        let node_idx = self.ensure_real_node(
            &note.path,
            &note.title,
            NoteFacts {
                unreadable: reason,
                base: note.base.as_deref(),
                stamp: note.stamp,
            },
            changes,
        );
        self.clear_outgoing(node_idx, changes);
        if let Some(links) = links {
            self.wire_links(node_idx, links, changes);
        }
        let affected = self.edges_naming(&note.path);
        self.reresolve(&affected, changes);
    }

    /// Removes a note from the graph.
    ///
    /// Its outgoing edges are dropped first. The links that reached it are resolved again, to another file that
    /// answers to them if there is one and otherwise to a not-found stand-in keyed by what each link says, which
    /// matches Obsidian's treatment of a broken link. Stand-ins and file nodes left with no link to them are pruned.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the updated graph cannot be persisted
    /// to its store.
    pub fn remove_note(&mut self, path: &str) -> Result<(), SearchError> {
        self.remove_files(path, false)
    }

    /// Rebuilds the entire graph from a full vault scan.
    ///
    /// Builds a fresh graph from every note in two passes: the first creates every note node so the second pass's
    /// link resolution has the complete set of vault files regardless of scan order. The store's previous contents
    /// are replaced in one atomic write at the end, after which the rebuild flag is cleared.
    ///
    /// A failed write leaves everything as it was: the store still holds
    /// the previous graph, this instance's in-memory graph is restored to
    /// it, and the rebuild flag is unchanged, so neither a reader nor a
    /// later call ever builds on a half-replaced graph.
    ///
    /// Does not catch up first, unlike every other public method: a
    /// rebuild is about to replace this instance's entire state
    /// from a fresh vault scan regardless of what a sibling has persisted,
    /// so catching up first would be pointless work immediately discarded.
    /// The `reset` record `persist_changes` writes below is what keeps a
    /// *sibling's* next catch-up correct despite this instance skipping its
    /// own.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the rebuilt graph cannot be persisted.
    pub fn rebuild(&mut self, notes: &[NoteLinks]) -> Result<(), SearchError> {
        self.rebuild_vault(VaultScan {
            notes: notes.to_vec(),
            unreadable: Vec::new(),
            files: Vec::new(),
            bases: std::collections::BTreeMap::new(),
            stamps: std::collections::BTreeMap::new(),
        })
    }

    /// As [`Self::rebuild`], from a scan that also lists the vault's other files and the notes whose links could not
    /// be read. Every note is a file too, so `scan.files` need not list them.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the rebuilt graph cannot be persisted.
    pub fn rebuild_vault(&mut self, scan: VaultScan) -> Result<(), SearchError> {
        let mut scan = Some(scan);
        self.rebuild_from(None, &mut |_| Ok(scan.take().unwrap_or_default()))
    }

    /// The store's generation as of now, for a caller about to scan the vault for [`Self::rebuild_from`]: `None` for a
    /// backend that does not track one, or a store not yet written or that cannot be read.
    #[must_use]
    pub fn store_generation(&self) -> Option<u64> {
        if self.backend != GraphBackend::Sqlite {
            return None;
        }
        self.store.current_generation().ok().flatten()
    }

    /// The notes written or removed through the store since generation `since`, by any instance, or `None` when that
    /// cannot be told: the backend keeps no history, the history has aged out, or another rebuild replaced the graph
    /// in between.
    fn notes_changed_since(&self, since: Option<u64>) -> Option<BTreeSet<String>> {
        let since = since?;
        if self.backend != GraphBackend::Sqlite {
            return None;
        }
        let delta = self.store.changes_since(since).ok().flatten()?;
        let mut changed = BTreeSet::new();
        for change in delta.changes {
            match change {
                GraphChange::UpsertNode(node) => {
                    changed.insert(node.path);
                }
                GraphChange::RemoveNode(path)
                | GraphChange::FileAdded(path)
                | GraphChange::FileRemoved { path, .. } => {
                    changed.insert(path);
                }
                GraphChange::UpsertEdge { edge, .. } => {
                    changed.insert(edge.from);
                }
                GraphChange::RemoveEdge { .. } | GraphChange::Reset => {}
            }
        }
        Some(changed)
    }

    /// As [`Self::rebuild`], but takes the notes from `notes`, which is called only once this graph holds its store's
    /// write lock. A caller that scanned the vault earlier, at store generation `scanned_at`, re-checks its scan there,
    /// so no note another instance writes between the scan and the commit is replaced by what the scan read: every
    /// other writer of the store writes the note first and takes the lock to record it after. `notes` is given the
    /// notes written or removed through the store since `scanned_at`, so it re-reads only those and keeps the lock
    /// briefly; given `None`, when that cannot be told, it re-checks every note.
    ///
    /// # Errors
    ///
    /// Returns `notes`' own error, a busy failure when another instance holds the store's write lock past its wait,
    /// and a storage error when the rebuilt graph cannot be persisted.
    pub fn rebuild_from(&mut self, scanned_at: Option<u64>, notes: &mut NoteSource) -> Result<(), SearchError> {
        let guard = self.store.write_guard()?;
        let written_since = self.notes_changed_since(scanned_at);
        let scan = notes(written_since.as_ref())?;
        let previous = (
            std::mem::take(&mut self.graph),
            std::mem::take(&mut self.index),
            std::mem::take(&mut self.edge_by_store_id),
            std::mem::take(&mut self.edges_by_name),
            std::mem::take(&mut self.next_edge_id),
            std::mem::replace(
                &mut self.files,
                scan.files
                    .iter()
                    .cloned()
                    .chain(scan.notes.iter().map(|(path, ..)| path.clone()))
                    .chain(scan.unreadable.iter().map(|note| note.path.clone()))
                    .collect::<VaultFiles>(),
            ),
            self.files_listed,
        );
        self.files_listed = true;
        self.base_records.clear();

        let mut changes = PendingChanges::default();
        for (path, title, _links) in &scan.notes {
            let facts = NoteFacts {
                unreadable: None,
                base: scan.bases.get(path).map(String::as_str),
                stamp: scan.stamps.get(path).copied(),
            };
            self.ensure_real_node(path, title, facts, &mut changes);
        }
        for UnreadableNote { path, title, reason } in &scan.unreadable {
            let facts = NoteFacts {
                unreadable: Some(reason),
                base: scan.bases.get(path).map(String::as_str),
                stamp: scan.stamps.get(path).copied(),
            };
            self.ensure_real_node(path, title, facts, &mut changes);
        }
        for (path, _title, links) in &scan.notes {
            if let Some(&node_idx) = self.index.get(path.as_str()) {
                self.wire_links(node_idx, links, &mut changes);
            }
        }

        match self.persist_changes(&guard, &changes, true) {
            Ok(()) => {
                self.mark_rebuilt();
                Ok(())
            }
            Err(error) => {
                (
                    self.graph,
                    self.index,
                    self.edge_by_store_id,
                    self.edges_by_name,
                    self.next_edge_id,
                    self.files,
                    self.files_listed,
                ) = previous;
                Err(error)
            }
        }
    }

    /// Resolves `changes` against the current in-memory graph into
    /// backend-agnostic [`GraphChange`] records, then hands them to the
    /// configured [`StoresGraph`] backend in one call: removed node and
    /// edge keys become removals, upserted nodes and edges are
    /// (re-)encoded from their current in-memory state. `StoresGraph::
    /// persist` unconditionally refreshes the store's format identifier
    /// and edge-id allocator alongside them.
    ///
    /// `reset` is `true` only from [`LinkGraph::rebuild`], which replaces the
    /// whole stored graph: a leading [`GraphChange::Reset`] record makes the
    /// backend discard the previous records within the same atomic write,
    /// and tells a propagation-tracking backend (`sqlite`) to force any
    /// sibling's next catch-up to a full reload rather than a partial
    /// merge, since the ordinary upsert/remove vocabulary cannot represent
    /// "the old state was replaced outright".
    ///
    /// After a successful write, also advances this instance's own
    /// `last_seen_generation` to match when the backend
    /// tracks one: without this, this instance's own next call would see
    /// the store's generation has advanced past what it last recorded (by
    /// its own write) and redundantly "catch up" to a change it already
    /// has in memory.
    ///
    /// [`StoresGraph`]: super::StoresGraph
    pub(super) fn persist_changes(
        &mut self,
        guard: &GraphWriteGuard,
        changes: &PendingChanges,
        reset: bool,
    ) -> Result<(), SearchError> {
        let mut records: Vec<GraphChange> = Vec::with_capacity(
            usize::from(reset)
                + changes.removed_node_keys.len()
                + changes.upserted_nodes.len()
                + changes.removed_edge_ids.len()
                + changes.upserted_edges.len()
                + changes.files_added.len()
                + changes.files_removed.len(),
        );

        if reset {
            records.push(GraphChange::Reset);
        }
        for path in &changes.removed_node_keys {
            records.push(GraphChange::RemoveNode(path.clone()));
        }
        for &idx in &changes.upserted_nodes {
            if let Some(node) = self.graph.node_weight(idx) {
                records.push(GraphChange::UpsertNode(node.clone()));
            }
        }
        for &store_id in &changes.removed_edge_ids {
            records.push(GraphChange::RemoveEdge { store_id });
        }
        for &idx in &changes.upserted_edges {
            if let Some((from_idx, to_idx)) = self.graph.edge_endpoints(idx)
                && let Some(record) = self.graph.edge_weight(idx)
                && let Some(from_node) = self.graph.node_weight(from_idx)
                && let Some(to_node) = self.graph.node_weight(to_idx)
            {
                records.push(GraphChange::UpsertEdge {
                    store_id: record.store_id,
                    edge: GraphEdge {
                        from: from_node.path.clone(),
                        to: to_node.path.clone(),
                        form: record.link.form,
                        target: record.link.target.clone(),
                        line: record.link.line,
                        raw: record.link.raw.clone(),
                    },
                });
            }
        }

        for path in &changes.files_added {
            records.push(GraphChange::FileAdded(path.clone()));
        }
        for (path, folder) in &changes.files_removed {
            records.push(GraphChange::FileRemoved {
                path: path.clone(),
                folder: *folder,
            });
        }

        let completeness = match (reset, &self.health) {
            (true, _) => StoreCompleteness::Complete,
            (false, StoreHealth::Ready) => StoreCompleteness::Unchanged,
            // A store in another format is left untouched until a rebuild replaces it: stamping it with this build's
            // format would make it read as a valid graph holding only the notes written since. The edit stays in
            // this instance's in-memory graph.
            (false, StoreHealth::Incompatible) => return Ok(()),
            (false, StoreHealth::Empty | StoreHealth::Unreadable(_)) => StoreCompleteness::Incomplete,
        };
        self.store.persist(guard, &records, self.next_edge_id, completeness)?;
        // The write has committed, so a failure to read the new generation back must not report it as failed. The
        // last-seen generation is left behind instead: the next call's catch-up reads it again, re-applies this
        // instance's own changes idempotently, and reports a read failure there.
        if self.backend == GraphBackend::Sqlite {
            match self.store.current_generation() {
                Ok(Some(generation)) => self.last_seen_generation = generation,
                Ok(None) => {}
                Err(failure) => tracing::warn!(
                    code = failure.code(),
                    %failure,
                    "link graph generation could not be read back after a write; the next call catches up"
                ),
            }
        }
        Ok(())
    }

    /// Resolves `path` to its node index, upgrading an existing phantom or file node to a note in place, or creating a
    /// new note node. `unreadable` is why the note's links could not be read, when they could not. Records the node as
    /// touched in `changes` either way, so it is (re-)persisted.
    pub(super) fn ensure_real_node(
        &mut self,
        path: &str,
        title: &str,
        facts: NoteFacts<'_>,
        changes: &mut PendingChanges,
    ) -> NodeIndex {
        let NoteFacts {
            unreadable,
            base,
            stamp,
        } = facts;
        let idx = if let Some(&idx) = self.index.get(path) {
            if let Some(node) = self.graph.node_weight_mut(idx) {
                node.kind = GraphNodeKind::Note;
                node.candidates.clear();
                title.clone_into(&mut node.title);
                node.unreadable = unreadable.map(str::to_owned);
                node.base = base.map(str::to_owned);
                node.stamp = stamp;
            }
            idx
        } else {
            let idx = self.graph.add_node(GraphNode {
                path: path.to_owned(),
                title: title.to_owned(),
                kind: GraphNodeKind::Note,
                candidates: Vec::new(),
                unreadable: unreadable.map(str::to_owned),
                base: base.map(str::to_owned),
                stamp,
            });
            self.index.insert(path.to_owned(), idx);
            idx
        };
        changes.upserted_nodes.push(idx);
        idx
    }

    /// Removes every outgoing edge of `node_idx`, recording each removed
    /// edge's persisted identity in `changes`.
    pub(super) fn clear_outgoing(&mut self, node_idx: NodeIndex, changes: &mut PendingChanges) {
        let outgoing: Vec<EdgeIndex> = self
            .graph
            .edges_directed(node_idx, Direction::Outgoing)
            .map(|edge_ref| edge_ref.id())
            .collect();
        for edge_id in outgoing {
            self.drop_edge(edge_id, changes);
        }
    }

    /// Adds one outgoing edge per link, each resolved by [`Self::wire_link`]. Every new edge is assigned the next value
    /// from `self.next_edge_id`, a monotonic, store-persisted allocator: this is what lets two parallel edges (a note
    /// linking to the same target twice) each keep a distinct, stable identity across a persist/reopen round trip.
    fn wire_links(&mut self, node_idx: NodeIndex, links: &[ObsidianLink], changes: &mut PendingChanges) {
        for link in links {
            if link.target.is_empty() {
                continue;
            }
            self.wire_link(node_idx, EdgeLink::from(link), changes);
        }
    }
}
