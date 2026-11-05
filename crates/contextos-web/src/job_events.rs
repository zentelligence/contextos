//! Events the Web scheduler records for vault jobs, from the same reconcile step that raises their tasks.
//!
//! A job that crashes cannot record its own failure, so the scheduler does: `job.failed` for each failed scheduled run,
//! `job.suspended` for each system suspension, and `job.recovered` for the first scheduled success after a failure that
//! no success has followed (runs that ended cancelled, interrupted, skipped, or degraded in between neither recover it
//! nor forget it) or after a suspension was resumed.
//! Each is recorded with `base_record_create` on `memory/events.base` and `unique_fields`, so the server, inside the
//! vault's write lock, refuses a second event for the same episode whichever process asks. A refusal that says the
//! event exists counts as recorded. Any other failure leaves the event unrecorded and the job's runs are read again on
//! the next pass. A vault with no events base, no kind registry, or a kind it does not declare records nothing, says
//! so once, and is looked at again after a pause.
//!
//! This step reads durable state only and never changes a job, a schedule, a task, or a run record. Shared jobs never
//! reach it, and neither do runs started by hand, dry runs, or any outcome but a failure or a success.

use std::future::Future;
use std::pin::Pin;

use contextos_workflows::{Actor, JobKey, JobLifecycle, JobState, JobsState};
use serde::Deserialize;
use serde_json::{Map, Value};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime, UtcOffset};

use crate::job_attention::JobAttentionMemory;
use crate::job_runs::{RunSummary, run_history};
use crate::job_validation::ObservedJob;
use crate::mcp_client::McpClient;
use crate::run_settings::HostContext;
use crate::task_sink::{call_failure_code, refusal_code};
use crate::vault_runs::claim_slot;

/// The vault's events base, relative to the vault root, and the sidecar that goes with it.
const EVENTS_BASE: &str = "memory/events.base";
const EVENTS_SIDECAR: &str = "memory/events.md";
/// How many of a job's latest run records are read for events.
const HISTORY_LIMIT: usize = 50;
/// How far back from a vault's first reconcile a run may have started and still be recorded, so a vault that has never
/// had events does not fill with the history of every run its job ever made. The first reconcile is remembered, so
/// every later one records every run since, however old by then, and none before.
pub(crate) const FIRST_RECONCILE_LOOKBACK: Duration = Duration::days(7);
/// How long a vault that cannot take job events is left alone before it is looked at again.
const UNSUPPORTED_PAUSE: Duration = Duration::minutes(5);
/// The code recorded for a failed run whose record names none.
const UNKNOWN_FAILURE_CODE: &str = "workflow/failed";

/// Which job event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobEventKind {
    Failed,
    Suspended,
    Recovered,
    Retired,
}

impl JobEventKind {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Failed => "job.failed",
            Self::Suspended => "job.suspended",
            Self::Recovered => "job.recovered",
            Self::Retired => "job.retired",
        }
    }
}

/// One event to record, with everything its note carries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobEvent {
    pub kind: JobEventKind,
    /// The package the job's workflow belongs to.
    pub package: String,
    pub workflow: String,
    pub job: String,
    pub entity: String,
    /// The link to the note the event is about: the run record, or the job file.
    pub subject: String,
    /// `R-{n}`, for a run event.
    pub run: Option<String>,
    /// The stable code, for a failure or a suspension.
    pub code: Option<String>,
    /// When the suspension took effect, as RFC 3339 in UTC.
    pub suspended_at: Option<String>,
    /// When a successor's enabling retired the job, as RFC 3339 in UTC.
    pub retired_at: Option<String>,
    /// The key of the successor whose enabling retired the job.
    pub replaced_by: Option<String>,
}

impl JobEvent {
    /// What identifies the episode, so the process remembers it is recorded: the kind, the job (package, workflow, and job), and the run or the time.
    #[must_use]
    pub fn id(&self) -> String {
        let episode = self
            .run
            .as_deref()
            .or(self.suspended_at.as_deref())
            .or(self.retired_at.as_deref())
            .unwrap_or_default();
        format!(
            "{}:{}/{}/{}:{episode}",
            self.kind.name(),
            self.package,
            self.workflow,
            self.job
        )
    }

