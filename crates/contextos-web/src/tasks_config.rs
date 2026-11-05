//! `.contextos/tasks.toml`: how the Command Centre Tasks screens read this vault's tasks. Read through an ordinary MCP
//! tool call, like every other `.contextos/`-rooted file, and written only by the Vault Settings Tasks editor
//! (`routes::vault_settings_tasks`), which validates the whole configuration first and keeps the file's comments.
//!
//! Four concerns: the task types, each with the id series it mints from when the tasks sidecar declares it, the state
//! its new tasks start in, and its own lifecycle of stages (the stage strip's tiles and the State choices, each with
//! an optional Focus bucket and whether it closes a task); the shared stages, the lifecycle of every type that declares
//! none of its own; display labels for the `parent`-tree hierarchy view's depths; and the priorities, most urgent
//! first, which a priority sort follows.
//!
//! An absent or malformed file degrades to [`TasksConfig::default`], as does a file that declares no `[[stage]]`
//! entries, and an absent `[[task_type]]` or `[[priority]]` list takes the shipped vocabulary. An unknown key counts
//! as malformed, so a save from the editor can never silently drop it.

use serde::{Deserialize, Serialize};
use toml_edit::DocumentMut;

use crate::mcp_client::McpClient;
use crate::vault_content::read_full_text;

const TASKS_TOML_PATH: &str = ".contextos/tasks.toml";

/// The `base_query` filter that selects the vault's task notes: every note under `memory/tasks` except the
/// directory's own index. Shared by the Tasks screen's row fetch and the sidebar's open-task count so the two can
/// never disagree about what a task is.
pub(crate) const TASK_NOTES_FILTER: &str = "file.inFolder(\"memory/tasks\") && file.basename != \"index\"";

/// The Focus view's four fixed triage sections a configured stage may
/// belong to. Distinct from a stage's own `state`/`label`/`icon`: an
/// operator's stage vocabulary is arbitrary and vault-specific, but the
/// Focus view's own layout is fixed to these four named groupings, so each
/// stage opts into at most one of them. A stage with no configured bucket
/// is simply excluded from Focus, never an error and never a forced guess
/// at an unfamiliar vocabulary's own intent.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Bucket {
    /// Work currently in flight ("Now, in flight").
    Now,
    /// Work waiting on a decision or input ("Needs your call").
    NeedsCall,
    /// Not yet started ("Backlog").
    Backlog,
    /// Finished ("Done").
    Done,
}

/// The colour a stage's state chip is shown in, from a fixed palette an operator picks by name in the Vault Settings
/// Tasks editor. Each maps onto the interface's own theme tokens (`static/contextos-tokens.css`), so a chip stays
/// legible in the light and dark themes alike, and no colour value is ever written into the page.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StageColour {
    Grey,
    Faint,
    Blue,
    Green,
    Amber,
    Red,
    Purple,
    Teal,
}

impl StageColour {
    /// Every colour, in the order the editor offers them.
    pub const ALL: [Self; 8] = [
        Self::Grey,
        Self::Faint,
        Self::Blue,
        Self::Green,
        Self::Amber,
        Self::Red,
        Self::Purple,
        Self::Teal,
    ];

    /// The stable name `tasks.toml` and the chip's CSS class carry.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Grey => "grey",
            Self::Faint => "faint",
            Self::Blue => "blue",
            Self::Green => "green",
            Self::Amber => "amber",
            Self::Red => "red",
            Self::Purple => "purple",
            Self::Teal => "teal",
        }
    }

    /// The name the editor shows an operator.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Grey => "Grey",
            Self::Faint => "Faint grey",
            Self::Blue => "Blue",
            Self::Green => "Green",
            Self::Amber => "Amber",
            Self::Red => "Red",
            Self::Purple => "Purple",
            Self::Teal => "Teal",
        }
    }

    /// The colour the shipped lifecycles give `state`, for a stage configured without one: grey for any state they do
    /// not name.
    #[must_use]
    pub fn shipped_for(state: &str) -> Self {
        match state {
            "next" => Self::Blue,
            "doing" => Self::Green,
            "waiting" | "open" => Self::Amber,
            "proposed" => Self::Teal,
            "review" => Self::Purple,
            "dropped" => Self::Faint,
            _ => Self::Grey,
        }
    }
}

