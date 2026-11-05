//! The Tasks category of `/{vault}/settings/`: a structured editor for `.contextos/tasks.toml`'s four parts, the task
//! types (with the id series each mints from, the state its new tasks start in, and its own stages or the shared
//! ones), the shared stages (state, label, icon, Focus bucket, colour, and whether it closes a task), the hierarchy
//! view's depth labels, and the priorities, most urgent first.
//!
//! Editing is driven by the server with no client script: each row's fields submit under an indexed name
//! (`stage.0.state`, or `task_type.1.stage.0.state` for a type's own), and the Add, Remove, Move up, and Move down
//! controls are submit buttons naming an `edit_action` (`add:stage`, `remove:priority:2`, `up:task_type:1`,
//! `down:type_stage:1:0`), as are the buttons that give a type its own stages or return it to the shared ones
//! (`own:task_type:1`, `share:task_type:1`). An edit action returns the changed draft without writing anything; only
//! Save validates the whole configuration and writes it, conflict-checked against the version the page was rendered
//! from, with every comment and unrelated key in the file kept.

use std::collections::BTreeMap;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value};

use crate::mcp_client::{McpCallError, McpClient};
use crate::routes::vault::{html_response, unreachable};
use crate::tasks_config::{
    Bucket, Closes, HierarchyConfig, PriorityEntry, StageColour, StageEntry, TaskTypeEntry, TasksConfig,
    TasksConfigError,
};
use crate::vault_content::{AttachedRead, read_attached};
use crate::vault_relative_path::VaultRelativePath;
use crate::vault_write::{WriteOutcome, write_text};

use super::vault_settings::{ConfigSections, render_current, render_sections};

pub(crate) const TASKS_TOML_PATH: &str = ".contextos/tasks.toml";

/// One stage row as typed.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct StageDraft {
    state: String,
    label: String,
    icon: String,
    bucket: String,
    colour: String,
    closes: String,
}

impl StageDraft {
    /// Whether the stage closes a task, as typed, else the shipped answer for its state.
    fn closing(&self) -> Closes {
        Closes::try_from(self.closes.as_str()).unwrap_or_else(|_| Closes::shipped_for(self.state.trim()))
    }
}

/// One task type row as typed, with the type's own stages, empty when it uses the shared ones.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct TypeDraft {
    value: String,
    label: String,
    prefix: String,
    initial: String,
    stages: Vec<StageDraft>,
}

/// One priority row as typed.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct PriorityDraft {
    value: String,
    label: String,
}

/// The Tasks configuration as the form holds it: every field as typed, before validation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct TasksDraft {
    stages: Vec<StageDraft>,
    depth_labels: Vec<String>,
    task_types: Vec<TypeDraft>,
    priorities: Vec<PriorityDraft>,
}

const BUCKETS: &[(&str, &str)] = &[
    ("now", "Now, in flight"),
    ("needs_call", "Needs your call"),
    ("backlog", "Backlog"),
    ("done", "Done"),
];

impl From<Bucket> for &'static str {
    fn from(bucket: Bucket) -> Self {
        match bucket {
            Bucket::Now => "now",
            Bucket::NeedsCall => "needs_call",
            Bucket::Backlog => "backlog",
            Bucket::Done => "done",
        }
    }
}

impl From<&StageEntry> for StageDraft {
    fn from(stage: &StageEntry) -> Self {
        Self {
            state: stage.state.clone(),
            label: stage.label.clone().unwrap_or_default(),
            icon: stage.icon.clone().unwrap_or_default(),
            bucket: stage.bucket.map(<&str>::from).unwrap_or_default().to_owned(),
            colour: stage.colour.map(StageColour::slug).unwrap_or_default().to_owned(),
            closes: stage.closes.map(Closes::slug).unwrap_or_default().to_owned(),
        }
    }
}

