//! Which id series `memory/tasks.md` declares, and so which task types the
//! creation screen can offer: each `task_type` in `.contextos/tasks.toml` may
//! name the prefix its ids mint from, and a type is available only when the
//! tasks sidecar has declared that prefix. Read through an ordinary MCP tool
//! call, never written by this crate.
//!
//! A sidecar that cannot be read, or that declares none of the configured
//! prefixes, degrades to the original behaviour: every task takes the first
//! configured type and the tasks base's own default series assigns its id.

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::mcp_client::McpClient;

use crate::tasks_config::TaskTypeEntry;

const TASKS_SIDECAR_PATH: &str = "memory/tasks.md";

/// One task type the creation screen can offer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct TaskTypeChoice {
    pub(super) value: String,
    pub(super) label: String,
    /// The id series this type mints from, or `None` when the sidecar could
    /// not say and the tasks base's own default series applies.
    pub(super) prefix: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct FrontmatterReadResult {
    #[serde(default)]
    frontmatter: Map<String, Value>,
}

/// The id prefixes the tasks sidecar declares: the keys of its
/// `key_prefixes` map, or, for a sidecar still in the single-series form,
/// its one `key_prefix`. Empty when it declares neither.
pub(super) fn declared_prefixes(frontmatter: &Map<String, Value>) -> Vec<String> {
    if let Some(Value::Object(map)) = frontmatter.get("key_prefixes") {
        return map.keys().cloned().collect();
    }
    frontmatter
        .get("key_prefix")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .into_iter()
        .collect()
}

/// Loads the prefixes `vault_name`'s tasks sidecar declares. Never fails
/// outright: an unreachable session, an absent sidecar, or a malformed one
/// all yield an empty list, the same degrade-to-default contract
/// `tasks_config::load_tasks_config` has.
pub(super) async fn load_declared_prefixes(client: &McpClient, vault_name: &str) -> Vec<String> {
    let mut args = Map::new();
    args.insert(
        "path".to_owned(),
        Value::String(format!("{vault_name}://{TASKS_SIDECAR_PATH}")),
    );
    let Ok(result) = client.call_tool("frontmatter_read".to_owned(), args).await else {
        return Vec::new();
    };
    if result.is_error == Some(true) {
        return Vec::new();
    }
    result
        .into_typed::<FrontmatterReadResult>()
        .map(|read| declared_prefixes(&read.frontmatter))
        .unwrap_or_default()
}

/// The task types the creation screen can offer given the `declared`
/// prefixes, in the configured vocabulary's own order: each type whose prefix
/// the sidecar declares. When none is (an unreadable sidecar included) the only
/// choice is the first configured type with no explicit prefix, exactly as
/// before series existed.
pub(super) fn available_task_types(types: &[TaskTypeEntry], declared: &[String]) -> Vec<TaskTypeChoice> {
    let choices: Vec<TaskTypeChoice> = types
        .iter()
        .filter_map(|entry| {
            let prefix = entry.prefix.as_ref()?;
            declared.contains(prefix).then(|| TaskTypeChoice {
                value: entry.value.clone(),
                label: entry.display_label().to_owned(),
                prefix: Some(prefix.clone()),
            })
        })
        .collect();
    if choices.is_empty() {
        types
            .first()
            .map(|entry| TaskTypeChoice {
                value: entry.value.clone(),
                label: entry.display_label().to_owned(),
                prefix: None,
            })
            .into_iter()
            .collect()
    } else {
        choices
    }
}

/// The type a task created without choosing one gets: the first available,
/// which is the first in the configured vocabulary's order.
pub(super) fn default_task_type(available: &[TaskTypeChoice]) -> Option<&TaskTypeChoice> {
    available.first()
}

#[cfg(test)]
#[path = "tasks_key_prefixes_test.rs"]
mod tests;