/// A colour named in the editor's submission, refused when it is not one of the palette's.
impl TryFrom<&str> for StageColour {
    type Error = UnknownStageColour;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::ALL
            .into_iter()
            .find(|colour| colour.slug() == value)
            .ok_or_else(|| UnknownStageColour(value.to_owned()))
    }
}

/// A colour name the palette does not have.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("`{0}` is not a stage colour; choose one of grey, faint, blue, green, amber, red, purple, or teal")]
pub struct UnknownStageColour(pub String);

/// Whether entering a stage closes a task, and which date the close stamps.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Closes {
    /// The task stays open.
    Never,
    /// The task is closed as finished: entering the stage stamps `completed`.
    Completed,
    /// The task is closed without being finished: entering the stage stamps `cancelled`.
    Cancelled,
}

impl Closes {
    /// Every value, in the order the editor offers them.
    pub const ALL: [Self; 3] = [Self::Never, Self::Completed, Self::Cancelled];

    /// Whether the shipped lifecycles close a task in `state`, for a stage configured without saying.
    #[must_use]
    pub fn shipped_for(state: &str) -> Self {
        match state {
            "done" | "adopted" | "answered" => Self::Completed,
            "dropped" => Self::Cancelled,
            _ => Self::Never,
        }
    }

    /// The stable name `tasks.toml` carries.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }

    /// The name the editor shows an operator.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Never => "Stays open",
            Self::Completed => "Closes, completed",
            Self::Cancelled => "Closes, cancelled",
        }
    }

    /// The date field entering the stage stamps, when it closes a task.
    #[must_use]
    pub const fn stamp(self) -> Option<&'static str> {
        match self {
            Self::Never => None,
            Self::Completed => Some("completed"),
            Self::Cancelled => Some("cancelled"),
        }
    }
}

/// A `closes` value named in the editor's submission, refused when it is not one of the three.
impl TryFrom<&str> for Closes {
    type Error = TasksConfigError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::ALL
            .into_iter()
            .find(|closes| closes.slug() == value)
            .ok_or_else(|| TasksConfigError::UnknownCloses(value.to_owned()))
    }
}

/// One stage of one task type, as a link names it: `decision:dropped`. `:doing` names the stage of a task whose type
/// is not declared, or which has none, since such a task follows the shared stages. A bare `dropped`, as links
/// written before each type had its own stages name it, matches that state in every type. Whether a task is in the
/// stage a key names is [`TasksConfig::in_stage`], since only the configuration knows which types are declared.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StageKey {
    pub task_type: Option<String>,
    pub state: String,
}

impl StageKey {
    /// The stage `state` of `task_type`.
    #[must_use]
    pub fn of(task_type: &str, state: &str) -> Self {
        Self {
            task_type: Some(task_type.to_owned()),
            state: state.to_owned(),
        }
    }

    /// The stage `state` of every task whose type is not declared, or which has none.
    #[must_use]
    pub fn other(state: &str) -> Self {
        Self::of("", state)
    }
}

impl From<&str> for StageKey {
    fn from(text: &str) -> Self {
        match text.split_once(':') {
            Some((task_type, state)) => Self {
                task_type: Some(task_type.to_owned()),
                state: state.to_owned(),
            },
            None => Self {
                task_type: None,
                state: text.to_owned(),
            },
        }
    }
}

impl std::fmt::Display for StageKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.task_type {
            Some(task_type) => write!(formatter, "{task_type}:{}", self.state),
            None => formatter.write_str(&self.state),
        }
    }
}

/// One flat stage-strip tile: a real `state` value, an optional
/// operator-supplied display label (falling back to `state` itself when
/// unset), an optional icon name (`static/icons/`-relative, matching every
/// other `SidebarLink`/nav icon's own convention), and an optional Focus
/// bucket.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StageEntry {
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bucket: Option<Bucket>,
    /// The colour the stage's state chip is shown in; the shipped one for its state when unset
    /// ([`StageColour::shipped_for`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<StageColour>,
    /// Whether entering the stage closes a task; the shipped answer for its state when unset ([`Closes::shipped_for`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closes: Option<Closes>,
}

impl StageEntry {
    /// The text a stage tile renders: the operator's own `label` when set,
    /// otherwise `state` verbatim.
    #[must_use]
    pub fn display_label(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.state)
    }

    /// Whether entering the stage closes a task: as configured, else the shipped answer for its state.
    #[must_use]
    pub fn closing(&self) -> Closes {
        self.closes.unwrap_or_else(|| Closes::shipped_for(&self.state))
    }
}

