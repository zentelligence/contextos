//! Background job scheduling: reads `.contextos/jobs.toml`, evaluates each registered job's cron
//! schedule against wall-clock time, dispatches a run through [`contextos_workflows::run_workflow_declaring`] when due, and
//! records the run's own record under `memory/jobs/` and an operation-log entry. Runs for `contextos-web`'s own process
//! lifetime: a plain background task in this same process, never a separate one; `contextos-mcp` gains no
//! jobs-awareness, no new tool, no new client kind.
//!
//! A job's claim is an operating-system lock on the host (`run_claim.rs`), so a `contextos-web` crash mid-run releases
//! it and the next run takes it: nothing is left in the vault to clear by hand.

use std::sync::Arc;
use std::time::Duration as StdDuration;

use contextos_workflows::{
    Clock, CronSchedule, DEFAULT_DAILY_PARTITION_THRESHOLD, JobLifecycle, JobsState, RunFailure, RunLabel, RunOutcome,
    RunResult, RunSettings, TickDecision, WorkflowFailureKind, evaluate_tick, needs_daily_partition,
    run_workflow_declaring,
};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;

use crate::job_probation::Settlement;
use crate::jobs::{RegisteredJob, discover_jobs};
use crate::mcp_client::{McpCallError, McpClient, McpClientSet};
use crate::rendering::shell;
use crate::run_claim::{ClaimSlot, RunClaim, watch_for_cancel};
use crate::run_records::{
    RunCompletion, RunLibrary, RunRecordHeader, RunRelease, Trigger, complete_run, create_run_record,
    log_run_completion, status_of,
};
use crate::run_report::RunReport;
use crate::run_scope::RunScope;
use crate::run_settings::{HostContext, RunPlan};
use crate::run_snapshot::{CodeRead, ReviewGate, log_libraries, version_for_run};
use crate::run_summary::{HostWarning, codes};
use crate::vault_job_release::{ReadForRun, read_job_release};
use crate::vault_runs::{ClaimRunError, ClaimedRun, claim_slot, next_run, take_claim};
use crate::vault_workflow_files::{VaultWorkflowFiles, VaultWorkflowFolder};

/// A fixed in-vault path: scheduler state travels
/// with the vault rather than staying host-local.
const JOBS_STATE_PATH: &str = ".contextos/jobs.toml";

/// How often the scheduler evaluates every configured vault's registered
/// jobs against wall-clock time. Not operator-configurable: a fixed cadence tight enough that a
/// minute-granularity cron field still fires close to on time.
const POLL_INTERVAL_SECS: u64 = 30;

/// How far back a job's very first evaluation looks for a due tick when
/// it has no recorded `last_scheduled_fire` yet: comfortably more than
/// one minute (cron's own finest field granularity) so a job's very
/// first poll after registration, or the first poll after a restart,
/// does not miss an occurrence that landed only slightly before this
/// evaluation purely because of poll timing, while staying nowhere near
/// large enough to retroactively catch up any real backlog. A missed tick is simply not run, matching ordinary
/// cron daemon behaviour.
const LOOKBACK_SECS: i64 = 90;

/// How many scheduler polls elapse between retention sweeps: a
/// 90-day-default window has no need to be re-evaluated on
/// `POLL_INTERVAL_SECS`'s own tight cadence, so the sweep runs far less
/// often (roughly once an hour at the default 30-second poll interval)
/// than schedule evaluation does.
const RETENTION_SWEEP_EVERY_N_POLLS: u64 = 120;

/// What a run is, for the lifecycle gate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RunKind {
    /// A run that makes real calls.
    Live,
    /// A run against scenarios, which reads and never writes, so it is how a validated job earns registration.
    Dry,
}

/// Whether a job in `lifecycle` may be run as `kind`. A live run needs `registered`, `enabled`, or `suspended`. A dry
/// run is allowed in every state but `retired`, because it reads and never writes.
pub(crate) const fn run_allowed(lifecycle: JobLifecycle, kind: RunKind) -> bool {
    match kind {
        RunKind::Live => lifecycle.runs_by_hand(),
        RunKind::Dry => !matches!(lifecycle, JobLifecycle::Retired),
    }
}

/// The jobs the scheduler may act on this pass, each with the cron expression the operator stored for it: those that
/// are `enabled`, and no others. A job's manifest has no schedule of its own.
pub(crate) fn scheduled_jobs(jobs: Vec<RegisteredJob>, state: &JobsState) -> Vec<(RegisteredJob, String)> {
    jobs.into_iter()
        .filter_map(|job| {
            let expression = state
                .scheduled_expression(&job.package, &job.workflow_slug, &job.slug)?
                .to_owned();
            Some((job, expression))
        })
        .collect()
}