impl From<&TasksConfig> for TasksDraft {
    fn from(config: &TasksConfig) -> Self {
        Self {
            stages: config.stage.iter().map(StageDraft::from).collect(),
            depth_labels: config.hierarchy.depth_labels.clone(),
            task_types: config
                .task_type
                .iter()
                .map(|entry| TypeDraft {
                    value: entry.value.clone(),
                    label: entry.label.clone().unwrap_or_default(),
                    prefix: entry.prefix.clone().unwrap_or_default(),
                    initial: entry.initial.clone().unwrap_or_default(),
                    stages: entry.stage.iter().map(StageDraft::from).collect(),
                })
                .collect(),
            priorities: config
                .priority
                .iter()
                .map(|entry| PriorityDraft {
                    value: entry.value.clone(),
                    label: entry.label.clone().unwrap_or_default(),
                })
                .collect(),
        }
    }
}

fn optional(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

impl TryFrom<&StageDraft> for StageEntry {
    type Error = TasksConfigError;

    /// A bucket, colour, or close the form cannot have offered is reported as an invalid value, never guessed at.
    fn try_from(row: &StageDraft) -> Result<Self, Self::Error> {
        let bucket = match row.bucket.as_str() {
            "" => None,
            "now" => Some(Bucket::Now),
            "needs_call" => Some(Bucket::NeedsCall),
            "backlog" => Some(Bucket::Backlog),
            "done" => Some(Bucket::Done),
            other => return Err(TasksConfigError::InvalidValue(other.to_owned())),
        };
        let colour = match row.colour.as_str() {
            "" => None,
            other => Some(StageColour::try_from(other).map_err(|_| TasksConfigError::InvalidValue(other.to_owned()))?),
        };
        let closes = match row.closes.as_str() {
            "" => None,
            other => Some(Closes::try_from(other)?),
        };
        Ok(Self {
            state: row.state.trim().to_owned(),
            label: optional(&row.label),
            icon: optional(&row.icon),
            bucket,
            colour,
            closes,
        })
    }
}

impl TryFrom<&TasksDraft> for TasksConfig {
    type Error = TasksConfigError;

    /// Turns the typed form into a configuration and validates it.
    fn try_from(draft: &TasksDraft) -> Result<Self, Self::Error> {
        let stage = draft
            .stages
            .iter()
            .map(StageEntry::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        let mut task_type = Vec::with_capacity(draft.task_types.len());
        for row in &draft.task_types {
            task_type.push(TaskTypeEntry {
                value: row.value.trim().to_owned(),
                label: optional(&row.label),
                prefix: optional(&row.prefix),
                initial: optional(&row.initial),
                stage: row
                    .stages
                    .iter()
                    .map(StageEntry::try_from)
                    .collect::<Result<Vec<_>, _>>()?,
            });
        }
        let config = Self {
            stage,
            hierarchy: HierarchyConfig {
                depth_labels: draft.depth_labels.iter().filter_map(|label| optional(label)).collect(),
            },
            task_type,
            priority: draft
                .priorities
                .iter()
                .map(|row| PriorityEntry {
                    value: row.value.trim().to_owned(),
                    label: optional(&row.label),
                })
                .collect(),
        };
        config.validate()?;
        Ok(config)
    }
}

/// One of the editor's lists: the four top-level ones, or one task type's own stages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum List {
    Stage,
    DepthLabel,
    TaskType,
    Priority,
    TypeStage(usize),
}

impl TryFrom<&str> for List {
    type Error = Malformed;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        match raw {
            "stage" => Ok(Self::Stage),
            "depth_label" => Ok(Self::DepthLabel),
            "task_type" => Ok(Self::TaskType),
            "priority" => Ok(Self::Priority),
            _ => Err(Malformed),
        }
    }
}

/// A change to the draft that writes nothing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EditAction {
    Add(List),
    Remove(List, usize),
    Up(List, usize),
    Down(List, usize),
    /// Gives a task type its own stages, starting from a copy of the shared ones.
    Own(usize),
    /// Returns a task type to the shared stages, dropping its own.
    Share(usize),
}

