//! Starting a vault job's run: the lifecycle gate, the job's claim, its run number, and marking what a
//! dead run left behind.
//!
//! A live run holds an operating-system lock on a file beside the job on the host (`run_claim.rs`), so the same job never
//! overlaps itself, whichever process starts it, and different jobs, even in one vault, run at the same time. A dry run
//! reads and never writes, so it takes no claim. Nothing a run needs to remember is written to the vault's
//! `.contextos/jobs.toml`: run numbers and the last tick are the host's (`run_bookkeeping.rs`).

use std::sync::Arc;

use contextos_workflows::{
    Clock, CronSchedule, DEFAULT_DAILY_PARTITION_THRESHOLD, JobLifecycle, needs_daily_partition,
};
use time::OffsetDateTime;

use crate::job_runs::{RunSummary, run_history};
use crate::jobs::{RegisteredJob, discover_jobs};
use crate::mcp_client::{McpClient, McpClientSet};
use crate::run_claim::{ActiveRun, ClaimSlot, RunClaim, RunClaimError, is_plain_segment};
use crate::run_records::mark_interrupted;
use crate::run_settings::HostContext;
use crate::scheduler::{RunKind, read_state, run_allowed};

/// The directory beneath the claims root that holds vault jobs' claims, beside the `shared` one.
pub(crate) const VAULT_CLAIMS_DIR: &str = "vaults";

/// How many of a job's most recent records are looked at to find its highest run number and any record a dead run left
/// `running`.
const RECENT_RECORDS: usize = 100;

/// One successfully claimed run: its number is taken, and a live one holds the job's claim, ready for the caller to
/// create a run record (or not) and execute.
pub(crate) struct ClaimedRun {
    pub client: Arc<McpClient>,
    pub job: RegisteredJob,
    pub run_id: u64,
    pub started: OffsetDateTime,
    pub daily_partition: bool,
    /// The job's claim, held for the whole run. A dry run takes none.
    pub claim: Option<RunClaim>,
}

/// Why [`claim_run`] could not claim a run, shared by the page's run and a command-line run, so both report identical
/// outcomes for identical conditions.
#[derive(Debug, thiserror::Error)]
pub enum ClaimRunError {
    #[error("the primary MCP server, or the vault's job files, could not be reached")]
    Unreachable,
    #[error("no such job is registered")]
    JobNotFound,
    /// A run of this job is already active.
    #[error("workflow/job-busy: a run of this job is already active")]
    Busy,
    /// The job's lifecycle state does not allow this kind of run.
    #[error("workflow/job-not-runnable: the job is {state}")]
    NotRunnable {
        state: JobLifecycle,
        /// Why the job is suspended, when it is and its state says.
        suspension: Option<contextos_workflows::SuspensionRecord>,
    },
    /// The job is one of a chain and the other job holds it, so a live run by hand is not available.
    #[error(
        "workflow/job-not-runnable: a live run by hand is not available because {0}; a chain of jobs has one job in use at a time, and a successor is tried by dry runs"
    )]
    ChainHeld(contextos_workflows::ChainHold),
    /// The host's claim, run number, or tick state could not be used.
    #[error("the host's claim, run number, or tick state for this job could not be used")]
    HostUnavailable,
    /// The job's owner is missing or not a member of the team.
    #[error("workflow/owner-unresolved: the job's owner is missing or not a member of the team")]
    OwnerUnresolved {
        owner: Option<contextos_core::DeclaredOwner>,
    },
    /// The vault's team is invalid.
    #[error("workflow/team-invalid: the vault's team is invalid")]
    TeamInvalid { rule: contextos_core::TeamRule },
}

/// Where a vault job's claim, active-run descriptor, cancel request, run number, and last tick are kept on the host.
///
/// A vault is named without regard to ASCII case, as the server matches it, so every spelling of one shares a claim.
///
/// # Errors
///
/// Returns [`ClaimRunError::HostUnavailable`] when the host has no claims directory or a name could leave it.
pub fn claim_slot(
    host: &HostContext,
    vault_name: &str,
    package: &str,
    workflow_slug: &str,
    slug: &str,
) -> Result<ClaimSlot, ClaimRunError> {
    let Some(claims_root) = &host.claims_root else {
        tracing::warn!("the host has no claims directory, so no vault job can be claimed");
        return Err(ClaimRunError::HostUnavailable);
    };
    claim_slot_under(claims_root, vault_name, package, workflow_slug, slug)
}