/// `pub(crate)`: also [`crate::routes::jobs::toggle_favourite`]'s own error type for a favourite
/// toggle's read-modify-write round trip against the identical `.contextos/jobs.toml`.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error(transparent)]
    Mcp(#[from] McpCallError),
    #[error("jobs.toml could not be serialised: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error(transparent)]
    Persistence(#[from] crate::vault_state::FavouriteError),
}

impl StateError {
    /// Whether the write was refused because the state changed since it was read, so reading it again and repeating the
    /// request is the right response.
    pub(crate) const fn is_conflict(&self) -> bool {
        matches!(self, Self::Persistence(crate::vault_state::FavouriteError::Rejected))
    }
}

/// Runs the scheduler for `contextos-web`'s own process lifetime: never
/// returns. Spawned once at startup (`main.rs`) alongside the axum
/// server.
///
/// Each poll looks at the vaults' jobs and, when `shared` is given, at the host's shared jobs, which need no vault.
pub async fn run_forever(
    clients: Arc<McpClientSet>,
    host: HostContext,
    shared: Option<crate::shared_scheduler::SharedSchedulerHost>,
) {
    let mut poll_count: u64 = 0;
    loop {
        tokio::time::sleep(StdDuration::from_secs(POLL_INTERVAL_SECS)).await;
        // Immediate once the server's zone is known; until then, asks again, so a server slow to answer at startup is
        // still followed once it does.
        host.clock.warm().await;
        // Concurrently, so a slow vault server never delays a shared job, which needs no vault.
        tokio::join!(poll_once(&clients, &host, host.clock.as_ref()), async {
            if let Some(shared) = &shared {
                crate::shared_scheduler::poll_shared_once(&clients, &host, shared, host.clock.as_ref()).await;
            }
        });
        poll_count += 1;
        if poll_count.is_multiple_of(RETENTION_SWEEP_EVERY_N_POLLS) {
            sweep_once(&clients, &host).await;
        }
    }
}

/// One retention pass across every configured vault's every registered
/// job, scheduled or manual-trigger-only alike:
/// a manual job's own run history still ages out on the same policy.
/// Never fails outright, the same "degrade, retry next time" discipline
/// [`poll_once`] follows.
async fn sweep_once(clients: &Arc<McpClientSet>, host: &HostContext) {
    let Some(client) = clients.primary() else {
        return;
    };
    let Ok(vault_names) = shell::configured_vault_names(&client).await else {
        return;
    };
    let known_mcp_servers = clients.names();
    let now = host.clock.now();
    let window = time::Duration::days(contextos_workflows::DEFAULT_RETENTION_DAYS);
    for vault_name in vault_names {
        let Ok(jobs) = discover_jobs(&client, &vault_name, &known_mcp_servers).await else {
            continue;
        };
        crate::retention::sweep_vault(&client, &vault_name, &jobs, now, window).await;
    }
}

/// One scheduler pass over every configured vault: validates the vault's jobs, then starts the jobs that are `enabled`
/// and due, and records the ticks skipped because a run was already active. It never fails outright: a vault or job that
/// cannot be evaluated this pass is simply retried on the next poll. Public so a test can drive a pass with a fixed
/// clock; the scheduler itself calls it on a timer.
pub async fn poll_once(clients: &Arc<McpClientSet>, host: &HostContext, clock: &dyn Clock) {
    let Some(client) = clients.primary() else {
        return;
    };
    let Ok(vault_names) = shell::configured_vault_names(&client).await else {
        return;
    };
    let known_mcp_servers = clients.names();
    for vault_name in vault_names {
        poll_vault(clients, &client, &vault_name, &known_mcp_servers, clock, host).await;
    }
}

/// The latest due tick for a job's schedule on the wall clock of `zone`, or `None` when none is due: pure (no I/O, no
/// clock read of its own), so it is unit-testable with fixed instants (`testing.md`).
///
/// Several ticks can have fallen due since the last one that fired, after a stopped process, a long sleep, or a vault
/// whose jobs were paused. Only the latest runs: the tick recorded as fired is that one, so the backlog does not fire
/// one tick on each later pass.
pub(crate) fn due_tick(
    schedule: &CronSchedule,
    last_scheduled_fire: Option<OffsetDateTime>,
    now: OffsetDateTime,
    zone: &contextos_core::OperatorTimeZone,
) -> Option<OffsetDateTime> {
    let floor = last_scheduled_fire.unwrap_or_else(|| now - time::Duration::seconds(LOOKBACK_SECS));
    // Walks forward from a start close to `now` and widens it until a tick is found, so a long backlog is not walked
    // tick by tick: the ticks inside the first window that has any are few.
    let mut width = time::Duration::hours(1);
    loop {
        let start = (now - width).max(floor);
        let mut latest = None;
        let mut from = start;
        for _ in 0..MAX_TICKS_PER_WINDOW {
            let Some(next) = schedule.next_fire_after(from, zone) else {
                break;
            };
            if next > now {
                break;
            }
            latest = Some(next);
            from = next;
        }
        if latest.is_some() || start == floor {
            return latest;
        }
        width = width.saturating_mul(4);
    }
}

/// How many ticks a search for the latest due one walks within one window, which bounds the work for a schedule that
/// fires far more often than a window is wide.
const MAX_TICKS_PER_WINDOW: usize = 100_000;

async fn poll_vault(
    clients: &Arc<McpClientSet>,
    client: &McpClient,
    vault_name: &str,
    known_mcp_servers: &[String],
    clock: &dyn Clock,
    host: &HostContext,
) {
    // The team first: a vault whose team cannot be used has its whole job pass wait.
    let crate::job_team_gate::PassTeam::Roster(roster) =
        crate::job_team_gate::team_for_pass(client, &host.attention, vault_name).await
    else {
        return;
    };
    // Validation runs first, so a version change has already sent a job back to draft, or put it on probation, and an
    // owner who left has already suspended the job, before anything is raised or scheduled.
    let observed =
        match crate::job_validation::validation_pass_for(client, vault_name, known_mcp_servers, &roster, clock.now())
            .await
        {
            Ok(outcome) => outcome.observed,
            Err(error) => {
                tracing::warn!(vault = vault_name, %error, "jobs could not be validated this pass");
                Vec::new()
            }
        };
    crate::job_reconcile::reconcile(crate::job_reconcile::Reconcile {
        client,
        host,
        vault_name,
        roster: &roster,
        observed: &observed,
        now: clock.now(),
    })
    .await;
    start_due_jobs_for(clients, host, vault_name, clock, &roster).await;
}

/// The second half of a scheduler pass over one vault: starts the `enabled` job that is due, and records the ticks
/// skipped because a run was already active. A due job whose current version is not the one that was reviewed is not
/// started this pass, because the review that would put it on probation has not happened, so a failed run could not
/// suspend it. Its tick stays due, and it runs once a pass has reviewed the version.
///
/// Public so a test can run it without the validation that would otherwise review the job first.
pub async fn start_due_jobs(clients: &Arc<McpClientSet>, host: &HostContext, vault_name: &str, clock: &dyn Clock) {
    let Some(client) = clients.primary() else {
        return;
    };
    let Ok(roster) = crate::team::load_roster(&client, vault_name).await else {
        // A team that cannot be used or read starts nothing: no owner can be checked.
        return;
    };
    start_due_jobs_for(clients, host, vault_name, clock, &roster).await;
}

/// [`start_due_jobs`] over the team the pass has already read and found valid. A due job whose owner is not a member of
/// it is not started, and its tick is not recorded as fired, so it is still due when the owner is back.
async fn start_due_jobs_for(
    clients: &Arc<McpClientSet>,
    host: &HostContext,
    vault_name: &str,
    clock: &dyn Clock,
    roster: &contextos_core::TeamRoster,
) {
    let Some(client) = clients.primary() else {
        return;
    };
    let Ok(jobs) = discover_jobs(&client, vault_name, &clients.names()).await else {
        return;
    };
    if jobs.is_empty() {
        return;
    }
    crate::jobs_base::ensure_jobs_base(&client, vault_name).await;
    let Ok(state) = read_state(&client, vault_name).await else {
        return;
    };
    let now = clock.now();
    // A schedule is wall-clock time where the operator is.
    let zone = clock.time_zone();
    for (job, expression) in scheduled_jobs(jobs, &state.value) {
        if !matches!(job.owner_standing(roster), contextos_core::OwnerStanding::Member(_)) {
            tracing::warn!(vault = vault_name, package = %job.package, workflow = %job.workflow_slug, job = %job.slug, code = "workflow/owner-unresolved", "a due job was not started because its owner is not a member of the team");
            continue;
        }
        let Some(schedule) = stored_schedule(vault_name, &job, &expression) else {
            continue;
        };
        let Ok(slot) = claim_slot(host, vault_name, &job.package, &job.workflow_slug, &job.slug) else {
            continue;
        };
        // What the operator's last schedule change counted from, or the last tick that fired, whichever is later.
        let counted_from = state
            .job(&job.package, &job.workflow_slug, &job.slug)
            .and_then(|entry| entry.last_scheduled_fire);
        let Some(last_fire) = last_fire_of(&slot, counted_from).await else {
            continue;
        };
        let Some(next) = due_tick(&schedule, last_fire, now, &zone) else {
            continue;
        };
        if evaluate_tick(last_fire, next) == TickDecision::AlreadyFired
            || !version_is_reviewed(&client, vault_name, &state.value, &job).await
        {
            continue;
        }
        // Derived fresh from the job's own static `schedule` each pass, never from observed run counts, so the decision
        // stays stable regardless of when it is evaluated.
        let daily_partition = needs_daily_partition(&schedule, DEFAULT_DAILY_PARTITION_THRESHOLD, now, &zone);
        let due = DueTick {
            job,
            slot,
            // Found on the operator's wall clock and shown at its offset, as the run record writes it.
            scheduled_for: next,
            now,
            daily_partition,
        };
        start_tick(clients, &client, host, vault_name, due).await;
    }
}

/// Whether the job is `enabled` in the state as it is now. The scheduler asks after it holds the job's claim, because an
/// enabling that retires a predecessor looks at the predecessor's claim: a run that took the claim must see the state
/// after taking it. A state that cannot be read is not enabled, so nothing runs on a guess.
pub async fn job_is_enabled_now(
    client: &McpClient,
    vault_name: &str,
    package: &str,
    workflow: &str,
    slug: &str,
) -> bool {
    match read_state(client, vault_name).await {
        Ok(state) => state.value.lifecycle_of(package, workflow, slug) == JobLifecycle::Enabled,
        Err(error) => {
            tracing::warn!(vault = vault_name, job = slug, %error, "a job's state could not be read after its claim was taken, so it was not run");
            false
        }
    }
}

/// The last tick that fired for the job: the later of what the operator's last schedule change counted from, which the
/// lifecycle writes, and the last tick the scheduler fired, which the host keeps. `None` when the host's record cannot
/// be read, so the job waits for a later pass.
async fn last_fire_of(slot: &ClaimSlot, counted_from: Option<OffsetDateTime>) -> Option<Option<OffsetDateTime>> {
    let slot = slot.clone();
    match tokio::task::spawn_blocking(move || slot.last_fire()).await {
        Ok(Ok(fired)) => Some(fired.into_iter().chain(counted_from).max()),
        Ok(Err(error)) => {
            tracing::warn!(%error, "a job's last tick could not be read, so it is not started this pass");
            None
        }
        Err(error) => {
            tracing::warn!(%error, "reading a job's last tick did not finish");
            None
        }
    }
}

/// A job whose tick is due and has not fired.
struct DueTick {
    job: RegisteredJob,
    slot: ClaimSlot,
    scheduled_for: OffsetDateTime,
    now: OffsetDateTime,
    daily_partition: bool,
}

/// Fires one due tick: starts a run under the job's claim, or records the tick `skipped` when a run of the job still holds
/// the claim, and records it as fired so it fires once across a restart and between two processes.
async fn start_tick(
    clients: &Arc<McpClientSet>,
    client: &Arc<McpClient>,
    host: &HostContext,
    vault_name: &str,
    due: DueTick,
) {
    let DueTick {
        job,
        slot,
        scheduled_for,
        now,
        daily_partition,
    } = due;
    match take_claim(&slot).await {
        Ok(claim) => {
            // The pass chose this job from a state it read before the claim. An enabling that retired it since looked at
            // the claim first, so what holds the claim must be enabled now, or it must not run.
            if !job_is_enabled_now(client, vault_name, &job.package, &job.workflow_slug, &job.slug).await {
                tracing::info!(vault = vault_name, job = %job.slug, "a due job was not run because it is no longer enabled");
                if let Err(error) = claim.release().await {
                    tracing::warn!(vault = vault_name, job = %job.slug, %error, "a job's claim could not be released cleanly");
                }
                return;
            }
            // Numbered and then recorded as fired, so a failure before either leaves the tick due for a later pass instead of
            // losing it.
            let run_id = match next_run(client, vault_name, &job, &slot, true, now).await {
                Ok(run_id) => run_id,
                Err(error) => {
                    tracing::warn!(vault = vault_name, job = %job.slug, ?error, "a due run could not be numbered, so it will be tried again");
                    return;
                }
            };
            if !record_tick(&slot, scheduled_for, vault_name, &job).await {
                return;
            }
            if let Err(error) = claim
                .announce(crate::run_claim::ActiveRun { run_id, started: now })
                .await
            {
                tracing::warn!(run_id, %error, "a run could not say it is active, so it cannot be cancelled from elsewhere");
            }
            let claimed = ClaimedRun {
                client: Arc::clone(client),
                job,
                run_id,
                started: now,
                daily_partition,
                claim: Some(claim),
            };
            let request = crate::cli_run::RunRequest::scheduled();
            match crate::cli_run::prepare_claimed(Arc::clone(clients), host, vault_name, claimed, request).await {
                Ok(prepared) => {
                    tokio::spawn(prepared.drive());
                }
                Err(error) => tracing::warn!(vault = vault_name, %error, "a due run could not be started"),
            }
        }
        Err(ClaimRunError::Busy) => {
            if !record_tick(&slot, scheduled_for, vault_name, &job).await {
                return;
            }
            match next_run(client, vault_name, &job, &slot, false, now).await {
                Ok(run_id) => {
                    record_skipped_run(client, vault_name, &job, run_id, scheduled_for, now, daily_partition).await;
                }
                Err(error) => {
                    tracing::warn!(vault = vault_name, job = %job.slug, ?error, "a skipped tick could not be numbered, so it was not recorded");
                }
            }
        }
        Err(error) => {
            tracing::warn!(vault = vault_name, job = %job.slug, ?error, "a due run could not take its claim, so it will be tried again");
        }
    }
}

/// Records that the tick fired, so it fires once across a restart and between two processes. `false` when another process
/// fired it first, or when it could not be recorded, in which case it stays due.
async fn record_tick(slot: &ClaimSlot, scheduled_for: OffsetDateTime, vault_name: &str, job: &RegisteredJob) -> bool {
    let slot = slot.clone();
    match tokio::task::spawn_blocking(move || slot.record_fire(scheduled_for)).await {
        Ok(Ok(fired)) => fired,
        Ok(Err(error)) => {
            tracing::warn!(vault = vault_name, job = %job.slug, %error, "a tick could not be recorded, so it is not fired");
            false
        }
        Err(error) => {
            tracing::warn!(%error, "recording a tick did not finish");
            false
        }
    }
}

/// Whether `job`, which is due, has the version that was reviewed. An `enabled` job whose version is not the recorded one
/// has not had the review that puts it on probation, so a failed run of it could not suspend it, and it waits for one. A
/// version that cannot be read counts as not reviewed.
async fn version_is_reviewed(client: &McpClient, vault_name: &str, state: &JobsState, job: &RegisteredJob) -> bool {
    let version = crate::job_validation::job_version(client, vault_name, job)
        .await
        .ok()
        .flatten();
    let reviewed = !state.new_version_under_enabled_job(&job.package, &job.workflow_slug, &job.slug, version.as_ref());
    if !reviewed {
        tracing::warn!(vault = vault_name, package = %job.package, workflow = %job.workflow_slug, job = %job.slug, "a due job was not started because its current version has not been reviewed");
    }
    reviewed
}

/// A job's stored schedule as the cron schedule the scheduler evaluates. An expression that does not parse, which only
/// a hand edit of the state file can leave, is reported and the job is not scheduled.
fn stored_schedule(vault_name: &str, job: &RegisteredJob, expression: &str) -> Option<CronSchedule> {
    match CronSchedule::parse(expression) {
        Ok(schedule) => Some(schedule),
        Err(error) => {
            tracing::warn!(
                vault = vault_name,
                workflow = %job.workflow_slug,
                job = %job.slug,
                %error,
                "the stored schedule is not a cron expression, so the job is not scheduled"
            );
            None
        }
    }
}

/// Records a tick that was not run, because a run of the job still held its claim, as its own terminal,
/// immediately-completed run: no execution ever starts.
async fn record_skipped_run(
    client: &McpClient,
    vault_name: &str,
    job: &RegisteredJob,
    run_id: u64,
    scheduled_for: OffsetDateTime,
    now: OffsetDateTime,
    daily_partition: bool,
) {
    let run_path = contextos_workflows::run_record_path(
        &job.package,
        &job.workflow_slug,
        &job.slug,
        run_id,
        scheduled_for,
        daily_partition,
    );
    let header = RunRecordHeader {
        job,
        run_id,
        trigger: Trigger::Scheduled,
        status: "skipped",
        started: scheduled_for,
        completed: Some(now),
        dry_run: false,
        overrides: &[],
    };
    if let Err(error) = create_run_record(client, vault_name, &run_path, &header).await {
        tracing::warn!(vault = vault_name, job = %job.slug, run_id, %error, "failed to record a skipped job run");
        return;
    }
    log_run_completion(client, vault_name, &run_path, job, run_id, "skipped").await;
}

/// Everything a run of a vault job needs to execute.
pub(crate) struct RunExecution {
    pub caller: Arc<dyn contextos_workflows::McpCaller>,
    pub clients: Arc<McpClientSet>,
    pub host: HostContext,
    pub vault_name: String,
    pub job: RegisteredJob,
    pub run_id: u64,
    /// The record to complete, or `None` for a run that is not recorded or is recorded when it ends.
    pub run_path: Option<String>,
    /// The job's claim, held until the run has recorded how it ended. A dry run takes none.
    pub claim: Option<RunClaim>,
    /// Cancelling it stops the run: the operator pressed Ctrl-C, or the process was asked to end. The claim's cancel
    /// watch cancels the same token, so a request written beside the claim stops the run too.
    pub cancel: CancellationToken,
    /// The input names given on the command line for this run, which replaced the job's own values.
    pub overridden: Vec<String>,
    /// Whether the run's code must be the reviewed version before it starts: a scheduled run only.
    pub review: ReviewGate,
}

/// How often a running job looks for a request to cancel it.
pub(crate) const CANCEL_POLL: StdDuration = StdDuration::from_secs(1);

/// Runs `job`'s entry script to completion (or timeout, or cancellation) through `caller` (the real `WebMcpCaller` for a
/// live run, or a `DryRunMcpCaller` for a dry one, `cli_run.rs`), then releases the job's claim. When `run_path` is
/// `Some`, also records the outcome there and appends the operation-log entry.
///
/// A run that holds a claim is cancelled when a request naming it is written beside the claim, by a page or another
/// process.
///
/// Returns what the run reported, so a command-line caller can await it directly: its outcome, whether its script
/// started, what it set as outputs, and what the host could not do for it.
pub(crate) async fn execute_run(execution: RunExecution) -> RunReport {
    let RunExecution {
        caller,
        clients,
        host,
        vault_name,
        job,
        run_id,
        run_path,
        claim,
        cancel,
        overridden: _,
        review,
    } = execution;
    let Some((primary_server, client)) = clients.primary_named() else {
        return RunReport::ended_before_start(RunOutcome::Failed(RunFailure {
            kind: Some(WorkflowFailureKind::ServerUnavailable),
            message: format!(
                "{}: no MCP server is configured to run the workflow through",
                WorkflowFailureKind::ServerUnavailable.code()
            ),
        }));
    };
    let watch = claim
        .as_ref()
        .map(|claim| watch_for_cancel(claim.slot().clone(), run_id, cancel.clone(), CANCEL_POLL));
    let (result, code) = Box::pin(run_against_release(
        &RunSetting {
            client: &client,
            primary_server: &primary_server,
            clients: &clients,
            host: &host,
            vault_name: &vault_name,
            job: &job,
            run_id,
            review: if claim.is_some() {
                review
            } else {
                ReviewGate::NotRequired
            },
        },
        caller,
        cancel,
    ))
    .await;
    drop(watch);
    let mut host_warnings = Vec::new();
    if let Some(run_path) = &run_path {
        let completed = host.clock.now();
        if let Err(error) = complete_run(
            &client,
            &vault_name,
            run_path,
            &RunCompletion {
                outcome: &result.outcome,
                outputs: &result.output_names,
                completed,
                script_started: result.started,
                release: code.release.as_ref(),
                libraries: &code.libraries,
            },
        )
        .await
        {
            tracing::warn!(vault = %vault_name, job = %job.slug, run_id, %error, "failed to record a job run's completion");
            host_warnings.push(HostWarning {
                code: codes::RECORD_UNAVAILABLE.to_owned(),
                message: "the run's record could not be completed".to_owned(),
            });
        }
        log_run_completion(&client, &vault_name, run_path, &job, run_id, status_of(&result.outcome)).await;
    }
    if let Some(claim) = claim
        && let Err(error) = claim.release().await
    {
        tracing::warn!(vault = %vault_name, job = %job.slug, run_id, %error, "a job's claim could not be released cleanly");
    }
    RunReport {
        started: result.started,
        outputs: result.outputs.map(|outputs| outputs.values().clone()),
        output_names: result.output_names,
        outcome: result.outcome,
        host_warnings,
        ended_at: Some(host.clock.now()),
        read_release: code.release,
        libraries: code.libraries,
        version: code.version,
        ..RunReport::from(RunOutcome::Succeeded)
    }
}

/// What a run of a job needs to be scoped and started.
struct RunSetting<'a> {
    client: &'a McpClient,
    primary_server: &'a str,
    clients: &'a Arc<McpClientSet>,
    host: &'a HostContext,
    vault_name: &'a str,
    job: &'a RegisteredJob,
    run_id: u64,
    review: ReviewGate,
}

