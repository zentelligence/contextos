//! The Web scheduler's pass over shared jobs.
//!
//! Each poll validates the shared jobs, which puts a changed enabled job on probation or suspends it, and then starts
//! every `enabled` job whose schedule is due. A job is started on its own task, so two jobs that are due together never
//! wait for each other, and a run takes the job's claim as a command-line run does, so a tick that finds it held is
//! recorded `skipped` and is not run late. The tick's last-fire value is kept in the host state under its lock, which keeps
//! one tick from firing twice across a restart or between two processes. Nothing here reads or locks a vault, so shared
//! jobs run with none configured.

use std::path::PathBuf;
use std::sync::Arc;

use contextos_workflows::{Clock, LibraryLock, LogLevel, PackageName};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use crate::cli_shared::{SharedHost, run_scheduled_shared_job_reported};
use crate::mcp_client::McpClientSet;
use crate::run_settings::HostContext;
use crate::scheduler::due_tick;
use crate::shared_claim::SharedJobKey;
use crate::shared_jobs::{SharedJobFile, list_shared_job_files, without_clashing_keys};
use crate::shared_lifecycle::{SharedPassHost, shared_validation_pass};
use crate::shared_snapshot::{SharedSnapshot, SnapshotRequest, SnapshotResult, read_snapshot};
use crate::shared_state::SharedStateStore;
use crate::shared_validation::SharedValidationHost;
use crate::shared_version::{NamedRelease, SharedVersion, recorded_lock, shared_job_state_blocking};
use contextos_workflows::CronSchedule;

/// Where the host keeps what a shared run needs, and the settings every one is started with.
#[derive(Clone, Debug)]
pub struct SharedSchedulerHost {
    pub store_root: PathBuf,
    pub jobs_root: PathBuf,
    pub claims_root: PathBuf,
    pub runs_root: PathBuf,
    pub state: SharedStateStore,
    /// How long a job's run records are kept.
    pub run_retention: time::Duration,
    /// The `[owner]` name from `web.toml`, named when a shared job fails.
    pub owner: Option<String>,
    /// `[workflows] log_level` from `web.toml`, the default a workflow or job may override.
    pub log_level: Option<LogLevel>,
}

/// What one pass over the shared jobs did.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct SharedPollReport {
    /// Jobs whose due tick was started on a task of its own. A run that then finds its claim held ends as a `skipped`
    /// record.
    pub spawned: usize,
    /// Due jobs that were not started, because their current version has not been reviewed.
    pub held_back: usize,
}

/// One pass over every shared job: validates them, then starts the enabled ones that are due. It never fails outright: a
/// job that cannot be evaluated this pass is simply retried on the next poll. Public so a test can drive a pass with a
/// fixed clock; the scheduler itself calls it on a timer.
pub async fn poll_shared_once(
    clients: &Arc<McpClientSet>,
    host: &HostContext,
    shared: &SharedSchedulerHost,
    clock: &dyn Clock,
) -> SharedPollReport {
    // Validation runs first, so a changed job has already gone back to draft, or been put on probation, before anything is
    // scheduled.
    let known_servers = clients.names();
    let primary_server = clients.primary_name().unwrap_or_default();
    shared_validation_pass(
        &SharedPassHost {
            jobs_root: &shared.jobs_root,
            state: &shared.state,
            validation: SharedValidationHost {
                store_root: &shared.store_root,
                clients,
                primary_server: &primary_server,
                known_servers: &known_servers,
            },
        },
        clock.now(),
    )
    .await;
    start_due_shared_jobs(clients, host, shared, clock).await
}

/// The second half of a pass: starts the `enabled` shared jobs that are due. A due job whose current version is not the
/// one that was reviewed is held back, because the review that would put it on probation has not happened, so a failed run
/// could not suspend it, and its tick stays due until a pass has reviewed the version.
///
/// Public so a test can run it without the validation that would otherwise review the job first.
pub async fn start_due_shared_jobs(
    clients: &Arc<McpClientSet>,
    host: &HostContext,
    shared: &SharedSchedulerHost,
    clock: &dyn Clock,
) -> SharedPollReport {
    let mut report = SharedPollReport::default();
    let jobs_root = shared.jobs_root.clone();
    let store = shared.state.clone();
    let Ok(Ok((files, state))) = tokio::task::spawn_blocking(move || -> Result<_, String> {
        let files = without_clashing_keys(list_shared_job_files(&jobs_root).map_err(|error| error.to_string())?);
        let state = store.read().map_err(|error| error.to_string())?;
        Ok((files, state))
    })
    .await
    else {
        tracing::warn!("the shared jobs could not be read, so none was scheduled this pass");
        return report;
    };
    let now = clock.now();
    // A schedule is wall-clock time where the operator is.
    let zone = clock.time_zone();
    for file in files {
        let Some(tracked) = file.tracked_key() else {
            continue;
        };
        let Some(next) = due_now(&state, &tracked, now, &zone) else {
            continue;
        };
        // The job runs the libraries its workflow release was locked to, read from the state this pass read.
        let lock = recorded_lock(&state, &tracked, &file.text);
        let version =
            match shared_job_state_blocking(&shared.store_root, &tracked.package, &file.text, lock.clone()).await {
                SharedVersion::Known(version) => Some(version),
                // A package, the release the job names, or a library of it that is gone leaves the job with no version, so a job
                // with a version on record is held back below, before the claim and with no run record, until the next pass
                // has reviewed it and suspended it.
                SharedVersion::Unknown => None,
            };
        if state.new_version_under_enabled_job(&tracked.package, &tracked.workflow, &tracked.job, version.as_ref()) {
            tracing::warn!(package = %tracked.package, workflow = %tracked.workflow, job = %tracked.job, "a due shared job was not started because its current version has not been reviewed");
            report.held_back += 1;
            continue;
        }
        // The job's code is read once, here, and the run executes that read. A release changing under it is a change on the
        // host, not a defect of the job, so when it happens the tick has not been claimed: it stays due, nothing is
        // recorded, and the next pass reads again. Any other outcome, including a snapshot that cannot be read, goes to the
        // run, which settles it from this same read.
        let snapshot = read_job_snapshot(shared, &tracked, &file.text, lock.as_ref()).await;
        if snapshot
            .as_ref()
            .is_some_and(|read| read.as_ref().is_err_and(|failure| failure.release_changed()))
        {
            tracing::info!(package = %tracked.package, workflow = %tracked.workflow, job = %tracked.job, "a due shared job was not started because its release changed while it was read; the tick stays due");
            continue;
        }
        if !claim_tick(&shared.state, &tracked, next).await {
            continue;
        }
        report.spawned += 1;
        spawn_run(clients, host, shared, file, snapshot);
    }
    report
}

