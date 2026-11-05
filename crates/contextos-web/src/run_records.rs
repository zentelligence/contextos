//! A vault job run's record under `memory/jobs/` and its operation-log entry, written through MCP.

use contextos_workflows::RunOutcome;
use serde_json::{Map, Value};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::jobs::RegisteredJob;
use crate::mcp_client::{McpCallError, McpClient};

/// How a run was started, recorded as its `trigger` field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Trigger {
    Scheduled,
    Manual,
}

impl Trigger {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::Manual => "manual",
        }
    }
}

/// What a new run record says about its run.
pub(crate) struct RunRecordHeader<'a> {
    pub job: &'a RegisteredJob,
    pub run_id: u64,
    pub trigger: Trigger,
    pub status: &'a str,
    pub started: OffsetDateTime,
    pub completed: Option<OffsetDateTime>,
    /// Whether the run was against scenarios, so the history shows it was synthetic.
    pub dry_run: bool,
    /// The names of the inputs given on the command line for this run, never their values.
    pub overrides: &'a [String],
}

/// The frontmatter a new run record carries.
pub(crate) fn frontmatter_of(header: &RunRecordHeader<'_>) -> Map<String, Value> {
    let RunRecordHeader {
        job,
        run_id,
        trigger,
        status,
        started,
        completed,
        dry_run,
        overrides,
    } = header;
    let mut frontmatter = Map::new();
    // `job-run`, not `task`: a run is no longer filed as a task, so it
    // no longer borrows the task schema's `task_type` discriminator
    // either.
    frontmatter.insert("type".to_owned(), Value::String("job-run".to_owned()));
    frontmatter.insert(
        "title".to_owned(),
        Value::String(format!(
            "Run: {}/{}/{} R-{run_id}",
            job.package, job.workflow_slug, job.slug
        )),
    );
    // The entity the job belongs to: its own, else its workflow's, else `vault`. Never a name nobody declared.
    frontmatter.insert("entity".to_owned(), Value::String(job.entity.as_str().to_owned()));
    frontmatter.insert("id".to_owned(), Value::String(format!("R-{run_id}")));
    frontmatter.insert("package".to_owned(), Value::String(job.package.clone()));
    frontmatter.insert("workflow".to_owned(), Value::String(job.workflow_slug.clone()));
    frontmatter.insert("job".to_owned(), Value::String(job.slug.clone()));
    frontmatter.insert("trigger".to_owned(), Value::String(trigger.as_str().to_owned()));
    frontmatter.insert("status".to_owned(), Value::String((*status).to_owned()));
    frontmatter.insert("dry_run".to_owned(), Value::Bool(*dry_run));
    frontmatter.insert("overrides".to_owned(), names(overrides));
    frontmatter.insert("outputs".to_owned(), names(&[]));
    frontmatter.insert("started".to_owned(), Value::String(rfc3339(*started)));
    frontmatter.insert(
        "completed".to_owned(),
        completed.map_or(Value::Null, |instant| Value::String(rfc3339(instant))),
    );
    // Inserted as a literal null (`note_create`'s `frontmatter` is a plain
    // map merge, not a merge patch, `obsidian.rs`): renders as YAML
    // `error:` with no value, matching the schema example, and can later
    // be given a real value by `complete_run`'s own `frontmatter_update`
    // patch.
    frontmatter.insert("error".to_owned(), Value::Null);
    // The stable code a failed run ends with, a field of its own so nothing that reads a failure parses the error text.
    frontmatter.insert("code".to_owned(), Value::Null);
    // Whether the run's script began. It is `false` until the run ends and says otherwise, so a run that never reaches
    // its script (an unreadable entry, an unavailable server) is told from one whose script started and failed.
    frontmatter.insert(SCRIPT_STARTED.to_owned(), Value::Bool(false));
    frontmatter
}

/// The release of a vault package a run read, as a run record names it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RunRelease {
    pub(crate) version: String,
    pub(crate) digest: String,
}

