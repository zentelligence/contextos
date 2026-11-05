//! Resolving links to graph nodes, and keeping every link resolved correctly as files come and go.
//!
//! Every link is resolved by `contextos-obsidian`'s resolver against the whole vault's files, so one rule decides
//! whether a link works wherever the question is asked. A file appearing or going can change how links in other notes
//! resolve (a nearer match, a name turning ambiguous, a missing target arriving), so the graph keeps each edge's target
//! as written and resolves again the edges a changed file could affect.

use std::collections::BTreeSet;

use std::sync::Arc;

use contextos_obsidian::{BaseRecords, DeclaredBase, LinkResolution, resolve_link, resolve_link_narrowed};
use petgraph::Direction;
use petgraph::stable_graph::{EdgeIndex, NodeIndex};
use petgraph::visit::EdgeRef;

use crate::SearchError;

use super::{EdgeLink, EdgeRecord, FileLister, GraphNode, GraphNodeKind, LinkGraph, PendingChanges, ReadsBaseFolders};

/// What a link resolved to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Destination {
    /// The one file the link reaches.
    Found(String),
    /// Nothing answers to the link.
    NotFound,
    /// Several files answer to it, none nearer than the others.
    Ambiguous(Vec<String>),
}

impl From<LinkResolution> for Destination {
    fn from(value: LinkResolution) -> Self {
        match value {
            LinkResolution::Found(path) => Self::Found(path),
            LinkResolution::NotFound => Self::NotFound,
            LinkResolution::Ambiguous { candidates } => Self::Ambiguous(candidates),
        }
    }
}

/// The key under which an edge is found when a file of that name appears or goes: the lower-cased last segment of its
/// target, or `index.md` for a target naming a folder.
pub(super) fn target_key(target: &str) -> String {
    if target.ends_with('/') {
        return "index.md".to_owned();
    }
    target.rsplit('/').next().unwrap_or(target).to_lowercase()
}

/// The keys a file at `path` can answer to: its lower-cased file name, and that name without a `.md` extension, which
/// is how a note is named in a link.
fn file_keys(path: &str) -> Vec<String> {
    let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    let stem = name.strip_suffix(".md").map(str::to_owned);
    std::iter::once(name).chain(stem).collect()
}

impl LinkGraph {
    /// Gives the graph the vault's files, which links resolve against, replacing any it held. Call it before the graph
    /// is first changed in a process: the graph is persisted without them, so a graph opened from its store knows only
    /// the notes written since. Resolves nothing again: the edges the store holds were resolved when they were written.
    pub fn seed_files(&mut self, files: impl IntoIterator<Item = String>) {
        self.files = files.into_iter().collect();
        self.files_listed = true;
        self.base_records.clear();
    }

    /// Whether the graph has been given a listing of the vault's files, by [`Self::seed_files`] or a rebuild.
    #[must_use]
    pub const fn files_listed(&self) -> bool {
        self.files_listed
    }

    /// Has the graph list the vault with `lister` now, and again whenever it must: after a full reload from a store
    /// another instance has rebuilt, since the store holds no listing. Call it when the graph opens, off any write, so
    /// the listing is taken before any change is applied.
    ///
    /// # Errors
    ///
    /// Returns the lister's error when the vault cannot be listed; the graph then lists it before its next change.
    pub fn list_files_with(&mut self, lister: FileLister) -> Result<(), SearchError> {
        self.lister = Some(lister);
        self.relist()
    }

    /// Has the graph read the folder of each base file a note declares with `reader`.
    pub fn read_bases_with(&mut self, reader: Arc<dyn ReadsBaseFolders>) {
        self.bases = Some(reader);
        self.base_records.clear();
    }

    /// Lists the vault again with the lister the graph was given, if it was given one.
    fn relist(&mut self) -> Result<(), SearchError> {
        let Some(list_vault) = self.lister.clone() else {
            return Ok(());
        };
        let listed = list_vault()?;
        self.seed_files(listed);
        Ok(())
    }

