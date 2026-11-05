//! What the Agents screen shows of whether a member's scheduled sessions are alive, read from session run events and the
//! claims tasks hold, with no new state. Pure rules over events and task claims already read: nothing here reads the
//! vault, writes anything, or decides the member's own state.
//!
//! A session records `session.run-started` (which assigns its run id) and `session.run-finished`. Only a finish ends a
//! run, so a session that crashed leaves a start with no finish. Such a run is overdue (amber) when every claim that
//! carries its run id has lapsed, or, when no task holds such a claim (because every task it claimed was finished and
//! so released), when it started more than the configured window ago.

use time::{Duration, OffsetDateTime};

use crate::signals_event::{Severity, SignalEvent};

/// How far back a lapsed claim is still listed under its member.
const LAPSED_WINDOW: Duration = Duration::days(7);
/// The kind a session records when it starts; its key is the run id.
pub const RUN_STARTED: &str = "session.run-started";
/// The kind a session records when it ends.
pub const RUN_FINISHED: &str = "session.run-finished";
/// The kind the server fires when a claim lapses.
pub const CLAIM_LAPSED: &str = "task.claim-lapsed";

/// A task as far as liveness needs it: who it is assigned to and who claims it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimedTask {
    /// The task note's vault path.
    pub path: String,
    pub id: String,
    pub assignee: Option<String>,
    /// `{assignee}/{run id}` while claimed.
    pub claimed_by: Option<String>,
    pub claimed_until: Option<OffsetDateTime>,
}

impl ClaimedTask {
    /// Whether this task is claimed for `slug`: `claimed_by` begins `{slug}/`.
    fn is_claimed_for(&self, slug: &str) -> bool {
        self.claimed_by.as_deref().is_some_and(|claim| {
            claim
                .split_once('/')
                .is_some_and(|(holder, _)| holder.eq_ignore_ascii_case(slug))
        })
    }

    /// Whether this task's claim carries `run_id`: `claimed_by` ends `/{run id}`.
    fn carries_run(&self, run_id: &str) -> bool {
        self.claimed_by
            .as_deref()
            .and_then(|claim| claim.split_once('/'))
            .is_some_and(|(_, run)| run == run_id)
    }

    /// Whether the claim's window has passed. A claim with no readable end has not lapsed as far as anyone can say.
    fn has_lapsed(&self, now: OffsetDateTime) -> bool {
        self.claimed_until.is_some_and(|until| until < now)
    }
}

/// A run that started and has not finished, past its window.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OverdueRun {
    pub run_id: String,
    pub started: OffsetDateTime,
}

/// A recent lapse of a claim on a task assigned to the member.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LapsedClaim {
    /// The task's key, which is how the task is referenced.
    pub task_id: String,
    pub at: OffsetDateTime,
}

/// What the Agents screen shows of one member's liveness.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MemberLiveness {
    /// When the member's last run finished, and how it ended.
    pub last_finish: Option<(OffsetDateTime, Severity)>,
    /// How many tasks are claimed for the member whose claim has not lapsed.
    pub claimed: usize,
    pub overdue: Vec<OverdueRun>,
    pub lapsed: Vec<LapsedClaim>,
    has_session_events: bool,
}

impl MemberLiveness {
    /// Whether any session event names the member, which is what makes liveness worth showing. A member the sessions
    /// never worked for shows nothing, and no amber.
    #[must_use]
    pub const fn shows_liveness(&self) -> bool {
        self.has_session_events
    }
}

/// Whether the comma-separated `members` field of a session event names `slug`.
fn names_member(event: &SignalEvent, slug: &str) -> bool {
    event.field("members").is_some_and(|members| {
        members
            .split(',')
            .any(|member| member.trim().eq_ignore_ascii_case(slug))
    })
}

/// Reads the liveness of the member `slug` from `events` (any order), the vault's task claims, and the instant `now`.
/// A started run is overdue after `overdue_after` when no task holds a claim carrying its run id.
#[must_use]
pub fn liveness(
    slug: &str,
    events: &[SignalEvent],
    tasks: &[ClaimedTask],
    now: OffsetDateTime,
    overdue_after: Duration,
) -> MemberLiveness {
    let mut found = MemberLiveness::default();
    let finished_runs: Vec<&str> = events
        .iter()
        .filter(|event| event.kind == RUN_FINISHED)
        .filter_map(|event| event.field("run_id"))
        .collect();
    let mut session_events: Vec<&SignalEvent> = events
        .iter()
        .filter(|event| matches!(event.kind.as_str(), RUN_STARTED | RUN_FINISHED) && names_member(event, slug))
        .collect();
    found.has_session_events = !session_events.is_empty();
    session_events.sort_by_key(|event| std::cmp::Reverse(event.cursor()));
    found.last_finish = session_events
        .iter()
        .find(|event| event.kind == RUN_FINISHED)
        .map(|event| (event.at, event.severity));
    found.claimed = tasks
        .iter()
        .filter(|task| task.is_claimed_for(slug) && !task.has_lapsed(now))
        .count();
    for event in session_events.iter().filter(|event| event.kind == RUN_STARTED) {
        let Some(run_id) = event.field("run_id") else {
            continue;
        };
        if finished_runs.contains(&run_id) {
            continue;
        }
        let holding: Vec<&ClaimedTask> = tasks.iter().filter(|task| task.carries_run(run_id)).collect();
        let overdue = if holding.is_empty() {
            now - event.at > overdue_after
        } else {
            holding.iter().all(|task| task.has_lapsed(now))
        };
        if overdue {
            found.overdue.push(OverdueRun {
                run_id: run_id.to_owned(),
                started: event.at,
            });
        }
    }
    found.lapsed = lapsed_claims(slug, events, tasks, now);
    found
}

/// The lapses of claims on tasks assigned to `slug` within the last week, newest first.
fn lapsed_claims(slug: &str, events: &[SignalEvent], tasks: &[ClaimedTask], now: OffsetDateTime) -> Vec<LapsedClaim> {
    let mut lapses: Vec<&SignalEvent> = events
        .iter()
        .filter(|event| event.kind == CLAIM_LAPSED && now - event.at <= LAPSED_WINDOW)
        .collect();
    lapses.sort_by_key(|event| std::cmp::Reverse(event.cursor()));
    lapses
        .into_iter()
        .filter_map(|event| {
            // A task is matched by its key within the tasks base, wherever it is filed now; a task with no key, by
            // its path.
            let task = tasks.iter().find(|task| match event.subject_identity() {
                Some(contextos_events::SubjectIdentity::Record { .. }) => {
                    event.is_about_record(crate::vault_links::TASKS_BASE, &task.id)
                }
                Some(contextos_events::SubjectIdentity::Note { path }) => {
                    task.path.strip_suffix(".md").unwrap_or(&task.path) == path
                }
                None => false,
            })?;
            task.assignee
                .as_deref()
                .is_some_and(|assignee| assignee.eq_ignore_ascii_case(slug))
                .then(|| LapsedClaim {
                    task_id: task.id.clone(),
                    at: event.at,
                })
        })
        .collect()
}

#[cfg(test)]
#[path = "agents_liveness_test.rs"]
mod tests;