impl TryFrom<&str> for EditAction {
    type Error = Malformed;

    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        let parts: Vec<&str> = raw.split(':').collect();
        let index = |raw: &str| raw.parse::<usize>().map_err(|_| Malformed);
        match parts.as_slice() {
            ["add", "type_stage", owner] => Ok(Self::Add(List::TypeStage(index(owner)?))),
            ["remove", "type_stage", owner, row] => Ok(Self::Remove(List::TypeStage(index(owner)?), index(row)?)),
            ["up", "type_stage", owner, row] => Ok(Self::Up(List::TypeStage(index(owner)?), index(row)?)),
            ["down", "type_stage", owner, row] => Ok(Self::Down(List::TypeStage(index(owner)?), index(row)?)),
            ["own", "task_type", owner] => Ok(Self::Own(index(owner)?)),
            ["share", "task_type", owner] => Ok(Self::Share(index(owner)?)),
            ["add", list] => Ok(Self::Add(List::try_from(*list)?)),
            ["remove", list, row] => Ok(Self::Remove(List::try_from(*list)?, index(row)?)),
            ["up", list, row] => Ok(Self::Up(List::try_from(*list)?, index(row)?)),
            ["down", list, row] => Ok(Self::Down(List::try_from(*list)?, index(row)?)),
            _ => Err(Malformed),
        }
    }
}

/// A submission this form could not have produced.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("malformed tasks configuration submission")]
pub(crate) struct Malformed;

fn edit_vec<T: Default>(rows: &mut Vec<T>, action: EditAction) {
    match action {
        EditAction::Add(_) => rows.push(T::default()),
        EditAction::Remove(_, index) if index < rows.len() => {
            rows.remove(index);
        }
        EditAction::Up(_, index) if index > 0 && index < rows.len() => rows.swap(index - 1, index),
        EditAction::Down(_, index) if index + 1 < rows.len() => rows.swap(index, index + 1),
        EditAction::Remove(..)
        | EditAction::Up(..)
        | EditAction::Down(..)
        | EditAction::Own(_)
        | EditAction::Share(_) => {}
    }
}

impl TasksDraft {
    fn apply(&mut self, action: EditAction) {
        let list = match action {
            EditAction::Add(list)
            | EditAction::Remove(list, _)
            | EditAction::Up(list, _)
            | EditAction::Down(list, _) => list,
            EditAction::Own(owner) => {
                if let Some(entry) = self.task_types.get_mut(owner)
                    && entry.stages.is_empty()
                {
                    entry.stages.clone_from(&self.stages);
                }
                return;
            }
            EditAction::Share(owner) => {
                if let Some(entry) = self.task_types.get_mut(owner) {
                    entry.stages.clear();
                }
                return;
            }
        };
        match list {
            List::Stage => edit_vec(&mut self.stages, action),
            List::DepthLabel => edit_vec(&mut self.depth_labels, action),
            List::TaskType => edit_vec(&mut self.task_types, action),
            List::Priority => edit_vec(&mut self.priorities, action),
            List::TypeStage(owner) => {
                if let Some(entry) = self.task_types.get_mut(owner) {
                    edit_vec(&mut entry.stages, action);
                }
            }
        }
    }
}

/// A parsed submission: the draft, the version it was edited from, and the draft-only change, if any.
pub(crate) struct Submission {
    draft: TasksDraft,
    expected_hash: String,
    action: Option<EditAction>,
}

fn row<'a, T: Default>(rows: &'a mut BTreeMap<usize, T>, index: &str) -> Result<&'a mut T, Malformed> {
    let index = index.parse::<usize>().map_err(|_| Malformed)?;
    Ok(rows.entry(index).or_default())
}

/// Sets one stage field, refusing a field a stage row does not have.
fn set_stage_field(stage: &mut StageDraft, field: &str, text: String) -> Result<(), Malformed> {
    match field {
        "state" => stage.state = text,
        "label" => stage.label = text,
        "icon" => stage.icon = text,
        "bucket" => stage.bucket = text,
        "colour" => stage.colour = text,
        "closes" => stage.closes = text,
        _ => return Err(Malformed),
    }
    Ok(())
}