    /// Lists the vault if the graph has a lister and no listing: opened without one that worked, or reloaded.
    pub(super) fn ensure_listed(&mut self) -> Result<(), SearchError> {
        if self.lister.is_some() && !self.files_listed {
            self.relist()?;
        }
        Ok(())
    }

    /// Records that the file at `path` now exists: any file that is not a note the graph indexes (an image, a PDF, a
    /// canvas, a base, a note in an excluded folder) and a note written through [`Self::upsert_note`] alike. Links in
    /// other notes that this file changes the meaning of are resolved again, whether or not the graph's listing
    /// already held the file: a listing taken after the file arrived says nothing of what the links did before.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the updated graph cannot be persisted to its store.
    pub fn file_added(&mut self, path: &str) -> Result<(), SearchError> {
        self.apply_entries(&[super::FileEntry::File(path.to_owned())])
    }

    /// Records that the file at `path` now exists, and resolves again the links it can change.
    pub(super) fn add_file(&mut self, path: &str, changes: &mut PendingChanges) {
        self.files.insert(path);
        changes.files_added.push(path.to_owned());
        self.reresolve_for_file(path, changes);
    }

    /// Records that the file at `path`, or every file inside the folder `path`, no longer exists. Links that reached it
    /// are resolved again: to a nearer or another match when there is one, otherwise to a not-found stand-in.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the updated graph cannot be persisted to its store.
    pub fn file_removed(&mut self, path: &str) -> Result<(), SearchError> {
        self.remove_files(path, true)
    }

    /// Resolves `target`, written in the note at `note`, against the vault's files, a name several files answer to being
    /// settled by the base the note declares.
    pub(super) fn resolution_of(&mut self, note: &str, target: &str) -> Destination {
        let records = self.declared_records(note);
        match resolve_link_narrowed(&self.files, note, target, records.as_ref().map(BaseRecords::folder)) {
            Ok(resolution) => resolution.into(),
            Err(never) => match never {},
        }
    }

    /// The folder the base the note at `note` declares keeps its records in, when it declares one that resolves to a
    /// base file and confines its records to a folder.
    fn declared_records(&mut self, note: &str) -> Option<BaseRecords> {
        let target = self
            .index
            .get(note)
            .and_then(|&idx| self.graph.node_weight(idx))
            .and_then(|node| node.base.clone())?;
        let LinkResolution::Found(base) = resolve_link(&self.files, note, &target).unwrap_or(LinkResolution::NotFound)
        else {
            return None;
        };
        if !DeclaredBase::is_base_file(&base) {
            return None;
        }
        if let Some(known) = self.base_records.get(&base) {
            return known.clone();
        }
        let read = self.bases.as_ref().and_then(|reader| reader.records_folder(&base));
        self.base_records.insert(base, read.clone());
        read
    }

    /// The node a link with `target` and `destination` reaches, created when the graph does not hold it yet. A stand-in
    /// for an unresolved target is keyed by the target as written and shared by every link that fails the same way.
    pub(super) fn destination_node(
        &mut self,
        target: &str,
        destination: &Destination,
        changes: &mut PendingChanges,
    ) -> NodeIndex {
        match destination {
            Destination::Found(path) => self.ensure_file_node(path, changes),
            Destination::NotFound => self.ensure_unresolved_node(target, GraphNodeKind::NotFound, &[], changes),
            Destination::Ambiguous(candidates) => {
                self.ensure_unresolved_node(target, GraphNodeKind::Ambiguous, candidates, changes)
            }
        }
    }