/// The `parent`-tree hierarchy view's own display configuration: nothing
/// beyond an optional label per depth. `depth_labels[0]` names the root
/// level, `depth_labels[1]` its children,
/// and so on; a depth beyond the configured list's own length renders with
/// no label at all, never an out-of-bounds panic or a repeated last label.
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HierarchyConfig {
    #[serde(default)]
    pub depth_labels: Vec<String>,
}

impl HierarchyConfig {
    /// The configured label for `depth` (0-based), or `None` when `depth`
    /// has no configured label.
    #[must_use]
    pub fn label_for_depth(&self, depth: usize) -> Option<&str> {
        self.depth_labels.get(depth).map(String::as_str)
    }
}

/// `.contextos/tasks.toml`'s full schema.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TasksConfig {
    /// The shared stages: the lifecycle of every task type that declares no stages of its own.
    #[serde(default = "default_stages")]
    pub stage: Vec<StageEntry>,
    pub hierarchy: HierarchyConfig,
    /// What a task can be (`task_type`), in the order the screens offer them.
    #[serde(default = "default_task_types")]
    pub task_type: Vec<TaskTypeEntry>,
    /// The `priority` values, most urgent first: the order a priority sort follows.
    #[serde(default = "default_priorities")]
    pub priority: Vec<PriorityEntry>,
}

impl Default for TasksConfig {
    fn default() -> Self {
        Self {
            stage: default_stages(),
            hierarchy: HierarchyConfig::default(),
            task_type: default_task_types(),
            priority: default_priorities(),
        }
    }
}

/// One `task_type` value.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskTypeEntry {
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The id series a task of this type takes its id from, when the tasks sidecar declares it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// The state a new task of this type starts in: one of its own open states. Its first open state when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial: Option<String>,
    /// The type's own lifecycle, in order. A type that declares none uses the shared stages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stage: Vec<StageEntry>,
}

impl TaskTypeEntry {
    #[must_use]
    pub fn display_label(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.value)
    }
}

/// One `priority` value.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PriorityEntry {
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl PriorityEntry {
    #[must_use]
    pub fn display_label(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.value)
    }
}

/// Why a Tasks configuration cannot be saved.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TasksConfigError {
    #[error("the task type `{0}` has no stages of its own, so it needs the shared stages, and there are none")]
    NoStages(String),
    #[error("at least one task type is required")]
    NoTaskTypes,
    #[error("`{0}` must be lower-case letters, digits, `-`, or `_`")]
    InvalidValue(String),
    #[error("the stage `{state}` is listed twice in {lifecycle}")]
    DuplicateStage { lifecycle: String, state: String },
    #[error("{0} have no state that leaves a task open, so a new task has nowhere to start")]
    NoOpenState(String),
    #[error("the task type `{0}` is listed twice")]
    DuplicateTaskType(String),
    #[error("the priority `{0}` is listed twice")]
    DuplicatePriority(String),
    #[error("the id prefix `{0}` must be 1 to 32 letters or digits, starting with a letter")]
    InvalidPrefix(String),
    #[error("the id prefix `{0}` is used by two task types")]
    DuplicatePrefix(String),
    #[error("the task type `{task_type}` starts new tasks in `{state}`, which is not one of its own open states")]
    InvalidInitialState { task_type: String, state: String },
    #[error("`{0}` is not a close: choose never, completed, or cancelled")]
    UnknownCloses(String),
    #[error("the configuration could not be written: {0}")]
    Write(String),
}

impl TasksConfigError {
    /// A stable machine-readable code for this failure.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NoStages(_) => "tasks-config/no-stages",
            Self::NoTaskTypes => "tasks-config/no-task-types",
            Self::InvalidValue(_) => "tasks-config/invalid-value",
            Self::DuplicateStage { .. } => "tasks-config/duplicate-stage",
            Self::NoOpenState(_) => "tasks-config/no-open-state",
            Self::DuplicateTaskType(_) => "tasks-config/duplicate-task-type",
            Self::DuplicatePriority(_) => "tasks-config/duplicate-priority",
            Self::InvalidPrefix(_) => "tasks-config/invalid-prefix",
            Self::DuplicatePrefix(_) => "tasks-config/duplicate-prefix",
            Self::InvalidInitialState { .. } => "tasks-config/invalid-initial-state",
            Self::UnknownCloses(_) => "tasks-config/unknown-closes",
            Self::Write(_) => "tasks-config/write",
        }
    }
}

