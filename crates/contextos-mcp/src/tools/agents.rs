//! `agent_state_change`: the narrow mechanism the Command Centre's own displayed agent state is built on. Updates
//! the named entry's own `state` field in `.contextos/team.toml` (renamed from `agents.toml`: the file now also
//! carries human team members, distinguished by a `type` field this tool never reads or writes, alongside every
//! agent) through the normal write pipeline (conflict-checked, never `force`), then records the transition as one
//! `member.state-changed` event under `memory/events/`, in the shape every event shares (see `contextos-events`).
//! The tool's own name and its `agent` parameter are unchanged by that rename: an MCP tool identity is a versioned
//! compatibility contract, and "which `team.toml` slug" is still exactly what `agent` names, whether that slug
//! belongs to an agent or a human.
//!
//! The team file update is an ordinary `Origin::Tool` mutation, so `memory/log/`, Git staging, and the text/search
//! indexes update through the existing [`contextos_core::routing`] policy with no bespoke logging path: this tool
//! never calls `vault_log_append` itself. The event note is an `Origin::Internal` write by the server's event writer,
//! so it reaches Git but neither the indexes nor the operation log.

use std::sync::Arc;

use contextos_core::{Clock, Origin, VaultPath, WriteMutation};
use contextos_events::{EventRecord, MemberStateChange};
use contextos_fs::{FileInfoRequest, FsError, ReadTextRequest};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{schemars, tool};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::events_writer::{EventWriteError, EventWriter, MAX_PATH_ATTEMPTS as MAX_EVENT_PATH_ATTEMPTS};
use crate::resource_support::fallible_output_schema_for;
use crate::server::ContextOsServer;
use crate::tool_error::{ToolError, ToolFailure, evaluate, execute};
use crate::tools::fs::child_path;

const TEAM_TOML_PATH: &str = ".contextos/team.toml";

#[rmcp::tool_router(router = agents_tool_router, vis = "pub(crate)")]
impl ContextOsServer {
    #[tool(
        name = "agent_state_change",
        description = "Record a team member's working/blocked/idle/awaiting-input state transition in .contextos/team.toml, with one member.state-changed event under memory/events/",
        output_schema = fallible_output_schema_for::<AgentStateChangeToolResult>()
    )]
    async fn agent_state_change(
        &self,
        Parameters(input): Parameters<AgentStateChangeInput>,
    ) -> Result<Json<AgentStateChangeToolResult>, ToolFailure> {
        let roots_for_resolve = Arc::clone(&self.roots);
        let (root, team_path, agent, state, reason) = evaluate(move || {
            let root = VaultPath::try_from_vault_selector(&roots_for_resolve, input.vault.as_deref().unwrap_or("."))?;
            let team_path = child_path(&root, &roots_for_resolve, TEAM_TOML_PATH)?;
            Ok((root, team_path, input.agent, input.state, input.reason))
        })
        .await?;
        let guards = self
            .writes
            .lock_roots(&[team_path.root_id()])
            .await
            .map_err(ToolFailure::from)?;
        let roots = Arc::clone(&self.roots);
        let filesystem = Arc::clone(&self.filesystem);
        let mutations = Arc::clone(&self.mutations);
        let clock = self.clock.clone();
        execute(move || {
            let _guards = guards;
            let read = filesystem
                .read_text(&ReadTextRequest {
                    path: team_path.clone(),
                    limit: None,
                })
                .map_err(|error| match error {
                    FsError::NotFound { .. } => ToolError::from(AgentStateError::TeamFileAbsent),
                    other => ToolError::from(other),
                })?;
            let mut file: TeamFile = toml::from_str(&read.content).map_err(AgentStateError::from)?;
            let at = clock.now();
            let outcome = apply_state_change(&mut file, &agent, state, at).map_err(ToolError::from)?;
            let serialised = toml::to_string(&file).map_err(AgentStateError::from)?;
            mutations.write_file(&WriteMutation {
                path: team_path,
                content: serialised.into(),
                expected_hash: Some(read.content_hash),
                force: false,
                origin: Origin::Tool("agent_state_change".to_owned()),
            })?;

            let has_definition = child_path(&root, &roots, &format!("registry/agents/{agent}.md"))
                .is_ok_and(|path| filesystem.file_info(&FileInfoRequest { path }).is_ok());
            let record = EventRecord::from((
                &MemberStateChange {
                    member: agent.clone(),
                    from: outcome.previous_state.map(|previous| previous.as_str().to_owned()),
                    to: state.as_str().to_owned(),
                    reason: reason.clone(),
                    has_definition,
                },
                at,
            ));
            let written = EventWriter {
                writes: mutations.as_ref(),
                roots: &roots,
                vault: &root,
            }
            .write(&record)
            .map_err(|error| match error {
                EventWriteError::PathsExhausted => ToolError::from(AgentStateError::EventPathExhausted),
                EventWriteError::Write(error) => error,
            })?;
            for warning in written.warnings {
                tracing::warn!(code = %warning.code, "a secondary service failed after a member event was written");
            }

            Ok(AgentStateChangeToolResult {
                agent,
                previous_state: outcome.previous_state,
                state,
                event_path: written.path,
            })
        })
        .await
    }
}