    /// The node at `path`, a file a link reaches. A note stays a note. A stand-in left at the same key becomes a file
    /// node in place, so the edges that already point at it keep pointing at it.
    fn ensure_file_node(&mut self, path: &str, changes: &mut PendingChanges) -> NodeIndex {
        if let Some(&idx) = self.index.get(path) {
            if let Some(node) = self.graph.node_weight_mut(idx)
                && node.phantom()
            {
                node.kind = GraphNodeKind::File;
                node.candidates.clear();
                changes.upserted_nodes.push(idx);
            }
            return idx;
        }
        let idx = self.graph.add_node(GraphNode::stand_in(path, GraphNodeKind::File));
        self.index.insert(path.to_owned(), idx);
        changes.upserted_nodes.push(idx);
        idx
    }

    /// The stand-in for an unresolved `target`, created or brought up to date with what is now known of it.
    fn ensure_unresolved_node(
        &mut self,
        target: &str,
        kind: GraphNodeKind,
        candidates: &[String],
        changes: &mut PendingChanges,
    ) -> NodeIndex {
        if let Some(&idx) = self.index.get(target) {
            if let Some(node) = self.graph.node_weight_mut(idx)
                && node.phantom()
                && (node.kind != kind || node.candidates != candidates)
            {
                node.kind = kind;
                node.candidates = candidates.to_vec();
                changes.upserted_nodes.push(idx);
            }
            return idx;
        }
        let mut node = GraphNode::stand_in(target, kind);
        node.candidates = candidates.to_vec();
        let idx = self.graph.add_node(node);
        self.index.insert(target.to_owned(), idx);
        changes.upserted_nodes.push(idx);
        idx
    }

    /// Adds an edge for `link` from `from` to `to`, with the next persisted identity.
    pub(super) fn add_edge(&mut self, from: NodeIndex, to: NodeIndex, link: EdgeLink, changes: &mut PendingChanges) {
        let store_id = self.next_edge_id;
        self.next_edge_id = self.next_edge_id.saturating_add(1);
        self.edges_by_name
            .entry(target_key(&link.target))
            .or_default()
            .insert(store_id);
        let idx = self.graph.add_edge(from, to, EdgeRecord { store_id, link });
        self.edge_by_store_id.insert(store_id, idx);
        changes.upserted_edges.push(idx);
    }

    /// Removes the edge at `idx`, returning the link it held. The node it reached is noted as possibly unreferenced.
    pub(super) fn drop_edge(&mut self, idx: EdgeIndex, changes: &mut PendingChanges) -> Option<EdgeLink> {
        let (_, to) = self.graph.edge_endpoints(idx)?;
        let record = self.graph.remove_edge(idx)?;
        self.forget_edge(&record);
        changes.removed_edge_ids.push(record.store_id);
        changes.unreferenced.push(to);
        Some(record.link)
    }

    /// Drops the lookups that find `record`'s edge.
    pub(super) fn forget_edge(&mut self, record: &EdgeRecord) {
        self.edge_by_store_id.remove(&record.store_id);
        let key = target_key(&record.link.target);
        if let Some(ids) = self.edges_by_name.get_mut(&key) {
            ids.remove(&record.store_id);
            if ids.is_empty() {
                self.edges_by_name.remove(&key);
            }
        }
    }

    /// Builds the by-name lookup from the edges the graph holds.
    pub(super) fn index_edges_by_name(&mut self) {
        let mut edges_by_name: std::collections::HashMap<String, BTreeSet<u64>> = std::collections::HashMap::new();
        for idx in self.graph.edge_indices() {
            if let Some(record) = self.graph.edge_weight(idx) {
                edges_by_name
                    .entry(target_key(&record.link.target))
                    .or_default()
                    .insert(record.store_id);
            }
        }
        self.edges_by_name = edges_by_name;
    }

    /// Resolves `link`, written in the note at `from`, and adds the edge it makes.
    pub(super) fn wire_link(&mut self, from: NodeIndex, link: EdgeLink, changes: &mut PendingChanges) {
        let Some(note) = self.graph.node_weight(from).map(|node| node.path.clone()) else {
            return;
        };
        let destination = self.resolution_of(&note, &link.target);
        let to = self.destination_node(&link.target, &destination, changes);
        self.add_edge(from, to, link, changes);
    }