/// A stored vocabulary value: what a task's frontmatter carries, so a plain lower-case token.
fn check_value(value: &str) -> Result<(), TasksConfigError> {
    let valid = !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_');
    if valid {
        Ok(())
    } else {
        Err(TasksConfigError::InvalidValue(value.to_owned()))
    }
}

fn first_duplicate<'a>(values: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let mut seen = std::collections::HashSet::new();
    values.into_iter().find(|value| !seen.insert(*value))
}

impl TasksConfig {
    /// The task type named `value`, matched without regard to ASCII case, if the vocabulary declares it.
    #[must_use]
    pub fn task_type(&self, value: &str) -> Option<&TaskTypeEntry> {
        self.task_type
            .iter()
            .find(|entry| entry.value.eq_ignore_ascii_case(value))
    }

    /// Whether the vocabulary declares `task_type`. A task whose type it does not declare, or which has none, follows
    /// the shared stages and is grouped with the others of its kind.
    #[must_use]
    pub fn is_declared(&self, task_type: &str) -> bool {
        self.task_type(task_type).is_some()
    }

    /// Whether a task of `task_type` in `state` is in the stage `key` names: its own type's, any type's for a bare
    /// key, or for `:state`, a type the vocabulary does not declare.
    #[must_use]
    pub fn in_stage(&self, key: &StageKey, task_type: &str, state: &str) -> bool {
        key.state == state
            && match key.task_type.as_deref() {
                None => true,
                Some("") => !self.is_declared(task_type),
                Some(own) => own.eq_ignore_ascii_case(task_type),
            }
    }

    /// The lifecycle of a task of `task_type`: the type's own stages, else the shared ones, which also serve a type
    /// the vocabulary does not declare and a task with no type.
    #[must_use]
    pub fn lifecycle(&self, task_type: &str) -> &[StageEntry] {
        self.task_type(task_type)
            .filter(|entry| !entry.stage.is_empty())
            .map_or(self.stage.as_slice(), |entry| entry.stage.as_slice())
    }

    /// Every declared type with its lifecycle, in declared order: what the stage strip groups its tiles by.
    #[must_use]
    pub fn lifecycles(&self) -> Vec<(&TaskTypeEntry, &[StageEntry])> {
        self.task_type
            .iter()
            .map(|entry| (entry, self.lifecycle(&entry.value)))
            .collect()
    }

    /// The stage a task of `task_type` in `state` is in, when its type's lifecycle declares that state.
    #[must_use]
    pub fn stage_of(&self, task_type: &str, state: &str) -> Option<&StageEntry> {
        self.lifecycle(task_type).iter().find(|entry| entry.state == state)
    }

    /// The colour a task in `state` is shown in: its stage's own, else the shipped one for that state, else grey.
    #[must_use]
    pub fn stage_colour(&self, task_type: &str, state: &str) -> StageColour {
        self.stage_of(task_type, state)
            .and_then(|entry| entry.colour)
            .unwrap_or_else(|| StageColour::shipped_for(state))
    }

    /// The Focus bucket a task of `task_type` in `state` belongs to, if any.
    #[must_use]
    pub fn bucket_of(&self, task_type: &str, state: &str) -> Option<Bucket> {
        self.stage_of(task_type, state).and_then(|entry| entry.bucket)
    }

    /// Every declared type's stage bucketed to `bucket`, type by type in declared order. The Focus view's bucketed
    /// sections read this rather than scanning the lifecycles themselves, so a stage's bucket has exactly one
    /// interpretation across the whole screen.
    #[must_use]
    pub fn stage_keys_in_bucket(&self, bucket: Bucket) -> Vec<StageKey> {
        self.lifecycles()
            .into_iter()
            .flat_map(|(entry, stages)| {
                stages
                    .iter()
                    .filter(move |stage| stage.bucket == Some(bucket))
                    .map(|stage| StageKey::of(&entry.value, &stage.state))
            })
            .collect()
    }

    /// Whether entering `state` closes a task of `task_type`, and how; `None` when its type does not declare the state.
    #[must_use]
    pub fn closing(&self, task_type: &str, state: &str) -> Option<Closes> {
        self.stage_of(task_type, state).map(StageEntry::closing)
    }

