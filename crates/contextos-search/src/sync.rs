//! Text-search synchronisation.
//!
//! `TextSearchService` keeps one vault root's text index aligned with its
//! markdown files. `update` applies `OperationEvent`s from the write pipeline
//! immediately, while `refresh` reconciles filesystem changes that bypassed
//! the pipeline entirely (external editors, synced files, or restored
//! backups) against the stored index state.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use contextos_core::{
    OpKind, OperationEvent, OperationWarning, PathError, UpdatesSearch, VaultPath, VaultPathInput, VaultRoot,
    VaultRootId, VaultSet,
};
use serde::Serialize;
use time::OffsetDateTime;

use crate::text::{IndexEntry, IndexesText, TextBatch};
use crate::{DocumentSource, IndexedDocument, SearchError};

/// Construction input for one vault root's `TextSearchService`.
pub struct TextSyncConfig<I> {
    /// Identity of the vault root this service synchronises.
    pub root_id: VaultRootId,
    /// Resolved filesystem path of the vault root.
    pub root: PathBuf,
    /// Forward-slash relative path prefixes excluded from synchronisation.
    pub excludes: Vec<String>,
    /// The text index kept in sync.
    pub index: I,
}

/// Keeps one vault root's text index aligned with its markdown files.
///
/// `update` (an `UpdatesSearch` implementation) applies `OperationEvent`s from
/// the write pipeline immediately. `refresh` reconciles filesystem changes
/// made outside the pipeline against the stored index state.
pub struct TextSearchService<I> {
    root_id: VaultRootId,
    root: PathBuf,
    excludes: Vec<String>,
    index: I,
}

impl<I> From<TextSyncConfig<I>> for TextSearchService<I> {
    fn from(value: TextSyncConfig<I>) -> Self {
        Self {
            root_id: value.root_id,
            root: value.root,
            excludes: value.excludes,
            index: value.index,
        }
    }
}

impl<I> TextSearchService<I> {
    /// Returns the underlying text index.
    #[must_use]
    pub const fn index(&self) -> &I {
        &self.index
    }
}

/// Scan and reconciliation counts for one `refresh` pass.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct FreshnessReport {
    /// Markdown files successfully visited under the vault root.
    pub scanned: usize,
    /// Files upserted because they were missing or stale in the index.
    pub reindexed: usize,
    /// Stored entries removed because their file no longer exists.
    pub removed: usize,
}

impl<I: IndexesText> TextSearchService<I> {
    /// Reconciles the text index against the current filesystem state,
    /// detecting external changes that bypassed the write pipeline (external
    /// editors, synced files, or restored backups).
    ///
    /// Reads the vault without holding the index's writer lock, then commits through
    /// [`IndexesText::apply_checked`], which re-checks each file it is about to write while holding it (see
    /// [`Self::commit_refresh`]), so a refresh never puts back a document older than one another write indexed while
    /// it ran.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the index or vault root cannot be read,
    /// or when the index cannot be written.
    pub fn refresh(&self) -> Result<FreshnessReport, SearchError> {
        let plan = self.plan_refresh()?;
        self.commit_refresh(plan)
    }

    /// Works out what bringing the index up to date requires, reading the vault and the index without writing.
    fn plan_refresh(&self) -> Result<RefreshPlan, SearchError> {
        let mut stored: BTreeMap<String, IndexEntry> = self
            .index
            .entries()?
            .into_iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect();

        let files = collect_markdown_files(&self.root, &self.excludes)?;
        let roots = resolve_single_root_set(&self.root)?;

        let mut plan = RefreshPlan::default();
        for (relative, absolute) in files {
            let existing = stored.remove(&relative);

            let Some(stamp) = SourceStamp::of(&absolute) else {
                continue;
            };
            let modified = OffsetDateTime::from(stamp.modified);

            if existing.as_ref().is_some_and(|entry| entry.modified == modified) {
                plan.report.scanned = plan.report.scanned.saturating_add(1);
                continue;
            }

            let Some(document) = read_document(&roots, &relative, &absolute, modified) else {
                continue;
            };
            plan.report.scanned = plan.report.scanned.saturating_add(1);
            let hash: &str = document.content_hash().into();
            let unchanged = existing.is_some_and(|entry| entry.content_hash == hash);
            plan.upserts.push((stamp, document));
            if !unchanged {
                plan.report.reindexed = plan.report.reindexed.saturating_add(1);
            }
        }

        plan.removals = stored.into_keys().collect();
        plan.report.removed = plan.removals.len();
        Ok(plan)
    }