    /// The edges whose target names a file name that a file at `path` can answer to.
    pub(super) fn edges_naming(&self, path: &str) -> BTreeSet<u64> {
        file_keys(path)
            .iter()
            .filter_map(|key| self.edges_by_name.get(key))
            .flatten()
            .copied()
            .collect()
    }

    /// Resolves again the links a file appearing at `path` can change. A base file changing can change how every note
    /// declaring a base settles a name, so it brings those notes' links with it.
    fn reresolve_for_file(&mut self, path: &str, changes: &mut PendingChanges) {
        let mut ids = self.edges_naming(path);
        if DeclaredBase::is_base_file(path) {
            self.base_records.remove(path);
            ids.extend(self.edges_of_notes_declaring_a_base());
        }
        self.reresolve(&ids, changes);
    }

    /// The edges written in notes that declare a base.
    fn edges_of_notes_declaring_a_base(&self) -> BTreeSet<u64> {
        self.graph
            .node_indices()
            .filter(|idx| self.graph.node_weight(*idx).is_some_and(|node| node.base.is_some()))
            .flat_map(|idx| self.graph.edges_directed(idx, Direction::Outgoing))
            .map(|edge| edge.weight().store_id)
            .collect()
    }

    /// Resolves the edges `ids` again and moves each one whose answer has changed to the node it now reaches. Stand-ins
    /// and file nodes left unreferenced go.
    pub(super) fn reresolve(&mut self, ids: &BTreeSet<u64>, changes: &mut PendingChanges) {
        for id in ids {
            let Some(&idx) = self.edge_by_store_id.get(id) else {
                continue;
            };
            let Some((from, to)) = self.graph.edge_endpoints(idx) else {
                continue;
            };
            let Some(record) = self.graph.edge_weight(idx).filter(|record| record.store_id == *id) else {
                continue;
            };
            let link = record.link.clone();
            let Some(note) = self.graph.node_weight(from).map(|node| node.path.clone()) else {
                continue;
            };
            let destination = self.resolution_of(&note, &link.target);
            if self.reaches(to, &link.target, &destination) {
                continue;
            }
            self.drop_edge(idx, changes);
            let reached = self.destination_node(&link.target, &destination, changes);
            self.add_edge(from, reached, link, changes);
        }
        self.prune_unreferenced(changes);
    }

    /// Whether the node `to` is already what `destination` says a link with `target` reaches.
    fn reaches(&self, to: NodeIndex, target: &str, destination: &Destination) -> bool {
        let Some(node) = self.graph.node_weight(to) else {
            return false;
        };
        match destination {
            Destination::Found(path) => {
                node.path == *path && matches!(node.kind, GraphNodeKind::Note | GraphNodeKind::File)
            }
            Destination::NotFound => node.path == target && node.kind == GraphNodeKind::NotFound,
            Destination::Ambiguous(candidates) => {
                node.path == target && node.kind == GraphNodeKind::Ambiguous && node.candidates == *candidates
            }
        }
    }

    /// Removes the file nodes and stand-ins that no link reaches any more.
    pub(super) fn prune_unreferenced(&mut self, changes: &mut PendingChanges) {
        for idx in std::mem::take(&mut changes.unreferenced) {
            let unreferenced = self
                .graph
                .node_weight(idx)
                .is_some_and(|node| node.kind != GraphNodeKind::Note)
                && self.graph.edges_directed(idx, Direction::Incoming).next().is_none();
            if unreferenced && let Some(node) = self.graph.remove_node(idx) {
                self.index.remove(&node.path);
                changes.removed_node_keys.push(node.path);
            }
        }
    }