impl From<&contextos_workflows::PackageManifest> for RunRelease {
    fn from(release: &contextos_workflows::PackageManifest) -> Self {
        Self {
            version: release.identity.version.to_string(),
            digest: release.identity.digest.as_str().to_owned(),
        }
    }
}

/// A library a run read, by name, and the release it read by version and digest, as a run record lists it. A library a
/// run of source read from its own source has no release.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RunLibrary {
    pub(crate) name: String,
    pub(crate) release: Option<RunRelease>,
}

impl From<&contextos_workflows::PackageManifest> for RunLibrary {
    fn from(release: &contextos_workflows::PackageManifest) -> Self {
        Self {
            name: release.identity.name.to_string(),
            release: Some(RunRelease::from(release)),
        }
    }
}

impl From<&contextos_workflows::PackageIdentity> for RunLibrary {
    fn from(identity: &contextos_workflows::PackageIdentity) -> Self {
        Self {
            name: identity.name.to_string(),
            release: Some(RunRelease {
                version: identity.version.to_string(),
                digest: identity.digest.as_str().to_owned(),
            }),
        }
    }
}

impl From<&RunLibrary> for Value {
    fn from(library: &RunLibrary) -> Self {
        serde_json::json!({
            "name": library.name,
            "version": library.release.as_ref().map(|release| &release.version),
            "digest": library.release.as_ref().map(|release| &release.digest),
        })
    }
}

/// The frontmatter field that says whether a run's script began.
pub(crate) const SCRIPT_STARTED: &str = "script_started";

pub(crate) async fn create_run_record(
    client: &McpClient,
    vault_name: &str,
    run_path: &str,
    header: &RunRecordHeader<'_>,
) -> Result<(), RecordWriteError> {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{run_path}")));
    args.insert("frontmatter".to_owned(), Value::Object(frontmatter_of(header)));
    args.insert("content".to_owned(), Value::String(String::new()));
    call_succeeding(client, "note_create", args).await
}

/// Why a run record could not be written or updated.
#[derive(Debug, thiserror::Error)]
pub enum RecordWriteError {
    /// The call never completed: the server is unreachable or answered with a protocol error.
    #[error(transparent)]
    Call(#[from] McpCallError),
    /// The server ran the tool and answered with a tool error, such as a location it cannot write.
    #[error("MCP server {server:?} could not complete {tool:?}")]
    ToolFailed { server: String, tool: &'static str },
}

/// Calls `tool` and requires it to have done its work: a tool error is a failure here, since a record that was not
/// written or updated must be reported as such and not assumed.
async fn call_succeeding(
    client: &McpClient,
    tool: &'static str,
    arguments: Map<String, Value>,
) -> Result<(), RecordWriteError> {
    let result = client.call_tool(tool.to_owned(), arguments).await?;
    if result.is_error == Some(true) {
        return Err(RecordWriteError::ToolFailed {
            server: client.name().to_owned(),
            tool,
        });
    }
    Ok(())
}

/// A list of input or output names as a frontmatter value.
fn names(names: &[String]) -> Value {
    Value::Array(names.iter().map(|name| Value::String(name.clone())).collect())
}

/// The `status` a finished run's record carries. A degraded run is its own status, not a flavour of `succeeded`, so
/// it is visible in a listing and is kept by retention like a failure.
pub(crate) const fn status_of(outcome: &RunOutcome) -> &'static str {
    match outcome {
        RunOutcome::Succeeded => "succeeded",
        RunOutcome::Degraded(_) => "degraded",
        RunOutcome::Failed(_) => "failed",
        RunOutcome::Cancelled => "cancelled",
    }
}

/// What a finished run's record is patched with.
pub(crate) struct RunCompletion<'a> {
    pub(crate) outcome: &'a RunOutcome,
    /// The names of the outputs the script set.
    pub(crate) outputs: &'a [String],
    pub(crate) completed: OffsetDateTime,
    pub(crate) script_started: bool,
    /// The release of the job's package the run read, when it read one.
    pub(crate) release: Option<&'a RunRelease>,
    /// The library releases the run read, in name order.
    pub(crate) libraries: &'a [RunLibrary],
}

