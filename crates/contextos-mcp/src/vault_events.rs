//! The events consumer: what the server fires when a completed tool write creates or changes a task, or when a
//! secondary service fails or recovers.
//!
//! The router hands this module each batch of tool-origin events after every other service has run. It compares the
//! tasks the batch touched with the last-written task table in the vault's state directory, classifies the batch's
//! warnings against the failures that are current, and writes one event note for each change. Notes are written with
//! `Origin::Internal` through the unrouted write service and handed back to the router as internal events, so Git
//! stages them with the write that raised them and nothing else follows them. A tool write holds the vault's
//! cross-process write lock while it is routed, which is what makes each change exactly once across processes.
//!
//! A note that cannot be written never fails the write that caused it: the consumer returns an `events/write-failed`
//! warning instead.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use contextos_core::{
    Clock, EventsBatch, EventsOutcome, OpKind, OperationEvent, OperationWarning, RecordsEvents, VaultPath,
    VaultPathInput, VaultRootId, VaultSet,
};
use contextos_events::{
    CurrentFailures, EventRecord, TaskChange, TaskLifecycles, TaskSite, TaskTable, TrackedTask, WRITE_FAILED_CODE,
};
use contextos_fs::{EntryKind, FileInfoRequest, Filesystem, FilesystemService, ReadTextRequest};
use contextos_obsidian::FrontmatterDocument;
use time::OffsetDateTime;

use crate::events_writer::EventWriter;
use crate::server::VaultClock;
use crate::tools::fs::child_path;
use crate::vault_events_scope::{ScopeCache, TaskScope, rebuild_table};

/// The file names of the derived events state in a vault's state directory.
const TASK_TABLE_FILE: &str = "events-tasks.json";
const HEALTH_FILE: &str = "events-health.json";

/// Where tasks configuration lives in every vault.
const TASKS_CONFIG: &str = ".contextos/tasks.toml";

/// What the consumer needs to know about one vault.
#[derive(Clone, Debug)]
pub(crate) struct VaultEventsSetting {
    /// Whether the vault is managed; events are fired only in a managed vault.
    pub(crate) enabled: bool,
    /// The vault's derived-state directory.
    pub(crate) state_directory: PathBuf,
}

/// Everything the consumer is composed from.
pub(crate) struct VaultEventsConfig {
    pub(crate) roots: VaultSet,
    pub(crate) filesystem: Filesystem,
    /// The write service with no routing, which the consumer writes event notes through.
    pub(crate) writer: FilesystemService<VaultClock>,
    pub(crate) clock: VaultClock,
    /// One setting per root, in the order of `roots`.
    pub(crate) vaults: Vec<VaultEventsSetting>,
}

#[derive(Debug)]
struct Vault {
    setting: VaultEventsSetting,
    root: VaultPath,
    root_id: VaultRootId,
    scope: ScopeCache,
}

#[derive(Debug)]
struct Inner {
    roots: VaultSet,
    filesystem: Filesystem,
    writer: FilesystemService<VaultClock>,
    clock: VaultClock,
    vaults: Vec<Vault>,
}

/// Fires events for the vaults it was composed with. Cheap to clone: the router holds one and the server holds one.
#[derive(Clone, Debug)]
pub(crate) struct VaultEvents {
    inner: Arc<Inner>,
}

impl TryFrom<VaultEventsConfig> for VaultEvents {
    type Error = contextos_core::PathError;

    fn try_from(value: VaultEventsConfig) -> Result<Self, Self::Error> {
        let mut vaults = Vec::with_capacity(value.vaults.len());
        for (index, setting) in value.vaults.into_iter().enumerate() {
            let root_id = VaultRootId::try_from(index)?;
            let name = value
                .roots
                .root(root_id)
                .map(|root| root.name().to_owned())
                .unwrap_or_default();
            let root = VaultPath::try_from(VaultPathInput {
                roots: &value.roots,
                raw: &format!("{name}://."),
            })?;
            vaults.push(Vault {
                setting,
                root,
                root_id,
                scope: ScopeCache::default(),
            });
        }
        Ok(Self {
            inner: Arc::new(Inner {
                roots: value.roots,
                filesystem: value.filesystem,
                writer: value.writer,
                clock: value.clock,
                vaults,
            }),
        })
    }
}