/// Reads the release `setting.job` names and runs it. The release is read before anything else: its own `workflow.toml`
/// declares the servers the run is scoped to, and a release that cannot be read whole refuses the run before its script
/// starts.
async fn run_against_release(
    setting: &RunSetting<'_>,
    caller: Arc<dyn contextos_workflows::McpCaller>,
    cancel: CancellationToken,
) -> (RunResult, CodeRead) {
    let RunSetting {
        client,
        primary_server,
        clients,
        host,
        vault_name,
        job,
        run_id,
        review,
    } = *setting;
    let declared = match read_job_release(client, vault_name, job).await {
        Ok(read) => read.declared_job(job).map(|declared| (read, declared)),
        Err(failure) => Err(failure),
    };
    let (read, declared) = match declared {
        Ok(found) => found,
        Err(failure) => {
            return (
                RunResult::ended_before_start(RunOutcome::Failed(failure)),
                CodeRead::default(),
            );
        }
    };
    match scoped_caller_for_run(client, primary_server, vault_name, &declared.calls, caller).await {
        Err(failure) => (
            RunResult::ended_before_start(RunOutcome::Failed(failure)),
            CodeRead::default(),
        ),
        Ok(caller) => {
            run_job_script(JobScript {
                client,
                clients,
                host,
                vault_name,
                job: &declared,
                read,
                run_id,
                caller,
                cancel,
                review,
            })
            .await
        }
    }
}