/// Reads the form's flat fields into a draft, refusing any field or value it could not have submitted.
///
/// # Errors
///
/// Returns [`Malformed`] for an unknown field, a non-text value, or an unknown edit action.
pub(crate) fn parse_submission(fields: &Map<String, Value>) -> Result<Submission, Malformed> {
    let mut stages: BTreeMap<usize, StageDraft> = BTreeMap::new();
    let mut depth_labels: BTreeMap<usize, String> = BTreeMap::new();
    let mut task_types: BTreeMap<usize, TypeDraft> = BTreeMap::new();
    let mut type_stages: BTreeMap<usize, BTreeMap<usize, StageDraft>> = BTreeMap::new();
    let mut priorities: BTreeMap<usize, PriorityDraft> = BTreeMap::new();
    let mut expected_hash = String::new();
    let mut action = None;
    for (key, value) in fields {
        let text = match value {
            Value::String(text) => text.clone(),
            Value::Null => String::new(),
            _ => return Err(Malformed),
        };
        let parts: Vec<&str> = key.split('.').collect();
        match parts.as_slice() {
            ["target"] => {}
            ["expected_hash"] => expected_hash = text,
            ["edit_action"] => action = Some(EditAction::try_from(text.as_str())?),
            ["stage", index, field] => set_stage_field(row(&mut stages, index)?, field, text)?,
            ["depth_label", index] => *row(&mut depth_labels, index)? = text,
            ["task_type", index, "value"] => row(&mut task_types, index)?.value = text,
            ["task_type", index, "label"] => row(&mut task_types, index)?.label = text,
            ["task_type", index, "prefix"] => row(&mut task_types, index)?.prefix = text,
            ["task_type", index, "initial"] => row(&mut task_types, index)?.initial = text,
            ["task_type", owner, "stage", index, field] => {
                row(&mut task_types, owner)?;
                set_stage_field(row(row(&mut type_stages, owner)?, index)?, field, text)?;
            }
            ["priority", index, "value"] => row(&mut priorities, index)?.value = text,
            ["priority", index, "label"] => row(&mut priorities, index)?.label = text,
            _ => return Err(Malformed),
        }
    }
    for (owner, entry) in &mut task_types {
        if let Some(own) = type_stages.remove(owner) {
            entry.stages = own.into_values().collect();
        }
    }
    Ok(Submission {
        draft: TasksDraft {
            stages: stages.into_values().collect(),
            depth_labels: depth_labels.into_values().collect(),
            task_types: task_types.into_values().collect(),
            priorities: priorities.into_values().collect(),
        },
        expected_hash,
        action,
    })
}

/// The Tasks section's rendered state.
pub(crate) struct TasksSectionState {
    pub(crate) draft: TasksDraft,
    pub(crate) expected_hash: Option<String>,
    pub(crate) error: Option<String>,
    /// What the form is showing when it is not simply the saved file.
    pub(crate) notice: Option<String>,
    pub(crate) unsaved: bool,
}

/// Reads the section fresh: the file's configuration, or the built-in defaults with a notice saying why.
pub(crate) async fn read_tasks_section(client: &McpClient, vault_name: &str) -> TasksSectionState {
    let (draft, expected_hash, notice) = match read_attached(client, &format!("{vault_name}://{TASKS_TOML_PATH}")).await
    {
        Ok(AttachedRead::Text(attached)) => match toml::from_str::<TasksConfig>(&attached.text) {
            Ok(config) => (TasksDraft::from(&config), attached.content_hash, None),
            Err(error) => (
                TasksDraft::from(&TasksConfig::default()),
                attached.content_hash,
                Some(format!(
                    "{TASKS_TOML_PATH} could not be read ({}), so the built-in defaults are shown and in use. \
                         Saving replaces it with what this form holds.",
                    error.message()
                )),
            ),
        },
        Ok(AttachedRead::Refused { code }) if code.as_deref() == Some("path/not-found") => (
            TasksDraft::from(&TasksConfig::default()),
            None,
            Some(format!(
                "There is no {TASKS_TOML_PATH} yet, so the built-in defaults are shown and in use. Saving creates it."
            )),
        ),
        Ok(AttachedRead::Refused { .. } | AttachedRead::NotText) | Err(_) => (
            TasksDraft::from(&TasksConfig::default()),
            None,
            Some(format!(
                "{TASKS_TOML_PATH} exists but could not be read, so the built-in defaults are shown and in use. It is \
                 not written over until it can be read."
            )),
        ),
    };
    TasksSectionState {
        draft,
        expected_hash,
        error: None,
        notice,
        unsaved: false,
    }
}

