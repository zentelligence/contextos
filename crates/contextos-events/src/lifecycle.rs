//! The states each task type declares, read from `.contextos/tasks.toml`, so a task written in a state its type does
//! not declare can be told from one that is simply in a different state.

use serde::Deserialize;

use crate::EventsError;

/// The action lifecycle, which also serves as the shared stages when a vault declares none.
const SHIPPED_SHARED: [&str; 7] = ["backlog", "next", "doing", "waiting", "review", "done", "dropped"];

/// The decision lifecycle of the shipped vocabulary.
const SHIPPED_DECISION: [&str; 3] = ["proposed", "adopted", "dropped"];

/// The question lifecycle of the shipped vocabulary.
const SHIPPED_QUESTION: [&str; 3] = ["open", "answered", "dropped"];

fn owned(states: &[&str]) -> Vec<String> {
    states.iter().map(|state| (*state).to_owned()).collect()
}

/// The subset of `tasks.toml` that names states. Every other key is ignored, because only the names matter here.
#[derive(Debug, Default, Deserialize)]
struct TasksFile {
    #[serde(default)]
    stage: Vec<Stage>,
    task_type: Option<Vec<TaskTypeEntry>>,
}

#[derive(Debug, Deserialize)]
struct Stage {
    state: String,
}

#[derive(Debug, Deserialize)]
struct TaskTypeEntry {
    value: String,
    #[serde(default)]
    stage: Vec<Stage>,
}

/// One declared task type: its own states, or none when it follows the shared stages.
#[derive(Clone, Debug, Eq, PartialEq)]
struct TypeLifecycle {
    value: String,
    own: Vec<String>,
}

/// The states each task type declares, with the shared stages that serve every type declaring none of its own, an
/// undeclared type, and a task with no type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskLifecycles {
    shared: Vec<String>,
    types: Vec<TypeLifecycle>,
}

impl Default for TaskLifecycles {
    /// The shipped vocabulary: the action lifecycle shared, with decisions and questions on their own.
    fn default() -> Self {
        Self {
            shared: owned(&SHIPPED_SHARED),
            types: vec![
                TypeLifecycle {
                    value: "action".to_owned(),
                    own: Vec::new(),
                },
                TypeLifecycle {
                    value: "decision".to_owned(),
                    own: owned(&SHIPPED_DECISION),
                },
                TypeLifecycle {
                    value: "question".to_owned(),
                    own: owned(&SHIPPED_QUESTION),
                },
            ],
        }
    }
}

impl TryFrom<&str> for TaskLifecycles {
    type Error = EventsError;

    /// Reads the states from the text of `.contextos/tasks.toml`. Absent `[[stage]]` or `[[task_type]]` lists keep the
    /// shipped ones.
    ///
    /// # Errors
    ///
    /// Returns [`EventsError::TasksConfigUnreadable`] when the text is not TOML or a stage has no `state`.
    fn try_from(text: &str) -> Result<Self, EventsError> {
        let file: TasksFile = toml::from_str(text).map_err(|error| EventsError::TasksConfigUnreadable {
            reason: error.message().to_owned(),
        })?;
        let shipped = Self::default();
        let shared = if file.stage.is_empty() {
            shipped.shared
        } else {
            file.stage.into_iter().map(|stage| stage.state).collect()
        };
        let types = match file.task_type {
            None => shipped.types,
            Some(entries) => entries
                .into_iter()
                .map(|entry| TypeLifecycle {
                    value: entry.value,
                    own: entry.stage.into_iter().map(|stage| stage.state).collect(),
                })
                .collect(),
        };
        Ok(Self { shared, types })
    }
}

impl TaskLifecycles {
    /// The states a task of `task_type` may be in: the type's own when it declares any, else the shared stages, which
    /// also serve an undeclared type and a task with no type. Types are matched without regard to ASCII case.
    #[must_use]
    pub fn declared_states(&self, task_type: Option<&str>) -> &[String] {
        task_type
            .and_then(|wanted| self.types.iter().find(|entry| entry.value.eq_ignore_ascii_case(wanted)))
            .filter(|entry| !entry.own.is_empty())
            .map_or(self.shared.as_slice(), |entry| entry.own.as_slice())
    }
}

#[cfg(test)]
#[path = "lifecycle_test.rs"]
mod tests;
