//! Settling the trial of a job's first live run.
//!
//! An enabled job whose new version passed validation keeps its schedule, and its first live run decides whether the
//! version works. [`settle_first_run`] applies what that run's outcome says: a success or a degraded run ends the
//! trial, a failure suspends the job with the failure as the reason, and a cancelled run decides nothing. A dry run
//! never reaches it, so a synthetic result cannot count as acceptance.

use contextos_workflows::{
    Actor, Applied, FirstRunVerdict, JobFingerprint, JobKey, JobLifecycle, RunOutcome, Transition, TransitionError,
};
use time::OffsetDateTime;

use crate::job_lifecycle::{LifecycleChange, LifecycleError, change_lifecycle};
use crate::jobs::RegisteredJob;
use crate::mcp_client::McpClient;
use crate::run_settings::HostContext;
use crate::vault_runs::claim_slot;

/// How many times a change another writer got in the way of is re-read and applied again before it is given up. The
/// state file is changed by the scheduler, by a command, and by a run's own lock, so a conflict is ordinary and not a
/// fault.
const ATTEMPTS: usize = 5;

/// What settling a run's outcome did.
#[derive(Debug, Eq, PartialEq)]
pub enum Settlement {
    /// The trial ended: the version works.
    Passed,
    /// The job was suspended, because the version failed its trial.
    Suspended,
    /// The outcome settles nothing: the run was cancelled, the job is not on trial, the version that ran is not the one
    /// on trial, or a suspended job failed another run by hand.
    NothingToSettle,
    /// The outcome could not be recorded. A failed run that cannot suspend its job is a safety gap, so this is reported
    /// at warning level.
    NotRecorded,
}

/// What [`settle_and_examine`] is given about a live run that ended.
pub struct LiveRunEnded<'a> {
    pub client: &'a McpClient,
    pub host: &'a HostContext,
    pub vault_name: &'a str,
    pub job: &'a RegisteredJob,
    /// The version the run ran, or `None` when the job's files could not be read before it.
    pub version: Option<JobFingerprint>,
    pub outcome: &'a RunOutcome,
    /// The number of the run that ended.
    pub run: u64,
    pub now: OffsetDateTime,
}

/// Settles the outcome of a live run as [`settle_first_run`] does, and then, only when that suspended the job, marks the
/// run examined in the host's run-failed bookkeeping: the suspension is what handles its failure, so no pass raises it
/// again as a failure of its own.
///
/// The order matters. A settlement that is not recorded leaves the job on trial with nothing handling the failure, so
/// the run must stay unexamined and be looked at later. The short gap between a recorded suspension and the mark moving
/// (or a host that stops inside it) is closed from durable state: a pass that meets a failed run the job's suspension
/// record names treats it as covered (see `job_reconcile`).
pub async fn settle_and_examine(input: LiveRunEnded<'_>) -> Settlement {
    let LiveRunEnded {
        client,
        host,
        vault_name,
        job,
        version,
        outcome,
        run,
        now,
    } = input;
    let settled = settle_first_run(client, vault_name, job, version, outcome, run, now).await;
    if settled == Settlement::Suspended
        && let Ok(slot) = claim_slot(host, vault_name, &job.package, &job.workflow_slug, &job.slug)
    {
        let key = JobKey {
            package: job.package.clone(),
            workflow: job.workflow_slug.clone(),
            job: job.slug.clone(),
        };
        crate::job_reconcile::advance_mark(&slot, vault_name, &key, run).await;
    }
    settled
}

/// Settles the first-run trial of `job` with how a live run of `version`, the version it ran, ended.
///
/// `version` is `None` when the job's files could not be read before the run, in which case nothing can be settled. `run`
/// is the number of the run that ended, which a suspension names.
pub async fn settle_first_run(
    client: &McpClient,
    vault_name: &str,
    job: &RegisteredJob,
    version: Option<JobFingerprint>,
    outcome: &RunOutcome,
    run: u64,
    now: OffsetDateTime,
) -> Settlement {
    let Ok(verdict) = FirstRunVerdict::try_from(outcome) else {
        return Settlement::NothingToSettle;
    };
    let Some(version) = version else {
        tracing::warn!(vault = vault_name, package = %job.package, workflow = %job.workflow_slug, job = %job.slug, "the version a live run ran could not be read, so its outcome was not applied to the job's trial");
        return Settlement::NotRecorded;
    };
    let transition = Transition::from((verdict, version, Some(run)));
    for _ in 0..ATTEMPTS {
        let change = change_lifecycle(
            client,
            vault_name,
            LifecycleChange {
                package: &job.package,
                workflow_slug: &job.workflow_slug,
                job_slug: &job.slug,
                actor: Actor::System,
                transition: transition.clone(),
                now,
            },
        )
        .await;
        match change {
            Ok(Applied::To(JobLifecycle::Suspended)) => {
                tracing::warn!(vault = vault_name, package = %job.package, workflow = %job.workflow_slug, job = %job.slug, "the first live run of a new version failed, so the job was suspended");
                return Settlement::Suspended;
            }
            Ok(Applied::To(_) | Applied::Removed) => {
                tracing::info!(vault = vault_name, package = %job.package, workflow = %job.workflow_slug, job = %job.slug, "the first live run of a new version passed");
                return Settlement::Passed;
            }
            Err(LifecycleError::Refused {
                source:
                    TransitionError::NotOnProbation { .. }
                    | TransitionError::WrongVersion { .. }
                    | TransitionError::WrongState { .. },
            }) => return Settlement::NothingToSettle,
            Err(LifecycleError::Refused { source }) => {
                tracing::warn!(vault = vault_name, package = %job.package, workflow = %job.workflow_slug, job = %job.slug, %source, "the outcome of a live run was refused by the lifecycle");
                return Settlement::NotRecorded;
            }
            Err(LifecycleError::Conflict) => {}
            Err(error @ LifecycleError::Unavailable(_)) => {
                tracing::warn!(vault = vault_name, package = %job.package, workflow = %job.workflow_slug, job = %job.slug, %error, "the outcome of a live run could not be applied to the job's trial");
                return Settlement::NotRecorded;
            }
        }
    }
    tracing::warn!(vault = vault_name, package = %job.package, workflow = %job.workflow_slug, job = %job.slug, "the job state kept changing, so the outcome of a live run was not applied to the job's trial");
    Settlement::NotRecorded
}
