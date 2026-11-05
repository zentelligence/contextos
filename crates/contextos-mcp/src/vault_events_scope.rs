//! Which notes are tasks: the notes the vault's `memory/tasks.base` selects that say `type: task`, and the silent
//! rebuild of the last-written task table from them.
//!
//! Selection reuses the base filter engine `base_query` runs, so "a task" means the same thing here as in a query. A
//! path outside the base's folder costs only a prefix check.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use contextos_core::{ContentHash, VaultPath, VaultRootId, VaultSet};
use contextos_events::{TaskChange, TaskSite, TaskTable, TrackedTask};
use contextos_fs::{Filesystem, ReadTextRequest};
use contextos_obsidian::{BaseDocument, FileMetadata, RowContext, evaluate_filters, records_folder};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::tools::base_query;
use crate::tools::fs::child_path;

/// Where the task base lives in every vault.
pub(crate) const TASKS_BASE: &str = "memory/tasks.base";

/// The notes a vault's task base selects.
#[derive(Debug)]
pub(crate) struct TaskScope {
    filters: Option<Value>,
}

impl TryFrom<&str> for TaskScope {
    type Error = contextos_obsidian::BaseError;

    fn try_from(source: &str) -> Result<Self, Self::Error> {
        let document = BaseDocument::try_from(source)?;
        Ok(Self {
            filters: document.definition().get("filters").cloned(),
        })
    }
}

impl TaskScope {
    /// The text of the task base at `memory/tasks.base`; `None` when the vault has none or it cannot be read, in which
    /// case the vault tracks no tasks.
    pub(crate) fn source(filesystem: &Filesystem, root: &VaultPath, roots: &VaultSet) -> Option<String> {
        let path = child_path(root, roots, TASKS_BASE).ok()?;
        filesystem
            .read_text(&ReadTextRequest { path, limit: None })
            .ok()
            .map(|file| file.content)
    }

    /// The base's own filters, as a scan takes them.
    pub(crate) fn filters(&self) -> Option<&Value> {
        self.filters.as_ref()
    }

    /// Whether a note at `relative` could be a task: a Markdown note in the base's folder, a prefix check and no more.
    pub(crate) fn holds(&self, relative: &str) -> bool {
        crate::events_folder::is_markdown(relative)
            && records_folder(self.filters.as_ref()).is_none_or(|folder| folder.contains(relative))
    }

    /// Whether a folder at `folder` (vault-relative) could hold, contain, or lie within the notes this base selects: it
    /// is the base's own folder, lies on either side of it, or the base names no folder at all.
    pub(crate) fn may_reach(&self, folder: &str) -> bool {
        records_folder(self.filters.as_ref()).is_none_or(|base| {
            base.path == folder
                || base.path.strip_prefix(folder).is_some_and(|rest| rest.starts_with('/'))
                || base.contains(&format!("{folder}/."))
        })
    }

    /// Whether the base's full filter selects the note at `relative` with this `frontmatter`.
    pub(crate) fn selects(&self, relative: &str, frontmatter: &Map<String, Value>, size: u64) -> bool {
        let (folder, name) = relative.rsplit_once('/').unwrap_or(("", relative));
        let basename = name.strip_suffix(".md").unwrap_or(name);
        let row = RowContext {
            frontmatter,
            file: FileMetadata {
                name,
                basename,
                path: relative,
                ext: "md",
                folder,
                size,
                ctime: None,
                mtime: None,
            },
            tags: &[],
            links: &[],
            embeds: &[],
            backlinks: &[],
        };
        evaluate_filters(self.filters.as_ref(), &row).unwrap_or(false)
    }
}

/// The parsed task base, kept until its text changes. The base is small, so it is identified by a hash of its text
/// rather than by its size and modification time, which an edit made within the clock's resolution can leave alone.
#[derive(Debug, Default)]
pub(crate) struct ScopeCache {
    held: Mutex<Option<(ContentHash, Option<Arc<TaskScope>>)>>,
}

impl ScopeCache {
    /// The vault's task scope for the base whose text is `source` (`None` when the vault has no base), parsed again
    /// only when the text differs from the text last parsed.
    pub(crate) fn get(
        &self,
        source: Option<&str>,
        parse: impl FnOnce(&str) -> Option<TaskScope>,
    ) -> Option<Arc<TaskScope>> {
        let Ok(mut held) = self.held.lock() else {
            return source.and_then(parse).map(Arc::new);
        };
        let Some(source) = source else {
            *held = None;
            return None;
        };
        let identity = ContentHash::from(<[u8; 32]>::from(Sha256::digest(source.as_bytes())));
        if let Some((known, scope)) = held.as_ref()
            && *known == identity
        {
            return scope.clone();
        }
        let scope = parse(source).map(Arc::new);
        *held = Some((identity, scope.clone()));
        scope
    }
}

/// Rebuilds the table from the task notes as they are, recording each without an event. `except` names the notes
/// the caller is about to record itself, so a task created by the write that triggered the rebuild still fires.
pub(crate) fn rebuild_table(
    filesystem: &Filesystem,
    roots: &VaultSet,
    root_id: VaultRootId,
    scope: &TaskScope,
    except: &HashSet<String>,
    now: OffsetDateTime,
) -> TaskTable {
    let mut table = TaskTable::default();
    let records = match base_query::scope_records(filesystem, roots, root_id, scope.filters()) {
        Ok(records) => records,
        Err(error) => {
            let code = crate::tool_error::ToolFailure::from(error).code;
            tracing::warn!(code = %code, "the task notes could not be listed to rebuild the last-written task table");
            return table;
        }
    };
    for record in records {
        if except.contains(&record.path) {
            continue;
        }
        if let Ok(task) = TrackedTask::try_from(&record.frontmatter) {
            // A task the table has not seen is recorded without an event, which is what a rebuild needs.
            table.apply(
                TaskSite {
                    base: TASKS_BASE,
                    path: &record.path,
                },
                TaskChange::Updated {
                    task,
                    declared_states: None,
                },
                now,
            );
        }
    }
    table
}

#[cfg(test)]
#[path = "vault_events_scope_test.rs"]
mod tests;
