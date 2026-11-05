//! The runs this process started from a job page, which the page shows while they run and after they end.
//!
//! A live run is visible to every process through its claim. A dry run takes no claim, so it is visible only here, and a
//! run that failed before it had a record leaves nothing but what is kept here. The table is in memory and holds the runs
//! of the pages of one process: it is a convenience for the page that asked, and never the record of a run.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use contextos_workflows::RunOutcome;
use time::OffsetDateTime;

use crate::run_records::status_of;

/// How many jobs the table follows at once. A job whose runs have all ended and which is the least recently started is
/// forgotten first.
const MAX_JOBS: usize = 256;

/// What kind of run a page started.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PageRunKind {
    Live,
    Dry,
}

/// How a run started from a page ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PageRunEnded {
    pub(crate) kind: PageRunKind,
    pub(crate) at: OffsetDateTime,
    /// Whether the run succeeded, with or without warnings.
    pub(crate) ok: bool,
    /// How it ended: its status, and for a failed run the failure, which begins with its `workflow/` code when it has one.
    pub(crate) text: String,
}

/// What the table knows of one job.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct PageRunsOfJob {
    /// The runs started from a page that have not ended, with when each started.
    pub(crate) running: Vec<(PageRunKind, OffsetDateTime)>,
    /// The run started from a page that ended last.
    pub(crate) last_ended: Option<PageRunEnded>,
}

#[derive(Default)]
struct Entry {
    running: BTreeMap<u64, (PageRunKind, OffsetDateTime)>,
    last_ended: Option<PageRunEnded>,
    /// When a run of the job last started, for forgetting the least recent.
    last_started: Option<OffsetDateTime>,
}

#[derive(Default)]
struct Table {
    jobs: BTreeMap<String, Entry>,
    next_run: u64,
}

/// The runs started from pages by this process, by job.
#[derive(Default)]
pub(crate) struct PageRuns {
    table: Mutex<Table>,
}

/// A run started from a page, which ends it in the table when finished or dropped.
pub(crate) struct PageRunGuard {
    runs: Arc<PageRuns>,
    job: String,
    run: u64,
    kind: PageRunKind,
    ended: bool,
}

impl PageRuns {
    fn table(&self) -> std::sync::MutexGuard<'_, Table> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Notes that a run of `job` started from a page at `now`.
    pub(crate) fn begin(self: &Arc<Self>, job: &str, kind: PageRunKind, now: OffsetDateTime) -> PageRunGuard {
        let mut table = self.table();
        let run = table.next_run;
        table.next_run += 1;
        if !table.jobs.contains_key(job) && table.jobs.len() >= MAX_JOBS {
            forget_least_recent(&mut table);
        }
        let entry = table.jobs.entry(job.to_owned()).or_default();
        entry.running.insert(run, (kind, now));
        entry.last_started = Some(now);
        drop(table);
        PageRunGuard {
            runs: Arc::clone(self),
            job: job.to_owned(),
            run,
            kind,
            ended: false,
        }
    }

    /// What the table knows of `job`.
    pub(crate) fn of(&self, job: &str) -> PageRunsOfJob {
        self.table()
            .jobs
            .get(job)
            .map(|entry| PageRunsOfJob {
                running: entry.running.values().copied().collect(),
                last_ended: entry.last_ended.clone(),
            })
            .unwrap_or_default()
    }
}

/// Forgets the job whose runs have all ended and which started a run least recently. A table full of running jobs is
/// left to grow, because a run in progress is never forgotten.
fn forget_least_recent(table: &mut Table) {
    let forgotten = table
        .jobs
        .iter()
        .filter(|(_, entry)| entry.running.is_empty())
        .min_by_key(|(_, entry)| entry.last_started)
        .map(|(job, _)| job.clone());
    if let Some(job) = forgotten {
        table.jobs.remove(&job);
    }
}

impl PageRunGuard {
    /// Notes that the run ended with `outcome` at `now`.
    pub(crate) fn finish(mut self, now: OffsetDateTime, outcome: &RunOutcome) {
        let (ok, text) = match outcome {
            RunOutcome::Succeeded => (true, status_of(outcome).to_owned()),
            RunOutcome::Degraded(_) => (true, "succeeded with warnings".to_owned()),
            RunOutcome::Failed(failure) => (false, failure.to_string()),
            RunOutcome::Cancelled => (false, status_of(outcome).to_owned()),
        };
        self.end(now, ok, text);
    }

    /// Notes that the run could not start or be driven to its end, and why.
    pub(crate) fn fail(mut self, now: OffsetDateTime, reason: String) {
        self.end(now, false, reason);
    }

    fn end(&mut self, at: OffsetDateTime, ok: bool, text: String) {
        self.ended = true;
        let mut table = self.runs.table();
        if let Some(entry) = table.jobs.get_mut(&self.job) {
            entry.running.remove(&self.run);
            entry.last_ended = Some(PageRunEnded {
                kind: self.kind,
                at,
                ok,
                text,
            });
        }
    }
}

impl Drop for PageRunGuard {
    fn drop(&mut self) {
        // A run whose task was dropped, or panicked, never said how it ended, so the table says so rather than showing it
        // running for ever.
        if !self.ended {
            let now = self
                .runs
                .table()
                .jobs
                .get(&self.job)
                .and_then(|entry| entry.running.get(&self.run).map(|(_, started)| *started))
                .unwrap_or(OffsetDateTime::UNIX_EPOCH);
            self.end(now, false, "the run did not finish".to_owned());
        }
    }
}

#[cfg(test)]
#[path = "page_runs_test.rs"]
mod tests;