    /// Whether a task of `task_type` in `state` is open: its type declares the state and the state closes nothing. A
    /// task in a state its type does not declare is not counted: an unfamiliar state is never guessed at.
    #[must_use]
    pub fn is_open(&self, task_type: &str, state: &str) -> bool {
        self.closing(task_type, state) == Some(Closes::Never)
    }

    /// The state a new task of `task_type` starts in: the type's `initial` state when it is one of the type's open
    /// states, else the first open state of its lifecycle.
    #[must_use]
    pub fn initial_state(&self, task_type: &str) -> Option<&str> {
        self.task_type(task_type)
            .and_then(|entry| entry.initial.as_deref())
            .filter(|state| self.is_open(task_type, state))
            .or_else(|| {
                self.lifecycle(task_type)
                    .iter()
                    .find(|stage| stage.closing() == Closes::Never)
                    .map(|stage| stage.state.as_str())
            })
    }

    /// The key a priority sort orders by: declared priorities in their declared order (most urgent first), then any
    /// undeclared value alphabetically, then no priority at all.
    #[must_use]
    pub fn priority_sort_key(&self, value: &str) -> (u8, usize, String) {
        if value.is_empty() {
            return (2, 0, String::new());
        }
        match self.priority.iter().position(|entry| entry.value == value) {
            Some(rank) => (0, rank, String::new()),
            None => (1, 0, value.to_ascii_lowercase()),
        }
    }

    /// Checks everything a save must satisfy: at least one task type; every lifecycle's states plain tokens listed
    /// once, with at least one that leaves a task open; shared stages for every type without its own; every other
    /// stored value a plain token listed once; every id prefix well formed and used once; and every initial state one
    /// of its type's own open states.
    ///
    /// # Errors
    ///
    /// Returns the first problem found.
    pub fn validate(&self) -> Result<(), TasksConfigError> {
        if self.task_type.is_empty() {
            return Err(TasksConfigError::NoTaskTypes);
        }
        for entry in &self.task_type {
            check_value(&entry.value)?;
        }
        if self.stage.is_empty()
            && let Some(entry) = self.task_type.iter().find(|entry| entry.stage.is_empty())
        {
            return Err(TasksConfigError::NoStages(entry.value.clone()));
        }
        let own = self
            .task_type
            .iter()
            .filter(|entry| !entry.stage.is_empty())
            .map(|entry| (format!("the {} stages", entry.value), entry.stage.as_slice()));
        let shared = (!self.stage.is_empty()).then(|| ("the shared stages".to_owned(), self.stage.as_slice()));
        for (lifecycle, stages) in shared.into_iter().chain(own) {
            for stage in stages {
                check_value(&stage.state)?;
            }
            if let Some(state) = first_duplicate(stages.iter().map(|stage| stage.state.as_str())) {
                return Err(TasksConfigError::DuplicateStage {
                    lifecycle,
                    state: state.to_owned(),
                });
            }
            if stages.iter().all(|stage| stage.closing() != Closes::Never) {
                return Err(TasksConfigError::NoOpenState(lifecycle));
            }
        }
        let values = self
            .task_type
            .iter()
            .map(|entry| entry.value.as_str())
            .chain(self.priority.iter().map(|entry| entry.value.as_str()));
        for value in values {
            check_value(value)?;
        }
        if let Some(value) = first_duplicate(self.task_type.iter().map(|entry| entry.value.as_str())) {
            return Err(TasksConfigError::DuplicateTaskType(value.to_owned()));
        }
        if let Some(value) = first_duplicate(self.priority.iter().map(|entry| entry.value.as_str())) {
            return Err(TasksConfigError::DuplicatePriority(value.to_owned()));
        }
        for entry in &self.task_type {
            if let Some(prefix) = &entry.prefix {
                let valid = (1..=32).contains(&prefix.len())
                    && prefix.starts_with(|ch: char| ch.is_ascii_alphabetic())
                    && prefix.bytes().all(|byte| byte.is_ascii_alphanumeric());
                if !valid {
                    return Err(TasksConfigError::InvalidPrefix(prefix.clone()));
                }
            }
            if let Some(state) = &entry.initial
                && !self.is_open(&entry.value, state)
            {
                return Err(TasksConfigError::InvalidInitialState {
                    task_type: entry.value.clone(),
                    state: state.clone(),
                });
            }
        }
        if let Some(prefix) = first_duplicate(self.task_type.iter().filter_map(|entry| entry.prefix.as_deref())) {
            return Err(TasksConfigError::DuplicatePrefix(prefix.to_owned()));
        }
        Ok(())
    }