/// What running a job's entry script needs, beyond the claim and the record.
struct JobScript<'a> {
    client: &'a McpClient,
    clients: &'a Arc<McpClientSet>,
    host: &'a HostContext,
    vault_name: &'a str,
    /// The job as its release declares its workflow.
    job: &'a RegisteredJob,
    /// The job's release, read and verified.
    read: ReadForRun,
    run_id: u64,
    caller: Arc<dyn contextos_workflows::McpCaller>,
    cancel: CancellationToken,
    /// Whether the run's code must be the reviewed version. A dry run, which takes no claim, never requires it.
    review: ReviewGate,
}

/// Reads the job's entry script and its workflow's modules and runs it, declaring the outputs its workflow declares. A script that cannot be read
/// ends the run before it starts.
async fn run_job_script(script: JobScript<'_>) -> (RunResult, CodeRead) {
    let JobScript {
        client,
        clients,
        host,
        vault_name,
        job,
        read,
        run_id,
        caller,
        cancel,
        review,
    } = script;
    let job_toml = read.job_toml;
    let release = RunRelease::from(&read.code.snapshot.manifest);
    let libraries: Vec<RunLibrary> = read.code.snapshot.libraries.iter().map(RunLibrary::from).collect();
    let source = read.code.snapshot.script;
    // Only a scheduled run is held to the reviewed version. A run by hand and a dry run take the code as it stands, and
    // still report the version of what they read, so the trial and the registration are of the version that ran.
    let version = match version_for_run(
        client,
        vault_name,
        job,
        Some(
            read.code
                .snapshot
                .basis
                .version_in(contextos_workflows::StoreKind::Vault, &job_toml),
        ),
        review,
    )
    .await
    {
        Ok(version) => version,
        Err(failure) => {
            tracing::warn!(vault = vault_name, package = %job.package, workflow = %job.workflow_slug, job = %job.slug, run_id, code = failure.kind.map_or("", |kind| kind.code()), "a run did not start because the job's code could not be confirmed as reviewed");
            return (
                RunResult::ended_before_start(RunOutcome::Failed(failure)),
                CodeRead::default(),
            );
        }
    };
    log_libraries(vault_name, job, run_id, &libraries);
    // Real time for the run's own engine (its timeout follows it), shown in the operator's zone.
    let clock: Arc<dyn Clock> = Arc::new(crate::vault_time::LocalisedSystemClock {
        zone: host.clock.clone(),
    });
    let settings = RunSettings::from(RunPlan {
        timeout_secs: job.timeout_secs,
        log_level: host.level_for(job.workflow_log_level, job.log_level),
        label: RunLabel {
            workflow: job.workflow_slug.clone(),
            job: Some(job.slug.clone()),
            run_id: Some(run_id),
        },
        cancel,
    });
    let result = run_workflow_declaring(
        source,
        job.inputs.clone(),
        job.outputs.clone(),
        caller,
        contextos_workflows::WorkflowFiles::new(
            job.templates.clone(),
            Arc::new(VaultWorkflowFiles::from(VaultWorkflowFolder {
                clients: Arc::clone(clients),
                vault: vault_name,
                package: &job.package,
            })),
        ),
        clock,
        settings,
    )
    .await;
    (
        result,
        CodeRead {
            release: Some(release),
            libraries,
            version,
        },
    )
}

