//! The recent runs of a shared job, read from the host's run records for a job page's run log.
//!
//! The records are the ones [`crate::shared_ledger`] writes, one Markdown note per run under
//! `{runs root}/{package}/{workflow}/{job}/{YYYY}/{MM}/R-{n}.md`. This reads the few header fields a log shows, newest run
//! first, and never anything a record holds beyond them. A record that cannot be read is logged and left out, so one damaged
//! file does not hide the rest.

use std::io;
use std::path::Path;

use contextos_workflows::{RunHeader, parse_run_id};

use crate::bounded_read::read_text_within;
use crate::job_chain::{ChainInput, JobChain, RunHistory};
use crate::job_runs::RunSummary;
use crate::shared_claim::{SharedJobKey, is_plain_segment};
use crate::shared_job_authoring::SharedJobAuthoring;
use crate::shared_ledger::records_of;
use crate::shared_state::SharedStateStore;
use crate::trailing_path::Trailing;

/// The largest record read for the log. A record lists at most a thousand calls, so this is far above any one written.
const MAX_RECORD_BYTES: u64 = 1024 * 1024;

/// The most recent `limit` runs of the job `key`, newest first.
///
/// Blocking: reads files, so call it from a blocking thread.
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidInput`] when a component of `key` could leave the runs folder, and the filesystem error
/// when the job's folder exists and cannot be listed. A job with no folder has no runs.
pub(crate) fn recent_shared_runs(runs_root: &Path, key: &SharedJobKey, limit: usize) -> io::Result<Vec<RunSummary>> {
    for name in [&key.package, &key.workflow, &key.job] {
        if !is_plain_segment(name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{name:?} cannot name a shared job component: it must be a single plain path segment"),
            ));
        }
    }
    let job_dir = runs_root.join(&key.package).join(&key.workflow).join(&key.job);
    let mut numbered: Vec<(u64, std::path::PathBuf)> = records_of(&job_dir)?
        .into_iter()
        .filter_map(|path| {
            let number = path.file_name()?.to_str().and_then(parse_run_id)?;
            Some((number, path))
        })
        .collect();
    numbered.sort_by_key(|(number, _)| std::cmp::Reverse(*number));
    let mut runs = Vec::new();
    for (number, path) in numbered {
        if runs.len() == limit {
            break;
        }
        match summary(number, &key.job, &path) {
            Some(run) => runs.push(run),
            None => {
                tracing::warn!(
                    record = %record_label(&path),
                    "a run record could not be read, and is left out of the log"
                );
            }
        }
    }
    Ok(runs)
}

/// Where the host keeps what the runs of a chain of jobs are found from.
pub(crate) struct ChainSources<'a> {
    pub(crate) jobs_root: &'a Path,
    pub(crate) store_root: &'a Path,
    pub(crate) state: SharedStateStore,
    pub(crate) runs_root: &'a Path,
}

/// The most recent `limit` runs of the job `key` and of every job it succeeded or was succeeded by, newest first.
///
/// Blocking: reads the job files, the state file, and run records, so call it from a blocking thread.
///
/// # Errors
///
/// Returns [`recent_shared_runs`]'s failure for any job of the chain. A chain that cannot be listed is not a failure: the
/// history is the job's own, and says the chain was not read.
pub(crate) fn recent_chain_runs(
    sources: &ChainSources<'_>,
    key: &SharedJobKey,
    limit: usize,
) -> io::Result<RunHistory> {
    let own = recent_shared_runs(sources.runs_root, key, limit)?;
    let listed = SharedJobAuthoring {
        jobs_root: sources.jobs_root.to_path_buf(),
        store_root: sources.store_root.to_path_buf(),
        state: sources.state.clone(),
    }
    .list();
    let listings = match listed {
        Ok(listings) => listings,
        Err(error) => {
            tracing::warn!(%error, "a shared job's chain could not be listed, so only its own runs are shown");
            return Ok(RunHistory::of_the_job_alone(own));
        }
    };
    let chain = JobChain::from(ChainInput {
        package: &key.package,
        workflow: &key.workflow,
        job: &key.job,
        listings: &listings,
    });
    let mut per_job = Vec::new();
    for member in chain.members() {
        if *member == key.job {
            per_job.push(own.clone());
            continue;
        }
        let member_key = SharedJobKey {
            package: key.package.clone(),
            workflow: key.workflow.clone(),
            job: member.clone(),
        };
        per_job.push(recent_shared_runs(sources.runs_root, &member_key, limit)?);
    }
    Ok(RunHistory::of_the_chain(per_job, &own, limit))
}

/// How a log event names a run record: by its job, month, and run, never the host folder the records are kept in.
fn record_label(path: &Path) -> Trailing<'_> {
    Trailing { path, count: 4 }
}

fn summary(number: u64, job: &str, path: &Path) -> Option<RunSummary> {
    let text = read_text_within(path, MAX_RECORD_BYTES).ok()?;
    let header = RunHeader::try_from(text.as_str()).ok()?;
    let frontmatter: Vec<&str> = text.lines().skip(1).take_while(|line| *line != "---").collect();
    let value_of = |key: &str| {
        frontmatter
            .iter()
            .find_map(|line| line.strip_prefix(key).and_then(|rest| rest.strip_prefix(':')))
            .map(str::trim)
            .unwrap_or_default()
    };
    Some(RunSummary {
        code: None,
        // Shared runs are not examined for tasks, so what the record says of the script is not read.
        script_started: crate::job_runs::ScriptStarted::Absent,
        path: String::new(),
        run_id: format!("R-{number}"),
        job: job.to_owned(),
        release: value_of("version").to_owned(),
        dry_run: value_of("dry_run") == "true",
        trigger: value_of("trigger").to_owned(),
        status: header.status.as_str().to_owned(),
        started: value_of("started").to_owned(),
        completed: value_of("completed").to_owned(),
        error: unquoted(value_of("error")),
    })
}

/// The text of a quoted scalar the ledger wrote, or the text as it is when it is not quoted.
fn unquoted(value: &str) -> String {
    serde_json::from_str::<String>(value).unwrap_or_else(|_| value.to_owned())
}

#[cfg(test)]
#[path = "shared_runs_test.rs"]
mod tests;
