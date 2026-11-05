//! The reconcile step of a scheduler pass over one vault: raises a task for each episode of a vault job that needs
//! somebody's attention, from the state the pass leaves and the run history, and nothing else.
//!
//! It is level triggered. Every pass reads what is durably there (the job state, the runs of an enabled job, the team)
//! and raises what is missing, so a failure the command line recorded, or one that happened while no scheduler was
//! running, is picked up by the next pass, and a raise that failed is simply tried again. It never changes a job, a
//! schedule, or a record, and it never returns an error to the pass: whatever goes wrong is logged once and retried.
//!
//! Which runs have been looked at is the host's examined mark (a file beside the job's claim). Every finished live run
//! above the mark is examined, lowest first, within the latest runs one history read returns: the first scheduled run
//! that failed after its script started raises a task unless one is open or the job's tasks already name it, the later
//! failures fold into that task, and the mark moves to the highest run examined once the decision is safely made. A run
//! the job's first-run trial still has to settle is not examined at all. A suspension that names a run covers that run's
//! failure: the scheduler moves the mark to it only after the settlement that recorded the suspension (a settlement that
//! was not recorded leaves the run to be looked at), and a pass that meets a failed run the job's suspension record
//! names treats it as covered and moves the mark past it, so the failure is never raised a second time.
//!
//! A mark that is not a number or cannot be read is reported once per job (until a read finds it usable), and the run
//! counter is forgotten on every pass while it is unusable, so the runs are looked at again as soon as it is repaired or
//! removed. A record whose `script_started` field is neither true nor false is reported once per job in the same way:
//! that run is not examined, and neither is anything after it, so the mark stays below it.

use contextos_core::{OwnerStanding, TeamRoster};
use contextos_workflows::{
    Assignee, AttentionEvent, AttentionPurpose, Episode, FinishedRun, JobFacts, JobKey, JobState, JobsState, Origin,
    RunStatus, RunTrigger, ends_before_the_script, events,
};
use time::OffsetDateTime;

use crate::job_attention::{
    FailedRun, JobAttentionMemory, JobCondition, JobFilePaths, TaskSource, is_unfolded_run_failure, raise_tasks,
};
use crate::job_runs::{RunSummary, ScriptStarted, run_history};
use crate::job_validation::ObservedJob;
use crate::mcp_client::McpClient;
use crate::run_claim::ClaimSlot;
use crate::run_settings::HostContext;
use crate::scheduler::read_state;
use crate::task_sink::{McpTaskSink, RaisedTask, TaskSink};
use crate::tasks_config::TasksConfig;
use crate::vault_runs::claim_slot;

/// How many of a job's latest run records are read to find the finished runs above the mark and the record of a run a
/// suspension names. Dry runs and skipped ticks share the history with the runs that matter.
const HISTORY_LIMIT: usize = 50;

/// What the reconcile step is given.
pub(crate) struct Reconcile<'a> {
    pub client: &'a McpClient,
    pub host: &'a HostContext,
    pub vault_name: &'a str,
    pub roster: &'a TeamRoster,
    /// What the pass saw of each of the vault's jobs.
    pub observed: &'a [ObservedJob],
    /// When the pass runs, in the operator's zone.
    pub now: OffsetDateTime,
}