fn with_submission(submission: Submission, error: Option<String>, unsaved: bool) -> impl FnOnce(&mut ConfigSections) {
    move |sections: &mut ConfigSections| {
        sections.tasks = TasksSectionState {
            draft: submission.draft,
            expected_hash: Some(submission.expected_hash).filter(|hash| !hash.is_empty()),
            error,
            notice: None,
            unsaved,
        };
    }
}

/// Answers a Tasks section submission: a draft-only edit, or a validated save.
pub(crate) async fn save_tasks(client: &McpClient, vault_name: &str, fields: &Map<String, Value>) -> Response {
    let Ok(mut submission) = parse_submission(fields) else {
        return (StatusCode::UNPROCESSABLE_ENTITY, "Malformed request.").into_response();
    };
    if let Some(action) = submission.action.take() {
        submission.draft.apply(action);
        return html_response(
            Box::pin(render_sections(
                client,
                vault_name,
                with_submission(submission, None, true),
            ))
            .await,
        );
    }
    let config = match TasksConfig::try_from(&submission.draft) {
        Ok(config) => config,
        Err(error) => {
            let message = format!("Not saved: {error}.");
            return html_response(
                Box::pin(render_sections(
                    client,
                    vault_name,
                    with_submission(submission, Some(message), true),
                ))
                .await,
            );
        }
    };
    let current = match read_attached(client, &format!("{vault_name}://{TASKS_TOML_PATH}")).await {
        Ok(AttachedRead::Text(attached)) => attached.text,
        Ok(AttachedRead::Refused { code }) if code.as_deref() == Some("path/not-found") => String::new(),
        Ok(AttachedRead::Refused { .. } | AttachedRead::NotText) => {
            let message = Some(format!(
                "Not saved: {TASKS_TOML_PATH} exists but could not be read, so it is not written over."
            ));
            return html_response(
                Box::pin(render_sections(
                    client,
                    vault_name,
                    with_submission(submission, message, true),
                ))
                .await,
            );
        }
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => return unreachable(),
    };
    // A file that is not valid TOML cannot be edited in place; the form already said saving replaces it. The rewrite
    // parses and re-renders a TOML document, CPU work kept off the async executor's threads.
    let content =
        match tokio::task::spawn_blocking(move || config.written_over(&current).or_else(|_| config.written_over("")))
            .await
        {
            Ok(Ok(content)) => content,
            Ok(Err(error)) => {
                let message = format!("Not saved: {error}.");
                return html_response(
                    Box::pin(render_sections(
                        client,
                        vault_name,
                        with_submission(submission, Some(message), true),
                    ))
                    .await,
                );
            }
            Err(_) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "The configuration could not be prepared.",
                )
                    .into_response();
            }
        };
    let path = VaultRelativePath::scaffold(TASKS_TOML_PATH);
    let hash = Some(submission.expected_hash.as_str()).filter(|hash| !hash.is_empty());
    match write_text(client, vault_name, &path, &content, hash).await {
        Ok(WriteOutcome::Written) => html_response(Box::pin(render_current(client, vault_name)).await),
        Ok(refused) => {
            let message = refused.refusal_reason();
            html_response(
                Box::pin(render_sections(
                    client,
                    vault_name,
                    with_submission(submission, message, true),
                ))
                .await,
            )
        }
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => unreachable(),
    }
}