    /// Commits `plan` in one write, re-checking each file it names while holding the index's writer lock: a document
    /// whose file has changed since it was read is read again, one whose file has gone is removed instead, and a
    /// removal whose file has reappeared indexes it instead. Every other writer of the index writes the file first and
    /// takes the lock to index it after, so whatever the lock holder finds on disk is at least as new as anything
    /// already indexed, and anything written later is indexed after this commit. Nothing is written for a plan with
    /// nothing to do.
    fn commit_refresh(&self, plan: RefreshPlan) -> Result<FreshnessReport, SearchError> {
        if plan.upserts.is_empty() && plan.removals.is_empty() {
            return Ok(plan.report);
        }
        let report = plan.report;
        let roots = resolve_single_root_set(&self.root)?;
        let mut plan = Some(plan);
        self.index.apply_checked(&mut || {
            let plan = plan.take().unwrap_or_default();
            let mut batch = TextBatch::default();
            for (stamp, document) in plan.upserts {
                let relative = document.path().to_owned();
                let absolute = self.root.join(&relative);
                match SourceStamp::of(&absolute) {
                    Some(current) if current == stamp => batch.upserts.push(document),
                    Some(current) => stage_reread(&roots, &relative, &absolute, current, &mut batch),
                    None => batch.removals.push(relative),
                }
            }
            for relative in plan.removals {
                let absolute = self.root.join(&relative);
                match SourceStamp::of(&absolute) {
                    Some(current) => stage_reread(&roots, &relative, &absolute, current, &mut batch),
                    None => batch.removals.push(relative),
                }
            }
            Ok(batch)
        })?;
        Ok(report)
    }

    /// Removes the document stored for `path`, ignoring any scope filter.
    fn remove_path(&self, path: &VaultPath) -> Result<(), SearchError> {
        self.index.remove(&path.portable_relative())
    }

    /// Works out what indexing `path` from its current filesystem content requires: a document to index, a removal
    /// (the file is missing or no longer a regular file), or nothing (the path is out of scope).
    fn plan_sync(&self, path: &VaultPath) -> Result<PlannedSync, SearchError> {
        let relative = path.portable_relative();
        if !in_scope(&self.excludes, &relative) {
            return Ok(PlannedSync::Skip);
        }

        let absolute = self.root.join(&relative);
        let is_file = fs::metadata(&absolute).is_ok_and(|metadata| metadata.is_file());
        if !is_file {
            return Ok(PlannedSync::Remove(relative));
        }

        let content = contextos_fs::read_regular_text(&absolute).map_err(|source| SearchError::DocumentRead {
            path: relative.clone(),
            source,
        })?;
        let metadata = fs::metadata(&absolute).map_err(|source| SearchError::DocumentRead {
            path: relative.clone(),
            source,
        })?;
        let modified = metadata
            .modified()
            .map(OffsetDateTime::from)
            .map_err(|source| SearchError::DocumentRead {
                path: relative.clone(),
                source,
            })?;

        Ok(PlannedSync::Index(Box::new(IndexedDocument::from(DocumentSource {
            path,
            content: &content,
            modified,
        }))))
    }

    /// Commits the documents gathered so far as one batch.
    fn flush(&self, pending: &mut Vec<IndexedDocument>) -> Result<(), SearchError> {
        if pending.is_empty() {
            return Ok(());
        }
        self.index.index(pending)?;
        pending.clear();
        Ok(())
    }

    /// Gathers `path`'s document for the next batch, or applies its removal now, after committing what precedes
    /// it so changes still take effect in the order they arrived.
    fn stage_sync(&self, pending: &mut Vec<IndexedDocument>, path: &VaultPath) -> Result<(), SearchError> {
        match self.plan_sync(path)? {
            PlannedSync::Index(document) => pending.push(*document),
            PlannedSync::Remove(relative) => {
                self.flush(pending)?;
                self.index.remove(&relative)?;
            }
            PlannedSync::Skip => {}
        }
        Ok(())
    }