    /// Removes the file at `path` and, when `with_children`, every file inside the folder `path`: their nodes and
    /// outgoing links go, and the links that reached them are resolved again. The files removed are those the listing
    /// holds and the real nodes the graph holds, so a listing taken after the removal does not hide any.
    pub(super) fn remove_files(&mut self, path: &str, with_children: bool) -> Result<(), SearchError> {
        self.apply_entries(&[super::FileEntry::Removed {
            path: path.to_owned(),
            folder: with_children,
        }])
    }

    /// Removes the file at `path` and, when `with_children`, every file inside the folder `path`, as
    /// [`Self::remove_files`] says, recording what changed in `changes`.
    pub(super) fn remove_inner(&mut self, path: &str, with_children: bool, changes: &mut PendingChanges) {
        let mut gone = BTreeSet::new();
        if self.files.remove(path) || self.is_real_node(path) {
            gone.insert(path.to_owned());
        }
        if with_children {
            gone.extend(self.files.remove_under(path));
            let prefix = format!("{}/", path.trim_end_matches('/'));
            let nodes: Vec<String> = self
                .index
                .iter()
                .filter(|(key, _)| key.starts_with(&prefix))
                .filter(|(_, idx)| self.graph.node_weight(**idx).is_some_and(|node| !node.phantom()))
                .map(|(key, _)| key.clone())
                .collect();
            for node in nodes {
                self.files.remove(&node);
                gone.insert(node);
            }
        }
        changes.files_removed.push((path.to_owned(), with_children));
        for removed in &gone {
            if let Some(&idx) = self.index.get(removed) {
                self.clear_outgoing(idx, changes);
            }
        }
        let mut detached = Vec::new();
        let mut base_gone = false;
        for removed in &gone {
            if DeclaredBase::is_base_file(removed) {
                self.base_records.remove(removed);
                base_gone = true;
            }
            if !self.is_real_node(removed) {
                continue;
            }
            let Some(idx) = self.index.remove(removed) else {
                continue;
            };
            let incoming: Vec<EdgeIndex> = self
                .graph
                .edges_directed(idx, Direction::Incoming)
                .map(|edge| edge.id())
                .collect();
            for edge in incoming {
                let from = self.graph.edge_endpoints(edge).map(|(from, _)| from);
                if let (Some(from), Some(link)) = (from, self.drop_edge(edge, changes)) {
                    detached.push((from, link));
                }
            }
            self.graph.remove_node(idx);
            changes.removed_node_keys.push(removed.clone());
        }
        let mut affected: BTreeSet<u64> = gone.iter().flat_map(|removed| self.edges_naming(removed)).collect();
        if base_gone {
            affected.extend(self.edges_of_notes_declaring_a_base());
        }
        self.reresolve(&affected, changes);
        for (from, link) in detached {
            self.wire_link(from, link, changes);
        }
        self.prune_unreferenced(changes);
    }

    /// What the graph holds, for a pass that compares it with the vault: the files it believes the vault holds, and the
    /// notes it holds with the stamp each had when read.
    ///
    /// # Errors
    ///
    /// Returns the failure that keeps the store from being read.
    pub fn knowledge(&mut self) -> Result<super::GraphKnowledge, SearchError> {
        self.catch_up()?;
        let mut knowledge = super::GraphKnowledge::default();
        for node in self.graph.node_weights().filter(|node| !node.phantom()) {
            knowledge.nodes.insert(node.path.clone());
            if node.kind == GraphNodeKind::Note {
                knowledge.notes.insert(node.path.clone(), node.stamp);
            }
        }
        knowledge.files = self.files.paths().cloned().collect();
        Ok(knowledge)
    }

    /// Whether the graph holds a note or a linked file at `path`, and not a stand-in for an unresolved target.
    fn is_real_node(&self, path: &str) -> bool {
        self.index
            .get(path)
            .and_then(|&idx| self.graph.node_weight(idx))
            .is_some_and(|node| !node.phantom())
    }
}
