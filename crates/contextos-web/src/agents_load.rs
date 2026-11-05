//! Reading what Agents liveness is derived from: the session and claim events, and the claims tasks hold.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Map, Value};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

use crate::agents_liveness::{CLAIM_LAPSED, ClaimedTask, MemberLiveness, RUN_FINISHED, RUN_STARTED, liveness};
use crate::mcp_client::McpClient;
use crate::signals_query::{EventsApi, kinds_filter};
use crate::tasks_config::TASK_NOTES_FILTER;

/// How many session and lapse events are read.
const EVENT_LIMIT: usize = 1_000;

#[derive(Deserialize)]
struct QueryAnswer {
    content: String,
}

/// One task row, with the columns liveness reads.
#[derive(Deserialize)]
struct TaskClaimRow {
    #[serde(rename = "file.path")]
    path: Option<String>,
    id: Option<String>,
    assignee: Option<String>,
    claimed_by: Option<String>,
    claimed_until: Option<Value>,
}

impl TryFrom<TaskClaimRow> for ClaimedTask {
    type Error = ();

    fn try_from(row: TaskClaimRow) -> Result<Self, ()> {
        let path = row.path.filter(|path| !path.is_empty()).ok_or(())?;
        Ok(Self {
            id: row.id.unwrap_or_default(),
            path,
            assignee: row.assignee.filter(|assignee| !assignee.trim().is_empty()),
            claimed_by: row.claimed_by.filter(|claim| !claim.trim().is_empty()),
            claimed_until: row
                .claimed_until
                .as_ref()
                .and_then(Value::as_str)
                .and_then(|text| OffsetDateTime::parse(text.trim(), &Rfc3339).ok()),
        })
    }
}

/// Every task with the columns liveness reads. `None` when tasks cannot be read.
async fn claimed_tasks(client: &McpClient, vault: &str) -> Option<Vec<ClaimedTask>> {
    let mut arguments = Map::new();
    arguments.insert("vault".to_owned(), Value::String(format!("{vault}://.")));
    arguments.insert("format".to_owned(), Value::String("json".to_owned()));
    arguments.insert(
        "definition".to_owned(),
        serde_json::json!({
            "filters": TASK_NOTES_FILTER,
            "order": ["file.path", "id", "assignee", "claimed_by", "claimed_until"],
        }),
    );
    let result = client.call_tool("base_query".to_owned(), arguments).await.ok()?;
    if result.is_error == Some(true) {
        return None;
    }
    let answer = result.into_typed::<QueryAnswer>().ok()?;
    let rows: Vec<TaskClaimRow> = serde_json::from_str(&answer.content).ok()?;
    Some(
        rows.into_iter()
            .filter_map(|row| ClaimedTask::try_from(row).ok())
            .collect(),
    )
}

/// The liveness of each of `members` (their slugs). Empty when events or tasks cannot be read: the screen then shows
/// the members as it did before, never an error.
pub async fn load_liveness(
    client: &McpClient,
    vault: &str,
    members: &[String],
    now: OffsetDateTime,
    overdue_after: Duration,
) -> HashMap<String, MemberLiveness> {
    let api = EventsApi {
        client,
        vault_name: vault,
    };
    let filter = kinds_filter(&[RUN_STARTED, RUN_FINISHED, CLAIM_LAPSED]);
    let columns = crate::signals_event::columns_for(&[]);
    let (events, tasks) = tokio::join!(
        api.newest_first(&filter, EVENT_LIMIT, &columns),
        claimed_tasks(client, vault),
    );
    let (Ok(events), Some(tasks)) = (events, tasks) else {
        return HashMap::new();
    };
    members
        .iter()
        .map(|slug| (slug.clone(), liveness(slug, &events.events, &tasks, now, overdue_after)))
        .collect()
}
