//! Shared run history for job screens and attention navigation.
use crate::mcp_client::McpClient;
use serde::Deserialize;
use serde_json::{Map, Value};

#[derive(Clone, Debug)]
pub(crate) struct RunSummary {
    /// The record's path in its vault, empty for a shared run, whose records are on the host.
    pub path: String,
    pub run_id: String,
    /// The key of the job the run belongs to. A job's page shows the runs of its whole chain of successors, and runs
    /// are numbered from 1 under each job's own key, so a number alone does not say whose run it is.
    pub job: String,
    /// The version of the release the run read, or nothing when its record does not say.
    pub release: String,
    /// Whether the run was against scenarios, so never a live run's outcome.
    pub dry_run: bool,
    pub trigger: String,
    pub status: String,
    pub started: String,
    pub completed: String,
    pub error: String,
    /// The stable code the run failed with, when it failed and had one.
    pub code: Option<String>,
    /// Whether the run's script started, as its record says.
    pub script_started: ScriptStarted,
}

/// What a run's record says of whether its script started. The field is read per record, so one record that holds
/// something else never makes the rest of the history unreadable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ScriptStarted {
    /// The record says the script started.
    Yes,
    /// The record says the script never started.
    No,
    /// The record predates the field, so it says nothing either way.
    Absent,
    /// The record has the field, and it is not true or false.
    Invalid,
}

impl From<Option<&Value>> for ScriptStarted {
    fn from(value: Option<&Value>) -> Self {
        match value {
            None | Some(Value::Null) => Self::Absent,
            Some(Value::Bool(true)) => Self::Yes,
            Some(Value::Bool(false)) => Self::No,
            Some(_) => Self::Invalid,
        }
    }
}

impl RunSummary {
    pub(crate) fn state(&self) -> &'static str {
        match self.status.as_str() {
            "succeeded" => "done",
            "failed" | "interrupted" => "blocked",
            "running" => "working",
            "skipped" | "cancelled" => "idle",
            "degraded" => "degraded",
            _ => "unknown",
        }
    }
    /// The run's number, which is what follows `R-` in its identifier.
    pub(crate) fn number(&self) -> Option<u64> {
        self.run_id.strip_prefix("R-")?.parse().ok()
    }

    pub(crate) fn attention(&self) -> Option<&'static str> {
        match self.status.as_str() {
            "failed" => Some("error"),
            "skipped" | "degraded" | "interrupted" => Some("warning"),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
struct QueryResult {
    content: String,
}

#[derive(Deserialize)]
struct RunRow {
    #[serde(rename = "file.path")]
    path: String,
    dry_run: Option<bool>,
    trigger: Option<String>,
    status: Option<String>,
    started: Option<String>,
    completed: Option<String>,
    error: Option<String>,
    code: Option<String>,
    #[serde(default)]
    script_started: Option<Value>,
    #[serde(default)]
    version: Option<String>,
}

impl TryFrom<RunRow> for RunSummary {
    type Error = &'static str;
    fn try_from(row: RunRow) -> Result<Self, Self::Error> {
        let name = row.path.rsplit('/').next().ok_or("run path is empty")?;
        run_number(name).ok_or("run path is not a numbered record")?;
        Ok(Self {
            run_id: name.trim_end_matches(".md").to_owned(),
            job: String::new(),
            release: row.version.unwrap_or_default(),
            dry_run: row.dry_run.unwrap_or(false),
            path: row.path.clone(),
            trigger: row.trigger.unwrap_or_default(),
            status: row.status.ok_or("run status is missing")?,
            started: row.started.ok_or("run start time is missing")?,
            completed: row.completed.unwrap_or_default(),
            error: row.error.unwrap_or_default(),
            code: row.code,
            script_started: ScriptStarted::from(row.script_started.as_ref()),
        })
    }
}

/// Sort the complete run set before limiting the response. A filesystem glob's result cap
/// can otherwise discard the latest run before the caller ever sees it.
pub(crate) async fn run_history(
    client: &McpClient,
    vault_name: &str,
    package: &str,
    workflow_slug: &str,
    slug: &str,
    limit: usize,
) -> Result<Vec<RunSummary>, &'static str> {
    let folder = serde_json::to_string(&contextos_workflows::job_records_folder(package, workflow_slug, slug))
        .map_err(|_| "invalid job path")?;
    let mut args = Map::new();
    args.insert("vault".into(), Value::String(format!("{vault_name}://.")));
    args.insert("format".into(), Value::String("json".into()));
    args.insert("limit".into(), serde_json::json!(limit));
    args.insert(
        "definition".into(),
        serde_json::json!({
            "filters": format!("file.inFolder({folder}) && note.type == \"job-run\""),
            "order": ["file.path", "dry_run", "trigger", "status", "started", "completed", "error", "code", "script_started", "version"],
            "sort": [{"property": "started", "direction": "DESC"}, {"property": "file.path", "direction": "DESC"}],
        }),
    );
    let result = client
        .call_tool("base_query".into(), args)
        .await
        .map_err(|_| "run history is unavailable")?;
    if result.is_error == Some(true) {
        return Err("run history query was rejected");
    }
    let result = result
        .into_typed::<QueryResult>()
        .map_err(|_| "invalid run history response")?;
    let rows: Vec<RunRow> = serde_json::from_str(&result.content).map_err(|_| "invalid run history rows")?;
    rows.into_iter()
        .map(|row| {
            RunSummary::try_from(row).map(|run| RunSummary {
                job: slug.to_owned(),
                ..run
            })
        })
        .collect()
}

/// The vault path of the record of run `number` of a job, wherever under the job's folder it was filed, or `None` when
/// there is none or it cannot be found. Reads only the path, so a record whose other fields are unusable is still found.
pub(crate) async fn run_record_path(
    client: &McpClient,
    vault_name: &str,
    package: &str,
    workflow_slug: &str,
    slug: &str,
    number: u64,
) -> Option<String> {
    let folder = serde_json::to_string(&contextos_workflows::job_records_folder(package, workflow_slug, slug)).ok()?;
    let name = serde_json::to_string(&format!("R-{number}")).ok()?;
    let mut args = Map::new();
    args.insert("vault".into(), Value::String(format!("{vault_name}://.")));
    args.insert("format".into(), Value::String("json".into()));
    args.insert("limit".into(), serde_json::json!(1));
    args.insert(
        "definition".into(),
        serde_json::json!({
            "filters": format!("file.inFolder({folder}) && file.basename == {name}"),
            "order": ["file.path"],
        }),
    );
    let result = client.call_tool("base_query".into(), args).await.ok()?;
    if result.is_error == Some(true) {
        return None;
    }
    let result = result.into_typed::<QueryResult>().ok()?;
    let rows: Vec<PathRow> = serde_json::from_str(&result.content).ok()?;
    rows.into_iter().next().map(|row| row.path)
}

#[derive(Deserialize)]
struct PathRow {
    #[serde(rename = "file.path")]
    path: String,
}

pub(crate) fn run_number(file_name: &str) -> Option<u64> {
    contextos_workflows::parse_run_id(file_name)
}

#[cfg(test)]
#[path = "job_runs_test.rs"]
mod tests;
