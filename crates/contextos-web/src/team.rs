//! `.contextos/team.toml`: the Command Centre's definitive source for every human and AI agent who can be assigned
//! a task or, for agents specifically, tracked on the Agents screen, read through an ordinary MCP tool call. This
//! crate never writes this file: the `state` field is written exclusively by `contextos-mcp`'s own
//! `agent_state_change` tool, elsewhere entirely from this web server.
//!
//! Renamed from `.contextos/agents.toml`: the Tasks `assignee` field used to hardcode a single literal operator
//! name with no configuration behind it, since the old file only ever described AI agents. `type: agent | human`
//! per entry lets a real, vault-configured human join the identical mechanism, rather than a second parallel file
//! or another hardcoded default.
//!
//! Deliberately carries no `entity` field: a team member works across every entity, not scoped to one. The Agents
//! screen renders with no entity switcher at all.

use contextos_core::{TeamRoster, TeamRosterError, TeamRule};
use serde::Deserialize;
use thiserror::Error;

use crate::mcp_client::{McpCallError, McpClient};
use crate::vault_content::{AttachedRead, read_attached};

/// The stable code of a refusal because the file does not exist.
const NOT_FOUND: &str = "path/not-found";

const TEAM_TOML_PATH: &str = ".contextos/team.toml";

/// A team member's working/blocked/idle/awaiting-input state. Absent in `.contextos/team.toml` means [`Self::Idle`]:
/// a member who has never transitioned through `agent_state_change` is not an error state, just one that has not
/// started anything yet. Applies identically to an agent or a human entry.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MemberState {
    Working,
    Blocked,
    #[default]
    Idle,
    AwaitingInput,
}

impl MemberState {
    /// The lowercase, kebab-case label this state renders as: identical to
    /// its own TOML/JSON spelling, so a screenshot of the rendered page and
    /// a raw `.contextos/team.toml` read never disagree on vocabulary.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Idle => "idle",
            Self::AwaitingInput => "awaiting-input",
        }
    }
}

/// Whether a `team.toml` entry is an AI agent or a human team member. Defaults to [`Self::Agent`]: an operator who
/// renames `agents.toml` to `team.toml` without editing its content keeps identical behaviour to before the rename.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TeamMemberKind {
    #[default]
    Agent,
    Human,
}

/// One configured team member, as offered on the Command Centre Agents screen (`kind: Agent` entries only) and the
/// Tasks `assignee` field (every entry, either kind).
#[derive(Clone, Debug, Deserialize)]
pub struct TeamMember {
    pub slug: String,
    pub name: String,
    #[serde(default, rename = "type")]
    pub kind: TeamMemberKind,
    #[serde(default)]
    pub domain: Option<String>,
    /// Which group of the deployment this member belongs to: free text, not a closed enum (a deployment may
    /// customise it). The plugins seed `"general"` (the Context OS plugin's vault-wide agents, which work across
    /// every kind of entity), `"business"` (Business OS), or `"personal"` (Personal OS), and an operator may place
    /// agents of their own in any of them. Distinct from `domain` (a finer-grained business domain or personal
    /// life area, e.g. `"Finance"` or `"Health"`): `team` is the coarser grouping the sidebar's own quick-access
    /// links filter on. Coincidentally named the same as this module/file - a per-entry grouping
    /// field, not the same concept as "the team this file configures".
    #[serde(default)]
    pub team: Option<String>,
    #[serde(default)]
    pub chief_of_staff: bool,
    #[serde(default)]
    pub state: MemberState,
}

impl TeamMember {
    /// Whether this member is in `state`; an absent `state` matches every member. Exact, matching the
    /// [`MemberState::label`] vocabulary the Agents screen's own filter links carry.
    #[must_use]
    pub fn matches_state(&self, state: Option<&str>) -> bool {
        state.is_none_or(|state| self.state.label() == state)
    }

    /// Whether this member belongs to `team`, case-insensitively: a deployment's `.contextos/team.toml` may spell
    /// it `"Business"`, `"business"`, or any other casing, and the sidebar's own links always pass lowercase. An
    /// absent `team` matches every member; a member with no `team` of its own matches no named team.
    #[must_use]
    pub fn matches_team(&self, team: Option<&str>) -> bool {
        team.is_none_or(|team| {
            self.team
                .as_deref()
                .is_some_and(|member_team| member_team.eq_ignore_ascii_case(team))
        })
    }
}

