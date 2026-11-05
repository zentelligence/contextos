//! The notices a vault's workflows page and a job's page show about the team: that the team cannot be used, so the
//! vault's scheduled jobs are paused, and that a job's owner is not a member of it.
//!
//! Read on render, so what a page says is what the team is now. The scheduler's own reading is
//! [`crate::job_team_gate`]'s, and it logs the same problem once when it starts and once when it clears.

use contextos_core::{OwnerStanding, TeamRoster};

use crate::jobs::RegisteredJob;
use crate::mcp_client::McpClient;
use crate::team::{RosterLoadError, load_roster};

/// The notice for a vault whose team cannot be used for its jobs, or `None` when it can, or when the vault has no job
/// that needs one. A team that cannot be read at all says nothing: it is unknown, which is not an error to report.
pub(crate) async fn team_notice(client: &McpClient, vault_name: &str) -> Option<String> {
    problem_notice(client, vault_name, &load_roster(client, vault_name).await).await
}

/// [`team_notice`] for a team that has already been read, so a page reads it once.
async fn problem_notice(
    client: &McpClient,
    vault_name: &str,
    loaded: &Result<TeamRoster, RosterLoadError>,
) -> Option<String> {
    let Err(RosterLoadError::Invalid(error)) = loaded else {
        return None;
    };
    if error.rule() == contextos_core::TeamRule::FileAbsent
        && !crate::job_team_gate::vault_has_job_files(client, vault_name)
            .await
            .unwrap_or(false)
    {
        // No jobs, or no way to tell: either way there is nothing to say about the team.
        return None;
    }
    Some(format!(
        "The vault's team cannot be used for jobs ({}, rule {}): scheduled jobs in this vault are paused until \
         .contextos/team.toml is corrected.",
        error.code(),
        error.rule().as_str()
    ))
}

/// The notice for a job whose owner is missing or is not a member of the vault's team, or `None` when it is a member,
/// or the team cannot be used, which [`team_notice`] already says.
fn owner_notice(roster: &TeamRoster, job: &RegisteredJob) -> Option<String> {
    match job.owner_standing(roster) {
        OwnerStanding::Member(_) => None,
        OwnerStanding::NotAMember(slug) => Some(format!(
            "The job's owner {:?} is not a member of the team (validation/owner-unresolved), so its live runs are \
             refused and, when it is enabled, the system suspends it. Name a member of .contextos/team.toml.",
            slug.as_str()
        )),
        OwnerStanding::InvalidSlug(text) => Some(format!(
            "The job's owner {text:?} is not a member of the team (validation/owner-unresolved), so its live runs are \
             refused and, when it is enabled, the system suspends it. Name a member of .contextos/team.toml."
        )),
        OwnerStanding::Undeclared => Some(
            "The job declares no owner (validation/owner-unresolved), so its live runs are refused and, when it is \
             enabled, the system suspends it. Name a member of .contextos/team.toml."
                .to_owned(),
        ),
    }
}

/// What a job's page says about the team before its facts: the vault's team problem, when it has one, and the job's owner,
/// when the job loads and its owner is not a member.
pub(crate) async fn job_notices(client: &McpClient, vault_name: &str, job: Option<&RegisteredJob>) -> Vec<String> {
    let loaded = load_roster(client, vault_name).await;
    let mut notices: Vec<String> = problem_notice(client, vault_name, &loaded).await.into_iter().collect();
    if let (Some(job), Ok(roster)) = (job, &loaded) {
        notices.extend(owner_notice(roster, job));
    }
    notices
}