    /// The job and kind of the event, whatever run or time it is about: what a failure to record is logged once for.
    #[must_use]
    pub fn job_and_kind(&self) -> String {
        format!("{}:{}/{}/{}", self.kind.name(), self.package, self.workflow, self.job)
    }

    /// The fields that make the event unique: its kind, the job (package, workflow, and job), and its run or when it was suspended, since a job may
    /// record kinds of its own about the same run.
    fn unique_fields(&self) -> Vec<&'static str> {
        match self.kind {
            JobEventKind::Failed | JobEventKind::Recovered => vec!["kind", "package", "workflow", "job", "run"],
            JobEventKind::Suspended => vec!["kind", "package", "workflow", "job", "suspended_at"],
            JobEventKind::Retired => vec!["kind", "package", "workflow", "job", "retired_at"],
        }
    }
}

impl From<&JobEvent> for Map<String, Value> {
    /// The frontmatter a recorded event is given: its kind, subject, and entity, then the kind's own fields. The
    /// severity is the kind's default, `at` and the path are stamped by the server.
    fn from(event: &JobEvent) -> Self {
        let mut frontmatter = Self::new();
        let mut put = |key: &str, value: &str| {
            frontmatter.insert(key.to_owned(), Value::String(value.to_owned()));
        };
        put("kind", event.kind.name());
        put("subject", &event.subject);
        put("entity", &event.entity);
        put("package", &event.package);
        put("workflow", &event.workflow);
        put("job", &event.job);
        for (key, value) in [
            ("run", &event.run),
            ("code", &event.code),
            ("suspended_at", &event.suspended_at),
            ("retired_at", &event.retired_at),
            ("replaced_by", &event.replaced_by),
        ] {
            if let Some(value) = value {
                put(key, value);
            }
        }
        frontmatter
    }
}

/// What asking the vault to record an event came to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecordOutcome {
    Recorded,
    /// The event exists already, whoever recorded it.
    AlreadyRecorded,
    /// The vault has no events base, no kind registry, or does not declare this kind.
    Unsupported,
    /// Anything else, with the stable code of the refusal or `unreachable`.
    Failed(String),
}

/// The vault could not say whether a job's suspension is still open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SuspensionUnreadable;

/// Where a job event goes: the vault's events base, behind a port so the rules can be tested with a fake.
pub trait EventSink: Send + Sync {
    /// Whether the vault can take recorded events at all: an events base whose sidecar names a kind registry.
    fn accepts_events<'a>(&'a self) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>>;

    /// Whether the vault holds a `job.suspended` event for `job` that no later `job.recovered` has followed. A vault
    /// that cannot be read for it is an error, never an answer of no, so the decision is made again on the next pass.
    fn suspension_open<'a>(
        &'a self,
        job: &'a JobKey,
    ) -> Pin<Box<dyn Future<Output = Result<bool, SuspensionUnreadable>> + Send + 'a>>;

    /// Records `event`, treating a repeat as already recorded.
    fn record<'a>(&'a self, event: &'a JobEvent) -> Pin<Box<dyn Future<Output = RecordOutcome> + Send + 'a>>;
}

/// The MCP adapter: `frontmatter_read` and `base_record_create` on a vault's events base.
pub struct McpEventSink<'a> {
    pub client: &'a McpClient,
    pub vault_name: &'a str,
}

#[derive(Deserialize)]
struct FrontmatterRead {
    frontmatter: Map<String, Value>,
}

/// What `base_query` returns: its rows, as JSON text.
#[derive(Deserialize)]
struct QueryContent {
    content: String,
}

#[derive(Deserialize)]
struct KindRow {
    kind: String,
}

