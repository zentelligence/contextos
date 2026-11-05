//! Raising a task when a vault job needs somebody's attention (one reconcile step per vault, in the Web scheduler's
//! pass). What needs attention, the origin, and the assignee are decided by `contextos_workflows`; this module turns an
//! episode into a task note, remembers what it has raised so it does not ask again, and decides what to log.
//!
//! The note cites evidence by vault path and stable code only: never a failure message, a call's arguments or result,
//! an input value, script text, or a host path.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use contextos_workflows::{Assignee, AttentionEvent, JobFacts, JobKey, Origin};
use time::OffsetDateTime;

use crate::task_sink::{ExistingTask, RaiseOutcome, RaisedTask, TaskSink};
use crate::tasks_config::{Closes, TasksConfig};

/// Where a job's files are, for the note's evidence section.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobFilePaths {
    /// The path under `registry/workflows/` that holds the job's workflow: its package's folder and its own.
    pub workflow_directory: String,
    pub entry: String,
    /// The job file's name within the workflow's `jobs/` folder.
    pub job_file: String,
}

/// Everything a task note is made from.
pub struct TaskSource<'a> {
    pub event: &'a AttentionEvent,
    pub facts: &'a JobFacts,
    pub files: &'a JobFilePaths,
    /// The vault path of the run record the episode concerns, when there is one.
    pub run_record: Option<&'a str>,
    pub entity: &'a str,
    pub assignee: &'a Assignee,
    /// When the episode happened, for the kinds whose facts do not say: a failed run's start. The scheduler's own clock
    /// is only the last resort, since a task says when something happened and not when it was noticed.
    pub at: OffsetDateTime,
}

impl From<TaskSource<'_>> for RaisedTask {
    fn from(source: TaskSource<'_>) -> Self {
        let TaskSource {
            event,
            facts,
            files,
            run_record,
            entity,
            assignee,
            at,
        } = source;
        let origin = Origin::from((&event.purpose, &facts.key, &event.episode));
        let source = TaskSource {
            event,
            facts,
            files,
            run_record,
            entity,
            assignee,
            at,
        };
        Self {
            origin,
            title: crate::job_task_note::title_of(&source),
            entity: entity.to_owned(),
            assignee: assignee.slug().clone(),
            body: crate::job_task_note::body_of(&source),
        }
    }
}

/// A condition worth one log line, reported once until it changes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AttentionWarning {
    /// The vault has no task system, so nothing is raised.
    NoTaskSystem { vault: String },
    /// A raise failed and will be tried again on the next pass.
    RaiseFailed { origin: String, code: String },
}

/// What one call to [`raise_tasks`] did.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RaiseReport {
    pub created: usize,
    pub already_raised: usize,
    pub warnings: Vec<AttentionWarning>,
}

/// A change in a vault's team problem that was worth a log line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TeamLogEntry {
    /// The team broke this rule: one `warn` when the problem starts.
    Started(contextos_core::TeamRule),
    /// The team is valid again: one `info` when the problem clears.
    Cleared,
}

/// A change in whether a vault's team can be read at all that was worth a log line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutageLogEntry {
    /// The team cannot be read: one `warn` when the outage starts.
    Started,
    /// The team can be read again: one `info` when the outage clears.
    Cleared,
}

/// A condition of one job that is logged once when it starts and is not logged again until it has cleared.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum JobCondition {
    /// The host's examined mark cannot be read or is not a number.
    MarkUnusable,
    /// A run record's `script_started` field is neither true nor false.
    RecordUnreadable,
}

/// A problem with the host's note of when a vault's job events were first reconciled.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MarkerProblem {
    /// The note exists but could not be read.
    Unreadable,
    /// The note was read and is not a time.
    Corrupt,
    /// The note could not be written.
    Unwritable,
}