/// One batch's work on one vault.
struct Batch {
    events: Vec<OperationEvent>,
    now: OffsetDateTime,
}

impl RecordsEvents for VaultEvents {
    fn record(&self, batch: &EventsBatch) -> EventsOutcome {
        let mut outcome = EventsOutcome::default();
        let mut seen: Vec<VaultRootId> = Vec::new();
        for path in batch.events.iter().flat_map(|event| &event.paths) {
            let root_id = path.root_id();
            if seen.contains(&root_id) {
                continue;
            }
            seen.push(root_id);
            let Some(vault) = self.inner.vault(root_id) else {
                continue;
            };
            if !vault.setting.enabled {
                continue;
            }
            let events = batch
                .events
                .iter()
                .filter_map(|event| scoped_to(event, root_id))
                .collect();
            // Each vault is judged by the warnings of its own writes, plus those raised once for the whole batch.
            let health = Some((batch.warnings_for(root_id), batch.attempted_for(root_id)));
            let work = Batch {
                events,
                now: self.inner.clock.now(),
            };
            self.inner.record_vault(vault, &work, health, &mut outcome);
        }
        outcome
    }
}

/// The part of an event that happened in one vault, or `None` when it did not touch the vault.
///
/// A move between vaults is a removal in the vault it left and a creation in the vault it reached.
fn scoped_to(event: &OperationEvent, root_id: VaultRootId) -> Option<OperationEvent> {
    let mut scoped = event.clone();
    scoped.paths.retain(|path| path.root_id() == root_id);
    if scoped.paths.is_empty() {
        return None;
    }
    if event.kind == OpKind::Move && scoped.paths.len() == 1 {
        let left = event.paths.first().is_some_and(|source| source.root_id() == root_id);
        scoped.kind = if left { OpKind::Delete } else { OpKind::Create };
    }
    Some(scoped)
}

impl Inner {
    fn vault(&self, root_id: VaultRootId) -> Option<&Vault> {
        usize::try_from(root_id).ok().and_then(|index| self.vaults.get(index))
    }

    fn task_scope(&self, vault: &Vault) -> Option<Arc<TaskScope>> {
        let source = TaskScope::source(&self.filesystem, &vault.root, &self.roots);
        vault
            .scope
            .get(source.as_deref(), |text| TaskScope::try_from(text).ok())
    }

    fn lifecycles(&self, vault: &Vault) -> Option<TaskLifecycles> {
        let path = child_path(&vault.root, &self.roots, TASKS_CONFIG).ok()?;
        match self.filesystem.read_text(&ReadTextRequest { path, limit: None }) {
            Ok(file) => TaskLifecycles::try_from(file.content.as_str())
                .inspect_err(|error| tracing::warn!(code = error.code(), "tasks configuration is unreadable"))
                .ok(),
            Err(contextos_fs::FsError::NotFound { .. }) => Some(TaskLifecycles::default()),
            Err(error) => {
                tracing::warn!(code = error.code(), "tasks configuration could not be read");
                None
            }
        }
    }

    fn record_vault(
        &self,
        vault: &Vault,
        batch: &Batch,
        health: Option<(Vec<OperationWarning>, Vec<contextos_core::OperationService>)>,
        outcome: &mut EventsOutcome,
    ) {
        let mut records = self.task_records(vault, batch);
        let mut failures = self.write_all(vault, &records, outcome);
        records.clear();
        if let Some((warnings, attempted)) = health {
            let mut seen = warnings;
            if failures > 0 {
                seen.push(write_failed_warning(failures));
            }
            records.extend(health_records(vault, &seen, &attempted, batch.now));
            failures += self.write_all(vault, &records, outcome);
        }
        if failures > 0 {
            outcome.warnings.push(write_failed_warning(failures));
        }
    }

