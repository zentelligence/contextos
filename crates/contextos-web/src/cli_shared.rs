//! Command-line runs of workflows from the host package store.
//!
//! A saved shared job holds a host-level claim for the length of its run and leaves a run record on the host
//!. A dry run of a saved job leaves a record flagged as one and takes no claim, so it never holds back a live
//! run. A one-off run takes no claim and writes no record. None of them touches a vault to start or finish, and any
//! failure that ends a run before its first MCP call comes back as a [`RunOutcome::Failed`] carrying its `workflow/`
//! code.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use contextos_workflows::{
    CallJournal, Clock, JobFingerprint, JobLifecycle, JobState, LibraryLock, LogLevel, McpCaller, RunFailure, RunLabel,
    RunOutcome, RunResult, RunTrigger, SuspensionRecord, WorkflowFailureKind,
};
use semver::Version;
use thiserror::Error;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use crate::cli_shared_record::{Begin, Ended, Recording, Started, end_recording, start_recording};
use crate::dry_run_caller::DryRunMcpCaller;
use crate::dry_run_scenario::ScenarioMatcher;
use crate::job_lifecycle::NotRegistered;
use crate::job_probation::Settlement;
use crate::job_validation::DryRunRecorded;
use crate::jobs::DEFAULT_TIMEOUT_SECS;
use crate::mcp_client::McpClientSet;
use crate::run_claim::{ActiveRun, CancelWatch, watch_for_cancel};
use crate::run_report::RunReport;
use crate::run_summary::{HostWarning, Registration, ReleaseRef};
use crate::scheduler::CANCEL_POLL;
use crate::scheduler::{RunKind, run_allowed};
use crate::shared_claim::{SharedClaim, SharedClaimError, SharedJobKey};
use crate::shared_jobs::{SharedJob, SharedJobError, load_shared_job_file};
use crate::shared_ledger::{RunRecorder, SharedLedger, SkippedRun};
use crate::shared_lifecycle::{record_shared_dry_run, settle_shared_first_run};
use crate::shared_run::{RunValues, SharedRun, SharedRunError, SharedRunRequest, SnapshotRun};
use crate::shared_runner::{SharedExecution, execute_shared, reported_vaults, require_vaults_configured};
use crate::shared_scenarios::{SharedScenarioError, load_shared_scenario};
use crate::shared_snapshot::{SharedSnapshot, SnapshotResult};
use crate::shared_state::{SharedStateError, SharedStateStore};
use crate::shared_version::{recorded_lock_blocking, shared_job_version_under_blocking};

/// What a shared run needs from its host.
pub struct SharedHost<'a> {
    pub clients: Arc<McpClientSet>,
    pub primary_server: &'a str,
    pub store_root: &'a Path,
    pub jobs_root: &'a Path,
    /// Where claims live, beside the jobs and packages under the app-data directory.
    pub claims_root: &'a Path,
    /// Where run records live, beside the claims.
    pub runs_root: &'a Path,
    /// Where the lifecycle state of shared jobs lives.
    pub state: &'a SharedStateStore,
    /// How long a job's run records are kept.
    pub run_retention: time::Duration,
    /// The host's time source, so a record's times and the retention window can be driven by a test.
    pub clock: Arc<dyn Clock>,
    /// The caller that issues MCP calls once they pass the scope checks.
    pub caller: Arc<dyn McpCaller>,
    /// The `[owner]` name from `web.toml`, named when a saved shared job fails.
    pub owner: Option<&'a str>,
    /// `[workflows] log_level` from `web.toml`, the default a workflow or job may override.
    pub log_level: Option<LogLevel>,
    /// Cancelling it stops the run: the operator pressed Ctrl-C, or the process was asked to end.
    pub cancel: CancellationToken,
}

/// One workflow to run once, with no saved job.
pub struct OneOffRequest {
    pub package: String,
    pub workflow: String,
    /// The release to run: the workflow's `current`, `future`, or `previous`, or an exact version. The default of the
    /// command is `current`.
    pub release: contextos_workflows::ReleaseChoice,
    /// One `name=value` argument per input value.
    pub arguments: Vec<String>,
    /// `Some(None)` dry-runs the only scenario the package ships for the workflow, and `Some(Some(name))` the one named.
    /// A dry run reads and never writes.
    pub dry_run_scenario: Option<Option<String>>,
}

