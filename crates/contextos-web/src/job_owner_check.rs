//! The owner check of a scheduler pass: an `enabled` job whose owner is not a member of the team is suspended by the
//! system, because nobody answers for what it does and its failures would have nobody to go to.
//!
//! It runs in the same state read and write as the validation pass, so one conflict-checked write covers both. A job
//! that is not enabled is left alone: it never runs unattended, so there is nothing to stop, and a run by hand is
//! refused at the point of the run.

use contextos_core::{DeclaredOwner, OwnerStanding, TeamRoster};
use contextos_workflows::{Actor, JobScope, JobsState, Suspension, SuspensionCause, SuspensionReason, Transition};
use time::OffsetDateTime;

/// Suspends `job` when it is enabled and its `declared` owner is not a member of `roster`.
/// Returns whether it did. The reason names what the job file declares and never a member.
pub(crate) fn suspend_for_unresolved_owner(
    state: &mut JobsState,
    job: JobScope<'_>,
    declared: Option<&DeclaredOwner>,
    roster: &TeamRoster,
    now: OffsetDateTime,
) -> bool {
    let standing = OwnerStanding::from((declared, roster));
    let Ok(reason) = SuspensionReason::try_from(&standing) else {
        return false;
    };
    state
        .transition(
            job.package,
            job.workflow,
            job.job,
            Actor::System,
            &Transition::Suspend(Suspension {
                cause: SuspensionCause::OwnerUnresolved,
                reason,
            }),
            now,
        )
        .is_ok()
}

#[cfg(test)]
#[path = "job_owner_check_test.rs"]
mod tests;
