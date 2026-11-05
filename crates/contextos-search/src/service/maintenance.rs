//! Private reconciliation helpers for [`VaultSearchService`]: link-graph
//! synchronisation from operation events, semantic-queue signalling and
//! draining, and the full-scan rebuild passes behind the public
//! `rebuild`/`status` surface.
//!
//! [`VaultSearchService`]: super::VaultSearchService

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use contextos_core::{OpKind, OperationEvent, OperationWarning, VaultPath, VaultPathInput, VaultSet};
use contextos_obsidian::{DeclaredBase, LinkCollection};
use time::OffsetDateTime;

use crate::Chunk;
use crate::sync::SourceStamp;
use crate::text::IndexEntry;
use crate::{
    ChunkSource, DocumentSource, FileEntry, FreshnessReport, IndexedDocument, LinkGraph, NoteEntry, NoteLinks,
    NoteStamp, PathEmbeddingOutcome, ReadsChunkSource, SearchError, UnreadableNote, VaultScan, chunk_document,
};

use super::report::{GraphRebuildReport, RebuildProgress, SemanticRebuildReport};
use super::scan::{is_excluded, is_markdown, resolve_single_root_set, walk_files, walk_markdown};
use super::{SemanticWorker, VaultSearchService};

impl VaultSearchService {
    /// Applies a write to the link graph when it is enabled. A graph that cannot be opened is reported as the write's
    /// warning and never recovered here: recovery rebuilds the graph, which must not hold up a vault write. A graph
    /// the write finds damaged is released, so it can be recovered.
    ///
    /// Every file the write touches reaches the graph, not only the notes it indexes: a link resolves against every
    /// file in the vault, so an image or a canvas arriving, going, or moving can change what links mean. A file the
    /// vault hides is no part of the graph.
    pub(super) fn update_graph(&self, event: &OperationEvent) -> Result<(), OperationWarning> {
        let Some(slot) = &self.graph else {
            return Ok(());
        };
        let graph = slot.get().map_err(OperationWarning::from)?;
        let graph = graph.as_ref();
        match event.kind {
            OpKind::Delete => {
                for path in &event.paths {
                    if path.root_id() == self.root_id {
                        self.remove_graph_path(graph, path)?;
                    }
                }
            }
            OpKind::Move => {
                if let Some(from) = event.paths.first()
                    && from.root_id() == self.root_id
                {
                    self.remove_graph_path(graph, from)?;
                }
                if let Some(to) = event.paths.get(1)
                    && to.root_id() == self.root_id
                {
                    self.sync_graph_path(graph, to)?;
                }
            }
            OpKind::Create | OpKind::Modify | OpKind::Restore => {
                for path in &event.paths {
                    if path.root_id() == self.root_id {
                        self.sync_graph_path(graph, path)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Whether the vault hides `relative`, whether it names a file or a folder.
    pub(super) fn hides(&self, relative: &str) -> bool {
        self.hidden.hides(relative, false) || self.hidden.hides(relative, true)
    }

    /// Removes the file or folder at `path` from the graph.
    fn remove_graph_path(&self, graph: &Mutex<LinkGraph>, path: &VaultPath) -> Result<(), OperationWarning> {
        let relative = path.portable_relative();
        if self.hides(&relative) {
            return Ok(());
        }
        Self::locked(graph)
            .file_removed(&relative)
            .map_err(|error| self.graph_warning(&error))
    }

    /// The warning a failed graph write reports, releasing the graph first when the failure is damage.
    fn graph_warning(&self, error: &SearchError) -> OperationWarning {
        if let Some(slot) = &self.graph {
            slot.note(error);
        }
        OperationWarning::from(error)
    }

    /// Brings the graph up to date with whatever is now at `path`: a note is read and its links recorded, any other file
    /// is recorded as existing, a folder is taken file by file in one change, and a path that is gone is removed.
    ///
    /// A file that cannot be recorded (a name the vault rejects, content that cannot be read) is skipped and reported,
    /// and the rest of its folder is recorded all the same; the first such failure is the warning.
    fn sync_graph_path(&self, graph: &Mutex<LinkGraph>, path: &VaultPath) -> Result<(), OperationWarning> {
        let relative = path.portable_relative();
        if self.hides(&relative) {
            return Ok(());
        }
        let absolute = self.root.join(&relative);
        let Ok(metadata) = fs::metadata(&absolute) else {
            return Self::locked(graph)
                .file_removed(&relative)
                .map_err(|error| self.graph_warning(&error));
        };
        if !metadata.is_dir() && !metadata.is_file() {
            return Self::locked(graph)
                .file_removed(&relative)
                .map_err(|error| self.graph_warning(&error));
        }
        let mut files = Vec::new();
        if metadata.is_dir() {
            walk_files(&self.root, &absolute, &self.hidden, &mut files).map_err(|error| self.graph_warning(&error))?;
        } else {
            files.push(relative);
        }
        let roots = resolve_single_root_set(&self.root).map_err(|error| self.graph_warning(&error))?;
        let mut entries = Vec::with_capacity(files.len());
        let mut failures = Vec::new();
        for file in &files {
            match self.graph_entry(&roots, file) {
                Ok(Some(entry)) => entries.push(entry),
                Ok(None) => {}
                Err(error) => failures.push(error),
            }
        }
        if !failures.is_empty() {
            tracing::warn!(
                skipped = failures.len(),
                recorded = entries.len(),
                "files that could not be recorded in the link graph were skipped"
            );
        }
        Self::locked(graph)
            .apply_entries(&entries)
            .map_err(|error| self.graph_warning(&error))?;
        failures.first().map_or(Ok(()), |error| Err(self.graph_warning(error)))
    }

    /// What the graph records of the regular file at `relative`: a note read for its links, or another file as present.
    /// `None` when the file vanished before it could be read.
    fn graph_entry(&self, roots: &VaultSet, relative: &str) -> Result<Option<FileEntry>, SearchError> {
        if !self.in_scope(relative) {
            return Ok(Some(FileEntry::File(relative.to_owned())));
        }
        Self::reread_note(roots, relative, &self.root.join(relative))
    }

    pub(super) fn in_scope(&self, relative: &str) -> bool {
        is_markdown(relative) && !is_excluded(&self.excludes, relative)
    }

    /// Signals every in-scope path touched by `event` to the semantic
    /// embedding queue, unconditionally of `event.kind`: unlike the link
    /// graph, the embedding worker itself decides add versus remove by
    /// checking whether the path still exists when it processes the signal
    /// (`EmbeddingWorker::process_path`), so no per-`OpKind` branching is
    /// needed here. Enqueuing is infallible and never blocks or fails the
    /// write pipeline; actual embedding happens later, off this call.
    pub(super) fn enqueue_semantic(&self, event: &OperationEvent) {
        let Some(semantic) = &self.semantic else {
            return;
        };
        for path in &event.paths {
            if path.root_id() == self.root_id {
                let relative = path.portable_relative();
                if self.in_scope(&relative) {
                    semantic.enqueue(relative);
                }
            }
        }
    }

    /// Re-reads and re-chunks `path`'s current content: the vector store
    /// holds only vectors and identifying metadata, never chunk text (see the
    /// `vector_store` module docs), so the prose has to be recovered from the
    /// source. Returns `None` when the path no longer exists.
    ///
    /// The path is validated against the vault root before anything is read,
    /// so a note swapped for a symlink that leaves the vault is rejected
    /// rather than followed.
    pub(super) fn rechunk(&self, semantic: &SemanticWorker, path: &str) -> Result<Option<Vec<Chunk>>, SearchError> {
        let roots = resolve_single_root_set(&self.root)?;
        let vault_path = VaultPath::try_from(VaultPathInput {
            roots: &roots,
            raw: path,
        })
        .map_err(|source| SearchError::EmbeddingPathInvalid {
            path: path.to_owned(),
            source,
        })?;
        let Some(content) = semantic.content().read(path)? else {
            return Ok(None);
        };
        Ok(Some(chunk_document(ChunkSource {
            path: &vault_path,
            content: &content,
        })))
    }

    /// Recovers the prose for the chunk at `ordinal`. Returns `None` when the
    /// path no longer exists or no longer produces a chunk at `ordinal` (the
    /// document changed since it was last embedded);
    /// [`VaultSearchService::query_semantic`] skips such a hit rather than
    /// returning stale or missing text.
    ///
    /// [`VaultSearchService::query_semantic`]: super::VaultSearchService::query_semantic
    pub(super) fn reread_chunk_text(
        &self,
        semantic: &SemanticWorker,
        path: &str,
        ordinal: usize,
    ) -> Result<Option<String>, SearchError> {
        Ok(self.rechunk(semantic, path)?.and_then(|chunks| {
            chunks
                .into_iter()
                .find(|chunk| chunk.ordinal() == ordinal)
                .map(|chunk| chunk.text().to_owned())
        }))
    }

    pub(super) fn text_stale_estimate(&self, entries: &[IndexEntry]) -> Result<usize, SearchError> {
        let stored: BTreeMap<&str, OffsetDateTime> = entries
            .iter()
            .map(|entry| (entry.path.as_str(), entry.modified))
            .collect();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut stale = 0_usize;

        for (relative, absolute) in self.collect_markdown_files()? {
            seen.insert(relative.clone());
            let Ok(metadata) = fs::metadata(&absolute) else {
                continue;
            };
            let Ok(modified) = metadata.modified().map(OffsetDateTime::from) else {
                continue;
            };
            match stored.get(relative.as_str()) {
                Some(stored_modified) if *stored_modified == modified => {}
                _ => stale = stale.saturating_add(1),
            }
        }
        for path in stored.keys() {
            if !seen.contains(*path) {
                stale = stale.saturating_add(1);
            }
        }
        Ok(stale)
    }

    fn rebuild_text(&self) -> Result<FreshnessReport, SearchError> {
        self.with_text(super::stores::TextStore::refresh)
    }

    pub(super) fn rebuild_text_reporting(
        &self,
        on_progress: &mut dyn FnMut(RebuildProgress),
    ) -> Result<FreshnessReport, SearchError> {
        on_progress(RebuildProgress::TextStarted);
        let report = self.rebuild_text()?;
        on_progress(RebuildProgress::TextComplete);
        Ok(report)
    }

    fn rebuild_graph(&self) -> Result<GraphRebuildReport, SearchError> {
        let graph = self.graph_store()?;
        let scanned = self.scan_graph_notes(&graph)?;
        self.commit_graph_rebuild(&graph, scanned).inspect_err(|error| {
            if let Some(slot) = &self.graph {
                slot.note(error);
            }
        })
    }

    /// Reads every in-scope note's title and links, with the stamp each file had when read, and lists every file in the
    /// vault, without touching the graph, noting the graph store's generation first so the commit can tell which notes
    /// were written since.
    pub(super) fn scan_graph_notes(&self, graph: &Mutex<LinkGraph>) -> Result<ScannedNotes, SearchError> {
        let generation = Self::locked(graph).store_generation();
        let roots = resolve_single_root_set(&self.root)?;
        let mut notes = BTreeMap::new();
        for (relative, absolute) in self.collect_markdown_files()? {
            if self.hides(&relative) {
                continue;
            }
            match read_graph_note(&roots, &relative, &absolute) {
                Ok(note) => {
                    notes.insert(relative, note);
                }
                // Removed between the walk and the read: it is no longer in the vault.
                Err(_) if SourceStamp::of(&absolute).is_none() => {}
                Err(error) => return Err(error),
            }
        }
        let mut files = Vec::new();
        walk_files(&self.root, &self.root, &self.hidden, &mut files)?;
        Ok(ScannedNotes {
            generation,
            notes,
            files: files.into_iter().collect(),
        })
    }

    /// Replaces the graph with the vault's notes in one write, re-checking `scanned` while holding the store's write
    /// lock. Every other writer of the graph writes the note first and takes the lock to record it after, so the graph
    /// committed here is never older than anything already recorded, and anything written later is recorded after it.
    /// Where the store can say which notes were written or removed since the scan, only those are read again, keeping
    /// the lock briefly; otherwise the vault is walked again, a note whose file is unchanged since the scan keeping
    /// what the scan read and one written, created, or deleted since being read again or dropped.
    pub(super) fn commit_graph_rebuild(
        &self,
        graph: &Mutex<LinkGraph>,
        scanned: ScannedNotes,
    ) -> Result<GraphRebuildReport, SearchError> {
        let roots = resolve_single_root_set(&self.root)?;
        let ScannedNotes {
            generation,
            notes,
            files,
        } = scanned;
        let mut scanned = Some((notes, files));
        let mut notes_scanned = 0;
        let mut locked = Self::locked(graph);
        locked.rebuild_from(generation, &mut |changed| {
            let (scanned, files) = scanned.take().unwrap_or_default();
            let scan = match changed {
                Some(changed) => self.recheck_changed(&roots, scanned, files, changed)?,
                None => self.recheck_all(&roots, scanned)?,
            };
            notes_scanned = scan.notes.len().saturating_add(scan.unreadable.len());
            Ok(scan)
        })?;
        let view = locked.full_view()?;
        Ok(GraphRebuildReport {
            notes_scanned,
            nodes: view.nodes.len(),
            edges: view.edges.len(),
        })
    }

    /// The scan with every note in `changed` read again, or dropped when it is gone or out of scope, and every changed
    /// path kept in or dropped from the file listing by whether it is a file now. A changed path that names a folder
    /// stands for everything inside it: what the scan held there is dropped, and what the folder holds now is taken.
    ///
    /// The changed set names graph nodes, and a stand-in for an unresolved link is keyed by what the link said, which
    /// may be anything: `../..`, `./x.md`, `sub/`. Only a path that the vault accepts and that is already written in
    /// its normal form is looked for on disk; any other names no file.
    pub(super) fn recheck_changed(
        &self,
        roots: &VaultSet,
        mut scanned: BTreeMap<String, ScannedNote>,
        mut files: BTreeSet<String>,
        changed: &BTreeSet<String>,
    ) -> Result<VaultScan, SearchError> {
        let mut pending: Vec<String> = changed.iter().cloned().collect();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        while let Some(relative) = pending.pop() {
            if !seen.insert(relative.clone()) {
                continue;
            }
            let folder = format!("{relative}/");
            scanned.retain(|path, _| path != &relative && !path.starts_with(&folder));
            files.retain(|path| path != &relative && !path.starts_with(&folder));
            let normal = VaultPath::try_from(VaultPathInput { roots, raw: &relative })
                .ok()
                .map(|path| path.portable_relative())
                .filter(|normal| *normal == relative);
            if normal.is_none() || self.hides(&relative) {
                continue;
            }
            let absolute = self.root.join(&relative);
            if fs::symlink_metadata(&absolute).is_ok_and(|metadata| metadata.is_dir()) {
                let mut inside = Vec::new();
                walk_files(&self.root, &absolute, &self.hidden, &mut inside)?;
                pending.extend(inside);
                continue;
            }
            if SourceStamp::of(&absolute).is_none() {
                continue;
            }
            files.insert(relative.clone());
            if !self.in_scope(&relative) {
                continue;
            }
            match read_graph_note(roots, &relative, &absolute) {
                Ok(note) => {
                    scanned.insert(relative.clone(), note);
                }
                Err(_) if SourceStamp::of(&absolute).is_none() => {
                    files.remove(&relative);
                }
                Err(error) => return Err(error),
            }
        }
        Ok(ScannedNote::into_scan(
            scanned.into_values(),
            files.into_iter().collect(),
        ))
    }

    /// The vault walked again: a note whose file is unchanged since the scan keeps what the scan read, and one
    /// written, created, or deleted since is read again or dropped.
    fn recheck_all(
        &self,
        roots: &VaultSet,
        mut scanned: BTreeMap<String, ScannedNote>,
    ) -> Result<VaultScan, SearchError> {
        let mut notes = Vec::new();
        for (relative, absolute) in self.collect_markdown_files()? {
            if self.hides(&relative) {
                continue;
            }
            let earlier = scanned.remove(&relative);
            let current = SourceStamp::of(&absolute);
            let note = match earlier {
                Some(earlier) if current == Some(earlier.stamp) => earlier,
                _ => match read_graph_note(roots, &relative, &absolute) {
                    Ok(note) => note,
                    // Removed between the walk and the read: it is no longer in the vault.
                    Err(_) if SourceStamp::of(&absolute).is_none() => continue,
                    Err(error) => return Err(error),
                },
            };
            notes.push(note);
        }
        let mut files = Vec::new();
        walk_files(&self.root, &self.root, &self.hidden, &mut files)?;
        Ok(ScannedNote::into_scan(notes, files))
    }

    pub(super) fn rebuild_graph_reporting(
        &self,
        on_progress: &mut dyn FnMut(RebuildProgress),
    ) -> Result<GraphRebuildReport, SearchError> {
        on_progress(RebuildProgress::GraphStarted);
        let report = self.rebuild_graph()?;
        on_progress(RebuildProgress::GraphComplete);
        Ok(report)
    }

    /// Enqueues every in-scope markdown file not already queued, then
    /// drains the queue synchronously. Re-embedding is still gated on
    /// content hash change only, so an unchanged chunk is skipped even
    /// during a rebuild rather than forced
    /// through the embedding provider again; this mirrors
    /// [`EmbeddingWorker`]'s existing hash-skip behaviour rather than
    /// introducing a second, forced-reembed code path.
    ///
    /// The vault walk and enqueue only happen when the queue is currently
    /// empty. `EmbeddingWorker::enqueue` dedupes against paths still
    /// queued, but not against paths a previous, budget-truncated call
    /// already dequeued and processed (those are gone from its queue
    /// entirely); unconditionally re-walking on every call would silently
    /// revive every already-completed path, making the reported `remaining`
    /// oscillate around the vault's total file count instead of trending
    /// to zero across repeated calls with the same target. Skipping the
    /// reseed while work is still queued costs nothing: any file changed
    /// mid-pass is already caught by the live write-pipeline's own
    /// `enqueue` calls, independent of this walk.
    ///
    /// [`EmbeddingWorker`]: crate::EmbeddingWorker
    pub(super) fn rebuild_semantic(
        &self,
        budget: Option<time::Duration>,
        on_progress: &mut dyn FnMut(RebuildProgress),
    ) -> Result<SemanticRebuildReport, SearchError> {
        let semantic = self.semantic.as_ref().ok_or(SearchError::SemanticUnavailable)?;
        if semantic.status().pending == 0 {
            for (relative, _absolute) in self.collect_markdown_files()? {
                semantic.enqueue(relative);
            }
        }
        self.drain_semantic_outcomes(budget, on_progress)
    }

    /// Drains only whatever is currently queued for this vault's semantic
    /// index (paths enqueued through the live write pipeline,
    /// [`VaultSearchService::update`], or a prior call), without ever
    /// performing [`Self::rebuild_semantic`]'s "queue empty -> walk the
    /// whole vault" catch-up scan.
    ///
    /// Suited to a caller that runs on a short, frequent interval (a
    /// background maintenance task) rather than an occasional explicit
    /// operator action: repeatedly re-walking and re-hashing every file in
    /// the vault on every idle tick is a real, serious cost this avoids
    /// entirely. A caller that also needs to discover content
    /// never routed through the live write pipeline (edited outside this
    /// server, for example directly in Obsidian) still needs an
    /// occasional [`Self::rebuild`]/[`Self::rebuild_with_budget`] call to
    /// find it.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::SemanticUnavailable`] when semantic search
    /// is disabled for this vault, and a storage error when the vector
    /// store or embedding provider fails.
    pub fn drain_semantic_queue(&self, budget: Option<time::Duration>) -> Result<SemanticRebuildReport, SearchError> {
        self.drain_semantic_outcomes(budget, &mut |_| {})
    }

    /// Shared by [`Self::rebuild_semantic`] (which may first enqueue the
    /// whole vault) and [`Self::drain_semantic_queue`] (which never does):
    /// drains whatever is queued right now and aggregates the outcomes.
    fn drain_semantic_outcomes(
        &self,
        budget: Option<time::Duration>,
        on_progress: &mut dyn FnMut(RebuildProgress),
    ) -> Result<SemanticRebuildReport, SearchError> {
        let semantic = self.semantic_store()?;
        let total = semantic.status().pending;

        let mut report = SemanticRebuildReport::default();
        let outcomes = match budget {
            Some(budget) => semantic.drain_until(budget),
            None => semantic.drain(),
        };
        for outcome in outcomes {
            report.paths_scanned = report.paths_scanned.saturating_add(1);
            on_progress(RebuildProgress::SemanticProgress {
                completed: report.paths_scanned,
                total,
            });
            match outcome {
                PathEmbeddingOutcome::Embedded { embedded, skipped, .. } => {
                    report.embedded = report.embedded.saturating_add(embedded);
                    report.skipped = report.skipped.saturating_add(skipped);
                }
                PathEmbeddingOutcome::Removed { .. } | PathEmbeddingOutcome::Superseded { .. } => {}
                PathEmbeddingOutcome::Failed { .. } => {
                    report.failed = report.failed.saturating_add(1);
                }
            }
        }
        report.remaining = semantic.status().pending;
        Ok(report)
    }

    pub(super) fn collect_markdown_files(&self) -> Result<Vec<(String, PathBuf)>, SearchError> {
        let mut files = Vec::new();
        walk_markdown(&self.root, &self.root, &self.excludes, &mut files)?;
        files.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(files)
    }
}

/// Every note a graph rebuild read, by forward-slash relative path, every file in the vault, and the graph store's
/// generation just before it began reading them.
#[derive(Default)]
pub(super) struct ScannedNotes {
    generation: Option<u64>,
    notes: BTreeMap<String, ScannedNote>,
    files: BTreeSet<String>,
}

/// One note as a graph rebuild read it: the stamp its file had when read, then what was read.
pub(super) struct ScannedNote {
    stamp: SourceStamp,
    read: NoteRead,
    base: Option<String>,
}

/// What reading a note for the graph found.
enum NoteRead {
    /// Its path, title, and outgoing links.
    Links(NoteLinks),
    /// Its links could not be read.
    Unreadable(UnreadableNote),
}

impl ScannedNote {
    /// The scan a rebuild takes, from the notes read and every file in the vault.
    fn into_scan(notes: impl IntoIterator<Item = Self>, files: Vec<String>) -> VaultScan {
        let mut scan = VaultScan {
            files,
            ..VaultScan::default()
        };
        for note in notes {
            let path = match &note.read {
                NoteRead::Links((path, ..)) | NoteRead::Unreadable(UnreadableNote { path, .. }) => path.clone(),
            };
            if let Some(base) = note.base {
                scan.bases.insert(path.clone(), base);
            }
            scan.stamps.insert(path, NoteStamp::from(note.stamp));
            match note.read {
                NoteRead::Links(links) => scan.notes.push(links),
                NoteRead::Unreadable(unreadable) => scan.unreadable.push(unreadable),
            }
        }
        scan
    }
}

impl From<ScannedNote> for FileEntry {
    fn from(note: ScannedNote) -> Self {
        let stamp = Some(NoteStamp::from(note.stamp));
        let (path, title, links) = match note.read {
            NoteRead::Links((path, title, links)) => (path, title, Ok(links)),
            NoteRead::Unreadable(UnreadableNote { path, title, reason }) => (path, title, Err(reason)),
        };
        Self::Note(NoteEntry {
            path,
            title,
            links,
            base: note.base,
            stamp,
        })
    }
}

impl From<SourceStamp> for NoteStamp {
    fn from(stamp: SourceStamp) -> Self {
        let modified = OffsetDateTime::from(stamp.modified);
        Self {
            modified_seconds: modified.unix_timestamp(),
            modified_nanoseconds: modified.nanosecond(),
            length: stamp.len,
        }
    }
}

/// The links of the note at `relative` that go in the graph, or why they cannot be read. A managed `index.md`'s table
/// wikilinks every entry in its own directory mechanically; treating those as real graph edges would give every file in
/// a managed vault a permanent inbound edge from its own directory listing, making orphan detection vacuous and
/// neighbour/backlink views noisy with auto-generated table-of-contents entries rather than genuine authored
/// references.
fn graph_links(relative: &str, content: &str) -> Result<Vec<contextos_obsidian::ObsidianLink>, String> {
    if relative == "index.md" || relative.ends_with("/index.md") {
        return Ok(Vec::new());
    }
    LinkCollection::try_from(content)
        .map(|collection| collection.outgoing().to_vec())
        .map_err(|error| error.to_string())
}

/// Reads the note at `absolute` for the link graph: its title and outgoing links, with the file's stamp taken before
/// it was read, so a write that lands during the read leaves the stamp behind the content and is caught at commit. A
/// note whose links cannot be parsed is read as unreadable, with the reason, and is never left out.
pub(super) fn read_graph_note(roots: &VaultSet, relative: &str, absolute: &Path) -> Result<ScannedNote, SearchError> {
    let read_failed = |source| SearchError::DocumentRead {
        path: relative.to_owned(),
        source,
    };
    let stamp =
        SourceStamp::of(absolute).ok_or_else(|| read_failed(std::io::Error::from(std::io::ErrorKind::NotFound)))?;
    let content = contextos_fs::read_regular_text(absolute).map_err(read_failed)?;
    let path = VaultPath::try_from(VaultPathInput { roots, raw: relative }).map_err(|source| {
        SearchError::GraphPathInvalid {
            path: relative.to_owned(),
            source,
        }
    })?;
    let title = IndexedDocument::from(DocumentSource {
        path: &path,
        content: &content,
        modified: OffsetDateTime::from(stamp.modified),
    })
    .title()
    .to_owned();
    let read = match graph_links(relative, &content) {
        Ok(links) => NoteRead::Links((relative.to_owned(), title, links)),
        Err(reason) => NoteRead::Unreadable(UnreadableNote {
            path: relative.to_owned(),
            title,
            reason,
        }),
    };
    let base = DeclaredBase::try_from(content.as_str())
        .ok()
        .map(|declared| declared.target().to_owned());
    Ok(ScannedNote { stamp, read, base })
}

#[cfg(test)]
#[path = "maintenance_test.rs"]
mod tests;
