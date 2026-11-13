//! Where a raised task goes: the vault's task system, behind a port so the raising rules can be tested with a fake.
//!
//! The adapter creates a task with `base_record_create` on `memory/tasks.base` from the `A` series with
//! `unique_fields: ["origin"]`, so the server, inside the vault's write lock, refuses a second task for the same
//! episode: the raise is idempotent across processes and restarts.

use std::future::Future;
use std::pin::Pin;

use contextos_core::MemberSlug;
use contextos_workflows::Origin;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::mcp_client::{McpCallError, McpClient};
use crate::tasks_config::TASK_NOTES_FILTER;

/// The vault's tasks base, relative to the vault root.
const TASKS_BASE: &str = "memory/tasks.base";
/// The id series actions are numbered in.
const ACTION_PREFIX: &str = "A";

/// A task to raise, with everything its note carries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RaisedTask {
    pub origin: Origin,
    pub title: String,
    /// The job's resolved entity.
    pub entity: String,
    pub assignee: MemberSlug,
    /// The note's body: `## Context`, `## Evidence`, and `## What to do`.
    pub body: String,
}

impl From<&RaisedTask> for Map<String, Value> {
    /// The task's frontmatter: `type`, `task_type`, `state`, `title`, `entity`, `assignee`, and `origin`, and nothing
    /// else beyond what record creation stamps.
    fn from(task: &RaisedTask) -> Self {
        let mut frontmatter = Self::new();
        for (key, value) in [
            ("type", "task"),
            ("task_type", "action"),
            ("state", "next"),
            ("title", task.title.as_str()),
            ("entity", task.entity.as_str()),
            ("assignee", task.assignee.as_str()),
        ] {
            frontmatter.insert(key.to_owned(), Value::String(value.to_owned()));
        }
        frontmatter.insert("origin".to_owned(), Value::String(task.origin.to_string()));
        frontmatter
    }
}

/// What asking the task system to create a task came to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RaiseOutcome {
    /// The task was created, under this key.
    Created(String),
    /// A task with this origin exists already, open or closed, whoever raised it.
    AlreadyRaised,
    /// The vault has no tasks base, or its sidecar declares no `A` series.
    NoTaskSystem,
    /// Anything else, with the stable code of the refusal, or `unreachable`.
    Failed(String),
}

/// A task already in the task system, as far as the fold check needs it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExistingTask {
    pub origin: String,
    pub task_type: String,
    pub state: String,
}

/// The vault's task system.
pub trait TaskSink: Send + Sync {
    /// Creates `task`, treating a repeated origin as already raised.
    fn create<'a>(&'a self, task: &'a RaisedTask) -> Pin<Box<dyn Future<Output = RaiseOutcome> + Send + 'a>>;

    /// Every task whose origin starts with `prefix`, or the stable code of why they could not be listed.
    fn tasks_with_origin_prefix<'a>(
        &'a self,
        prefix: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ExistingTask>, String>> + Send + 'a>>;
}

/// The MCP adapter: `base_record_create` and `base_query` on a vault's tasks base.
pub struct McpTaskSink<'a> {
    pub client: &'a McpClient,
    pub vault_name: &'a str,
}

impl TaskSink for McpTaskSink<'_> {
    fn create<'a>(&'a self, task: &'a RaisedTask) -> Pin<Box<dyn Future<Output = RaiseOutcome> + Send + 'a>> {
        Box::pin(async move {
            let mut arguments = Map::new();
            arguments.insert(
                "path".to_owned(),
                Value::String(format!("{}://{TASKS_BASE}", self.vault_name)),
            );
            arguments.insert("frontmatter".to_owned(), Value::Object(Map::from(task)));
            arguments.insert("content".to_owned(), Value::String(task.body.clone()));
            arguments.insert("prefix".to_owned(), Value::String(ACTION_PREFIX.to_owned()));
            arguments.insert("unique_fields".to_owned(), serde_json::json!(["origin"]));
            let result = match self.client.call_tool("base_record_create".to_owned(), arguments).await {
                Ok(result) => result,
                Err(error) => return RaiseOutcome::Failed(call_failure_code(&error).to_owned()),
            };
            if result.is_error == Some(true) {
                return match refusal_code(&result).as_deref() {
                    Some("io/duplicate-record") => RaiseOutcome::AlreadyRaised,
                    Some("path/not-found" | "io/invalid-key-prefix") => RaiseOutcome::NoTaskSystem,
                    Some(other) => RaiseOutcome::Failed(other.to_owned()),
                    None => RaiseOutcome::Failed("tool-error".to_owned()),
                };
            }
            match result.into_typed::<CreatedTask>() {
                Ok(created) => RaiseOutcome::Created(created.key.unwrap_or(created.path)),
                Err(_) => RaiseOutcome::Failed("result-unreadable".to_owned()),
            }
        })
    }

    fn tasks_with_origin_prefix<'a>(
        &'a self,
        prefix: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ExistingTask>, String>> + Send + 'a>> {
        Box::pin(async move {
            let mut arguments = Map::new();
            arguments.insert("vault".to_owned(), Value::String(format!("{}://.", self.vault_name)));
            arguments.insert("format".to_owned(), Value::String("json".to_owned()));
            arguments.insert(
                "definition".to_owned(),
                serde_json::json!({
                    "filters": format!("{TASK_NOTES_FILTER} && origin.contains(\"{}\")", escaped(prefix)),
                    "order": ["origin", "task_type", "state"],
                }),
            );
            let result = self
                .client
                .call_tool("base_query".to_owned(), arguments)
                .await
                .map_err(|error| call_failure_code(&error).to_owned())?;
            if result.is_error == Some(true) {
                return Err(refusal_code(&result).unwrap_or_else(|| "tool-error".to_owned()));
            }
            let answer = result
                .into_typed::<QueryAnswer>()
                .map_err(|_| "result-unreadable".to_owned())?;
            let rows: Vec<QueriedTask> =
                serde_json::from_str(&answer.content).map_err(|_| "result-unreadable".to_owned())?;
            Ok(rows
                .into_iter()
                .filter_map(|row| {
                    let origin = row.origin.filter(|origin| origin.starts_with(prefix))?;
                    Some(ExistingTask {
                        origin,
                        task_type: row.task_type.unwrap_or_default(),
                        state: row.state.unwrap_or_default(),
                    })
                })
                .collect())
        })
    }
}

/// What `base_record_create` answers with.
#[derive(Deserialize)]
struct CreatedTask {
    path: String,
    #[serde(default)]
    key: Option<String>,
}

#[derive(Deserialize)]
struct QueryAnswer {
    content: String,
}

/// A task row as `base_query` reports it. A task with no value for a property reports `null`.
#[derive(Deserialize)]
struct QueriedTask {
    #[serde(default)]
    origin: Option<String>,
    #[serde(default)]
    task_type: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

/// The stable code a tool's refusal carries in its structured content.
pub(crate) fn refusal_code(result: &rmcp::model::CallToolResult) -> Option<String> {
    result
        .structured_content
        .as_ref()
        .and_then(|content| content.get("code"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// A word for a call that did not produce a result: either the server was not reached, or it refused the call itself.
pub(crate) const fn call_failure_code(error: &McpCallError) -> &'static str {
    match error {
        McpCallError::Unreachable { .. } => "unreachable",
        McpCallError::Refused { .. } => "refused",
    }
}

/// `text` as the inside of a double-quoted string in a base filter.
fn escaped(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
#[path = "task_sink_test.rs"]
mod tests;