/// Raises the tasks the vault's jobs need. See the module documentation.
pub(crate) async fn reconcile(input: Reconcile<'_>) {
    let Reconcile {
        client,
        host,
        vault_name,
        roster,
        observed,
        now,
    } = input;
    let Ok(state) = read_state(client, vault_name).await else {
        tracing::warn!(
            vault = vault_name,
            "the job state could not be read, so no task was raised this pass"
        );
        return;
    };
    crate::job_events::record_job_events(crate::job_events::JobEventPass {
        client,
        host,
        vault_name,
        observed,
        state: &state.value,
        now,
    })
    .await;
    let sink = McpTaskSink { client, vault_name };
    let mut tasks = Vec::new();
    let mut run_failed_keys = Vec::new();
    for job in observed {
        let found = JobEpisodes::gather(client, host, vault_name, job, &state.value).await;
        if let Some(through) = found.settled_through
            && let Ok(slot) = claim_slot(host, vault_name, &job.package, &job.workflow_slug, &job.job_slug)
        {
            advance_mark(&slot, vault_name, &found.facts.key, through).await;
        }
        let mut judging = Judging::default();
        for event in &found.events {
            if event.purpose == AttentionPurpose::RunFailed {
                let Episode::Run(number) = event.episode else {
                    continue;
                };
                let Some(slot) = judge_failure(client, host, &sink, job, &found, number, &mut judging).await else {
                    continue;
                };
                let through = found.examined_through.unwrap_or(number).max(number);
                run_failed_keys.push((found.facts.key.clone(), tasks.len(), through, slot));
            }
            let standing = OwnerStanding::from((job.declared_owner.as_ref(), roster));
            let assignee = Assignee::from((event, &standing, roster));
            let run_record = found.run_record_for(event);
            let files = JobFilePaths {
                workflow_directory: format!("{}/{}", job.package, job.directory),
                entry: job.entry.clone().unwrap_or_default(),
                job_file: job.file.clone(),
            };
            tasks.push(RaisedTask::from(TaskSource {
                event,
                facts: &found.facts,
                files: &files,
                run_record: run_record.as_deref(),
                entity: job.entity.as_str(),
                assignee: &assignee,
                at: found.episode_time(event, now),
            }));
        }
        if judging.folded_all()
            && let Some(through) = found.examined_through
            && let Ok(slot) = claim_slot(host, vault_name, &job.package, &job.workflow_slug, &job.job_slug)
        {
            // Every failure was folded, and the runs after the last were examined with them.
            advance_mark(&slot, vault_name, &found.facts.key, through).await;
        }
    }
    if tasks.is_empty() {
        return;
    }
    raise_tasks(&sink, &host.attention, vault_name, &tasks).await;
    for (key, index, number, slot) in run_failed_keys {
        let raised = tasks
            .get(index)
            .is_some_and(|task| host.attention.knows(vault_name, &task.origin));
        if raised {
            // The create succeeded, or the task was there already, so the run has been decided.
            advance_mark(&slot, vault_name, &key, number).await;
        } else {
            // It was not raised, so the run is looked at again on the next pass, with its mark unmoved.
            host.attention.forget_run_counter(vault_name, &key);
        }
    }
}

/// What deciding a job's failed runs has found so far, lowest run first.
#[derive(Default)]
struct Judging {
    /// A run was found to need a task, so the failures after it fold into it.
    raising: bool,
    /// At least one run was folded into an open or named task.
    folded: bool,
    /// A run could not be decided, so none after it is.
    undecided: bool,
}

impl Judging {
    /// Whether every failure so far was folded, so nothing is raised and nothing is left to decide.
    const fn folded_all(&self) -> bool {
        self.folded && !self.raising && !self.undecided
    }
}

/// Decides one failed run of a job: the first that no task covers raises one, and those after it fold into it. Returns
/// the job's claim slot when the run needs a task of its own, so the caller can move the mark once the task exists.
async fn judge_failure(
    client: &McpClient,
    host: &HostContext,
    sink: &McpTaskSink<'_>,
    job: &ObservedJob,
    found: &JobEpisodes,
    number: u64,
    judging: &mut Judging,
) -> Option<ClaimSlot> {
    if judging.raising || judging.undecided {
        return None;
    }
    let vault_name = sink.vault_name;
    let config = crate::tasks_config::load_tasks_config(client, vault_name).await;
    let slot = claim_slot(host, vault_name, &job.package, &job.workflow_slug, &job.job_slug).ok()?;
    let decision = decide_run_failure(
        sink,
        &host.attention,
        vault_name,
        &found.facts.key,
        &config,
        &slot,
        &FailedRun { number },
    )
    .await;
    match decision {
        RunDecision::New => {
            judging.raising = true;
            Some(slot)
        }
        RunDecision::Folded => {
            judging.folded = true;
            None
        }
        RunDecision::Undecided => {
            judging.undecided = true;
            None
        }
    }
}

/// Moves the job's examined mark to `number` off the async executor, and logs a mark that cannot be moved.
pub(crate) async fn advance_mark(slot: &ClaimSlot, vault_name: &str, key: &JobKey, number: u64) {
    let slot = slot.clone();
    let moved = tokio::task::spawn_blocking(move || slot.advance_run_failure_mark(number)).await;
    if let Ok(Err(error)) = moved {
        tracing::warn!(vault = vault_name, workflow = %key.workflow, job = %key.job, error = %error, "the run-failed mark could not be moved");
    }
}