impl EventSink for McpEventSink<'_> {
    fn accepts_events<'a>(&'a self) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move {
            let mut arguments = Map::new();
            arguments.insert(
                "path".to_owned(),
                Value::String(format!("{}://{EVENTS_SIDECAR}", self.vault_name)),
            );
            let Ok(result) = self.client.call_tool("frontmatter_read".to_owned(), arguments).await else {
                return false;
            };
            if result.is_error == Some(true) {
                return false;
            }
            result.into_typed::<FrontmatterRead>().is_ok_and(|read| {
                read.frontmatter
                    .get("event_kinds")
                    .and_then(Value::as_str)
                    .is_some_and(|folder| !folder.trim().is_empty())
            })
        })
    }

    fn suspension_open<'a>(
        &'a self,
        job: &'a JobKey,
    ) -> Pin<Box<dyn Future<Output = Result<bool, SuspensionUnreadable>> + Send + 'a>> {
        Box::pin(async move {
            let (Ok(package), Ok(workflow), Ok(slug)) = (
                serde_json::to_string(&job.package),
                serde_json::to_string(&job.workflow),
                serde_json::to_string(&job.job),
            ) else {
                return Err(SuspensionUnreadable);
            };
            let mut arguments = Map::new();
            arguments.insert("vault".to_owned(), Value::String(format!("{}://.", self.vault_name)));
            arguments.insert("format".to_owned(), Value::String("json".to_owned()));
            arguments.insert("limit".to_owned(), serde_json::json!(1));
            arguments.insert(
                "definition".to_owned(),
                serde_json::json!({
                    "filters": format!(
                        "file.inFolder(\"memory/events\") && note.type == \"event\" && note.package == {package} \
                         && note.workflow == {workflow} && note.job == {slug} && (note.kind == \"job.suspended\" || note.kind == \"job.recovered\")"
                    ),
                    "order": ["kind"],
                    "sort": [{"property": "at", "direction": "DESC"}, {"property": "file.path", "direction": "DESC"}],
                }),
            );
            let result = self
                .client
                .call_tool("base_query".to_owned(), arguments)
                .await
                .map_err(|_| SuspensionUnreadable)?;
            if result.is_error == Some(true) {
                return Err(SuspensionUnreadable);
            }
            let query = result.into_typed::<QueryContent>().map_err(|_| SuspensionUnreadable)?;
            let rows = serde_json::from_str::<Vec<KindRow>>(&query.content).map_err(|_| SuspensionUnreadable)?;
            Ok(rows
                .into_iter()
                .next()
                .is_some_and(|row| row.kind == JobEventKind::Suspended.name()))
        })
    }

    fn record<'a>(&'a self, event: &'a JobEvent) -> Pin<Box<dyn Future<Output = RecordOutcome> + Send + 'a>> {
        Box::pin(async move {
            let mut arguments = Map::new();
            arguments.insert(
                "path".to_owned(),
                Value::String(format!("{}://{EVENTS_BASE}", self.vault_name)),
            );
            arguments.insert("frontmatter".to_owned(), Value::Object(Map::from(event)));
            arguments.insert("unique_fields".to_owned(), serde_json::json!(event.unique_fields()));
            let result = match self.client.call_tool("base_record_create".to_owned(), arguments).await {
                Ok(result) => result,
                Err(error) => return RecordOutcome::Failed(call_failure_code(&error).to_owned()),
            };
            if result.is_error != Some(true) {
                return RecordOutcome::Recorded;
            }
            match refusal_code(&result).as_deref() {
                Some("io/duplicate-record") => RecordOutcome::AlreadyRecorded,
                Some("path/not-found" | "events/unknown-kind") => RecordOutcome::Unsupported,
                Some(other) => RecordOutcome::Failed(other.to_owned()),
                None => RecordOutcome::Failed("tool-error".to_owned()),
            }
        })
    }
}

/// A wiki link to the note at the vault path `path`, labelled `label`.
fn link_to(path: &str, label: &str) -> String {
    format!("[[{}|{label}]]", path.strip_suffix(".md").unwrap_or(path))
}