    /// Removes `path` now, after committing what precedes it.
    fn stage_removal(&self, pending: &mut Vec<IndexedDocument>, path: &VaultPath) -> Result<(), SearchError> {
        self.flush(pending)?;
        self.remove_path(path)
    }

    /// Applies every event in order. A path that cannot be applied (an unreadable document, say) is remembered and
    /// the rest of the batch still goes ahead, so one bad file cannot leave everything behind it unindexed; the first
    /// such failure is what is returned.
    fn apply_events(&self, events: &[OperationEvent], pending: &mut Vec<IndexedDocument>) -> Result<(), SearchError> {
        let mut first_failure = None;
        let mut note = |outcome: Result<(), SearchError>| {
            if let Err(error) = outcome {
                first_failure.get_or_insert(error);
            }
        };
        for event in events {
            match event.kind {
                OpKind::Delete => {
                    for path in event.paths.iter().filter(|path| path.root_id() == self.root_id) {
                        note(self.stage_removal(pending, path));
                    }
                }
                OpKind::Move => {
                    if let Some(from) = event.paths.first().filter(|path| path.root_id() == self.root_id) {
                        note(self.stage_removal(pending, from));
                    }
                    if let Some(to) = event.paths.get(1).filter(|path| path.root_id() == self.root_id) {
                        note(self.stage_sync(pending, to));
                    }
                }
                OpKind::Create | OpKind::Modify | OpKind::Restore => {
                    for path in event.paths.iter().filter(|path| path.root_id() == self.root_id) {
                        note(self.stage_sync(pending, path));
                    }
                }
            }
        }
        first_failure.map_or(Ok(()), Err)
    }
}

/// What one refresh found to do: the documents it read, each with the stamp its file had when read, and the stored
/// entries whose files it found missing.
#[derive(Default)]
struct RefreshPlan {
    upserts: Vec<(SourceStamp, IndexedDocument)>,
    removals: Vec<String>,
    report: FreshnessReport,
}

/// A file's modification time and length when it was read: if either differs later, the file has been written since.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SourceStamp {
    pub(crate) modified: std::time::SystemTime,
    pub(crate) len: u64,
}

impl SourceStamp {
    /// The stamp of the regular file at `path`, or `None` when it is missing, not a regular file, or its metadata
    /// cannot be read.
    pub(crate) fn of(path: &Path) -> Option<Self> {
        let metadata = fs::metadata(path).ok().filter(std::fs::Metadata::is_file)?;
        Some(Self {
            modified: metadata.modified().ok()?,
            len: metadata.len(),
        })
    }
}

/// Reads the markdown file at `absolute` as the indexed document for `relative`, or `None` when it cannot be read as
/// UTF-8 text or its path is not valid in the vault.
fn read_document(
    roots: &VaultSet,
    relative: &str,
    absolute: &Path,
    modified: OffsetDateTime,
) -> Option<IndexedDocument> {
    let content = contextos_fs::read_regular_text(absolute).ok()?;
    let path = VaultPath::try_from(VaultPathInput { roots, raw: relative }).ok()?;
    Some(IndexedDocument::from(DocumentSource {
        path: &path,
        content: &content,
        modified,
    }))
}

/// Stages the current content of a file found changed at a refresh's commit. A file that cannot be read as text is left
/// as it is in the index, exactly as the refresh's scan leaves one.
fn stage_reread(roots: &VaultSet, relative: &str, absolute: &Path, stamp: SourceStamp, batch: &mut TextBatch) {
    if let Some(document) = read_document(roots, relative, absolute, OffsetDateTime::from(stamp.modified)) {
        batch.upserts.push(document);
    }
}

/// What bringing one path's index entry up to date requires.
enum PlannedSync {
    Index(Box<IndexedDocument>),
    Remove(String),
    Skip,
}