    /// Writes each record, returning how many could not be written.
    fn write_all(&self, vault: &Vault, records: &[EventRecord], outcome: &mut EventsOutcome) -> usize {
        let writer = EventWriter {
            writes: &self.writer,
            roots: &self.roots,
            vault: &vault.root,
        };
        let mut failed = 0;
        for record in records {
            match writer.write(record) {
                Ok(written) => outcome.events.extend(written.event),
                Err(error) => {
                    failed += 1;
                    tracing::warn!(kind = %record.kind, code = %String::from(error), "an event note could not be written");
                }
            }
        }
        failed
    }

    /// Compares the tasks the batch touched with the table, stores the table, and returns the events that fire.
    fn task_records(&self, vault: &Vault, batch: &Batch) -> Vec<EventRecord> {
        let Some(scope) = self.task_scope(vault) else {
            return Vec::new();
        };
        let operations = Operations::from(TaskBatch {
            scope: &scope,
            events: &batch.events,
        });
        if operations.is_empty() {
            return Vec::new();
        }
        let table_file = vault.setting.state_directory.join(TASK_TABLE_FILE);
        let loaded = TaskTable::load(&table_file)
            .inspect_err(|error| {
                tracing::warn!(code = error.code(), "the task table is unreadable and will be rebuilt");
            })
            .ok()
            .flatten();
        let seeded = loaded.is_none();
        let mut table = loaded.unwrap_or_else(|| {
            rebuild_table(
                &self.filesystem,
                &self.roots,
                vault.root_id,
                &scope,
                &operations.touched(),
                batch.now,
            )
        });
        let before = table.clone();
        let lifecycles = self.lifecycles(vault);
        let mut records = Vec::new();
        for operation in &operations.list {
            match operation {
                Operation::Touch { path, created } => {
                    // A write that leaves a task the table has not seen, in a table that was read rather than
                    // rebuilt from the notes as they are now, turned a note that was not a task into one.
                    let becomes_task = !seeded && table.get(&path.portable_relative()).is_none();
                    records.extend(self.touch(
                        &mut table,
                        &scope,
                        lifecycles.as_ref(),
                        path,
                        *created || becomes_task,
                        batch.now,
                    ));
                }
                Operation::Remove { relative } => {
                    table.apply(task_site(relative), TaskChange::Deleted, batch.now);
                }
                Operation::Move { from, to } => {
                    // A task moved out of the base's folder is no longer one of its records, so it is forgotten and
                    // fires nothing.
                    if !scope.holds(&to.portable_relative()) {
                        table.apply(task_site(from), TaskChange::Deleted, batch.now);
                    } else if scope.holds(from) {
                        records.extend(table.apply(
                            task_site(to.portable_relative().as_str()),
                            TaskChange::Moved { from },
                            batch.now,
                        ));
                    }
                    records.extend(self.touch(&mut table, &scope, lifecycles.as_ref(), to, false, batch.now));
                }
            }
        }
        for (from, to) in &operations.folders_moved {
            records.extend(moved_folder_records(&mut table, &scope, from, to, batch.now));
        }
        if self.tasks_moved_as_folders(&table, &operations) {
            table = rebuild_table(
                &self.filesystem,
                &self.roots,
                vault.root_id,
                &scope,
                &HashSet::new(),
                batch.now,
            );
        }
        if (seeded || table != before)
            && let Err(error) = table.store(&table_file)
        {
            tracing::warn!(code = error.code(), "the task table could not be stored");
        }
        records
    }

    /// Whether a folder the batch took away held tracked tasks, or a folder it brought in is there now: either way the
    /// table is rebuilt from the notes as they are. What the folder was is read from the table and the file system,
    /// not guessed from its name.
    fn tasks_moved_as_folders(&self, table: &TaskTable, operations: &Operations) -> bool {
        operations.below.iter().any(|folder| table.holds_below(folder))
            || operations.arrived.iter().any(|path| {
                self.filesystem
                    .file_info(&FileInfoRequest { path: path.clone() })
                    .is_ok_and(|info| info.kind == EntryKind::Directory)
            })
    }

