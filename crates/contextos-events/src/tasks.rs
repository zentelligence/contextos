use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::persist::{read_json, write_json};
use crate::record::rfc3339_utc;
use crate::{EventRecord, EventSubject, EventsError, FieldName, FieldValue, KindName, ServerKind, resolve_entity};

/// The state a claimed task is in while someone works it.
const DOING: &str = "doing";

/// The table file's format version. A file of any other version is rebuilt, never interpreted.
const TABLE_VERSION: u32 = 1;

/// What the server tracks about one task between writes: enough to tell what a write changed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TrackedTask {
    pub task_type: Option<String>,
    pub id: Option<String>,
    pub state: Option<String>,
    pub assignee: Option<String>,
    pub claimed_by: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub claimed_until: Option<OffsetDateTime>,
    /// The task's `entity`, kept so an event about it carries the same one.
    pub entity: Option<String>,
}

/// A note that is not a task: its frontmatter has no `type: task`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NotATask;

/// A frontmatter value as text: a non-blank string, and nothing else.
fn text_of(frontmatter: &Map<String, Value>, field: &str) -> Option<String> {
    frontmatter
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

impl TryFrom<&Map<String, Value>> for TrackedTask {
    type Error = NotATask;

    /// Reads what the server tracks from a note's frontmatter. A value that is not a non-blank string, and a claim
    /// time that is not RFC 3339, read as absent, because the server never repairs what a person wrote.
    fn try_from(frontmatter: &Map<String, Value>) -> Result<Self, NotATask> {
        if frontmatter.get("type").and_then(Value::as_str) != Some("task") {
            return Err(NotATask);
        }
        Ok(Self {
            task_type: text_of(frontmatter, "task_type"),
            id: text_of(frontmatter, "id"),
            state: text_of(frontmatter, "state"),
            assignee: text_of(frontmatter, "assignee"),
            claimed_by: text_of(frontmatter, "claimed_by"),
            claimed_until: text_of(frontmatter, "claimed_until")
                .and_then(|text| OffsetDateTime::parse(&text, &Rfc3339).ok()),
            entity: text_of(frontmatter, "entity"),
        })
    }
}

/// Where a task note is: the base it is a record of, and its vault-relative path.
#[derive(Clone, Copy, Debug)]
pub struct TaskSite<'a> {
    pub base: &'a str,
    pub path: &'a str,
}

/// What a completed write did to a task note.
#[derive(Clone, Debug)]
pub enum TaskChange<'a> {
    /// The write created the task.
    Created {
        task: TrackedTask,
        /// What made the task, such as the tool or workflow that raised it, when known.
        origin: Option<String>,
        /// The states the task's type declares, when the type is known.
        declared_states: Option<&'a [String]>,
    },
    /// The write left the task at `path` with these values.
    Updated {
        task: TrackedTask,
        declared_states: Option<&'a [String]>,
    },
    /// The task now at `path` was moved or renamed from `from`.
    Moved { from: &'a str },
    /// The task at `path` was deleted.
    Deleted,
}

/// The last values the server wrote for each task, keyed by vault-relative path. Derived and rebuildable: a task it
/// has not seen is recorded silently the first time it is written, never with invented events.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TaskTable {
    version: u32,
    tasks: BTreeMap<String, TrackedTask>,
}

impl Default for TaskTable {
    fn default() -> Self {
        Self {
            version: TABLE_VERSION,
            tasks: BTreeMap::new(),
        }
    }
}

impl TaskTable {
    /// The values last written for the task at `path`.
    #[must_use]
    pub fn get(&self, path: &str) -> Option<&TrackedTask> {
        self.tasks.get(path)
    }

    /// Whether any tracked task lies below the folder `folder` (vault-relative, no trailing `/`): the whole-component
    /// test, so `memory/tasks/zen` is not `memory/tasks/zenith`.
    #[must_use]
    pub fn holds_below(&self, folder: &str) -> bool {
        let below = format!("{folder}/");
        self.tasks
            .range::<str, _>((std::ops::Bound::Included(below.as_str()), std::ops::Bound::Unbounded))
            .next()
            .is_some_and(|(path, _)| path.starts_with(&below))
    }

    /// The vault-relative paths of the tracked tasks below the folder `folder` (no trailing `/`), by the same
    /// whole-component test as [`Self::holds_below`].
    #[must_use]
    pub fn paths_below(&self, folder: &str) -> Vec<String> {
        let below = format!("{folder}/");
        self.tasks
            .range::<str, _>((std::ops::Bound::Included(below.as_str()), std::ops::Bound::Unbounded))
            .take_while(|(path, _)| path.starts_with(&below))
            .map(|(path, _)| path.clone())
            .collect()
    }