#[derive(Debug, Default)]
struct Remembered {
    /// The marker problems logged and not yet cleared, by vault.
    marker_problems: HashSet<(MarkerProblem, String)>,
    team_problem: HashMap<String, contextos_core::TeamRule>,
    /// The job conditions logged and not yet cleared, by vault, workflow, and job.
    conditions: HashSet<(JobCondition, String, String, String)>,
    /// The vaults whose team cannot be read at all, until a pass reads it.
    team_outage: HashSet<String>,
    raised: HashMap<String, HashSet<String>>,
    examined: HashMap<(String, String, String), u64>,
    /// The vaults whose lack of a task system has been logged, until a raise there succeeds.
    no_task_system_logged: HashSet<String>,
    /// The failures logged, by vault, origin, and code, so a failure that repeats is logged once.
    failures_logged: HashSet<(String, String, String)>,
    /// The job events known to be recorded, by vault, with when each was noted, so a pass makes no further call for them.
    /// Nothing older than [`EVENT_MEMORY`] is kept: a run that old is never recorded again, and a suspension that old is
    /// at worst asked for once more, and answered as already recorded.
    events_recorded: HashMap<String, HashMap<String, OffsetDateTime>>,
    /// The run counter last looked at for job events, by vault, workflow, and job.
    events_examined: HashMap<(String, String, String), u64>,
    /// The vaults that cannot take job events (no events base, no kind registry, or a kind not declared), with when
    /// that was found, so the vault is looked at again only after a pause.
    events_unsupported: HashMap<String, OffsetDateTime>,
    /// The failures to record a job event logged, by vault and the job and kind (see `JobEvent::job_and_kind`), with
    /// when, forgotten after [`EVENT_MEMORY`].
    event_failures_logged: HashMap<(String, String), OffsetDateTime>,
}

/// How long the scheduler remembers a job event it recorded or failed to record: the lookback in which a run can still be
/// recorded.
const EVENT_MEMORY: time::Duration = time::Duration::days(7);

/// What the scheduler remembers between passes so it does not repeat a call: the origins known to be raised, the run
/// counter last examined for each job, the team problem last logged, and the job conditions logged. Never persisted: a restart costs at most one refused call per episode still
/// present.
#[derive(Clone, Debug, Default)]
pub struct JobAttentionMemory {
    inner: Arc<Mutex<Remembered>>,
}

impl JobAttentionMemory {
    /// Notes that `origin` is raised in `vault`, and that the vault has a task system after all.
    fn remember(&self, vault: &str, origin: &Origin) {
        if let Ok(mut inner) = self.inner.lock() {
            inner
                .raised
                .entry(vault.to_owned())
                .or_default()
                .insert(origin.to_string());
            inner.no_task_system_logged.remove(vault);
        }
    }

