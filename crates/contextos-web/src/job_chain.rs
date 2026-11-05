//! The jobs a job succeeded and the jobs that succeeded it, and their runs together.
//!
//! The chain is walked both ways through retired jobs, by the predecessor each job's file names and by the replacement each
//! retirement record names. A successor is a new job that takes over its predecessor's place, so the history of the place is the runs of every
//! job in the chain. Runs are numbered from 1 under each job's own key, so a chain's runs are told apart by the job
//! as well as the number.

use std::collections::BTreeSet;

use crate::job_runs::RunSummary;
use crate::shared_job_authoring::JobListing;

/// What the chain of one job is found from.
pub(crate) struct ChainInput<'a> {
    pub(crate) package: &'a str,
    pub(crate) workflow: &'a str,
    pub(crate) job: &'a str,
    /// Every job of the store.
    pub(crate) listings: &'a [JobListing],
}

/// The keys of the jobs in one chain, the oldest predecessor first and the newest successor last.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct JobChain(Vec<String>);

impl JobChain {
    /// The keys, oldest first.
    pub(crate) fn members(&self) -> &[String] {
        &self.0
    }
}

impl From<ChainInput<'_>> for JobChain {
    fn from(input: ChainInput<'_>) -> Self {
        let row_of = |job: &str| {
            input.listings.iter().find(|row| {
                row.key.package == input.package && row.key.workflow == input.workflow && row.key.job == job
            })
        };
        // A job's predecessor is the job its file names or, when the file is gone, the job whose retirement names it.
        let before = |job: &str| {
            row_of(job).and_then(|row| row.predecessor.clone()).or_else(|| {
                input
                    .listings
                    .iter()
                    .find(|row| {
                        row.key.package == input.package
                            && row.key.workflow == input.workflow
                            && row.replaced_by.as_deref() == Some(job)
                    })
                    .map(|row| row.key.job.clone())
            })
        };
        // A job's successor is the job its retirement names, which outlives the job being retired, or else the job that
        // stands as its successor.
        let after = |job: &str| row_of(job).and_then(|row| row.replaced_by.clone().or_else(|| row.successor.clone()));
        let mut seen: BTreeSet<String> = BTreeSet::from([input.job.to_owned()]);
        let mut earlier = Vec::new();
        let mut cursor = before(input.job);
        while let Some(predecessor) = cursor {
            if !seen.insert(predecessor.clone()) {
                break;
            }
            cursor = before(&predecessor);
            earlier.push(predecessor);
        }
        earlier.reverse();
        let mut chain = earlier;
        chain.push(input.job.to_owned());
        let mut cursor = after(input.job);
        while let Some(successor) = cursor {
            if !seen.insert(successor.clone()) {
                break;
            }
            cursor = after(&successor);
            chain.push(successor);
        }
        Self(chain)
    }
}

/// A job's run history as its page shows it.
#[derive(Clone, Debug, Default)]
pub(crate) struct RunHistory {
    /// The newest runs of the job's chain, newest first; the job's own when the chain could not be read.
    pub(crate) runs: Vec<RunSummary>,
    /// Whether the jobs of the chain could not be listed, so `runs` are the job's own and not the whole chain's.
    pub(crate) chain_unread: bool,
    /// The job's own newest run, which a successor's newer runs do not hide.
    pub(crate) own_latest: Option<RunSummary>,
}

impl From<Vec<RunSummary>> for RunHistory {
    /// The history of a job that has no chain beyond itself: its own runs.
    fn from(own: Vec<RunSummary>) -> Self {
        Self {
            own_latest: own.first().cloned(),
            runs: own,
            chain_unread: false,
        }
    }
}

impl RunHistory {
    /// The history of a job whose chain could not be read: its own runs, newest first, and the fact that they are only
    /// those.
    pub(crate) fn of_the_job_alone(own: Vec<RunSummary>) -> Self {
        Self {
            own_latest: own.first().cloned(),
            runs: own,
            chain_unread: true,
        }
    }

    /// The history of a job whose chain was read: `per_job` holds each member's runs, and `own` is the job's own.
    pub(crate) fn of_the_chain(per_job: Vec<Vec<RunSummary>>, own: &[RunSummary], limit: usize) -> Self {
        Self {
            runs: merge_runs(per_job, limit),
            chain_unread: false,
            own_latest: own.first().cloned(),
        }
    }
}

/// The newest `limit` runs of several jobs together, newest first, each job's runs having been read for itself.
pub(crate) fn merge_runs(per_job: Vec<Vec<RunSummary>>, limit: usize) -> Vec<RunSummary> {
    let mut runs: Vec<RunSummary> = per_job.into_iter().flatten().collect();
    runs.sort_by(|left, right| {
        right
            .started
            .cmp(&left.started)
            .then_with(|| right.number().cmp(&left.number()))
            .then_with(|| left.job.cmp(&right.job))
    });
    runs.truncate(limit);
    runs
}

#[cfg(test)]
#[path = "job_chain_test.rs"]
mod tests;