/// One `<option>`.
pub(crate) struct Choice {
    pub(crate) value: String,
    pub(crate) label: String,
    pub(crate) selected: bool,
}

fn choices<'a>(blank: &str, values: impl IntoIterator<Item = (&'a str, &'a str)>, current: &str) -> Vec<Choice> {
    let mut options = vec![Choice {
        value: String::new(),
        label: blank.to_owned(),
        selected: current.is_empty(),
    }];
    options.extend(values.into_iter().map(|(value, label)| Choice {
        selected: value == current,
        value: value.to_owned(),
        label: label.to_owned(),
    }));
    if !current.is_empty() && !options.iter().any(|option| option.selected) {
        options.push(Choice {
            value: current.to_owned(),
            label: current.to_owned(),
            selected: true,
        });
    }
    options
}

/// A row's position, for its Move and Remove controls.
pub(crate) struct Position {
    pub(crate) index: usize,
    pub(crate) first: bool,
    pub(crate) last: bool,
}

fn position(index: usize, count: usize) -> Position {
    Position {
        index,
        first: index == 0,
        last: index + 1 == count,
    }
}

pub(crate) struct StageRowView {
    pub(crate) at: Position,
    /// The prefix every field of the row submits under: `stage.0`, or `task_type.1.stage.0` for a type's own.
    pub(crate) name: String,
    /// The list the row's Move and Remove controls name: `stage`, or `type_stage:1` for a type's own.
    pub(crate) list: String,
    pub(crate) state: String,
    pub(crate) label: String,
    pub(crate) icons: Vec<Choice>,
    pub(crate) buckets: Vec<Choice>,
    pub(crate) colours: Vec<Choice>,
    pub(crate) closes: Vec<Choice>,
    /// The colour the stage is shown in now, for the swatch beside its choice.
    pub(crate) colour: &'static str,
}

pub(crate) struct LabelRowView {
    pub(crate) at: Position,
    pub(crate) label: String,
}

pub(crate) struct TypeRowView {
    pub(crate) at: Position,
    pub(crate) value: String,
    pub(crate) label: String,
    pub(crate) prefix: String,
    /// The type's open states, any of which its new tasks may start in.
    pub(crate) initials: Vec<Choice>,
    /// Whether the type has stages of its own, rather than the shared ones.
    pub(crate) own_stages: bool,
}

/// One task type's own stages, edited beneath the task types.
pub(crate) struct TypeStagesView {
    pub(crate) owner: usize,
    pub(crate) label: String,
    pub(crate) stages: Vec<StageRowView>,
}

pub(crate) struct PriorityRowView {
    pub(crate) at: Position,
    pub(crate) value: String,
    pub(crate) label: String,
}

/// The Tasks pane as the template renders it.
pub(crate) struct TasksPaneView {
    pub(crate) stages: Vec<StageRowView>,
    /// The types that use the shared stages, in words, or `None` when every type has its own.
    pub(crate) shared_by: Option<String>,
    pub(crate) type_stages: Vec<TypeStagesView>,
    pub(crate) depth_labels: Vec<LabelRowView>,
    pub(crate) task_types: Vec<TypeRowView>,
    pub(crate) priorities: Vec<PriorityRowView>,
    pub(crate) expected_hash: String,
    pub(crate) error: Option<String>,
    pub(crate) notice: Option<String>,
    pub(crate) unsaved: bool,
}

/// What a type is called in the editor: its label, else its value, else its position.
fn type_name(entry: &TypeDraft, index: usize) -> String {
    [entry.label.trim(), entry.value.trim()]
        .into_iter()
        .find(|name| !name.is_empty())
        .map_or_else(|| format!("Task type {}", index + 1), str::to_owned)
}