    /// The number of tasks tracked.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    /// Reports whether no task is tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Reads the table from `path`; `None` when the file does not exist, which is how a new vault starts.
    ///
    /// # Errors
    ///
    /// Returns [`EventsError::TableUnreadable`] when the file cannot be read, is not a table, or is of a version this
    /// build does not read. The caller starts again from an empty table and so seeds silently.
    pub fn load(path: &Path) -> Result<Option<Self>, EventsError> {
        match read_json::<Self>(path)? {
            Some(table) if table.version != TABLE_VERSION => Err(EventsError::TableUnreadable {
                reason: format!("version {} is not the supported {TABLE_VERSION}", table.version),
            }),
            table => Ok(table),
        }
    }

    /// Writes the table to `path` atomically, creating its folder when missing.
    ///
    /// # Errors
    ///
    /// Returns [`EventsError::TableUnwritable`] when the table cannot be encoded or written; `path` is unchanged.
    pub fn store(&self, path: &Path) -> Result<(), EventsError> {
        write_json(path, self)
    }

    /// Records what a completed write did to the task at `site` and returns the events it fires, in the order
    /// created, state changed, reassigned, claim lapsed, unknown state, all stamped `now`; a move fires `task.moved`.
    ///
    /// A task the table has not seen is recorded without an event, and a delete, or a move of a task the table has
    /// not seen, fires none.
    pub fn apply(&mut self, site: TaskSite<'_>, change: TaskChange<'_>, now: OffsetDateTime) -> Vec<EventRecord> {
        let TaskSite { base, path } = site;
        match change {
            TaskChange::Created {
                task,
                origin,
                declared_states,
            } => {
                let narrator = Narrator {
                    base,
                    path,
                    task: &task,
                    now,
                };
                let mut events = vec![narrator.created(origin.as_deref())];
                events.extend(narrator.unknown_state(declared_states));
                self.tasks.insert(path.to_owned(), task);
                events
            }
            TaskChange::Updated { task, declared_states } => {
                let events = self.tasks.get(path).map(|previous| {
                    Narrator {
                        base,
                        path,
                        task: &task,
                        now,
                    }
                    .changes(previous, declared_states)
                });
                self.tasks.insert(path.to_owned(), task);
                events.unwrap_or_default()
            }
            TaskChange::Moved { from } => {
                if from == path {
                    return Vec::new();
                }
                let Some(entry) = self.tasks.remove(from) else {
                    return Vec::new();
                };
                let moved = Narrator {
                    base,
                    path,
                    task: &entry,
                    now,
                }
                .moved(from);
                self.tasks.insert(path.to_owned(), entry);
                vec![moved]
            }
            TaskChange::Deleted => {
                self.tasks.remove(path);
                Vec::new()
            }
        }
    }
}

/// Builds the events about one task at one moment.
struct Narrator<'a> {
    base: &'a str,
    path: &'a str,
    task: &'a TrackedTask,
    now: OffsetDateTime,
}