/// `.contextos/team.toml`'s full contents, as `agent_state_change` reads and writes it: the file's other fields
/// (`name`, `type`, `domain`, `team`, `chief_of_staff`) are operator-authored and preserved unchanged by every call
/// this tool makes. Every one of them is declared here even though this tool never reads or writes any of them
/// itself, precisely so that round-tripping through this struct (`toml::from_str` then `toml::to_string`) cannot
/// silently drop an operator-authored field a future entry happens to carry that an earlier version of this struct
/// did not yet know about.
#[derive(Debug, Deserialize, Serialize)]
struct TeamFile {
    #[serde(default)]
    member: Vec<TeamRecord>,
}

#[derive(Debug, Deserialize, Serialize)]
struct TeamRecord {
    slug: String,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "type")]
    kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    team: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    chief_of_staff: Option<bool>,
    /// Absent means "idle" (the Command Centre's own displayed default for a team member that has never
    /// transitioned): this tool is the sole writer of this field, but never invents one for an entry the operator
    /// has not yet exercised through it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    state: Option<AgentStateValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    state_updated_at: Option<String>,
}

/// One team member's working/blocked/idle/awaiting-input state: the one enum shared by `.contextos/team.toml`'s
/// own TOML representation, this tool's JSON input, and its JSON output, all under the identical kebab-case
/// spelling.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
enum AgentStateValue {
    Working,
    Blocked,
    Idle,
    AwaitingInput,
}

impl AgentStateValue {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Idle => "idle",
            Self::AwaitingInput => "awaiting-input",
        }
    }
}

/// [`apply_state_change`]'s own result: the matched agent's state
/// immediately before this call, `None` when it had never been set.
#[derive(Debug)]
struct StateChangeOutcome {
    previous_state: Option<AgentStateValue>,
}

/// Finds `agent_slug` in `file.member` and sets its `state`/`state_updated_at`, leaving every other field (and
/// every other team member) untouched. Pure: takes `at` rather than reading a clock itself, so it is
/// deterministically testable.
///
/// # Errors
///
/// Returns [`AgentStateError::AgentNotFound`] when no entry's `slug`
/// matches `agent_slug`.
fn apply_state_change(
    file: &mut TeamFile,
    agent_slug: &str,
    state: AgentStateValue,
    at: OffsetDateTime,
) -> Result<StateChangeOutcome, AgentStateError> {
    let entry = file
        .member
        .iter_mut()
        .find(|entry| entry.slug == agent_slug)
        .ok_or_else(|| AgentStateError::AgentNotFound {
            agent: agent_slug.to_owned(),
        })?;
    let previous_state = entry.state;
    entry.state = Some(state);
    entry.state_updated_at = Some(at.format(&Rfc3339).unwrap_or_else(|_| at.to_string()));
    Ok(StateChangeOutcome { previous_state })
}

/// Error codes (`code()`) are an unchanged, stable MCP contract even though this file was renamed: `agent/...`
/// still means "a `.contextos/team.toml` slug problem", not specifically an AI agent.
#[derive(Debug, Error)]
pub(crate) enum AgentStateError {
    #[error("no team members are configured at .contextos/team.toml")]
    TeamFileAbsent,
    #[error("no team member named '{agent}' is configured in .contextos/team.toml")]
    AgentNotFound { agent: String },
    #[error(".contextos/team.toml is not valid TOML: {0}")]
    Malformed(#[from] toml::de::Error),
    #[error("failed to serialise the updated team")]
    Serialise(#[from] toml::ser::Error),
    #[error("could not find an unused memory/events/ path for this agent after {MAX_EVENT_PATH_ATTEMPTS} attempts")]
    EventPathExhausted,
}

impl AgentStateError {
    pub(crate) const fn code(&self) -> &'static str {
        match self {
            Self::TeamFileAbsent => "agent/not-configured",
            Self::AgentNotFound { .. } => "agent/not-found",
            Self::Malformed(_) => "agent/malformed",
            Self::Serialise(_) => "agent/serialise",
            Self::EventPathExhausted => "agent/event-path-exhausted",
        }
    }

    pub(crate) const fn remediation(&self) -> &'static str {
        match self {
            Self::TeamFileAbsent => {
                "Create .contextos/team.toml with this team member's entry before recording a state change."
            }
            Self::AgentNotFound { .. } => {
                "Add this team member to .contextos/team.toml, or pass an existing member's slug."
            }
            Self::Malformed(_) => "Correct .contextos/team.toml's TOML syntax, and retry.",
            Self::Serialise(_) | Self::EventPathExhausted => {
                "Retry the operation and inspect server diagnostics if it fails again."
            }
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct AgentStateChangeToolResult {
    agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_state: Option<AgentStateValue>,
    state: AgentStateValue,
    event_path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct AgentStateChangeInput {
    /// A configured vault's bare name, or a path/
    /// `{name}://{relative-path}`; omit to use the sole configured vault.
    vault: Option<String>,
    /// The team member's own `slug`, as configured in `.contextos/team.toml`.
    agent: String,
    state: AgentStateValue,
    /// Free-text context for this transition, recorded in the `memory/events/` record; never written to
    /// `.contextos/team.toml` itself.
    #[serde(default)]
    reason: Option<String>,
}

#[cfg(test)]
#[path = "agents_test.rs"]
mod tests;