/// Runs `job` live and settles the trial of its first run: the version it ran is the one of the snapshot it read, which
/// the run returns, so what the outcome decides is about that version, and the outcome is applied once the run has released its claim. Every live run,
/// scheduled or by hand, goes through here. A dry run goes through [`execute_run`] alone, so it can never count as
/// acceptance. A run with input overrides settles nothing, because the version covers the job's own values and a run
/// with others says nothing about it.
pub(crate) async fn execute_live_run(execution: RunExecution) -> RunReport {
    let Some(client) = execution.clients.primary() else {
        return execute_run(execution).await;
    };
    let vault_name = execution.vault_name.clone();
    let job = execution.job.clone();
    let clock = Arc::clone(&execution.host.clock);
    let host = execution.host.clone();
    let settles = execution.overridden.is_empty();
    let run_id = execution.run_id;
    let mut report = execute_run(execution).await;
    // A run whose script never started says nothing about the version, so it settles nothing.
    if settles && report.started {
        let now = clock.now();
        let settled = crate::job_probation::settle_and_examine(crate::job_probation::LiveRunEnded {
            client: &client,
            host: &host,
            vault_name: &vault_name,
            job: &job,
            version: report.version.clone(),
            outcome: &report.outcome,
            run: run_id,
            now,
        })
        .await;
        if settled == Settlement::NotRecorded {
            report.host_warnings.push(HostWarning {
                code: codes::STATE_UNAVAILABLE.to_owned(),
                message: "the outcome of the run could not be saved to the job's state".to_owned(),
            });
        }
    }
    report
}

