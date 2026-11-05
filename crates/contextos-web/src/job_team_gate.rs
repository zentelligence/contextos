//! What a scheduler pass makes of a vault's team before it does anything else with the vault's jobs.
//!
//! A pass needs a valid team: owners are checked against it, and failures go to its chief of staff. A team that cannot be
//! used is a configuration error, and the vault's whole job pass waits: nothing is validated, suspended, raised, or
//! started, and nothing is recorded against any job. A team that cannot be read at all is an outage, which is neither a
//! team error nor a defect in a job, so the pass waits and says so once, with the `workflow/server-unavailable` code,
//! when the outage starts and once when it clears.

use contextos_core::{TeamRoster, TeamRule};

use crate::job_attention::{JobAttentionMemory, OutageLogEntry, TeamLogEntry};
use crate::jobs::list_job_files;
use crate::mcp_client::{McpCallError, McpClient};
use crate::team::{RosterLoadError, load_roster};

/// What the team allows a pass to do.
pub(crate) enum PassTeam {
    /// The team is valid: the pass goes ahead with it.
    Roster(Box<TeamRoster>),
    /// The vault's job pass waits: the team is a configuration error, which is remembered and logged when it starts, or
    /// the team is unknown because it could not be read.
    Waits,
}

/// Reads the vault's team for a pass, and remembers and logs when it starts being a problem and when it stops.
pub(crate) async fn team_for_pass(client: &McpClient, memory: &JobAttentionMemory, vault_name: &str) -> PassTeam {
    match load_roster(client, vault_name).await {
        Ok(roster) => {
            note_outage(memory, vault_name, false);
            note(memory, vault_name, None);
            PassTeam::Roster(Box::new(roster))
        }
        Err(RosterLoadError::Invalid(error)) => {
            note_outage(memory, vault_name, false);
            if error.rule() == TeamRule::FileAbsent {
                match vault_has_job_files(client, vault_name).await {
                    Ok(true) => {}
                    Ok(false) => {
                        // A vault with no jobs needs no team, so an absent one is no problem and the pass has
                        // nothing to do.
                        note(memory, vault_name, None);
                        return PassTeam::Waits;
                    }
                    Err(_) => {
                        // Whether the vault has jobs is unknown, which says nothing about the team: what was noted of
                        // it stays as it was, and the pass is skipped.
                        tracing::debug!(
                            vault = vault_name,
                            "the vault's jobs could not be listed, so the pass waits"
                        );
                        return PassTeam::Waits;
                    }
                }
            }
            note(memory, vault_name, Some(error.rule()));
            PassTeam::Waits
        }
        Err(RosterLoadError::Unreachable(_) | RosterLoadError::Unreadable { .. }) => {
            note_outage(memory, vault_name, true);
            PassTeam::Waits
        }
    }
}

fn note(memory: &JobAttentionMemory, vault_name: &str, problem: Option<TeamRule>) {
    match memory.note_team(vault_name, problem) {
        Some(TeamLogEntry::Started(rule)) => {
            tracing::warn!(
                vault = vault_name,
                code = "workflow/team-invalid",
                rule = rule.as_str(),
                "the vault's team cannot be used for jobs, so scheduled jobs in this vault are paused"
            );
        }
        Some(TeamLogEntry::Cleared) => {
            tracing::info!(
                vault = vault_name,
                code = "workflow/team-invalid",
                "the vault's team is valid again, so its scheduled jobs resume"
            );
        }
        None => {}
    }
}

/// Logs once when the vault's team becomes unreadable, which is an outage and neither a team error nor a defect in a
/// job, and once when it can be read again. A pass that finds the same thing as the last says nothing.
fn note_outage(memory: &JobAttentionMemory, vault_name: &str, down: bool) {
    match memory.note_team_outage(vault_name, down) {
        Some(OutageLogEntry::Started) => {
            tracing::warn!(
                vault = vault_name,
                code = "workflow/server-unavailable",
                "the vault's team could not be read, so its scheduled jobs wait until it can be"
            );
        }
        Some(OutageLogEntry::Cleared) => {
            tracing::info!(
                vault = vault_name,
                code = "workflow/server-unavailable",
                "the vault's team can be read again, so its scheduled jobs resume"
            );
        }
        None => {}
    }
}

/// Whether any workflow of the vault has a saved job.
///
/// # Errors
///
/// Returns the call's failure when the vault's workflows or a workflow's jobs cannot be listed, since an unknown answer
/// is not a "no".
pub(crate) async fn vault_has_job_files(client: &McpClient, vault_name: &str) -> Result<bool, McpCallError> {
    for package in crate::job_discovery::read_package_folders(client, vault_name).await? {
        for folder in package.folders() {
            if !list_job_files(client, vault_name, folder).await?.is_empty() {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

#[cfg(test)]
#[path = "job_team_gate_test.rs"]
mod tests;