    /// Whether this is the first time `code` is seen for `origin` in `vault` since the last success, so it is worth a
    /// log line. `origin` is `None` for the vault having no task system.
    fn first_failure_of(&self, vault: &str, origin: Option<&str>, code: &str) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            return true;
        };
        match origin {
            None => inner.no_task_system_logged.insert(vault.to_owned()),
            Some(origin) => inner
                .failures_logged
                .insert((vault.to_owned(), origin.to_owned(), code.to_owned())),
        }
    }

    /// Whether `origin` is known to be raised in `vault`.
    #[must_use]
    pub fn knows(&self, vault: &str, origin: &Origin) -> bool {
        self.inner.lock().is_ok_and(|inner| {
            inner
                .raised
                .get(vault)
                .is_some_and(|raised| raised.contains(&origin.to_string()))
        })
    }

    /// Notes whether `condition` holds for the job now, and says whether it has just started, which is the one time it
    /// is worth a log line. A pass that finds it still holding, or still absent, says nothing.
    #[must_use]
    pub fn note_condition(&self, condition: JobCondition, vault: &str, key: &JobKey, holds: bool) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            return holds;
        };
        let entry = (condition, vault.to_owned(), key.workflow.clone(), key.job.clone());
        if holds {
            inner.conditions.insert(entry)
        } else {
            inner.conditions.remove(&entry);
            false
        }
    }

    /// Notes whether `problem` holds for the vault's reconcile marker now, and says whether it has just started, which
    /// is the one time it is worth a log line: a pass that finds it still holding, or still absent, says nothing.
    #[must_use]
    pub fn note_marker_problem(&self, problem: MarkerProblem, vault: &str, holds: bool) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            return holds;
        };
        let entry = (problem, vault.to_owned());
        if holds {
            inner.marker_problems.insert(entry)
        } else {
            inner.marker_problems.remove(&entry);
            false
        }
    }

    /// Whether the job's run counter differs from the one last examined, noting `counter` as examined. The first
    /// examination is always a change.
    #[must_use]
    pub fn run_counter_moved(&self, vault: &str, key: &JobKey, counter: u64) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            return true;
        };
        let slot = (vault.to_owned(), key.workflow.clone(), key.job.clone());
        inner.examined.insert(slot, counter) != Some(counter)
    }

    /// Forgets the counter examined for the job, so the next pass examines its runs again: for a run that had not
    /// finished when it was looked at, and for a task that could not be raised.
    pub fn forget_run_counter(&self, vault: &str, key: &JobKey) {
        if let Ok(mut inner) = self.inner.lock() {
            inner
                .examined
                .remove(&(vault.to_owned(), key.workflow.clone(), key.job.clone()));
        }
    }

    /// Notes what a pass found of the vault's team, and says what changed worth logging: a problem that starts, or one
    /// that clears. `None` is a valid team. Nothing is said for a pass that finds what the last one did.
    #[must_use]
    pub fn note_team(&self, vault: &str, problem: Option<contextos_core::TeamRule>) -> Option<TeamLogEntry> {
        let mut inner = self.inner.lock().ok()?;
        let before = inner.team_problem.get(vault).copied();
        if before == problem {
            return None;
        }
        let entry = if let Some(rule) = problem {
            inner.team_problem.insert(vault.to_owned(), rule);
            TeamLogEntry::Started(rule)
        } else {
            inner.team_problem.remove(vault);
            TeamLogEntry::Cleared
        };
        Some(entry)
    }

    /// Whether the job event `id` is known to be recorded in `vault`.
    #[must_use]
    pub fn knows_event(&self, vault: &str, id: &str) -> bool {
        self.inner.lock().is_ok_and(|inner| {
            inner
                .events_recorded
                .get(vault)
                .is_some_and(|known| known.contains_key(id))
        })
    }

    /// Notes that the job event `id` is recorded in `vault`, and that the vault takes job events after all.
    pub fn remember_event(&self, vault: &str, id: &str, now: OffsetDateTime) {
        if let Ok(mut inner) = self.inner.lock() {
            let known = inner.events_recorded.entry(vault.to_owned()).or_default();
            known.insert(id.to_owned(), now);
            known.retain(|_, noted| now - *noted <= EVENT_MEMORY);
            inner
                .event_failures_logged
                .retain(|_, noted| now - *noted <= EVENT_MEMORY);
            inner.events_unsupported.remove(vault);
        }
    }

    /// Whether the job's run counter differs from the one last looked at for job events, noting `counter` as looked at.
    /// The first look is always a change.
    #[must_use]
    pub fn event_counter_moved(&self, vault: &str, key: &JobKey, counter: u64) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            return true;
        };
        let slot = (vault.to_owned(), key.workflow.clone(), key.job.clone());
        inner.events_examined.insert(slot, counter) != Some(counter)
    }

    /// Forgets the counter looked at for job events, so the job's runs are read again on the next pass.
    pub fn forget_event_counter(&self, vault: &str, key: &JobKey) {
        if let Ok(mut inner) = self.inner.lock() {
            inner
                .events_examined
                .remove(&(vault.to_owned(), key.workflow.clone(), key.job.clone()));
        }
    }

    /// Notes that `vault` cannot take job events as of `now`, and says whether that is news worth a log line.
    #[must_use]
    pub fn note_events_unsupported(&self, vault: &str, now: OffsetDateTime) -> bool {
        self.inner.lock().map_or(true, |mut inner| {
            inner.events_unsupported.insert(vault.to_owned(), now).is_none()
        })
    }

    /// When `vault` was found unable to take job events, until a record there succeeds.
    #[must_use]
    pub fn events_unsupported_since(&self, vault: &str) -> Option<OffsetDateTime> {
        self.inner
            .lock()
            .ok()
            .and_then(|inner| inner.events_unsupported.get(vault).copied())
    }

    /// Whether this is the first time a failure to record a job event is seen for `job_and_kind` in `vault`, so it is
    /// worth a log line: once per job and kind, however many of its runs or codes follow.
    #[must_use]
    pub fn first_event_failure_of(&self, vault: &str, job_and_kind: &str, now: OffsetDateTime) -> bool {
        self.inner.lock().map_or(true, |mut inner| {
            inner
                .event_failures_logged
                .retain(|_, noted| now - *noted <= EVENT_MEMORY);
            inner
                .event_failures_logged
                .insert((vault.to_owned(), job_and_kind.to_owned()), now)
                .is_none()
        })
    }

    /// Notes whether a pass could read the vault's team at all (`down` when it could not), and says what changed worth
    /// logging: an outage that starts, or one that clears. Nothing is said for a pass that finds what the last one did.
    #[must_use]
    pub fn note_team_outage(&self, vault: &str, down: bool) -> Option<OutageLogEntry> {
        let mut inner = self.inner.lock().ok()?;
        match (down, inner.team_outage.contains(vault)) {
            (true, false) => {
                inner.team_outage.insert(vault.to_owned());
                Some(OutageLogEntry::Started)
            }
            (false, true) => {
                inner.team_outage.remove(vault);
                Some(OutageLogEntry::Cleared)
            }
            _ => None,
        }
    }
}