/// Confines `caller` to `vault_name` and the workflow's call rules. When the primary server's vault list
/// cannot be read the run is refused, because an unknown list cannot be checked against.
pub(crate) async fn scoped_caller_for_run(
    client: &McpClient,
    primary_server: &str,
    vault_name: &str,
    calls: &contextos_workflows::call_rules::CallRules,
    caller: Arc<dyn contextos_workflows::McpCaller>,
) -> Result<Arc<dyn contextos_workflows::McpCaller>, RunFailure> {
    let reported_vaults = shell::configured_vault_names(client)
        .await
        .map_err(|source| RunFailure {
            kind: Some(WorkflowFailureKind::ServerUnavailable),
            message: format!(
                "{}: the configured vaults could not be listed to scope the run: {source}",
                WorkflowFailureKind::ServerUnavailable.code()
            ),
        })?;
    Ok(RunScope {
        primary_server,
        home_vault: vault_name,
        reported_vaults,
        calls: calls.clone(),
    }
    .apply(caller))
}

/// Reads a versioned snapshot shared by the scheduler and favourite updates.
pub(crate) async fn read_state(
    client: &McpClient,
    vault_name: &str,
) -> Result<crate::vault_state::Snapshot<JobsState>, StateError> {
    Ok(crate::vault_state::read::<JobsState>(client, vault_name, JOBS_STATE_PATH).await?)
}