/// The snapshot a run of the job would execute, or `None` when the job names no readable package or its file names no
/// exact release, which the run settles by itself before it reads anything.
async fn read_job_snapshot(
    shared: &SharedSchedulerHost,
    key: &SharedJobKey,
    job_toml: &str,
    lock: Option<&LibraryLock>,
) -> Option<SnapshotResult<SharedSnapshot>> {
    let package = PackageName::try_from(key.package.as_str()).ok()?;
    let NamedRelease(release) = NamedRelease::try_from(job_toml).ok()?;
    Some(
        read_snapshot(SnapshotRequest {
            store_root: &shared.store_root,
            package,
            version: &release,
            workflow: &key.workflow,
            lock,
        })
        .await,
    )
}

/// The tick of `key` that is due at `now`, when the job is enabled and has one.
fn due_now(
    state: &contextos_workflows::JobsState,
    key: &SharedJobKey,
    now: OffsetDateTime,
    zone: &contextos_core::OperatorTimeZone,
) -> Option<OffsetDateTime> {
    let expression = state.scheduled_expression(&key.package, &key.workflow, &key.job)?;
    let schedule = match CronSchedule::parse(expression) {
        Ok(schedule) => schedule,
        Err(error) => {
            tracing::warn!(package = %key.package, workflow = %key.workflow, job = %key.job, %error, "a shared job's stored schedule is not a cron expression, so it was not scheduled");
            return None;
        }
    };
    due_tick(
        &schedule,
        state.last_scheduled_fire(&key.package, &key.workflow, &key.job),
        now,
        zone,
    )
}

/// Records that the tick `at` has fired, under the state's lock, and says whether this caller is the one that fired it. A
/// tick another process, or an earlier pass, already fired, or an earlier tick than one that has, is not fired, and neither
/// is one for a job that is no longer enabled, so the last-fire value only ever moves forward and a job suspended since the
/// pass read the state loses no tick.
async fn claim_tick(state: &SharedStateStore, key: &SharedJobKey, at: OffsetDateTime) -> bool {
    let store = state.clone();
    let owned = key.clone();
    let claimed = tokio::task::spawn_blocking(move || {
        store.update(|jobs| {
            let fired = jobs
                .last_scheduled_fire(&owned.package, &owned.workflow, &owned.job)
                .is_some_and(|last| last >= at);
            let enabled = jobs
                .scheduled_expression(&owned.package, &owned.workflow, &owned.job)
                .is_some();
            if fired || !enabled {
                false
            } else {
                jobs.record_fire(&owned.package, &owned.workflow, &owned.job, at);
                true
            }
        })
    })
    .await;
    match claimed {
        Ok(Ok(fired)) => fired,
        Ok(Err(error)) => {
            tracing::warn!(package = %key.package, workflow = %key.workflow, job = %key.job, %error, "a shared job's tick could not be recorded, so it was not run");
            false
        }
        Err(error) => {
            tracing::warn!(package = %key.package, workflow = %key.workflow, job = %key.job, %error, "a shared job's tick could not be recorded, so it was not run");
            false
        }
    }
}

/// Runs the job on a task of its own, so a slow job never delays a poll or another job.
fn spawn_run(
    clients: &Arc<McpClientSet>,
    host: &HostContext,
    shared: &SharedSchedulerHost,
    file: SharedJobFile,
    snapshot: Option<SnapshotResult<SharedSnapshot>>,
) {
    let clients = Arc::clone(clients);
    let primary_server = clients.primary_name().unwrap_or_default();
    let clock: Arc<dyn Clock> = host.clock.clone();
    let shared = shared.clone();
    tokio::spawn(async move {
        let caller = Arc::new(crate::WebMcpCaller::new(Arc::clone(&clients)));
        let run = run_scheduled_shared_job_reported(
            SharedHost {
                clients,
                primary_server: &primary_server,
                store_root: &shared.store_root,
                jobs_root: &shared.jobs_root,
                claims_root: &shared.claims_root,
                runs_root: &shared.runs_root,
                state: &shared.state,
                run_retention: shared.run_retention,
                clock,
                caller,
                owner: shared.owner.as_deref(),
                log_level: shared.log_level,
                cancel: CancellationToken::new(),
            },
            file.key.clone(),
            snapshot,
        )
        .await;
        if let Err(error) = run {
            tracing::warn!(package = %file.key.package, workflow = %file.key.workflow, job = %file.key.job, %error, "a scheduled shared job could not be run");
        }
    });
}

#[cfg(test)]
#[path = "shared_scheduler_test.rs"]
mod tests;