/// As [`claim_slot`], for a claims directory already in hand.
///
/// # Errors
///
/// Returns [`ClaimRunError::HostUnavailable`] when a name could leave the claims directory.
pub fn claim_slot_under(
    claims_root: &std::path::Path,
    vault_name: &str,
    package: &str,
    workflow_slug: &str,
    slug: &str,
) -> Result<ClaimSlot, ClaimRunError> {
    for name in [vault_name, package, workflow_slug, slug] {
        if !is_plain_segment(name) {
            tracing::warn!(name, "a name that could leave the claims directory was refused");
            return Err(ClaimRunError::HostUnavailable);
        }
    }
    ClaimSlot::new(
        claims_root
            .join(VAULT_CLAIMS_DIR)
            .join(vault_name.to_ascii_lowercase())
            .join(package)
            .join(workflow_slug),
        slug,
    )
    .map_err(|error| {
        tracing::warn!(%error, "a job's claim could not be placed");
        ClaimRunError::HostUnavailable
    })
}

/// Takes the claim without blocking the executor thread.
pub(crate) async fn take_claim(slot: &ClaimSlot) -> Result<RunClaim, ClaimRunError> {
    let slot = slot.clone();
    match tokio::task::spawn_blocking(move || RunClaim::acquire(&slot)).await {
        Ok(Ok(claim)) => Ok(claim),
        Ok(Err(RunClaimError::Busy { .. })) => Err(ClaimRunError::Busy),
        Ok(Err(error)) => {
            tracing::warn!(%error, "a job's claim could not be taken");
            Err(ClaimRunError::HostUnavailable)
        }
        Err(error) => {
            tracing::warn!(%error, "taking a job's claim did not finish");
            Err(ClaimRunError::HostUnavailable)
        }
    }
}

/// Takes the job's next run number. When `holds_claim`, any record of the job still `running` is marked `interrupted`
/// first, because holding the claim proves the process that wrote it is gone. A dry run's record is never one of them: it
/// is written when its run ends, and takes no claim to say it is over.
pub(crate) async fn next_run(
    client: &McpClient,
    vault_name: &str,
    job: &RegisteredJob,
    slot: &ClaimSlot,
    holds_claim: bool,
    now: time::OffsetDateTime,
) -> Result<u64, ClaimRunError> {
    let history = run_history(client, vault_name, &job.package, &job.workflow_slug, &job.slug, RECENT_RECORDS)
        .await
        .map_err(|reason| {
            tracing::warn!(vault = vault_name, job = %job.slug, reason, "a run number could not be taken without the job's records");
            ClaimRunError::Unreachable
        })?;
    if holds_claim {
        mark_abandoned(client, vault_name, job, &history, now).await;
    }
    let highest = history.iter().filter_map(RunSummary::number).max().unwrap_or(0);
    let slot = slot.clone();
    match tokio::task::spawn_blocking(move || slot.take_run_number(highest)).await {
        Ok(Ok(number)) => Ok(number),
        Ok(Err(error)) => {
            tracing::warn!(vault = vault_name, job = %job.slug, %error, "a run number could not be taken");
            Err(ClaimRunError::HostUnavailable)
        }
        Err(error) => {
            tracing::warn!(%error, "taking a run number did not finish");
            Err(ClaimRunError::HostUnavailable)
        }
    }
}

async fn mark_abandoned(
    client: &McpClient,
    vault_name: &str,
    job: &RegisteredJob,
    history: &[RunSummary],
    now: time::OffsetDateTime,
) {
    for record in history
        .iter()
        .filter(|record| record.status == "running" && !record.dry_run)
    {
        if let Err(error) = mark_interrupted(client, vault_name, &record.path, now).await {
            tracing::warn!(vault = vault_name, job = %job.slug, run = %record.run_id, %error, "an abandoned run's record could not be marked interrupted");
        }
    }
}