/// What one job's facts say.
struct JobEpisodes {
    facts: JobFacts,
    events: Vec<AttentionEvent>,
    /// The job's latest run records, for the record of the run an event concerns.
    history: Vec<RunSummary>,
    /// The record of the run a suspension names, found on its own because a record whose other fields are unusable
    /// still has a path.
    suspended_record: Option<String>,
    /// The highest run examined, which the mark moves to once what the runs above it call for has been decided.
    examined_through: Option<u64>,
    /// `examined_through` when no run-failed task is left to raise for the job (no run failed, or the failure's task is
    /// already known), so the mark can move at once.
    settled_through: Option<u64>,
}

impl JobEpisodes {
    async fn gather(
        client: &McpClient,
        host: &HostContext,
        vault_name: &str,
        job: &ObservedJob,
        state: &JobsState,
    ) -> Self {
        let key = JobKey {
            package: job.package.clone(),
            workflow: job.workflow_slug.clone(),
            job: job.job_slug.clone(),
        };
        let stored = state.job(&job.package, &job.workflow_slug, &job.job_slug);
        let lifecycle = state.lifecycle_of(&job.package, &job.workflow_slug, &job.job_slug);
        let facts = JobFacts {
            key: key.clone(),
            lifecycle,
            ready: job.ready,
            version: job.version.clone(),
            validation: stored.and_then(JobState::validation).cloned(),
            suspension: stored.and_then(JobState::suspension).cloned(),
            latest_run: None,
        };
        let mut history = Vec::new();
        let mut examined_through = None;
        let mut failures = Vec::new();
        // The run a suspension record names is the failure that suspended the job, which the suspension's task covers.
        let covered = facts.suspension.as_ref().and_then(|record| record.run);
        let suspended_with_run = lifecycle == contextos_workflows::JobLifecycle::Suspended && covered.is_some();
        if lifecycle == contextos_workflows::JobLifecycle::Enabled || suspended_with_run {
            let on_trial = stored.is_some_and(JobState::first_run_pending);
            let looked = look_at_runs(client, host, vault_name, &key, on_trial, covered).await;
            history = looked.history;
            examined_through = looked.through;
            failures = looked.failures;
        }
        // Only an episode not yet known to be raised needs more than the facts: the record of the run a suspension names.
        let mut found = events(&facts);
        // One run-failed event for each run above the mark that failed on a schedule after its script started, which is
        // the only kind of run that can need a task: a run that never reached its script is no defect of the job.
        for run in failures {
            let with_run = JobFacts {
                latest_run: Some(run),
                ..facts.clone()
            };
            found.extend(
                events(&with_run)
                    .into_iter()
                    .filter(|event| event.purpose == AttentionPurpose::RunFailed),
            );
        }
        found.retain(|event| {
            !host
                .attention
                .knows(vault_name, &Origin::from((&event.purpose, &key, &event.episode)))
        });
        let settled_through =
            examined_through.filter(|_| !found.iter().any(|event| event.purpose == AttentionPurpose::RunFailed));
        let suspended_record = match covered {
            Some(number) if found.iter().any(|event| event.purpose == AttentionPurpose::Suspended) => {
                crate::job_runs::run_record_path(client, vault_name, &key.package, &key.workflow, &key.job, number)
                    .await
            }
            _ => None,
        };
        Self {
            facts,
            events: found,
            history,
            suspended_record,
            examined_through,
            settled_through,
        }
    }

    /// When an event's episode began as far as the facts do not say: the start of the run a run-failed event is for,
    /// and otherwise `now`.
    fn episode_time(&self, event: &AttentionEvent, now: OffsetDateTime) -> OffsetDateTime {
        let AttentionPurpose::RunFailed = event.purpose else {
            return now;
        };
        let Episode::Run(number) = event.episode else {
            return now;
        };
        self.history
            .iter()
            .find(|record| record.number() == Some(number) && !record.dry_run)
            .and_then(|record| {
                OffsetDateTime::parse(&record.started, &time::format_description::well_known::Rfc3339).ok()
            })
            .unwrap_or(now)
    }

    /// The vault path of the run record an event concerns: the run that failed the trial for a suspension, and the run
    /// that failed for a run-failed event.
    fn run_record_for(&self, event: &AttentionEvent) -> Option<String> {
        let number = match event.purpose {
            AttentionPurpose::Suspended => return self.suspended_record.clone(),
            AttentionPurpose::RunFailed => match event.episode {
                Episode::Run(number) => number,
                _ => return None,
            },
            AttentionPurpose::ValidationFailed => return None,
        };
        self.history
            .iter()
            .find(|record| record.number() == Some(number) && !record.dry_run)
            .map(|record| record.path.clone())
    }
}