/// Creates each of `tasks` in `vault` through `sink`, skipping those already known to be raised, and says what to log.
pub async fn raise_tasks(
    sink: &dyn TaskSink,
    memory: &JobAttentionMemory,
    vault: &str,
    tasks: &[RaisedTask],
) -> RaiseReport {
    let mut report = RaiseReport::default();
    for task in tasks {
        if memory.knows(vault, &task.origin) {
            continue;
        }
        let origin = task.origin.to_string();
        match sink.create(task).await {
            RaiseOutcome::Created(key) => {
                tracing::info!(vault, origin = %origin, task = %key, "a task was raised for a job");
                memory.remember(vault, &task.origin);
                report.created += 1;
            }
            RaiseOutcome::AlreadyRaised => {
                memory.remember(vault, &task.origin);
                report.already_raised += 1;
            }
            RaiseOutcome::NoTaskSystem => {
                if memory.first_failure_of(vault, None, "no-task-system") {
                    tracing::warn!(
                        vault,
                        "the vault has no task system for the A series, so no task is raised for jobs"
                    );
                    report.warnings.push(AttentionWarning::NoTaskSystem {
                        vault: vault.to_owned(),
                    });
                }
                // Nothing can be raised in this vault this pass, so the rest would only be refused the same way.
                break;
            }
            RaiseOutcome::Failed(code) => {
                if memory.first_failure_of(vault, Some(&origin), &code) {
                    tracing::warn!(vault, origin = %origin, code = %code, "a task could not be raised, so it will be tried again");
                    report.warnings.push(AttentionWarning::RaiseFailed { origin, code });
                }
            }
        }
    }
    report
}

/// A failed, started, scheduled run, as far as deciding whether it needs a task of its own.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FailedRun {
    pub number: u64,
}

/// Whether the tasks already in the task system include an open `run-failed` task, so a later failed run is folded into
/// it. Which states are open comes from the vault's tasks configuration.
#[must_use]
pub fn run_failed_task_is_open(existing: &[ExistingTask], config: &TasksConfig) -> bool {
    !existing.iter().all(|task| {
        matches!(
            config.closing(&task.task_type, &task.state),
            Some(Closes::Completed | Closes::Cancelled)
        )
    })
}

/// The highest run number named by an `R-n` origin among `existing`, the job's `run-failed` tasks.
#[must_use]
pub fn highest_named_run(existing: &[ExistingTask]) -> Option<u64> {
    existing
        .iter()
        .filter_map(|task| task.origin.rsplit_once(":R-"))
        .filter_map(|(_, number)| number.parse::<u64>().ok())
        .max()
}

/// Whether a failed scheduled run is one to raise a task for, given `existing` (the job's `run-failed` tasks) and
/// `mark` (the highest run the host has already examined). It is when no such task is open, and the run is numbered
/// after both the mark and every run those tasks name. A failure that came while a task was open was folded into it,
/// and the mark records that it was examined, so closing that task does not bring it back as a task of its own,
/// however many passes or restarts come between.
#[must_use]
pub fn is_unfolded_run_failure(
    existing: &[ExistingTask],
    config: &TasksConfig,
    mark: Option<u64>,
    run: &FailedRun,
) -> bool {
    !run_failed_task_is_open(existing, config)
        && mark.is_none_or(|mark| run.number > mark)
        && highest_named_run(existing).is_none_or(|named| run.number > named)
}

#[cfg(test)]
#[path = "job_attention_test.rs"]
mod tests;