/// Discovers `package`/`workflow_slug`/`slug` in `vault_name`, checks that its lifecycle state allows `kind`, takes its claim when
/// the run is live, and takes its run number. Shared by the page's run, a scheduled run, and a command-line run.
///
/// # Errors
///
/// See [`ClaimRunError`].
pub(crate) async fn claim_run(
    clients: &Arc<McpClientSet>,
    host: &HostContext,
    vault_name: &str,
    package: &str,
    workflow_slug: &str,
    slug: &str,
    kind: RunKind,
) -> Result<ClaimedRun, ClaimRunError> {
    let Some(client) = clients.primary() else {
        return Err(ClaimRunError::Unreachable);
    };
    let Ok(jobs) = discover_jobs(&client, vault_name, &clients.names()).await else {
        return Err(ClaimRunError::Unreachable);
    };
    let Some(job) = jobs
        .into_iter()
        .find(|job| job.package == package && job.workflow_slug == workflow_slug && job.slug == slug)
    else {
        return Err(ClaimRunError::JobNotFound);
    };
    let Ok(state) = read_state(&client, vault_name).await else {
        return Err(ClaimRunError::Unreachable);
    };
    let lifecycle = state.value.lifecycle_of(&job.package, &job.workflow_slug, &job.slug);
    if !run_allowed(lifecycle, kind) {
        let suspension = state
            .value
            .job(&job.package, &job.workflow_slug, &job.slug)
            .and_then(contextos_workflows::JobState::suspension)
            .filter(|_| lifecycle == JobLifecycle::Suspended)
            .cloned();
        return Err(ClaimRunError::NotRunnable {
            state: lifecycle,
            suspension,
        });
    }
    if kind == RunKind::Live {
        // Before the claim, so a refusal takes no run number, writes no record, and never looks like a busy job.
        if let Some(hold) = state
            .value
            .chain_hold_on_live_run(&job.package, &job.workflow_slug, &job.slug)
        {
            return Err(ClaimRunError::ChainHeld(hold));
        }
        require_owner(&client, vault_name, &job).await?;
    }
    let slot = claim_slot(host, vault_name, &job.package, &job.workflow_slug, &job.slug)?;
    let claim = match kind {
        RunKind::Live => Some(take_claim(&slot).await?),
        RunKind::Dry => None,
    };
    // The run's folder and stamps are read synchronously from here on, so the server's zone is fetched first: without
    // it, a run started before anything else asked would be dated in this host's zone instead of the operator's.
    host.clock.warm().await;
    let run_id = next_run(&client, vault_name, &job, &slot, claim.is_some(), host.clock.now()).await?;
    let now = host.clock.now();
    if let Some(claim) = &claim
        && let Err(error) = claim.announce(ActiveRun { run_id, started: now }).await
    {
        // A claim that cannot say is still a claim: the run goes ahead, and cannot be cancelled from elsewhere.
        tracing::warn!(run_id, %error, "a run could not say it is active, so it cannot be cancelled from elsewhere");
    }
    let stored_schedule = state
        .value
        .job(&job.package, &job.workflow_slug, &job.slug)
        .and_then(|entry| entry.schedule().map(str::to_owned));
    Ok(ClaimedRun {
        client,
        job,
        run_id,
        started: now,
        daily_partition: daily_partition_for(stored_schedule.as_deref(), now, &host.clock.time_zone()),
        claim,
    })
}

/// Refuses a live run of a job whose owner is missing or is not a member of the vault's team, and any live run while the
/// team cannot be used. The team is read strictly: a job has an owner to answer for it, or it does not run live. A dry
/// run does not call this, because it reads and never writes.
///
/// # Errors
///
/// Returns [`ClaimRunError::OwnerUnresolved`] for an owner who is no member, [`ClaimRunError::TeamInvalid`] for a team
/// that breaks a rule, and [`ClaimRunError::Unreachable`] when the team cannot be read.
pub(crate) async fn require_owner(
    client: &McpClient,
    vault_name: &str,
    job: &RegisteredJob,
) -> Result<(), ClaimRunError> {
    match crate::team::load_roster(client, vault_name).await {
        Ok(roster) => {
            if matches!(job.owner_standing(&roster), contextos_core::OwnerStanding::Member(_)) {
                Ok(())
            } else {
                Err(ClaimRunError::OwnerUnresolved {
                    owner: job.declared_owner.clone(),
                })
            }
        }
        Err(crate::team::RosterLoadError::Invalid(error)) => Err(ClaimRunError::TeamInvalid { rule: error.rule() }),
        Err(crate::team::RosterLoadError::Unreachable(_) | crate::team::RosterLoadError::Unreadable { .. }) => {
            Err(ClaimRunError::Unreachable)
        }
    }
}

/// Whether `job`'s own schedule needs day-level run-record partitioning,
/// re-derived from its `schedule` string rather than any cached or
/// persisted decision. A manual-trigger-only job (no
/// `schedule`) never escalates; neither does one whose expression fails
/// to parse here, which should not happen past registration but is not
/// worth a panic over.
pub(crate) fn daily_partition_for(
    schedule: Option<&str>,
    now: OffsetDateTime,
    zone: &contextos_core::OperatorTimeZone,
) -> bool {
    schedule
        .and_then(|expression| CronSchedule::parse(expression).ok())
        .is_some_and(|schedule| needs_daily_partition(&schedule, DEFAULT_DAILY_PARTITION_THRESHOLD, now, zone))
}

#[cfg(test)]
#[path = "vault_runs_test.rs"]
mod tests;