/// The agents (never the human members) that `team` and `state` select, in configured order: the one definition of
/// what the Agents screen lists for a filter, shared with the counts the sidebar shows beside each filter link.
pub fn matching_agents<'a>(
    members: &'a [TeamMember],
    team: Option<&'a str>,
    state: Option<&'a str>,
) -> impl Iterator<Item = &'a TeamMember> {
    members
        .iter()
        .filter(|member| member.kind == TeamMemberKind::Agent)
        .filter(move |member| member.matches_state(state))
        .filter(move |member| member.matches_team(team))
}

#[derive(Debug, Default, Deserialize)]
struct TeamFile {
    #[serde(default)]
    member: Vec<TeamMember>,
}

/// Loads `vault_name`'s configured team. Never fails outright: an absent
/// file, an unreachable MCP session, or malformed TOML all degrade to an
/// empty list, not an error.
pub async fn load_team(client: &McpClient, vault_name: &str) -> Vec<TeamMember> {
    try_load_team(client, vault_name).await.unwrap_or_default()
}

/// [`load_team`], but telling a team that could not be read apart from one that is empty: `None` when the MCP
/// session is unreachable or `team.toml` is malformed, an empty list when the file is simply absent. A count
/// built from this shows no badge for an unreadable team instead of a false zero.
pub async fn try_load_team(client: &McpClient, vault_name: &str) -> Option<Vec<TeamMember>> {
    let read = match read_attached(client, &format!("{vault_name}://{TEAM_TOML_PATH}"))
        .await
        .ok()?
    {
        AttachedRead::Refused { .. } => return Some(Vec::new()),
        AttachedRead::NotText => return None,
        AttachedRead::Text(read) => read.text,
    };
    toml::from_str::<TeamFile>(&read).map(|file| file.member).ok()
}

/// Why a vault's team could not be used for its jobs. Either way the vault's job pass waits: a team that cannot be read
/// is an outage, and one that breaks a rule is a configuration error, and neither is a defect in any job.
#[derive(Debug, Error)]
pub enum RosterLoadError {
    /// The server could not be reached, so the team is unknown.
    #[error(transparent)]
    Unreachable(#[from] McpCallError),
    /// The server refused to read the file for a reason other than its being absent (a permission or a size limit, for
    /// example), so the team is unknown. An outage like [`Self::Unreachable`], not a configuration error: the pass is
    /// skipped and nothing is recorded against any job.
    #[error("the vault's team file could not be read")]
    Unreadable { code: Option<String> },
    /// The file is absent, is not text, does not parse, or breaks a rule, and the error names which.
    #[error("workflow/team-invalid: the vault's team cannot be used for jobs")]
    Invalid(TeamRosterError),
}

impl TryFrom<AttachedRead> for TeamRoster {
    type Error = RosterLoadError;

    fn try_from(read: AttachedRead) -> Result<Self, RosterLoadError> {
        let invalid = |rule: TeamRule| RosterLoadError::Invalid(TeamRosterError::from(rule));
        match read {
            AttachedRead::Text(text) => {
                let file: contextos_core::TeamFile =
                    toml::from_str(&text.text).map_err(|_| invalid(TeamRule::FileUnparsable))?;
                TeamRoster::try_from(file).map_err(RosterLoadError::Invalid)
            }
            AttachedRead::NotText => Err(invalid(TeamRule::FileNotText)),
            AttachedRead::Refused { code } if code.as_deref() == Some(NOT_FOUND) => Err(invalid(TeamRule::FileAbsent)),
            AttachedRead::Refused { code } => Err(RosterLoadError::Unreadable { code }),
        }
    }
}

/// Loads `vault_name`'s team strictly, as a job depends on it: an absent file counts as invalid, because a vault with
/// jobs needs a chief of staff. [`load_team`] stays lenient for the screens.
///
/// # Errors
///
/// Returns [`RosterLoadError::Unreachable`] when the server cannot be reached, [`RosterLoadError::Unreadable`] when it
/// refuses to read the file for a reason other than its absence, and [`RosterLoadError::Invalid`] when the file cannot
/// be used.
pub async fn load_roster(client: &McpClient, vault_name: &str) -> Result<TeamRoster, RosterLoadError> {
    let read = read_attached(client, &format!("{vault_name}://{TEAM_TOML_PATH}")).await?;
    TeamRoster::try_from(read)
}

#[cfg(test)]
#[path = "team_test.rs"]
mod tests;