/// What deciding a failed run found.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RunDecision {
    /// No task is open or names the run, and it is above the mark: raise one.
    New,
    /// A task is open or names the run, so it is folded in. The mark has been moved to it.
    Folded,
    /// The mark or the task list could not be read, so nothing was decided and the mark stays.
    Undecided,
}

/// Decides whether a failed scheduled run is one to raise a task for. See [`is_unfolded_run_failure`], whose rule this
/// applies to the job's tasks as they are now and to the host's examined mark: it is decided from the task list, the
/// mark, and the run's number, never from what this process remembers or from any file's modification time.
///
/// A run that is folded (a task is open, or its number is at or below the mark or a run a task names) moves the mark to
/// it here. A run that is new does not: the caller moves the mark once the task is created, so a create that fails is
/// tried again. When the tasks cannot be listed, or the mark cannot be read, nothing can be decided and the mark stays,
/// and the job's run counter is forgotten so the run is looked at again on the next pass; a mark that is not a number
/// is reported once per job and left for the operator (see [`read_mark`]).
pub(crate) async fn decide_run_failure(
    sink: &dyn TaskSink,
    memory: &JobAttentionMemory,
    vault_name: &str,
    key: &JobKey,
    config: &TasksConfig,
    slot: &ClaimSlot,
    run: &FailedRun,
) -> RunDecision {
    let Some(mark) = read_mark(slot, memory, vault_name, key).await else {
        return RunDecision::Undecided;
    };
    let prefix = format!("contextos-web:run-failed:{}/{}/{}:", key.package, key.workflow, key.job);
    let existing = match sink.tasks_with_origin_prefix(&prefix).await {
        Ok(existing) => existing,
        Err(code) => {
            memory.forget_run_counter(vault_name, key);
            tracing::warn!(vault = vault_name, workflow = %key.workflow, job = %key.job, code = %code, "the tasks could not be listed, so a failed run waits for the next pass");
            return RunDecision::Undecided;
        }
    };
    if is_unfolded_run_failure(&existing, config, mark, run) {
        return RunDecision::New;
    }
    advance_mark(slot, vault_name, key, run.number).await;
    RunDecision::Folded
}

/// Reads the job's examined mark off the async executor. A mark that cannot be used (it is not a number, it cannot be
/// read, or the read was lost) is reported once per job until a read finds it usable, and the job's run counter is
/// forgotten, so its runs are looked at again on every pass until the mark is repaired or removed. `None` is that
/// problem, and `Some(None)` is a job with no mark yet.
async fn read_mark(
    slot: &ClaimSlot,
    memory: &JobAttentionMemory,
    vault_name: &str,
    key: &JobKey,
) -> Option<Option<u64>> {
    let read = {
        let slot = slot.clone();
        tokio::task::spawn_blocking(move || slot.run_failure_mark()).await
    };
    let problem = match read {
        Ok(Ok(mark)) => {
            let _ = memory.note_condition(JobCondition::MarkUnusable, vault_name, key, false);
            return Some(mark);
        }
        Ok(Err(error)) => error.to_string(),
        Err(error) => error.to_string(),
    };
    memory.forget_run_counter(vault_name, key);
    if memory.note_condition(JobCondition::MarkUnusable, vault_name, key, true) {
        tracing::warn!(vault = vault_name, workflow = %key.workflow, job = %key.job, error = %problem, "the run-failed mark could not be read, so a failed run raises no task until it is fixed");
    }
    None
}

/// What looking at a job's runs found.
struct LookedAt {
    history: Vec<RunSummary>,
    /// The runs above the mark that failed on a schedule after their scripts started, lowest first.
    failures: Vec<FinishedRun>,
    /// The highest run above the mark that was examined.
    through: Option<u64>,
}

impl LookedAt {
    const fn nothing() -> Self {
        Self {
            history: Vec::new(),
            failures: Vec::new(),
            through: None,
        }
    }
}