    /// Records the task note at `path` as it is now, or drops its entry when it is no longer a task.
    fn touch(
        &self,
        table: &mut TaskTable,
        scope: &TaskScope,
        lifecycles: Option<&TaskLifecycles>,
        path: &VaultPath,
        created: bool,
        now: OffsetDateTime,
    ) -> Vec<EventRecord> {
        let relative = path.portable_relative();
        if !scope.holds(&relative) {
            // A note moved out of the base's folder is no longer tracked.
            table.apply(task_site(&relative), TaskChange::Deleted, now);
            return Vec::new();
        }
        let Ok(note) = self.filesystem.read_text(&ReadTextRequest {
            path: path.clone(),
            limit: None,
        }) else {
            return Vec::new();
        };
        let Ok(document) = FrontmatterDocument::try_from(note.content.as_str()) else {
            // Frontmatter a person is mid-way through editing is left as the table last knew it.
            return Vec::new();
        };
        let frontmatter = document.frontmatter();
        let task = TrackedTask::try_from(frontmatter)
            .ok()
            .filter(|_| scope.selects(&relative, frontmatter, note.size));
        let Some(task) = task else {
            table.apply(task_site(&relative), TaskChange::Deleted, now);
            return Vec::new();
        };
        let declared_states = lifecycles.map(|known| known.declared_states(task.task_type.as_deref()));
        let change = if created {
            TaskChange::Created {
                origin: frontmatter
                    .get("origin")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned),
                task,
                declared_states,
            }
        } else {
            TaskChange::Updated { task, declared_states }
        };
        table.apply(task_site(&relative), change, now)
    }

    /// Fills the last-written task table from the task notes when it is missing or unreadable, with no event.
    pub(crate) fn seed_if_missing(&self, vault: &Vault) {
        let Some(scope) = self.task_scope(vault) else {
            return;
        };
        let table_file = vault.setting.state_directory.join(TASK_TABLE_FILE);
        if matches!(TaskTable::load(&table_file), Ok(Some(_))) {
            return;
        }
        let table = rebuild_table(
            &self.filesystem,
            &self.roots,
            vault.root_id,
            &scope,
            &HashSet::new(),
            self.clock.now(),
        );
        if let Err(error) = table.store(&table_file) {
            tracing::warn!(code = error.code(), "the task table could not be stored");
        }
    }
}

impl VaultEvents {
    /// Fills every managed vault's task table that is missing, silently, as serving starts.
    ///
    /// `lock` takes the vault's write lock by its position and returns the guard that holds it, so a table is never
    /// seeded over a newer one another process has written since. A vault whose lock cannot be taken is left alone:
    /// the first locked write that touches a task finds the table missing and rebuilds it then.
    pub(crate) fn seed_missing_tables<G>(&self, lock: impl Fn(usize) -> Result<G, crate::tool_error::ToolError>) {
        for (index, vault) in self.inner.vaults.iter().enumerate() {
            if !vault.setting.enabled {
                continue;
            }
            match lock(index) {
                Ok(_guard) => self.inner.seed_if_missing(vault),
                Err(error) => {
                    tracing::warn!(
                        code = %crate::tool_error::ToolFailure::from(error).code,
                        "the task table was not seeded because the vault's write lock could not be taken"
                    );
                }
            }
        }
    }
}

fn health_records(
    vault: &Vault,
    warnings: &[OperationWarning],
    attempted: &[contextos_core::OperationService],
    now: OffsetDateTime,
) -> Vec<EventRecord> {
    let path = vault.setting.state_directory.join(HEALTH_FILE);
    let mut current = CurrentFailures::load(&path)
        .inspect_err(|error| tracing::warn!(code = error.code(), "the current-failures record is unreadable"))
        .ok()
        .flatten()
        .unwrap_or_default();
    let before = current.clone();
    let changes = current.observe(warnings, attempted);
    if current != before
        && let Err(error) = current.store(&path)
    {
        tracing::warn!(code = error.code(), "the current-failures record could not be stored");
    }
    changes.iter().map(|change| EventRecord::from((change, now))).collect()
}

/// Where the task note at `path` is: a record of the vault's task base.
fn task_site(path: &str) -> TaskSite<'_> {
    TaskSite {
        base: crate::vault_events_scope::TASKS_BASE,
        path,
    }
}