/// Saves only if no scheduler, favourite request, or external editor changed the snapshot.
pub(crate) async fn write_state(
    client: &McpClient,
    vault_name: &str,
    state: &crate::vault_state::Snapshot<JobsState>,
) -> Result<(), StateError> {
    state.save(client, vault_name, JOBS_STATE_PATH).await?;
    Ok(())
}

/// Sets `package`/`workflow_slug`/`slug`'s own favourite flag in `.contextos/jobs.toml` and persists the result, the
/// jobs-side equivalent of `apps_favourites::set_app_favourite`. Unlike apps favouriting, the entry is not
/// removed when un-favourited: it may already carry `last_scheduled_fire` the scheduler depends on, so only
/// the one field is flipped, never the whole entry.
pub(crate) async fn set_job_favourite(
    client: &McpClient,
    vault_name: &str,
    package: &str,
    workflow_slug: &str,
    slug: &str,
    favourite: bool,
) -> Result<JobsState, StateError> {
    let mut state = read_state(client, vault_name).await?;
    state.job_entry(package, workflow_slug, slug).favourite = favourite;
    write_state(client, vault_name, &state).await?;
    Ok(state.value)
}

/// Every registered job in `vault_name` whose own `.contextos/jobs.toml` entry is favourited, for the header
/// Favourites panel's "Favourite jobs" section (`routes::shell::favourites_panel`); mirrors
/// [`shell::favourited_app_links`]'s identical "join discovery against a favourites file" shape. Never fails
/// outright: an unreachable MCP session degrades to no favourited jobs, the same "chrome, not a hard
/// dependency" contract every other favourites lookup here follows.
pub(crate) async fn favourited_jobs(
    client: &McpClient,
    vault_name: &str,
    known_mcp_servers: &[String],
) -> Vec<RegisteredJob> {
    let Ok(state) = read_state(client, vault_name).await else {
        return Vec::new();
    };
    let any_favourite = state.recorded_jobs().into_iter().any(|(package, workflow, job)| {
        state
            .job(package, workflow, job)
            .is_some_and(|job_state| job_state.favourite)
    });
    if !any_favourite {
        return Vec::new();
    }
    let Ok(registered) = discover_jobs(client, vault_name, known_mcp_servers).await else {
        return Vec::new();
    };
    registered
        .into_iter()
        .filter(|job| {
            state
                .job(&job.package, &job.workflow_slug, &job.slug)
                .is_some_and(|job_state| job_state.favourite)
        })
        .collect()
}

#[cfg(test)]
#[path = "scheduler_test.rs"]
mod tests;