/// The events the finished runs in `history` call for, lowest run first: `job.failed` for each scheduled run that
/// failed, and `job.recovered` for each scheduled run that succeeded straight after one that failed. `resumed` is when
/// the job was resumed from a suspension that has been recorded and not yet recovered from: the first scheduled run
/// that succeeded after that moment also makes a `job.recovered`. Only runs that started within [`LOOKBACK`] of `now`
/// are considered, and dry runs and runs started by hand never are.
#[must_use]
pub(crate) fn run_events(
    job: &ObservedJob,
    history: &[RunSummary],
    resumed: Option<OffsetDateTime>,
    not_before: OffsetDateTime,
) -> Vec<JobEvent> {
    let mut scheduled: Vec<(u64, &RunSummary)> = history
        .iter()
        .filter(|record| !record.dry_run && record.trigger == "scheduled")
        .filter(|record| {
            matches!(
                record.status.as_str(),
                "succeeded" | "degraded" | "failed" | "cancelled" | "interrupted"
            )
        })
        .filter_map(|record| record.number().map(|number| (number, record)))
        .collect();
    scheduled.sort_by_key(|(number, _)| *number);
    let recent = |record: &RunSummary| {
        OffsetDateTime::parse(&record.started, &Rfc3339).is_ok_and(|started| started >= not_before)
    };
    let mut events = Vec::new();
    let mut previous: Option<&RunSummary> = None;
    // Whether a scheduled run has failed and no scheduled run has succeeded since. Runs that ended cancelled,
    // interrupted, or degraded neither recover the job nor forget the failure.
    let mut failure_open = false;
    for (number, record) in scheduled {
        let label = format!("R-{number}");
        let event = |kind, code| JobEvent {
            kind,
            package: job.package.clone(),
            workflow: job.workflow_slug.clone(),
            job: job.job_slug.clone(),
            entity: job.entity.as_str().to_owned(),
            subject: link_to(&record.path, &label),
            run: Some(label.clone()),
            code,
            suspended_at: None,
            retired_at: None,
            replaced_by: None,
        };
        match record.status.as_str() {
            "failed" => {
                failure_open = true;
                if recent(record) {
                    events.push(event(
                        JobEventKind::Failed,
                        Some(
                            record
                                .code
                                .clone()
                                .filter(|code| !code.is_empty())
                                .unwrap_or_else(|| UNKNOWN_FAILURE_CODE.to_owned()),
                        ),
                    ));
                }
            }
            "succeeded" => {
                if recent(record) && (failure_open || first_after_resumption(record, previous, resumed)) {
                    events.push(event(JobEventKind::Recovered, None));
                }
                failure_open = false;
            }
            _ => {}
        }
        previous = Some(record);
    }
    events
}

/// Whether `record` is the first scheduled run to start since the job was resumed at `resumed`.
fn first_after_resumption(record: &RunSummary, previous: Option<&RunSummary>, resumed: Option<OffsetDateTime>) -> bool {
    let started_at = |run: &RunSummary| OffsetDateTime::parse(&run.started, &Rfc3339).ok();
    let Some(resumed) = resumed else {
        return false;
    };
    started_at(record).is_some_and(|started| started >= resumed)
        && previous.and_then(started_at).is_none_or(|before| before < resumed)
}

/// When the job was resumed from a suspension the vault records as unrecovered, if its first run since could be that
/// recovery. The vault is asked only when `history` holds a scheduled success since the job last changed state, which
/// is rare, so an ordinary pass never queries for it.
async fn resumption(
    sink: &dyn EventSink,
    job: &ObservedJob,
    state: &JobsState,
    history: &[RunSummary],
) -> Result<Option<OffsetDateTime>, SuspensionUnreadable> {
    let Some(changed) = state
        .job(&job.package, &job.workflow_slug, &job.job_slug)
        .filter(|current| current.lifecycle() == JobLifecycle::Enabled)
        .and_then(JobState::lifecycle_changed)
    else {
        return Ok(None);
    };
    let succeeded_since = history.iter().any(|record| {
        !record.dry_run
            && record.trigger == "scheduled"
            && record.status == "succeeded"
            && OffsetDateTime::parse(&record.started, &Rfc3339).is_ok_and(|started| started >= changed)
    });
    if !succeeded_since {
        return Ok(None);
    }
    let key = JobKey {
        package: job.package.clone(),
        workflow: job.workflow_slug.clone(),
        job: job.job_slug.clone(),
    };
    Ok(sink.suspension_open(&key).await?.then_some(changed))
}