/// One stage table's rows: `name` and `list` say where the rows' fields submit and what their controls edit.
fn stage_rows(stages: &[StageDraft], name: &str, list: &str) -> Vec<StageRowView> {
    let icon_names = crate::static_assets::icon_names();
    stages
        .iter()
        .enumerate()
        .map(|(index, stage)| {
            let shipped = Closes::shipped_for(stage.state.trim());
            StageRowView {
                at: position(index, stages.len()),
                name: format!("{name}.{index}"),
                list: list.to_owned(),
                state: stage.state.clone(),
                label: stage.label.clone(),
                icons: choices(
                    "No icon",
                    icon_names.iter().map(|name| (name.as_str(), name.as_str())),
                    &stage.icon,
                ),
                buckets: choices("Not in Focus", BUCKETS.iter().copied(), &stage.bucket),
                colours: choices(
                    "Default for its state",
                    StageColour::ALL.iter().map(|colour| (colour.slug(), colour.label())),
                    &stage.colour,
                ),
                closes: choices(
                    &format!("Default: {}", shipped.label().to_lowercase()),
                    Closes::ALL.iter().map(|closes| (closes.slug(), closes.label())),
                    &stage.closes,
                ),
                colour: StageColour::try_from(stage.colour.as_str())
                    .unwrap_or_else(|_| StageColour::shipped_for(stage.state.trim()))
                    .slug(),
            }
        })
        .collect()
}

/// `a`, `a and b`, or `a, b, and c`.
fn listed(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [only] => only.clone(),
        [first, second] => format!("{first} and {second}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}

impl From<TasksSectionState> for TasksPaneView {
    fn from(state: TasksSectionState) -> Self {
        let draft = &state.draft;
        let label_count = draft.depth_labels.len();
        let type_count = draft.task_types.len();
        let priority_count = draft.priorities.len();
        let shared_users: Vec<String> = draft
            .task_types
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.stages.is_empty())
            .map(|(index, entry)| type_name(entry, index))
            .collect();
        Self {
            stages: stage_rows(&draft.stages, "stage", "stage"),
            shared_by: (!shared_users.is_empty()).then(|| listed(&shared_users)),
            type_stages: draft
                .task_types
                .iter()
                .enumerate()
                .filter(|(_, entry)| !entry.stages.is_empty())
                .map(|(index, entry)| TypeStagesView {
                    owner: index,
                    label: type_name(entry, index),
                    stages: stage_rows(
                        &entry.stages,
                        &format!("task_type.{index}.stage"),
                        &format!("type_stage:{index}"),
                    ),
                })
                .collect(),
            depth_labels: draft
                .depth_labels
                .iter()
                .enumerate()
                .map(|(index, label)| LabelRowView {
                    at: position(index, label_count),
                    label: label.clone(),
                })
                .collect(),
            task_types: draft
                .task_types
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    let lifecycle = if entry.stages.is_empty() {
                        &draft.stages
                    } else {
                        &entry.stages
                    };
                    let open: Vec<(&str, &str)> = lifecycle
                        .iter()
                        .filter(|stage| !stage.state.trim().is_empty() && stage.closing() == Closes::Never)
                        .map(|stage| {
                            let label = if stage.label.trim().is_empty() {
                                stage.state.as_str()
                            } else {
                                stage.label.as_str()
                            };
                            (stage.state.as_str(), label)
                        })
                        .collect();
                    TypeRowView {
                        at: position(index, type_count),
                        value: entry.value.clone(),
                        label: entry.label.clone(),
                        prefix: entry.prefix.clone(),
                        initials: choices("Its first open state", open, &entry.initial),
                        own_stages: !entry.stages.is_empty(),
                    }
                })
                .collect(),
            priorities: draft
                .priorities
                .iter()
                .enumerate()
                .map(|(index, entry)| PriorityRowView {
                    at: position(index, priority_count),
                    value: entry.value.clone(),
                    label: entry.label.clone(),
                })
                .collect(),
            expected_hash: state.expected_hash.unwrap_or_default(),
            error: state.error,
            notice: state.notice,
            unsaved: state.unsaved,
        }
    }
}

#[cfg(test)]
#[path = "vault_settings_tasks_test.rs"]
mod tests;