#[derive(Debug, Error)]
pub enum CliSharedError {
    #[error(transparent)]
    Job(#[from] SharedJobError),
    #[error(transparent)]
    Claim(#[from] SharedClaimError),
    #[error("the MCP client set could not be created")]
    NoClients,
    #[error(
        "workflow/job-not-runnable: the job is {state}, so {what} is not available; {remedy}{why}",
        what = if *dry_run { "a dry run" } else if *scheduled { "a scheduled run" } else { "a live run" },
        remedy = if *dry_run {
            "re-activate it first"
        } else if *scheduled {
            "only an enabled job runs on its schedule"
        } else {
            "a job is run by hand once it is registered, which needs it validated and then dry-run"
        },
        why = suspension.as_ref().map(|record| format!(" (suspension: {record})")).unwrap_or_default()
    )]
    NotRunnable {
        state: JobLifecycle,
        dry_run: bool,
        scheduled: bool,
        /// Why the job is suspended, when it is and its state says.
        suspension: Option<SuspensionRecord>,
    },
    /// The job is one of a chain and the other job holds it, so a live run by hand is not available.
    #[error(
        "workflow/job-not-runnable: a live run by hand is not available because {0}; a chain of jobs has one job in use at a time, and a successor is tried by dry runs"
    )]
    ChainHeld(contextos_workflows::ChainHold),
    #[error(
        "workflow/job-not-runnable: the version the job would run has not been reviewed, so a scheduled run of it is held \
         back until a validation pass has put it on probation"
    )]
    NotReviewed,
    #[error("workflow/state-unavailable: {0}")]
    State(#[from] SharedStateError),
    #[error(transparent)]
    Scenario(#[from] SharedScenarioError),
    /// A blocking task that runs before the claim was lost, so nothing had begun and the run is a refusal.
    #[error("the run's preparation ended without a result: {0}")]
    LostBeforeClaim(tokio::task::JoinError),
    /// A blocking task lost after the claim was taken, so the run had begun and is a failed run.
    #[error("the run's task ended without a result: {0}")]
    Interrupted(#[from] tokio::task::JoinError),
}

/// How a saved shared job is run.
#[derive(Clone, Debug)]
pub struct SharedRunOptions {
    /// What started the run: an operator, or the Web scheduler when the job's schedule fell due.
    pub trigger: RunTrigger,
    /// `Some(None)` dry-runs the only scenario the package ships for the workflow, and `Some(Some(name))` the one named.
    /// A dry run reads and never writes, takes no claim, and leaves a run record flagged as a dry run.
    pub dry_run_scenario: Option<Option<String>>,
}

impl Default for SharedRunOptions {
    fn default() -> Self {
        Self {
            trigger: RunTrigger::Manual,
            dry_run_scenario: None,
        }
    }
}

/// Runs the saved shared job `key` to completion, as an operator runs it by hand, and reports how it ended.
///
/// In order: load the job; check that its state allows this run; select the release, validate the inputs (the job's own
/// values with `overrides` on top, one `name=value` argument per input whose value replaces the job's), and check each
/// `vault` input against the primary server's vault list; take the claim; start the run's record; run; finish the record,
/// sweep the job's old records, and release the claim; and settle what the run decides about the job. A failure before
/// the claim leaves none behind and no record. A run that cannot be recorded still runs, and the failure is reported as
/// a host warning and logged.
///
/// A live run that ends settles the trial of the job's first run of a version, and a dry run that completes
/// registers a job that was validated for the version it ran. A run with overrides does neither, because the job's
/// version covers its own values and a run with other values says nothing about it. A dry run takes no claim, and so
/// neither sweeps old records nor is held back by a live run. A scheduled tick that finds the claim held is recorded
/// `skipped` and runs nothing.
///
/// # Errors
///
/// Returns [`CliSharedError::Job`] when the job file is missing or invalid, [`CliSharedError::NotRunnable`] when the
/// job's state does not allow the run, [`CliSharedError::Scenario`] when a dry run has no usable scenario, and
/// [`CliSharedError::Claim`] when the claim could not be taken for a reason other than another run holding it.
pub async fn run_shared_job_reported(
    host: SharedHost<'_>,
    key: SharedJobKey,
    options: SharedRunOptions,
    overrides: Vec<String>,
) -> Result<RunReport, CliSharedError> {
    run_shared_job_with(host, key, options, overrides, None).await
}

/// Runs the saved shared job `key` for the tick the Web scheduler has just claimed, executing `snapshot`, the read the
/// scheduler made before it claimed the tick, instead of reading the store again. The run therefore runs exactly the code
/// the scheduler looked at, and a release changing between the two can no longer fail it. The snapshot's version is still
/// checked against the job's state, as for any scheduled run, so a snapshot of a version nobody reviewed is refused. A
/// snapshot that could not be read is passed on as it is, and the run ends with its failure. `None` is for a job whose
/// package or pins are unusable before any read, which the run settles on its own.
///
/// # Errors
///
/// As [`run_shared_job_reported`].
pub async fn run_scheduled_shared_job_reported(
    host: SharedHost<'_>,
    key: SharedJobKey,
    snapshot: Option<SnapshotResult<SharedSnapshot>>,
) -> Result<RunReport, CliSharedError> {
    let options = SharedRunOptions {
        trigger: RunTrigger::Scheduled,
        dry_run_scenario: None,
    };
    run_shared_job_with(host, key, options, Vec::new(), snapshot).await
}

async fn run_shared_job_with(
    host: SharedHost<'_>,
    key: SharedJobKey,
    options: SharedRunOptions,
    overrides: Vec<String>,
    snapshot: Option<SnapshotResult<SharedSnapshot>>,
) -> Result<RunReport, CliSharedError> {
    let started_at = host.clock.now();
    let jobs_root = host.jobs_root.to_path_buf();
    let (job, text) = before_the_claim(tokio::task::spawn_blocking(move || {
        load_shared_job_file(&jobs_root, &key)
    }))
    .await??;
    require_runnable(host.state, &job.key, &options).await?;
    // Whether this run can say anything about the version on trial is decided by the request, before preparation, so a run
    // that cannot be prepared decides as little as one that can.
    let decides_trial = options.dry_run_scenario.is_none() && overrides.is_empty();
    let mut finished = run_loaded(&host, &job, &text, &options, (overrides, snapshot)).await?;
    // What a run settles is about the release it resolved. A run that ended before it resolved one is about the version the
    // job has now.
    let version = if let Some(version) = finished.version.clone() {
        Some(version)
    } else {
        let lock = recorded_lock_blocking(host.state, &job.key, &text)
            .await
            .unwrap_or_default();
        shared_job_version_under_blocking(host.store_root, &job.key.package, &text, lock).await
    };
    let Settled { registration, warning } = settle(&host, &job.key, version, &finished, decides_trial).await;
    finished.recording.host_warnings.extend(warning);
    let owner = host.owner.map(str::to_owned);
    let ended_at = host.clock.now();
    Ok(finished.into_report(Timing { started_at, ended_at }, registration, owner))
}

/// When a run started and ended, from the host's clock.
#[derive(Clone, Copy)]
struct Timing {
    started_at: OffsetDateTime,
    ended_at: OffsetDateTime,
}

/// What a run came to, for settling what it decides about the job and for reporting it.
struct Finished {
    result: RunResult,
    /// Whether the run was tried. A run that found the claim held was not, so it decides nothing about the job.
    tried: bool,
    /// For a dry run, the scenario, to ask whether the whole of it was used.
    matcher: Option<Arc<ScenarioMatcher>>,
    /// The version of the release the run resolved, once it had.
    version: Option<JobFingerprint>,
    /// The name of the scenario a dry run used.
    scenario: Option<String>,
    /// The input names given on the command line for this run.
    overridden: Vec<String>,
    release: Option<ReleaseRef>,
    /// The library releases the run resolved, in name order.
    libraries: Vec<crate::run_records::RunLibrary>,
    recording: Recording,
}

impl Finished {
    /// A run that ended before its script started, for a reason that is no fault of the job's own.
    fn not_started(outcome: RunOutcome, tried: bool) -> Self {
        Self {
            result: RunResult {
                outcome,
                outputs: None,
                output_names: Vec::new(),
                started: false,
            },
            tried,
            matcher: None,
            version: None,
            scenario: None,
            overridden: Vec::new(),
            release: None,
            libraries: Vec::new(),
            recording: Recording::default(),
        }
    }

    fn into_report(self, timing: Timing, registration: Option<Registration>, owner: Option<String>) -> RunReport {
        let failed = matches!(self.result.outcome, RunOutcome::Failed(_));
        RunReport {
            started: self.result.started,
            outputs: self.result.outputs.map(|outputs| outputs.values().clone()),
            output_names: self.result.output_names,
            outcome: self.result.outcome,
            run_id: self.recording.run_id,
            record: self.recording.record,
            registration,
            host_warnings: self.recording.host_warnings,
            overridden_inputs: self.overridden,
            release: self.release,
            scenario: self.scenario,
            started_at: Some(timing.started_at),
            ended_at: Some(timing.ended_at),
            owner: owner.filter(|_| failed),
            read_release: None,
            libraries: self.libraries,
            version: None,
        }
    }
}

/// Refuses a run the job's state does not allow, before anything is claimed or recorded.
///
/// # Errors
///
/// Returns [`CliSharedError::NotRunnable`] when the job's state does not allow the run, and [`CliSharedError::State`] when
/// the state cannot be read.
pub async fn require_runnable(
    state: &SharedStateStore,
    key: &SharedJobKey,
    options: &SharedRunOptions,
) -> Result<(), CliSharedError> {
    let store = state.clone();
    let owned = key.clone();
    let state = before_the_claim(tokio::task::spawn_blocking(move || {
        store.read().map(|jobs| {
            (
                jobs.lifecycle_of(&owned.package, &owned.workflow, &owned.job),
                jobs.job(&owned.package, &owned.workflow, &owned.job)
                    .and_then(JobState::suspension)
                    .cloned(),
                jobs.chain_hold_on_live_run(&owned.package, &owned.workflow, &owned.job),
            )
        })
    }))
    .await??;
    let (state, suspension, hold) = state;
    let dry_run = options.dry_run_scenario.is_some();
    let scheduled = options.trigger == RunTrigger::Scheduled;
    let kind = if dry_run { RunKind::Dry } else { RunKind::Live };
    if run_allowed(state, kind) && (dry_run || !scheduled || state.runs_on_schedule()) {
        match hold.filter(|_| !dry_run) {
            Some(hold) => Err(CliSharedError::ChainHeld(hold)),
            None => Ok(()),
        }
    } else {
        Err(CliSharedError::NotRunnable {
            state,
            dry_run,
            scheduled,
            suspension: suspension.filter(|_| state == JobLifecycle::Suspended),
        })
    }
}

/// The library lock recorded for the workflow release of `job`, which its run reads and resolves nothing beyond. A state
/// that cannot be read cannot say which libraries the job is held to, so the run does not start.
async fn recorded_lock_for_run(
    host: &SharedHost<'_>,
    job: &SharedJob,
    job_toml: &str,
) -> Result<Option<LibraryLock>, RunOutcome> {
    recorded_lock_blocking(host.state, &job.key, job_toml)
        .await
        .map_err(|error| {
            RunOutcome::Failed(RunFailure {
                kind: Some(WorkflowFailureKind::PackageUnavailable),
                message: format!(
                    "{}: the libraries the job is locked to could not be read: {error}",
                    WorkflowFailureKind::PackageUnavailable.code()
                ),
            })
        })
}

/// The values a saved job runs with: its own, or its own with the `name=value` arguments given for this run only.
impl From<(&SharedJob, Vec<String>)> for RunValues {
    fn from((job, overrides): (&SharedJob, Vec<String>)) -> Self {
        if overrides.is_empty() {
            Self::Structured(job.inputs.clone())
        } else {
            Self::Overridden {
                job_values: job.inputs.clone(),
                arguments: overrides,
            }
        }
    }
}

/// Prepares and runs the job, and ends its record.
async fn run_loaded(
    host: &SharedHost<'_>,
    job: &SharedJob,
    job_toml: &str,
    options: &SharedRunOptions,
    (overrides, snapshot): (Vec<String>, Option<SnapshotResult<SharedSnapshot>>),
) -> Result<Finished, CliSharedError> {
    let values = RunValues::from((job, overrides));
    let lock = match recorded_lock_for_run(host, job, job_toml).await {
        Ok(lock) => lock,
        Err(outcome) => return Ok(Finished::not_started(outcome, true)),
    };
    let order = Order {
        store_root: host.store_root.to_path_buf(),
        package: job.key.package.clone(),
        workflow: job.key.workflow.clone(),
        version: job.release.version.as_semver().clone(),
        lock,
        snapshot,
        values,
    };
    let (run, reported_vaults) = match prepare(host, order).await {
        Ok(prepared) => prepared,
        Err(outcome) => return Ok(Finished::not_started(outcome, true)),
    };
    let release = ReleaseRef::from(&run.identity);
    let libraries_run: Vec<crate::run_records::RunLibrary> =
        run.libraries.iter().map(crate::run_records::RunLibrary::from).collect();
    let overridden = run.overridden.clone();
    let version = run.version(job_toml);
    if options.trigger == RunTrigger::Scheduled && options.dry_run_scenario.is_none() {
        require_reviewed(host, &job.key, &version).await?;
    }
    let (scenario, matcher) = match &options.dry_run_scenario {
        Some(requested) => {
            let (name, scenario) = load_scenario(host, &run, job, requested.as_deref()).await?;
            (Some(name), Some(Arc::new(ScenarioMatcher::new(scenario))))
        }
        None => (None, None),
    };
    let claim = match acquire_claim(host, job, options, &run, matcher.is_some()).await? {
        ClaimTaken::Held(claim) => claim,
        ClaimTaken::Busy(outcome) => {
            return Ok(Finished {
                version: Some(version),
                release: Some(release),
                overridden,
                ..Finished::not_started(outcome, false)
            });
        }
    };
    let ledger = Arc::new(SharedLedger::new(
        host.runs_root.to_path_buf(),
        Arc::clone(&host.clock),
        host.run_retention,
    ));
    let begun = Begin {
        identity: run.identity.clone(),
        libraries: run.libraries.clone(),
        inputs: run.inputs.values().keys().cloned().collect(),
        overridden: overridden.clone(),
        trigger: options.trigger,
    };
    let Started {
        claim,
        recorder,
        recording,
    } = start_recording(host, &job.key, &ledger, claim, begun).await?;
    let launch = Launch {
        timeout_secs: job.timeout_secs,
        job_log_level: job.log_level,
        label: RunLabel {
            workflow: job.key.workflow.clone(),
            job: Some(job.key.job.clone()),
            run_id: recorder.as_ref().map(|recorder| recorder.run_id()),
        },
        journal: recorder.clone().map(|recorder| recorder as Arc<dyn CallJournal>),
    };
    let watch = say_active_and_watch(host, claim.as_ref(), recorder.as_deref()).await;
    let mut execution = execution(host, run, reported_vaults, launch);
    if let Some(matcher) = &matcher {
        execution.caller = Arc::new(DryRunMcpCaller::sharing(Arc::clone(&host.clients), Arc::clone(matcher)));
    }
    let result = execute_shared(execution).await;
    drop(watch);
    let ended = Ended {
        ledger: &ledger,
        key: &job.key,
        claim,
        recorder,
        recording,
        dry: matcher.is_some(),
    };
    let recording = end_recording(ended, &result).await;
    Ok(Finished {
        result,
        tried: true,
        matcher,
        version: Some(version),
        scenario,
        overridden,
        release: Some(release),
        libraries: libraries_run,
        recording,
    })
}

/// How taking a job's claim went.
enum ClaimTaken {
    /// The claim is held, or none was needed.
    Held(Option<SharedClaim>),
    /// Another run holds it, so this one is refused with `workflow/job-busy`.
    Busy(RunOutcome),
}

/// Takes the job's claim for a live run. A dry run takes none: it is synthetic, so a live run of the job is never held
/// back by it, and it never holds one back. A scheduled tick that finds the claim held leaves a `skipped` record.
async fn acquire_claim(
    host: &SharedHost<'_>,
    job: &SharedJob,
    options: &SharedRunOptions,
    run: &SharedRun,
    dry: bool,
) -> Result<ClaimTaken, CliSharedError> {
    if dry {
        return Ok(ClaimTaken::Held(None));
    }
    let claims_root = host.claims_root.to_path_buf();
    let claim_key = job.key.clone();
    match before_the_claim(tokio::task::spawn_blocking(move || {
        SharedClaim::acquire(&claims_root, &claim_key)
    }))
    .await?
    {
        Ok(claim) => {
            if options.trigger == RunTrigger::Scheduled {
                // Dropping the claim on a refusal frees it for the run that now owns the job.
                require_still_enabled(host.state, &job.key).await?;
            }
            Ok(ClaimTaken::Held(Some(claim)))
        }
        Err(busy @ SharedClaimError::Busy { .. }) => {
            if options.trigger == RunTrigger::Scheduled {
                record_skipped_tick(host, job, run).await;
            }
            Ok(ClaimTaken::Busy(refused(WorkflowFailureKind::JobBusy, &busy)))
        }
        Err(other) => Err(other.into()),
    }
}

/// Refuses a scheduled run whose job is no longer `enabled` now that the run holds its claim. The scheduler chose the job
/// from a state it read earlier; an operator may have retired or suspended it since, and an enabling that retires a
/// predecessor looks at the predecessor's claim, so a run that took the claim must see the state as it is after taking it.
async fn require_still_enabled(state: &SharedStateStore, key: &SharedJobKey) -> Result<(), CliSharedError> {
    let store = state.clone();
    let owned = key.clone();
    let (lifecycle, suspension) = before_the_claim(tokio::task::spawn_blocking(move || {
        store.read().map(|jobs| {
            (
                jobs.lifecycle_of(&owned.package, &owned.workflow, &owned.job),
                jobs.job(&owned.package, &owned.workflow, &owned.job)
                    .and_then(JobState::suspension)
                    .cloned(),
            )
        })
    }))
    .await??;
    if lifecycle == JobLifecycle::Enabled {
        Ok(())
    } else {
        Err(CliSharedError::NotRunnable {
            state: lifecycle,
            dry_run: false,
            scheduled: true,
            suspension: suspension.filter(|_| lifecycle == JobLifecycle::Suspended),
        })
    }
}

/// Says which run holds the claim, so a page or another process can see it and ask it to stop, and starts watching for
/// that request, which cancels the run's token like Ctrl-C does. A run with no record has no number to say, and cannot be
/// asked to stop from elsewhere. A claim that cannot say is still a claim, so the run goes ahead.
async fn say_active_and_watch(
    host: &SharedHost<'_>,
    claim: Option<&SharedClaim>,
    recorder: Option<&RunRecorder>,
) -> Option<CancelWatch> {
    let (claim, recorder) = claim.zip(recorder)?;
    let active = ActiveRun {
        run_id: recorder.run_id(),
        started: host.clock.now(),
    };
    if let Err(error) = claim.run_claim().announce(active.clone()).await {
        tracing::warn!(%error, "a run could not say it is active, so it cannot be cancelled from elsewhere");
        return None;
    }
    Some(watch_for_cancel(
        claim.run_claim().slot().clone(),
        active.run_id,
        host.cancel.clone(),
        CANCEL_POLL,
    ))
}

/// Refuses a scheduled live run of a version nobody has reviewed. The scheduler holds such a job back too, but the release
/// can change between its check and this run's own, and a run of a version that is not the recorded one could not be
/// settled, so a failure of it could not suspend the job.
async fn require_reviewed(
    host: &SharedHost<'_>,
    key: &SharedJobKey,
    version: &JobFingerprint,
) -> Result<(), CliSharedError> {
    let store = host.state.clone();
    let owned = key.clone();
    let version = version.clone();
    let unreviewed = before_the_claim(tokio::task::spawn_blocking(move || {
        store
            .read()
            .map(|jobs| jobs.new_version_under_enabled_job(&owned.package, &owned.workflow, &owned.job, Some(&version)))
    }))
    .await??;
    if unreviewed {
        Err(CliSharedError::NotReviewed)
    } else {
        Ok(())
    }
}

/// Loads the scenario a dry run uses from the release the run resolved, off the async executor.
async fn load_scenario(
    host: &SharedHost<'_>,
    run: &SharedRun,
    job: &SharedJob,
    requested: Option<&str>,
) -> Result<(String, crate::dry_run_scenario::Scenario), CliSharedError> {
    let store_root = host.store_root.to_path_buf();
    let package = run.identity.name.clone();
    let workflow = job.key.workflow.clone();
    let release = run.identity.version.clone();
    let requested = requested.map(str::to_owned);
    Ok(before_the_claim(tokio::task::spawn_blocking(move || {
        load_shared_scenario(&store_root, &package, &release, &workflow, requested.as_deref())
    }))
    .await??)
}

/// Leaves a `skipped` record for a scheduled tick that found the claim held. A record that cannot be written is logged,
/// and the tick is still not run.
async fn record_skipped_tick(host: &SharedHost<'_>, job: &SharedJob, run: &SharedRun) {
    let ledger = SharedLedger::new(
        host.runs_root.to_path_buf(),
        Arc::clone(&host.clock),
        host.run_retention,
    );
    let key = job.key.clone();
    let identity = run.identity.clone();
    let inputs: Vec<String> = run.inputs.values().keys().cloned().collect();
    let written = tokio::task::spawn_blocking(move || {
        ledger.record_skipped(
            &key,
            SkippedRun {
                identity: &identity,
                inputs,
                reason: "the job's previous run still held its claim",
            },
        )
    })
    .await;
    match written {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            tracing::warn!(job = %job.key.job, %error, "a skipped tick could not be recorded");
        }
        Err(error) => {
            tracing::warn!(job = %job.key.job, %error, "a skipped tick could not be recorded");
        }
    }
}

/// What settling a run did: the registration a dry run is to report, and a warning when the host could not save state.
struct Settled {
    registration: Option<Registration>,
    warning: Option<HostWarning>,
}

impl Settled {
    const fn nothing() -> Self {
        Self {
            registration: None,
            warning: None,
        }
    }
}

fn state_unavailable(message: &str) -> HostWarning {
    HostWarning {
        code: crate::run_summary::codes::STATE_UNAVAILABLE.to_owned(),
        message: message.to_owned(),
    }
}

/// Settles what the run decides about the job: a live run ends the trial of its first run or suspends the job, and a
/// dry run that completed registers a job validated for the version it ran. A run that found the claim held decides
/// nothing, and neither does a run with overrides or a dry run, whatever became of it: `decides_trial` is false for
/// those, from the request and not from how far preparation got. Says what a dry run did to the job's registration, and
/// warns when the host could not save the state.
async fn settle(
    host: &SharedHost<'_>,
    key: &SharedJobKey,
    version: Option<JobFingerprint>,
    finished: &Finished,
    decides_trial: bool,
) -> Settled {
    if !finished.tried {
        return Settled::nothing();
    }
    let now = host.clock.now();
    let outcome = &finished.result.outcome;
    let Some(matcher) = &finished.matcher else {
        // A run whose script never started says nothing about the job, so a refusal never settles a trial or changes the
        // job's lifecycle: an outage or a missing release is not a defect of the job.
        if !decides_trial || !finished.result.started {
            return Settled::nothing();
        }
        let warning =
            match settle_shared_first_run(host.state, key, version, outcome, finished.recording.run_id, now).await {
                Settlement::NotRecorded => Some(state_unavailable(
                    "the outcome of the run could not be saved to the job's state",
                )),
                Settlement::Passed | Settlement::Suspended | Settlement::NothingToSettle => None,
            };
        return Settled {
            registration: None,
            warning,
        };
    };
    let completed = matches!(outcome, RunOutcome::Succeeded | RunOutcome::Degraded(_));
    let registration = if !completed {
        Registration::NotRegistered(NotRegistered::DryRunNotCompleted)
    } else if matcher.unconsumed() != 0 {
        Registration::NotRegistered(NotRegistered::ScenarioCallsUnused)
    } else if !finished.overridden.is_empty() {
        Registration::NotRegistered(NotRegistered::InputsOverridden)
    } else if let Some(version) = version {
        let recorded = record_shared_dry_run(host.state, key, version, now).await;
        tracing::info!(package = %key.package, workflow = %key.workflow, job = %key.job, ?recorded, "a dry run completed");
        match recorded {
            DryRunRecorded::Registered => Registration::Registered,
            DryRunRecorded::NotRecorded(reason) => Registration::NotRegistered(reason),
        }
    } else {
        Registration::NotRegistered(NotRegistered::StateUnavailable)
    };
    let warning = (registration == Registration::NotRegistered(NotRegistered::StateUnavailable))
        .then(|| state_unavailable("the dry run's completion could not be saved to the job's state"));
    Settled {
        registration: Some(registration),
        warning,
    }
}

/// Runs one workflow once with no saved job, no claim, and no run record, and reports how it ended.
///
/// # Errors
///
/// Returns [`CliSharedError`] only for failures that are not run outcomes.
pub async fn run_shared_once_reported(host: SharedHost<'_>, once: OneOffRequest) -> Result<RunReport, CliSharedError> {
    let started_at = host.clock.now();
    let workflow = once.workflow.clone();
    let (version, lock) = match one_off_release(&host, &once).await {
        Ok(found) => found,
        Err(outcome) => {
            let finished = Finished::not_started(outcome, true);
            let ended_at = host.clock.now();
            return Ok(finished.into_report(Timing { started_at, ended_at }, None, None));
        }
    };
    let order = Order {
        store_root: host.store_root.to_path_buf(),
        package: once.package,
        workflow: once.workflow,
        version,
        // A release with a recorded lock runs with it. One without resolves the libraries over the installed library
        // releases for this run only, and the run records nothing.
        lock,
        snapshot: None,
        values: RunValues::CommandLine(once.arguments),
    };
    let (run, reported_vaults) = match prepare(&host, order).await {
        Ok(prepared) => prepared,
        Err(outcome) => {
            let finished = Finished::not_started(outcome, true);
            let ended_at = host.clock.now();
            return Ok(finished.into_report(Timing { started_at, ended_at }, None, None));
        }
    };
    let release = ReleaseRef::from(&run.identity);
    let libraries_run: Vec<crate::run_records::RunLibrary> =
        run.libraries.iter().map(crate::run_records::RunLibrary::from).collect();
    let (scenario, matcher) = match &once.dry_run_scenario {
        Some(requested) => {
            let store_root = host.store_root.to_path_buf();
            let package = run.identity.name.clone();
            let version = run.identity.version.clone();
            let named = workflow.clone();
            let requested = requested.clone();
            let (name, scenario) = before_the_claim(tokio::task::spawn_blocking(move || {
                load_shared_scenario(&store_root, &package, &version, &named, requested.as_deref())
            }))
            .await??;
            (Some(name), Some(Arc::new(ScenarioMatcher::new(scenario))))
        }
        None => (None, None),
    };
    let launch = Launch {
        timeout_secs: DEFAULT_TIMEOUT_SECS,
        job_log_level: None,
        label: RunLabel {
            workflow: format!("{}/{workflow}", run.identity.name),
            job: None,
            run_id: None,
        },
        journal: None,
    };
    let mut execution = execution(&host, run, reported_vaults, launch);
    if let Some(matcher) = matcher {
        execution.caller = Arc::new(DryRunMcpCaller::sharing(Arc::clone(&host.clients), matcher));
    }
    let result = execute_shared(execution).await;
    let finished = Finished {
        result,
        tried: true,
        matcher: None,
        version: None,
        scenario,
        overridden: Vec::new(),
        release: Some(release),
        libraries: libraries_run,
        recording: Recording::default(),
    };
    let ended_at = host.clock.now();
    Ok(finished.into_report(Timing { started_at, ended_at }, None, None))
}

/// The release a one-off run takes, from what the host's state records for the workflow, and the library lock recorded
/// for it when there is one. A choice the workflow cannot run ends the run before it starts.
async fn one_off_release(
    host: &SharedHost<'_>,
    once: &OneOffRequest,
) -> Result<(Version, Option<LibraryLock>), RunOutcome> {
    let state = host.state.clone();
    let read = tokio::task::spawn_blocking(move || state.read())
        .await
        .map_err(|error| {
            refused(
                WorkflowFailureKind::StateUnavailable,
                &format!("the host's job state could not be read: {error}"),
            )
        })?
        .map_err(|error| {
            refused(
                WorkflowFailureKind::StateUnavailable,
                &format!("the host's job state could not be read: {error}"),
            )
        })?;
    let version = Version::try_from(contextos_workflows::OneOffRelease {
        choice: &once.release,
        workflow: &once.workflow,
        releases: read.package_releases(&once.package),
    })
    .map_err(|error| refused(WorkflowFailureKind::JobNotRunnable, &error))?;
    if matches!(once.release, contextos_workflows::ReleaseChoice::Version(_)) {
        match crate::release_record::host_never_recorded(host.store_root, &once.package, &version).await {
            Ok(true) => {
                let why = format!("the host's package store has never recorded {} {version}", once.package);
                return Err(refused(WorkflowFailureKind::JobNotRunnable, &why));
            }
            Ok(false) => {}
            Err(why) => return Err(refused(WorkflowFailureKind::PackageUnavailable, &why)),
        }
    }
    let lock = read.library_lock(&once.package, &once.workflow, &version).cloned();
    Ok((version, lock))
}

/// The owned form of a run request, so its filesystem work can move to a blocking thread.
struct Order {
    store_root: PathBuf,
    package: String,
    workflow: String,
    version: Version,
    /// The library lock recorded for the workflow release, when a job on it has passed validation.
    lock: Option<LibraryLock>,
    /// A snapshot already read for this run, which it executes instead of reading its own.
    snapshot: Option<SnapshotResult<SharedSnapshot>>,
    values: RunValues,
}

/// Everything that can end a run before its first call: the release, the inputs, and the vault list.
async fn prepare(host: &SharedHost<'_>, order: Order) -> Result<(SharedRun, Vec<String>), RunOutcome> {
    let run = tokio::task::spawn_blocking(move || match order.snapshot {
        Some(read) => read.map_err(SharedRunError::from).and_then(|snapshot| {
            SharedRun::try_from(SnapshotRun {
                snapshot,
                values: order.values,
            })
        }),
        None => SharedRun::try_from(SharedRunRequest {
            store_root: &order.store_root,
            package: &order.package,
            workflow: &order.workflow,
            version: &order.version,
            lock: order.lock.as_ref(),
            values: order.values,
        }),
    })
    .await
    .map_err(|error| {
        RunOutcome::Failed(RunFailure {
            kind: Some(WorkflowFailureKind::PackageUnavailable),
            message: format!(
                "{}: the package could not be read: {error}",
                WorkflowFailureKind::PackageUnavailable.code()
            ),
        })
    })?
    .map_err(|error| refused(error.kind(), &error))?;
    // A run naming a vault reaches it through the primary server; while that is not connected, the vault is not
    // missing, the server is, and the failure says so.
    if !run.inputs.vault_names().is_empty()
        && host.clients.is_configured(host.primary_server)
        && host.clients.get(host.primary_server).is_none()
    {
        return Err(RunOutcome::Failed(RunFailure {
            kind: Some(WorkflowFailureKind::ServerUnavailable),
            message: format!(
                "{}: the primary server {:?} is not connected, so the run's vaults cannot be reached",
                WorkflowFailureKind::ServerUnavailable.code(),
                host.primary_server
            ),
        }));
    }
    let reported = reported_vaults(&host.clients, host.primary_server)
        .await
        .map_err(RunOutcome::Failed)?;
    require_vaults_configured(&run.inputs, &reported).map_err(RunOutcome::Failed)?;
    Ok((run, reported))
}

/// How one run is started: its timeout, its job's log level, and its identity in the log.
struct Launch {
    timeout_secs: u64,
    job_log_level: Option<LogLevel>,
    label: RunLabel,
    journal: Option<Arc<dyn CallJournal>>,
}

fn execution(host: &SharedHost<'_>, run: SharedRun, reported_vaults: Vec<String>, launch: Launch) -> SharedExecution {
    let level = contextos_workflows::effective_log_level(host.log_level, run.log_level, launch.job_log_level);
    SharedExecution {
        run,
        timeout_secs: launch.timeout_secs,
        log_level: level,
        label: launch.label,
        caller: Arc::clone(&host.caller),
        primary_server: host.primary_server.to_owned(),
        reported_vaults,
        cancel: host.cancel.clone(),
        journal: launch.journal,
        clock: Arc::clone(&host.clock),
    }
}

/// Waits for a blocking task that runs before the run's claim is taken, so a task that was lost leaves nothing begun.
///
/// # Errors
///
/// Returns [`CliSharedError::LostBeforeClaim`] when the task panicked or was cancelled, which is a refusal because the
/// run had not begun.
pub(crate) async fn before_the_claim<T>(task: tokio::task::JoinHandle<T>) -> Result<T, CliSharedError> {
    task.await.map_err(CliSharedError::LostBeforeClaim)
}

fn refused(kind: WorkflowFailureKind, reason: &dyn std::fmt::Display) -> RunOutcome {
    RunOutcome::Failed(RunFailure {
        kind: Some(kind),
        message: format!("{}: {reason}", kind.code()),
    })
}

#[cfg(test)]
#[path = "cli_shared_test.rs"]
mod tests;