/// [`resumption`], where a suspension that could not be read is not taken for no recovery: whether it is still open is
/// unknown, so the job's runs are arranged to be read again on the next pass.
async fn resumption_or_retry(
    sink: &dyn EventSink,
    memory: &JobAttentionMemory,
    vault_name: &str,
    key: &JobKey,
    job: &ObservedJob,
    state: &JobsState,
    history: &[RunSummary],
) -> Option<OffsetDateTime> {
    resumption(sink, job, state, history)
        .await
        .unwrap_or_else(|SuspensionUnreadable| {
            memory.forget_event_counter(vault_name, key);
            None
        })
}

/// The `job.suspended` event for the job's current suspension, when the system made it and recorded when.
#[must_use]
pub(crate) fn suspension_event(job: &ObservedJob, state: &JobsState) -> Option<JobEvent> {
    if state.lifecycle_of(&job.package, &job.workflow_slug, &job.job_slug) != JobLifecycle::Suspended {
        return None;
    }
    let record = state
        .job(&job.package, &job.workflow_slug, &job.job_slug)
        .and_then(JobState::suspension)?;
    let (Some(Actor::System), Some(at)) = (record.by, record.at) else {
        return None;
    };
    let file = format!("registry/workflows/{}/{}/jobs/{}", job.package, job.directory, job.file);
    Some(JobEvent {
        kind: JobEventKind::Suspended,
        package: job.package.clone(),
        workflow: job.workflow_slug.clone(),
        job: job.job_slug.clone(),
        entity: job.entity.as_str().to_owned(),
        subject: link_to(&file, &job.job_slug),
        run: None,
        code: Some(record.code.clone()),
        suspended_at: at.to_offset(UtcOffset::UTC).format(&Rfc3339).ok(),
        retired_at: None,
        replaced_by: None,
    })
}

/// The `job.retired` event for a job a successor's enabling retired, when the state records when.
#[must_use]
pub(crate) fn retirement_event(job: &ObservedJob, state: &JobsState) -> Option<JobEvent> {
    if state.lifecycle_of(&job.package, &job.workflow_slug, &job.job_slug) != JobLifecycle::Retired {
        return None;
    }
    let retirement = state
        .job(&job.package, &job.workflow_slug, &job.job_slug)
        .and_then(JobState::retirement)?;
    let file = format!("registry/workflows/{}/{}/jobs/{}", job.package, job.directory, job.file);
    Some(JobEvent {
        kind: JobEventKind::Retired,
        package: job.package.clone(),
        workflow: job.workflow_slug.clone(),
        job: job.job_slug.clone(),
        entity: job.entity.as_str().to_owned(),
        subject: link_to(&file, &job.job_slug),
        run: None,
        code: None,
        suspended_at: None,
        retired_at: retirement.at.to_offset(UtcOffset::UTC).format(&Rfc3339).ok(),
        replaced_by: Some(retirement.replaced_by.job.clone()),
    })
}

/// What the step is given.
pub(crate) struct JobEventPass<'a> {
    pub client: &'a McpClient,
    pub host: &'a HostContext,
    pub vault_name: &'a str,
    pub observed: &'a [ObservedJob],
    pub state: &'a JobsState,
    pub now: OffsetDateTime,
}

/// Records the events the vault's jobs call for. See the module documentation.
pub(crate) async fn record_job_events(pass: JobEventPass<'_>) {
    let JobEventPass {
        client,
        host,
        vault_name,
        observed,
        state,
        now,
    } = pass;
    let memory = &host.attention;
    if memory
        .events_unsupported_since(vault_name)
        .is_some_and(|since| now - since < UNSUPPORTED_PAUSE)
    {
        return;
    }
    let sink = McpEventSink { client, vault_name };
    let reconciled = crate::job_events_marker::read(host, vault_name).await;
    let not_before = reconciled.since().unwrap_or(now) - FIRST_RECONCILE_LOOKBACK;
    let mut pending: Vec<(JobKey, JobEvent)> = Vec::new();
    for job in observed {
        let key = JobKey {
            package: job.package.clone(),
            workflow: job.workflow_slug.clone(),
            job: job.job_slug.clone(),
        };
        let mut found: Vec<JobEvent> = suspension_event(job, state)
            .into_iter()
            .chain(retirement_event(job, state))
            .collect();
        found.extend(job_run_events(client, host, vault_name, job, state, not_before).await);
        pending.extend(
            found
                .into_iter()
                .filter(|event| !memory.knows_event(vault_name, &event.id()))
                .map(|event| (key.clone(), event)),
        );
    }
    record_pending(&sink, memory, vault_name, &pending, now).await;
    if reconciled.is_never() && sink.accepts_events().await {
        crate::job_events_marker::write(host, vault_name, now).await;
    }
}