impl<I: IndexesText> UpdatesSearch for TextSearchService<I> {
    fn update(&self, event: &OperationEvent) -> Result<(), OperationWarning> {
        self.update_all(std::slice::from_ref(event))
    }

    fn update_all(&self, events: &[OperationEvent]) -> Result<(), OperationWarning> {
        self.apply_all(events).map_err(OperationWarning::from)
    }
}

impl<I: IndexesText> TextSearchService<I> {
    /// Applies `events` as [`UpdatesSearch::update_all`] does, returning the first failure typed, so a caller can tell
    /// a damaged index from any other failure before reporting it as a warning.
    ///
    /// # Errors
    ///
    /// Returns the first failure to read a document or write the index.
    pub(crate) fn apply_all(&self, events: &[OperationEvent]) -> Result<(), SearchError> {
        let mut pending = Vec::new();
        let applied = self.apply_events(events, &mut pending);
        // What was gathered before a failure is still committed, so a partial batch is not silently dropped.
        let flushed = self.flush(&mut pending);
        applied.and(flushed)
    }
}

/// Reports whether `relative` is an in-scope markdown path: it must end with
/// `.md` and must not fall under any configured exclude prefix.
fn in_scope(excludes: &[String], relative: &str) -> bool {
    is_markdown(relative) && !is_excluded(excludes, relative)
}

/// Reports whether `relative` names a markdown file by extension: this
/// crate's one definition of "markdown file" (case-insensitive `.md`),
/// reused by `contextos-mcp`'s MCP resources surface so that surface
/// never grows a second, potentially divergent definition.
#[must_use]
pub fn is_markdown(relative: &str) -> bool {
    Path::new(relative)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
}

/// Reports whether `relative` equals or falls under one of `excludes`, each a
/// forward-slash relative path prefix.
fn is_excluded(excludes: &[String], relative: &str) -> bool {
    excludes.iter().any(|prefix| {
        relative == prefix.as_str()
            || relative
                .strip_prefix(prefix.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// Builds a single-root `VaultSet` from the already-resolved vault root, used
/// to construct throwaway `VaultPath`s for files discovered by the walk.
fn resolve_single_root_set(root: &Path) -> Result<VaultSet, SearchError> {
    let vault_root = VaultRoot::try_from(root.to_path_buf()).map_err(|source| root_error(root, source))?;
    VaultSet::try_from(vec![vault_root]).map_err(|source| root_error(root, source))
}

fn root_error(root: &Path, source: PathError) -> SearchError {
    SearchError::IndexDirectory {
        path: root.display().to_string(),
        source: std::io::Error::other(source),
    }
}

/// Recursively walks `root` in deterministic sorted order, returning every
/// markdown file not under an exclude prefix as a (relative, absolute) pair.
fn collect_markdown_files(root: &Path, excludes: &[String]) -> Result<Vec<(String, PathBuf)>, SearchError> {
    let mut files = Vec::new();
    walk_directory(root, root, excludes, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(files)
}

fn walk_directory(
    root: &Path,
    directory: &Path,
    excludes: &[String],
    files: &mut Vec<(String, PathBuf)>,
) -> Result<(), SearchError> {
    let entries = fs::read_dir(directory).map_err(|source| SearchError::DocumentRead {
        path: directory.display().to_string(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| SearchError::DocumentRead {
            path: directory.display().to_string(),
            source,
        })?;
        let path = entry.path();
        let Ok(relative_path) = path.strip_prefix(root) else {
            continue;
        };
        let relative = forward_slash(relative_path);
        if is_excluded(excludes, &relative) {
            continue;
        }
        let file_type = entry.file_type().map_err(|source| SearchError::DocumentRead {
            path: path.display().to_string(),
            source,
        })?;
        if file_type.is_dir() {
            walk_directory(root, &path, excludes, files)?;
        } else if file_type.is_file() && is_markdown(&relative) {
            files.push((relative, path));
        }
    }
    Ok(())
}

/// Joins path components with `/`, matching [`VaultPath::portable_relative`] but
/// for a plain filesystem path rather than a validated `VaultPath`.
fn forward_slash(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
#[path = "sync_test.rs"]
mod tests;