impl Narrator<'_> {
    fn changes(&self, previous: &TrackedTask, declared_states: Option<&[String]>) -> Vec<EventRecord> {
        let mut events = Vec::new();
        let state_changed = previous.state != self.task.state;
        if state_changed {
            events.push(self.state_changed(previous));
        }
        if previous.assignee != self.task.assignee {
            events.push(self.reassigned(previous));
        }
        if self.has_lapsed(previous) {
            events.push(self.claim_lapsed(previous));
        }
        if state_changed {
            events.extend(self.unknown_state(declared_states));
        }
        events
    }

    /// A claim lapsed when it had expired by now and a write claims the task afresh or takes it out of `doing`.
    fn has_lapsed(&self, previous: &TrackedTask) -> bool {
        let (Some(_), Some(until)) = (&previous.claimed_by, previous.claimed_until) else {
            return false;
        };
        let claims_afresh = self.task.claimed_by.is_some()
            && (self.task.claimed_by != previous.claimed_by || self.task.claimed_until != previous.claimed_until);
        let leaves_doing = previous.state.as_deref() == Some(DOING) && self.task.state.as_deref() != Some(DOING);
        until < self.now && (claims_afresh || leaves_doing)
    }

    /// The name a heading and file name use for the task: its id, else its note's name.
    fn name(&self) -> String {
        self.task.id.clone().unwrap_or_else(|| {
            let file = self.path.rsplit('/').next().unwrap_or(self.path);
            file.strip_suffix(".md").unwrap_or(file).to_owned()
        })
    }

    fn event(&self, kind: ServerKind, fields: Vec<(&str, Option<FieldValue>)>, heading: String) -> EventRecord {
        EventRecord {
            kind: KindName::from(kind),
            severity: kind.default_severity(),
            at: self.now,
            entity: resolve_entity(self.task.entity.as_deref(), None, None),
            subject: Some(match self.task.id.as_deref() {
                Some(id) => EventSubject::for_record(self.path, id, self.base),
                None => EventSubject::for_note(self.path),
            }),
            actor: self.task.claimed_by.clone(),
            fields: fields
                .into_iter()
                .filter_map(|(name, value)| Some((FieldName::try_from(name).ok()?, value?)))
                .collect(),
            heading,
            content: None,
        }
    }

    fn created(&self, origin: Option<&str>) -> EventRecord {
        self.event(
            ServerKind::TaskCreated,
            vec![
                ("task_type", text(self.task.task_type.as_deref())),
                ("state", text(self.task.state.as_deref())),
                ("assignee", text(self.task.assignee.as_deref())),
                ("origin", text(origin)),
            ],
            format!("{} created", self.name()),
        )
    }

    fn moved(&self, from: &str) -> EventRecord {
        self.event(
            ServerKind::TaskMoved,
            vec![("from", text(Some(from))), ("to", text(Some(self.path)))],
            format!("{} moved to {}", self.name(), folder_of(self.path)),
        )
    }

    fn state_changed(&self, previous: &TrackedTask) -> EventRecord {
        let heading = match (&previous.state, &self.task.state) {
            (Some(from), Some(to)) => format!("{}: {from} \u{2192} {to}", self.name()),
            (None, Some(to)) => format!("{}: \u{2192} {to}", self.name()),
            (Some(from), None) => format!("{}: {from} \u{2192} none", self.name()),
            (None, None) => self.name(),
        };
        self.event(
            ServerKind::TaskStateChanged,
            vec![
                ("from", text(previous.state.as_deref())),
                ("to", text(self.task.state.as_deref())),
                ("task_type", text(self.task.task_type.as_deref())),
                ("assignee", text(self.task.assignee.as_deref())),
                ("claimed_by", text(self.task.claimed_by.as_deref())),
            ],
            heading,
        )
    }

    fn reassigned(&self, previous: &TrackedTask) -> EventRecord {
        let shown = |value: &Option<String>| value.clone().unwrap_or_else(|| "nobody".to_owned());
        self.event(
            ServerKind::TaskReassigned,
            vec![
                ("from", text(previous.assignee.as_deref())),
                ("to", text(self.task.assignee.as_deref())),
            ],
            format!(
                "{}: reassigned {} \u{2192} {}",
                self.name(),
                shown(&previous.assignee),
                shown(&self.task.assignee)
            ),
        )
    }

    fn claim_lapsed(&self, previous: &TrackedTask) -> EventRecord {
        let until = previous.claimed_until.map(rfc3339_utc);
        self.event(
            ServerKind::TaskClaimLapsed,
            vec![
                ("claimed_by", text(previous.claimed_by.as_deref())),
                ("claimed_until", text(until.as_deref())),
            ],
            format!(
                "{}: claim by {} lapsed",
                self.name(),
                previous.claimed_by.as_deref().unwrap_or("nobody")
            ),
        )
    }

    /// An event when the task is in a state its type does not declare. Silent while the type's states are unknown.
    fn unknown_state(&self, declared_states: Option<&[String]>) -> Option<EventRecord> {
        let state = self.task.state.as_deref()?;
        if declared_states?.iter().any(|declared| declared == state) {
            return None;
        }
        Some(self.event(
            ServerKind::TaskUnknownState,
            vec![
                ("state", text(Some(state))),
                ("task_type", text(self.task.task_type.as_deref())),
            ],
            format!("{}: unknown state {state}", self.name()),
        ))
    }
}

/// The folder of a vault-relative path, or the vault root's name for one directly in it.
fn folder_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("the vault root", |(folder, _)| folder)
}

fn text(value: Option<&str>) -> Option<FieldValue> {
    value.map(FieldValue::from)
}

#[cfg(test)]
#[path = "tasks_test.rs"]
mod tests;