/// Examines the job's finished live runs above the examined mark, lowest first, but only when the job's run counter has
/// moved since it was last looked at. A run still going when it is looked at leaves the counter to be looked at again
/// once the run has ended.
///
/// A job on its first-run trial has none of its runs examined: the trial's settlement owns the outcome of the version
/// on trial, and a failure it suspends the job for is not raised again. The counter is forgotten, so the runs are looked
/// at as soon as the trial is over.
///
/// `covered` is the run the job's current suspension record names. Its failure is the suspension's to report, so it
/// raises no run-failed task and the mark moves past it, whether or not the scheduler had moved the mark itself yet. A
/// suspension record exists only while the job is suspended, so this reads the durable state and never the order of the
/// scheduler's steps.
async fn look_at_runs(
    client: &McpClient,
    host: &HostContext,
    vault_name: &str,
    key: &JobKey,
    on_trial: bool,
    covered: Option<u64>,
) -> LookedAt {
    let Ok(slot) = claim_slot(host, vault_name, &key.package, &key.workflow, &key.job) else {
        return LookedAt::nothing();
    };
    let peeked = {
        let slot = slot.clone();
        tokio::task::spawn_blocking(move || slot.peek_run_number()).await
    };
    let Ok(Ok(counter)) = peeked else {
        return LookedAt::nothing();
    };
    if !host.attention.run_counter_moved(vault_name, key, counter) {
        return LookedAt::nothing();
    }
    if on_trial && covered.is_none() {
        host.attention.forget_run_counter(vault_name, key);
        return LookedAt::nothing();
    }
    let Some(mark) = read_mark(&slot, &host.attention, vault_name, key).await else {
        return LookedAt::nothing();
    };
    let Ok(history) = run_history(client, vault_name, &key.package, &key.workflow, &key.job, HISTORY_LIMIT).await
    else {
        host.attention.forget_run_counter(vault_name, key);
        return LookedAt::nothing();
    };
    if history
        .iter()
        .any(|record| record.status == "running" && !record.dry_run)
    {
        host.attention.forget_run_counter(vault_name, key);
    }
    let mut unexamined: Vec<(&RunSummary, FinishedRun)> = history
        .iter()
        .filter_map(|record| FinishedRun::try_from(record).ok().map(|run| (record, run)))
        .filter(|(_, run)| mark.is_none_or(|mark| run.number > mark))
        .collect();
    unexamined.sort_by_key(|(_, run)| run.number);
    let mut failures = Vec::new();
    let mut through = None;
    let mut unreadable = false;
    for (record, run) in unexamined {
        if run.trigger == RunTrigger::Scheduled && run.status == RunStatus::Failed && covered != Some(run.number) {
            let started = match record.script_started {
                ScriptStarted::Yes => true,
                ScriptStarted::No => false,
                ScriptStarted::Absent => !ends_before_the_script(run.failure_code.as_deref()),
                ScriptStarted::Invalid => {
                    // Neither this run nor any after it can be told from a failure of the job, so none is examined and
                    // the mark stays below them until the record is repaired.
                    unreadable = true;
                    if host
                        .attention
                        .note_condition(JobCondition::RecordUnreadable, vault_name, key, true)
                    {
                        tracing::warn!(vault = vault_name, workflow = %key.workflow, job = %key.job, record = %record.path, "a run record's script_started field is neither true nor false, so that run and the runs after it are not examined until it is repaired");
                    }
                    break;
                }
            };
            if started {
                failures.push(run.clone());
            }
        }
        through = Some(run.number);
    }
    if unreadable {
        host.attention.forget_run_counter(vault_name, key);
    } else {
        let _ = host
            .attention
            .note_condition(JobCondition::RecordUnreadable, vault_name, key, false);
    }
    LookedAt {
        history,
        failures,
        through,
    }
}

/// A run record that says how a live run ended. A dry run, a run still going, and a tick that was skipped are not one.
pub(crate) struct NotFinished;

impl TryFrom<&RunSummary> for FinishedRun {
    type Error = NotFinished;

    fn try_from(record: &RunSummary) -> Result<Self, NotFinished> {
        let status = match record.status.as_str() {
            "succeeded" => RunStatus::Succeeded,
            "degraded" => RunStatus::Degraded,
            "failed" => RunStatus::Failed,
            "cancelled" => RunStatus::Cancelled,
            "interrupted" => RunStatus::Interrupted,
            _ => return Err(NotFinished),
        };
        let trigger = match record.trigger.as_str() {
            "scheduled" => RunTrigger::Scheduled,
            "manual" => RunTrigger::Manual,
            _ => return Err(NotFinished),
        };
        if record.dry_run {
            return Err(NotFinished);
        }
        Ok(Self {
            number: record.number().ok_or(NotFinished)?,
            trigger,
            status,
            dry_run: false,
            failure_code: record.code.clone().filter(|code| !code.is_empty()),
        })
    }
}

#[cfg(test)]
#[path = "job_reconcile_test.rs"]
mod tests;