pub(crate) async fn complete_run(
    client: &McpClient,
    vault_name: &str,
    run_path: &str,
    completion: &RunCompletion<'_>,
) -> Result<(), RecordWriteError> {
    let RunCompletion {
        outcome,
        outputs,
        completed,
        script_started,
        release,
        libraries,
    } = completion;
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{run_path}")));
    let mut patch = completion_patch(outcome, outputs, *completed, *script_started);
    if let Some(release) = release {
        patch.insert("version".to_owned(), Value::String(release.version.clone()));
        patch.insert("digest".to_owned(), Value::String(release.digest.clone()));
    }
    if !libraries.is_empty() {
        patch.insert(
            "libraries".to_owned(),
            Value::Array(libraries.iter().map(Value::from).collect()),
        );
    }
    args.insert("patch".to_owned(), Value::Object(patch));
    call_succeeding(client, "frontmatter_update", args).await
}

/// The frontmatter a finished run's record is patched with.
pub(crate) fn completion_patch(
    outcome: &RunOutcome,
    outputs: &[String],
    completed: OffsetDateTime,
    script_started: bool,
) -> Map<String, Value> {
    let mut patch = Map::new();
    patch.insert(SCRIPT_STARTED.to_owned(), Value::Bool(script_started));
    patch.insert("status".to_owned(), Value::String(status_of(outcome).to_owned()));
    patch.insert("outputs".to_owned(), names(outputs));
    if let RunOutcome::Failed(failure) = outcome {
        patch.insert("error".to_owned(), Value::String(failure.to_string()));
        patch.insert(
            "code".to_owned(),
            failure
                .kind
                .map_or(Value::Null, |kind| Value::String(kind.code().to_owned())),
        );
    }
    patch.insert("completed".to_owned(), Value::String(rfc3339(completed)));
    patch
}

/// Marks a record `interrupted`, as a shared run's record is: the process that wrote it is gone, so it will never say how
/// its run ended, and its last call may or may not have taken effect.
pub(crate) async fn mark_interrupted(
    client: &McpClient,
    vault_name: &str,
    run_path: &str,
    at: OffsetDateTime,
) -> Result<(), RecordWriteError> {
    let mut patch = Map::new();
    patch.insert("status".to_owned(), Value::String("interrupted".to_owned()));
    patch.insert("completed".to_owned(), Value::String(rfc3339(at)));
    patch.insert(
        "error".to_owned(),
        Value::String(contextos_workflows::INTERRUPTED_ERROR.to_owned()),
    );
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{run_path}")));
    args.insert("patch".to_owned(), Value::Object(patch));
    call_succeeding(client, "frontmatter_update", args).await
}

/// One `vault_log_append` entry naming the run's own record: a forward
/// link only, best-effort (a failure here is logged and otherwise
/// ignored, since the run itself already completed and recorded its own
/// outcome in the record regardless).
pub(crate) async fn log_run_completion(
    client: &McpClient,
    vault_name: &str,
    run_path: &str,
    job: &RegisteredJob,
    run_id: u64,
    status: &str,
) {
    let mut args = Map::new();
    args.insert(
        "entry".to_owned(),
        Value::String(format!("Job run {} R-{run_id} {status}", job.slug)),
    );
    args.insert(
        "files".to_owned(),
        Value::Array(vec![Value::String(format!("{vault_name}://{run_path}"))]),
    );
    if let Err(error) = client.call_tool("vault_log_append".to_owned(), args).await {
        tracing::warn!(vault = vault_name, job = %job.slug, run_id, %error, "failed to append the job run's operation-log entry");
    }
}

pub(crate) fn rfc3339(instant: OffsetDateTime) -> String {
    instant.format(&Rfc3339).unwrap_or_else(|_| instant.to_string())
}

#[cfg(test)]
#[path = "run_records_test.rs"]
mod tests;
