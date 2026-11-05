//! Bringing the link graph up to date with a vault that changed without the server: files created, changed, or deleted
//! by Obsidian, a sync client, or a shell. The write pipeline keeps the graph right for what it writes, so this is the
//! one way the graph learns of the rest, and it is cheap enough to run before a graph question is answered.
//!
//! The cost is one walk of the vault's directories, one metadata read per note, and a re-read of only the notes whose
//! modification time or length differs from what the graph recorded when it last read them. The text index finds its
//! external changes the same way. Nothing here replaces a rebuild, which re-reads every note and is how a damaged or
//! never-built graph is repaired.

use std::collections::BTreeSet;
use std::path::Path;

use contextos_core::VaultSet;

use crate::sync::SourceStamp;
use crate::{FileEntry, NoteStamp, SearchError};

use super::VaultSearchService;
use super::maintenance::read_graph_note;
use super::scan::{resolve_single_root_set, walk_files};

/// What bringing the graph up to date found.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GraphReconcileReport {
    /// How many files the vault holds, hidden ones left out.
    pub files: usize,
    /// How many notes were read again because they were new or had changed since the graph read them.
    pub notes_read: usize,
    /// How many files the graph learned of that it did not hold.
    pub files_added: usize,
    /// How many files the graph held that the vault no longer does.
    pub files_removed: usize,
}

impl GraphReconcileReport {
    /// Whether the graph already agreed with the vault.
    #[must_use]
    pub const fn is_unchanged(&self) -> bool {
        self.notes_read == 0 && self.files_added == 0 && self.files_removed == 0
    }
}

impl VaultSearchService {
    /// Brings the link graph up to date with the vault's files as they are now, however they changed: the vault is
    /// listed, files the graph does not hold are added, files it holds that are gone are removed, and each note whose
    /// modification time or length differs from the stamp the graph recorded when it read it is read again. Links that
    /// the changes affect are resolved again, and the whole is one write to the graph's store.
    ///
    /// Call it before answering a question that depends on the graph being complete, such as which links are
    /// unresolved. The graph's own questions (neighbours, backlinks, path, orphans) call it first.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::GraphDisabled`] when the link graph is disabled for this vault, and the failure that
    /// keeps the vault from being listed or read, or the graph's store from being read or written.
    pub fn reconcile_graph(&self) -> Result<GraphReconcileReport, SearchError> {
        let graph = self.graph_store()?;
        let roots = resolve_single_root_set(&self.root)?;
        let mut listed = Vec::new();
        walk_files(&self.root, &self.root, &self.hidden, &mut listed)?;
        let disk: BTreeSet<String> = listed.into_iter().collect();
        let known = Self::locked(&graph).knowledge()?;

        let mut report = GraphReconcileReport {
            files: disk.len(),
            ..GraphReconcileReport::default()
        };
        let mut entries = Vec::new();
        for gone in known.files.union(&known.nodes).filter(|path| !disk.contains(*path)) {
            report.files_removed = report.files_removed.saturating_add(1);
            entries.push(FileEntry::Removed {
                path: gone.clone(),
                folder: false,
            });
        }
        for path in &disk {
            let held = known.files.contains(path);
            if !self.in_scope(path) {
                if !held {
                    report.files_added = report.files_added.saturating_add(1);
                    entries.push(FileEntry::File(path.clone()));
                }
                continue;
            }
            let absolute = self.root.join(path);
            let Some(stamp) = SourceStamp::of(&absolute) else {
                continue;
            };
            let unchanged = matches!(known.notes.get(path), Some(Some(read)) if *read == NoteStamp::from(stamp));
            if unchanged {
                if !held {
                    entries.push(FileEntry::File(path.clone()));
                }
                continue;
            }
            if let Some(entry) = Self::reread_note(&roots, path, &absolute)? {
                if !held {
                    report.files_added = report.files_added.saturating_add(1);
                }
                report.notes_read = report.notes_read.saturating_add(1);
                entries.push(entry);
            }
        }
        if entries.is_empty() {
            return Ok(report);
        }
        Self::locked(&graph).apply_entries(&entries).inspect_err(|error| {
            if let Some(slot) = &self.graph {
                slot.note(error);
            }
        })?;
        Ok(report)
    }

    /// Reconciles the graph before a question is answered. A graph that cannot be reconciled is answered from as it
    /// stands, and the failure is logged, because a question about links should not fail over a file that could not be
    /// read.
    pub(super) fn reconcile_before_query(&self) {
        if let Err(error) = self.reconcile_graph() {
            tracing::warn!(
                code = error.code(),
                "the link graph could not be brought up to date with the vault before a question; answering from what it holds"
            );
        }
    }

    /// The graph's entry for the note at `absolute`, read now, or `None` when the file vanished before it was read.
    pub(super) fn reread_note(
        roots: &VaultSet,
        relative: &str,
        absolute: &Path,
    ) -> Result<Option<FileEntry>, SearchError> {
        match read_graph_note(roots, relative, absolute) {
            Ok(note) => Ok(Some(FileEntry::from(note))),
            Err(_) if SourceStamp::of(absolute).is_none() => Ok(None),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
#[path = "reconcile_test.rs"]
mod tests;