    /// `source` (the file's current text, or empty) with this configuration's four sections written over it, every
    /// comment and unrelated key kept.
    ///
    /// # Errors
    ///
    /// Returns [`TasksConfigError::Write`] when `source` is not valid TOML or this configuration cannot be serialised.
    pub fn written_over(&self, source: &str) -> Result<String, TasksConfigError> {
        let mut document = source
            .parse::<DocumentMut>()
            .map_err(|error| TasksConfigError::Write(error.message().to_owned()))?;
        let rendered = toml::to_string(self).map_err(|error| TasksConfigError::Write(error.to_string()))?;
        let fresh = rendered
            .parse::<DocumentMut>()
            .map_err(|error| TasksConfigError::Write(error.message().to_owned()))?;
        for key in ["stage", "hierarchy", "task_type", "priority"] {
            // A comment written above a section belongs to that section's first table in the parsed document, so it
            // is carried over to the section that replaces it rather than discarded with the old one.
            let leading = document.remove(key).and_then(|old| leading_decor(&old));
            if let Some(item) = fresh.get(key) {
                let mut item = item.clone();
                if let Some(prefix) = leading {
                    set_leading_decor(&mut item, prefix);
                }
                document.insert(key, item);
            }
        }
        Ok(document.to_string())
    }

    /// The display label `task_type`'s lifecycle gives `state`, or `state` itself when it does not declare it (an
    /// operator's own vocabulary is always shown verbatim, never silently dropped for lacking a stage entry).
    #[must_use]
    pub fn label_for_state<'a>(&'a self, task_type: &str, state: &'a str) -> &'a str {
        self.stage_of(task_type, state).map_or(state, StageEntry::display_label)
    }
}

/// One shipped stage.
fn stage(state: &str, label: &str, icon: &str, bucket: Option<Bucket>, colour: StageColour) -> StageEntry {
    let closes = Closes::shipped_for(state);
    StageEntry {
        state: state.to_owned(),
        label: Some(label.to_owned()),
        icon: Some(icon.to_owned()),
        bucket,
        colour: Some(colour),
        closes: (closes != Closes::Never).then_some(closes),
    }
}

/// The shipped action lifecycle, also the shared stages: the fallback whenever `.contextos/tasks.toml` is absent,
/// malformed, or declares no `[[stage]]` entries of its own. Two states share the Backlog bucket (`backlog`, `next`)
/// and two share Needs your call (`waiting`, held work awaiting a response, and `review`, an agent's finished work
/// awaiting a person's check before it is done): the Focus view's bucketed sections show every row across every
/// stage in a bucket, not just one, precisely because a lifecycle needs more than one stage per triage grouping.
/// `dropped` gets no bucket: a dropped task is neither in flight, awaiting a call, backlogged, nor done, so forcing it
/// into one of the four sections would misrepresent it; it remains visible in the Table and Hierarchy views.
fn default_stages() -> Vec<StageEntry> {
    vec![
        stage("backlog", "Backlog", "inbox", Some(Bucket::Backlog), StageColour::Grey),
        stage("next", "Next", "circle", Some(Bucket::Backlog), StageColour::Blue),
        stage("doing", "Doing", "loader", Some(Bucket::Now), StageColour::Green),
        stage(
            "waiting",
            "Waiting",
            "clock",
            Some(Bucket::NeedsCall),
            StageColour::Amber,
        ),
        stage(
            "review",
            "Review",
            "list-checks",
            Some(Bucket::NeedsCall),
            StageColour::Purple,
        ),
        stage("done", "Done", "check", Some(Bucket::Done), StageColour::Grey),
        stage("dropped", "Dropped", "circle", None, StageColour::Faint),
    ]
}

/// A decision is raised, then made or abandoned. One awaiting its call needs the operator's.
fn decision_stages() -> Vec<StageEntry> {
    vec![
        stage(
            "proposed",
            "Proposed",
            "scale",
            Some(Bucket::NeedsCall),
            StageColour::Teal,
        ),
        stage("adopted", "Adopted", "check", Some(Bucket::Done), StageColour::Grey),
        stage("dropped", "Dropped", "circle", None, StageColour::Faint),
    ]
}