/// The `task.moved` events for every tracked task below the folder `from` that a move took to `to`, keeping each one
/// tracked at its new path. A task the move took out of the base's folder is left for the rebuild to forget.
fn moved_folder_records(
    table: &mut TaskTable,
    scope: &TaskScope,
    from: &str,
    to: &str,
    now: OffsetDateTime,
) -> Vec<EventRecord> {
    let mut records = Vec::new();
    for old in table.paths_below(from) {
        let Some(rest) = old.strip_prefix(from) else {
            continue;
        };
        let new = format!("{to}{rest}");
        if scope.holds(&new) {
            records.extend(table.apply(task_site(&new), TaskChange::Moved { from: &old }, now));
        }
    }
    records
}

/// The warning that says event notes could not be written while the write that raised them stood.
fn write_failed_warning(count: usize) -> OperationWarning {
    OperationWarning {
        code: WRITE_FAILED_CODE.to_owned(),
        message: format!("{count} event note(s) could not be written; the write itself completed"),
    }
}

/// What a batch did to the notes that may be tasks.
enum Operation {
    /// A note was written: created when the batch created it.
    Touch { path: VaultPath, created: bool },
    /// A note was deleted.
    Remove { relative: String },
    /// A note was moved.
    Move { from: String, to: VaultPath },
}

struct Operations {
    list: Vec<Operation>,
    /// Folders a delete or a move took away, which tracked tasks may have lain below, by vault-relative path.
    below: Vec<String>,
    /// Folders a move brought into a place the base may reach, which tasks may have come with.
    arrived: Vec<VaultPath>,
    /// Folders moved from one place the base may reach to another, as vault-relative `(from, to)` pairs, whose tracked
    /// tasks moved with them.
    folders_moved: Vec<(String, String)>,
}

/// The operation events of one batch and the scope of the notes that may be tasks.
struct TaskBatch<'a> {
    scope: &'a TaskScope,
    events: &'a [OperationEvent],
}

impl From<TaskBatch<'_>> for Operations {
    fn from(batch: TaskBatch<'_>) -> Self {
        let TaskBatch { scope, events } = batch;
        let mut operations = Self {
            list: Vec::new(),
            below: Vec::new(),
            arrived: Vec::new(),
            folders_moved: Vec::new(),
        };
        for event in events {
            match (event.kind, event.paths.as_slice()) {
                (OpKind::Create | OpKind::Modify | OpKind::Restore, paths) => {
                    for path in paths {
                        if scope.holds(&path.portable_relative()) {
                            operations.list.push(Operation::Touch {
                                path: path.clone(),
                                created: event.kind == OpKind::Create,
                            });
                        }
                    }
                }
                (OpKind::Delete, paths) => {
                    for path in paths {
                        let relative = path.portable_relative();
                        if scope.holds(&relative) {
                            operations.list.push(Operation::Remove { relative });
                        } else if scope.may_reach(&relative) {
                            operations.below.push(relative);
                        }
                    }
                }
                (OpKind::Move, [source, destination]) => {
                    let (from, to) = (source.portable_relative(), destination.portable_relative());
                    if scope.holds(&from) || scope.holds(&to) {
                        operations.list.push(Operation::Move {
                            from,
                            to: destination.clone(),
                        });
                    } else {
                        if scope.may_reach(&from) && scope.may_reach(&to) {
                            operations.folders_moved.push((from.clone(), to));
                        }
                        if scope.may_reach(&from) {
                            operations.below.push(from);
                        }
                        if scope.may_reach(&destination.portable_relative()) {
                            operations.arrived.push(destination.clone());
                        }
                    }
                }
                (OpKind::Move, _) => {}
            }
        }
        operations
    }
}

impl Operations {
    fn is_empty(&self) -> bool {
        self.list.is_empty() && self.below.is_empty() && self.arrived.is_empty()
    }

    /// The notes the batch wrote, which a rebuild leaves out so they can still fire.
    fn touched(&self) -> HashSet<String> {
        self.list
            .iter()
            .filter_map(|operation| match operation {
                Operation::Touch { path, .. } | Operation::Move { to: path, .. } => Some(path.portable_relative()),
                Operation::Remove { .. } => None,
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "vault_events_test.rs"]
mod tests;