/// The run events of one job whose runs are worth reading: a job that has left draft and whose run counter has moved
/// since they were last read.
async fn job_run_events(
    client: &McpClient,
    host: &HostContext,
    vault_name: &str,
    job: &ObservedJob,
    state: &JobsState,
    not_before: OffsetDateTime,
) -> Vec<JobEvent> {
    // A job disabled or taken out of use since a run failed still failed that run; only one that never left draft has
    // no scheduled runs to read.
    if state.lifecycle_of(&job.package, &job.workflow_slug, &job.job_slug) == JobLifecycle::Draft {
        return Vec::new();
    }
    let key = JobKey {
        package: job.package.clone(),
        workflow: job.workflow_slug.clone(),
        job: job.job_slug.clone(),
    };
    let Ok(slot) = claim_slot(host, vault_name, &key.package, &key.workflow, &key.job) else {
        return Vec::new();
    };
    let peeked = tokio::task::spawn_blocking(move || slot.peek_run_number()).await;
    let Ok(Ok(counter)) = peeked else {
        return Vec::new();
    };
    if !host.attention.event_counter_moved(vault_name, &key, counter) {
        return Vec::new();
    }
    let Ok(history) = run_history(client, vault_name, &key.package, &key.workflow, &key.job, HISTORY_LIMIT).await
    else {
        host.attention.forget_event_counter(vault_name, &key);
        return Vec::new();
    };
    if history
        .iter()
        .any(|record| record.status == "running" && !record.dry_run)
    {
        // A run still going is looked at again once it has ended.
        host.attention.forget_event_counter(vault_name, &key);
    }
    let sink = McpEventSink { client, vault_name };
    let resumed = resumption_or_retry(&sink, &host.attention, vault_name, &key, job, state, &history).await;
    run_events(job, &history, resumed, not_before)
}

/// Records each of `pending` through `sink`, and says what to log.
async fn record_pending(
    sink: &dyn EventSink,
    memory: &JobAttentionMemory,
    vault_name: &str,
    pending: &[(JobKey, JobEvent)],
    now: OffsetDateTime,
) {
    if pending.is_empty() {
        return;
    }
    if !sink.accepts_events().await {
        leave_vault_alone(memory, vault_name, pending, now);
        return;
    }
    for (key, event) in pending {
        let id = event.id();
        match sink.record(event).await {
            RecordOutcome::Recorded => {
                tracing::info!(vault = vault_name, kind = event.kind.name(), job = %key.job, "a job event was recorded");
                memory.remember_event(vault_name, &id, now);
            }
            RecordOutcome::AlreadyRecorded => memory.remember_event(vault_name, &id, now),
            RecordOutcome::Unsupported => {
                leave_vault_alone(memory, vault_name, pending, now);
                return;
            }
            RecordOutcome::Failed(code) => {
                if memory.first_event_failure_of(vault_name, &event.job_and_kind(), now) {
                    tracing::warn!(vault = vault_name, kind = event.kind.name(), job = %key.job, code = %code, "a job event could not be recorded, so it will be tried again");
                }
                memory.forget_event_counter(vault_name, key);
            }
        }
    }
}

/// Notes that the vault cannot take job events, logs it once, and arranges for every job's runs to be read again when
/// the vault is next looked at.
fn leave_vault_alone(
    memory: &JobAttentionMemory,
    vault_name: &str,
    pending: &[(JobKey, JobEvent)],
    now: OffsetDateTime,
) {
    if memory.note_events_unsupported(vault_name, now) {
        tracing::warn!(
            vault = vault_name,
            "the vault has no events base, no kind registry, or does not declare the job event kinds, so no job event is recorded"
        );
    }
    for (key, _) in pending {
        memory.forget_event_counter(vault_name, key);
    }
}

#[cfg(test)]
#[path = "job_events_test.rs"]
mod tests;