/// A question is asked, then answered or abandoned. One awaiting its answer needs the operator's call.
fn question_stages() -> Vec<StageEntry> {
    vec![
        stage(
            "open",
            "Open",
            "circle-help",
            Some(Bucket::NeedsCall),
            StageColour::Amber,
        ),
        stage("answered", "Answered", "check", Some(Bucket::Done), StageColour::Grey),
        stage("dropped", "Dropped", "circle", None, StageColour::Faint),
    ]
}

/// The text written above `item` (comments and blank lines), when it has any.
fn leading_decor(item: &toml_edit::Item) -> Option<toml_edit::RawString> {
    let prefix = match item {
        toml_edit::Item::Table(table) => table.decor().prefix(),
        toml_edit::Item::ArrayOfTables(tables) => tables.get(0).and_then(|table| table.decor().prefix()),
        toml_edit::Item::None | toml_edit::Item::Value(_) => None,
    }?;
    (!prefix.as_str().is_some_and(|text| text.trim().is_empty())).then(|| prefix.clone())
}

fn set_leading_decor(item: &mut toml_edit::Item, prefix: toml_edit::RawString) {
    match item {
        toml_edit::Item::Table(table) => table.decor_mut().set_prefix(prefix),
        toml_edit::Item::ArrayOfTables(tables) => {
            if let Some(table) = tables.get_mut(0) {
                table.decor_mut().set_prefix(prefix);
            }
        }
        toml_edit::Item::None | toml_edit::Item::Value(_) => {}
    }
}

/// The shipped task types, each with the id series the shipped tasks sidecar declares for it: an action, on the shared
/// stages and starting in `next`; a decision, starting `proposed`; and a question, starting `open`.
fn default_task_types() -> Vec<TaskTypeEntry> {
    [
        ("action", "Action", "A", "next", Vec::new()),
        ("decision", "Decision", "D", "proposed", decision_stages()),
        ("question", "Question", "Q", "open", question_stages()),
    ]
    .into_iter()
    .map(|(value, label, prefix, initial, stage)| TaskTypeEntry {
        value: value.to_owned(),
        label: Some(label.to_owned()),
        prefix: Some(prefix.to_owned()),
        initial: Some(initial.to_owned()),
        stage,
    })
    .collect()
}

/// The shipped priorities, most urgent first.
fn default_priorities() -> Vec<PriorityEntry> {
    [
        ("highest", "Highest"),
        ("high", "High"),
        ("medium", "Medium"),
        ("normal", "Normal"),
        ("low", "Low"),
    ]
    .into_iter()
    .map(|(value, label)| PriorityEntry {
        value: value.to_owned(),
        label: Some(label.to_owned()),
    })
    .collect()
}

/// Loads `vault_name`'s configured Tasks display settings. Never fails
/// outright: an absent file, an unreachable MCP session, or malformed TOML
/// all degrade to [`TasksConfig::default`], not an error, matching
/// `agents.rs`'s identical `load_agents` contract.
pub async fn load_tasks_config(client: &McpClient, vault_name: &str) -> TasksConfig {
    let Ok(Some(read)) = read_full_text(client, &format!("{vault_name}://{TASKS_TOML_PATH}")).await else {
        return TasksConfig::default();
    };
    toml::from_str::<TasksConfig>(&read).unwrap_or_default()
}

/// Loads `vault_name`'s Tasks configuration for a write that must be checked against it: the shipped defaults when
/// there is no `.contextos/tasks.toml`, but never when the file exists and cannot be read, since checking a state
/// against defaults the operator did not choose would refuse their own states and accept ones they removed.
///
/// # Errors
///
/// Returns why the configuration could not be used, ready to show the operator.
pub async fn load_tasks_config_for_write(client: &McpClient, vault_name: &str) -> Result<TasksConfig, String> {
    match read_full_text(client, &format!("{vault_name}://{TASKS_TOML_PATH}")).await {
        Ok(None) => Ok(TasksConfig::default()),
        Ok(Some(text)) => toml::from_str::<TasksConfig>(&text).map_err(|error| {
            format!(
                "{TASKS_TOML_PATH} could not be read ({}), so the task's states cannot be checked. Fix it in Vault \
                 Settings, Tasks",
                error.message().trim()
            )
        }),
        Err(_) => Err(format!(
            "{TASKS_TOML_PATH} could not be reached, so the task's states cannot be checked"
        )),
    }
}

#[cfg(test)]
#[path = "tasks_config_test.rs"]
mod tests;
