//! The counts the sidebar shows beside a destination, read from the vault on every full-page render: the open
//! tasks, the items waiting in the Inbox and Outbox, and the agents each agent filter selects. A count that could
//! not be read (the session is unreachable, the team file is malformed, or a task query hit its result cap and
//! so would under-count) is `None` and renders no badge, never a made-up zero. An absent team file or directory is
//! a genuine zero.
//!
//! The Inbox count is the *pending* items only, the ones waiting for the operator: the Inbox screen's merged root
//! also lists what has been processed, so its rows outnumber the badge by design.
//!
//! Each count uses the same definition the destination's own screen does: agents through [`team::matching_agents`],
//! open tasks through [`TasksConfig::is_open`] over the notes [`TASK_NOTES_FILTER`] selects, and the Inbox and
//! Outbox through the same directory listing their screens render. No count is derived from a task's `state`
//! as an indicator of it being blocked, which the vault's own requirements reserve for an events mechanism.

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::inbox_outbox::{self, DirectoryListing};
use crate::inbox_outbox_config::{self, InboxOutboxConfig};
use crate::mcp_client::{McpCallError, McpClient};
use crate::tasks_config::{self, TASK_NOTES_FILTER, TasksConfig};
use crate::team::{self, MemberState, TeamMember};

/// One count per sidebar destination that has one; `None` where the count could not be read.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct NavCounts {
    pub tasks: Option<usize>,
    /// The events still needing attention ([`crate::signals_attention`]).
    pub signals: Option<usize>,
    pub inbox: Option<usize>,
    pub outbox: Option<usize>,
    pub agents_general: Option<usize>,
    pub agents_business: Option<usize>,
    pub agents_personal: Option<usize>,
    pub agents_blocked: Option<usize>,
}

/// Reads every sidebar count for `vault`, concurrently: none needs another's result.
pub(crate) async fn load(client: &McpClient, vault: &str) -> NavCounts {
    // The two directory counts wait on the configured directory names; nothing else waits on anything.
    let directories = async {
        let config = inbox_outbox_config::load_inbox_outbox_config(client, vault).await;
        tokio::join!(
            pending_inbox_items(client, vault, &config),
            outbox_items(client, vault, &config),
        )
    };
    let (tasks, (inbox, outbox), team, signals) = tokio::join!(
        open_tasks(client, vault),
        directories,
        team::try_load_team(client, vault),
        open_signals(client, vault),
    );
    NavCounts {
        tasks,
        signals,
        inbox,
        outbox,
        agents_general: team.as_deref().map(|team| agents_in_team(team, "general")),
        agents_business: team.as_deref().map(|team| agents_in_team(team, "business")),
        agents_personal: team.as_deref().map(|team| agents_in_team(team, "personal")),
        agents_blocked: team.as_deref().map(blocked_agents),
    }
}

/// The warning and error events nothing since has cleared, or `None` when events cannot be read. The count is kept
/// briefly per vault ([`crate::open_count_cache`]), so that rendering a page does not read every event note.
async fn open_signals(client: &McpClient, vault: &str) -> Option<usize> {
    let lookup = client.open_counts().lookup(vault);
    if let Some(count) = lookup.count {
        return Some(count);
    }
    let api = crate::signals_query::EventsApi {
        client,
        vault_name: vault,
    };
    let count = crate::signals_load::open_events(
        &api,
        None,
        &crate::signals_load::decision_columns(),
        time::OffsetDateTime::now_utc(),
    )
    .await
    .ok()
    .map(|open| open.len())?;
    client.open_counts().store(vault, lookup.generation, count);
    Some(count)
}

fn agents_in_team(members: &[TeamMember], team: &str) -> usize {
    team::matching_agents(members, Some(team), None).count()
}

fn blocked_agents(members: &[TeamMember]) -> usize {
    team::matching_agents(members, None, Some(MemberState::Blocked.label())).count()
}

/// Every item a listing shows: its folders and its files.
fn listed_items(listing: &DirectoryListing) -> usize {
    listing.folders.len() + listing.files.len()
}

/// How many items wait in the Inbox, `None` when it could not be listed.
pub(crate) async fn pending_inbox_items(client: &McpClient, vault: &str, config: &InboxOutboxConfig) -> Option<usize> {
    inbox_outbox::list_directory(client, vault, &config.inbox_dir)
        .await
        .ok()
        .map(|listing| listed_items(&listing))
}

async fn outbox_items(client: &McpClient, vault: &str, config: &InboxOutboxConfig) -> Option<usize> {
    inbox_outbox::list_directory(client, vault, &config.outbox_dir)
        .await
        .ok()
        .map(|listing| listed_items(&listing))
}

#[derive(Debug, Deserialize)]
struct BaseQueryResult {
    content: String,
    /// Set when the query hit the tool's own result cap, so `content` holds only some of the matches.
    #[serde(default)]
    truncated: bool,
}

/// One task's type and state: all an open count needs.
#[derive(Debug, Eq, PartialEq)]
struct TaskStage {
    task_type: String,
    state: String,
}

/// The type and state of every task a query returned.
#[derive(Debug, Eq, PartialEq)]
struct TaskStages(Vec<TaskStage>);

/// A task query result that cannot support a count: not rows at all, or cut short by the result cap.
#[derive(Debug, Eq, PartialEq)]
struct TaskStatesUnreadable;

impl TryFrom<BaseQueryResult> for TaskStages {
    type Error = TaskStatesUnreadable;

    fn try_from(result: BaseQueryResult) -> Result<Self, Self::Error> {
        if result.truncated {
            return Err(TaskStatesUnreadable);
        }
        let rows: Vec<Map<String, Value>> = serde_json::from_str(&result.content).map_err(|_| TaskStatesUnreadable)?;
        let text =
            |row: &Map<String, Value>, key: &str| row.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
        Ok(Self(
            rows.iter()
                .map(|row| TaskStage {
                    task_type: text(row, "task_type"),
                    state: text(row, "state"),
                })
                .collect(),
        ))
    }
}

/// The tasks in an open stage of their own type. Reads only each task's `task_type` and `state`, since a count needs
/// nothing else.
async fn open_tasks(client: &McpClient, vault: &str) -> Option<usize> {
    let (config, stages) = tokio::join!(
        tasks_config::load_tasks_config(client, vault),
        task_stages(client, vault),
    );
    stages.map(|stages| count_open(&config, &stages))
}

fn count_open(config: &TasksConfig, stages: &[TaskStage]) -> usize {
    stages
        .iter()
        .filter(|stage| config.is_open(&stage.task_type, &stage.state))
        .count()
}

async fn task_stages(client: &McpClient, vault: &str) -> Option<Vec<TaskStage>> {
    let mut args = Map::new();
    args.insert("vault".to_owned(), Value::String(format!("{vault}://.")));
    args.insert("format".to_owned(), Value::String("json".to_owned()));
    args.insert(
        "definition".to_owned(),
        json!({
            "filters": TASK_NOTES_FILTER,
            "properties": { "state": {}, "task_type": {} },
            "views": [{ "type": "table", "name": "rows", "order": ["state", "task_type"] }],
        }),
    );
    let result = match client.call_tool("base_query".to_owned(), args).await {
        Ok(result) => result,
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => return None,
    };
    if result.is_error == Some(true) {
        return None;
    }
    let parsed = result.into_typed::<BaseQueryResult>().ok()?;
    TaskStages::try_from(parsed).ok().map(|stages| stages.0)
}

#[cfg(test)]
#[path = "nav_counts_test.rs"]
mod tests;
