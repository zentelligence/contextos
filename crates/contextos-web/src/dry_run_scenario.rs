//! Parses a dry-run scenario file (`registry/workflows/{{package}}/{{workflow}}/tests/{{scenario}}.toml`)
//! and matches a workflow script's own `call_mcp` invocations against its
//! declared, ordered call sequence.
//!
//! A scenario is authored entirely outside the workflow's own script: the
//! production `.rhai` entry never references dry-run concerns at all, so
//! the identical script text runs unmodified whether dry-run mocking is
//! engaged or not - only which [`crate::dry_run_caller::DryRunMcpCaller`]
//! (or the real one) backs its `call_mcp` calls differs.

use std::sync::Mutex;

use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

/// One focused error enum for scenario parsing and matching failures
/// (`rust-quality.md`).
#[derive(Debug, Error, PartialEq)]
pub enum ScenarioError {
    #[error("the scenario file could not be parsed: {0}")]
    Malformed(String),
    #[error("a scenario file must declare at least one [[call]]")]
    Empty,
    #[error("call {position} ({server:?}/{tool:?}) declares kind = \"valid\"/\"invalid\" but no response")]
    MissingResponse {
        position: usize,
        server: String,
        tool: String,
    },
    #[error("call {position} ({server:?}/{tool:?}) declares kind = \"error\" but no error message")]
    MissingError {
        position: usize,
        server: String,
        tool: String,
    },
    #[error("call {position} ({server:?}/{tool:?}) declares kind = \"error\" but also a response; only one is valid")]
    ConflictingResponseAndError {
        position: usize,
        server: String,
        tool: String,
    },
    #[error(
        "expected call {position} to be {expected_server:?}/{expected_tool:?} per this scenario, the script called {actual_server:?}/{actual_tool:?} instead"
    )]
    Mismatch {
        position: usize,
        expected_server: String,
        expected_tool: String,
        actual_server: String,
        actual_tool: String,
    },
    #[error("the script made a call_mcp call beyond this scenario's own {position} declared call(s)")]
    Exhausted { position: usize },
}

/// A dry-run scenario's own, fully parsed and validated call sequence.
#[derive(Clone, Debug, PartialEq)]
pub struct Scenario {
    pub calls: Vec<ScenarioCall>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScenarioCall {
    pub server: String,
    pub tool: String,
    pub response: ScenarioResponse,
}

/// The three mocked outcomes one `[[call]]` entry can declare.
#[derive(Clone, Debug, PartialEq)]
pub enum ScenarioResponse {
    /// Checked against the real tool's own declared `outputSchema` (when
    /// present) before being returned: a mismatched "valid" fixture is
    /// itself a dry-run failure.
    Valid(Value),
    /// Returned exactly as declared, deliberately never checked against
    /// `outputSchema` - authored to exercise a script's own handling of
    /// malformed or unexpected data.
    Invalid(Value),
    /// The call fails outright with this message; no schema fetch is
    /// attempted at all.
    Error(String),
}

impl Scenario {
    /// # Errors
    ///
    /// Returns [`ScenarioError::Malformed`] when `source` is not valid
    /// TOML matching this schema, [`ScenarioError::Empty`] when it
    /// declares zero `[[call]]` entries, or one of
    /// [`ScenarioError::MissingResponse`]/[`ScenarioError::MissingError`]/
    /// [`ScenarioError::ConflictingResponseAndError`] naming the first
    /// malformed call.
    pub fn parse(source: &str) -> Result<Self, ScenarioError> {
        let raw: ScenarioRaw = toml::from_str(source).map_err(|error| ScenarioError::Malformed(error.to_string()))?;
        if raw.call.is_empty() {
            return Err(ScenarioError::Empty);
        }
        let calls = raw
            .call
            .into_iter()
            .enumerate()
            .map(|(position, call)| ScenarioCall::try_from((position, call)))
            .collect::<Result<_, _>>()?;
        Ok(Self { calls })
    }
}

impl TryFrom<(usize, ScenarioCallRaw)> for ScenarioCall {
    type Error = ScenarioError;

    fn try_from((position, raw): (usize, ScenarioCallRaw)) -> Result<Self, ScenarioError> {
        let response = match raw.kind {
            ResponseKindRaw::Valid | ResponseKindRaw::Invalid => {
                let Some(response) = raw.response else {
                    return Err(ScenarioError::MissingResponse {
                        position,
                        server: raw.server,
                        tool: raw.tool,
                    });
                };
                if raw.error.is_some() {
                    return Err(ScenarioError::ConflictingResponseAndError {
                        position,
                        server: raw.server,
                        tool: raw.tool,
                    });
                }
                match raw.kind {
                    ResponseKindRaw::Valid => ScenarioResponse::Valid(response),
                    ResponseKindRaw::Invalid => ScenarioResponse::Invalid(response),
                    ResponseKindRaw::Error => unreachable!("matched above"),
                }
            }
            ResponseKindRaw::Error => {
                if raw.response.is_some() {
                    return Err(ScenarioError::ConflictingResponseAndError {
                        position,
                        server: raw.server,
                        tool: raw.tool,
                    });
                }
                let Some(message) = raw.error else {
                    return Err(ScenarioError::MissingError {
                        position,
                        server: raw.server,
                        tool: raw.tool,
                    });
                };
                ScenarioResponse::Error(message)
            }
        };
        Ok(Self {
            server: raw.server,
            tool: raw.tool,
            response,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioRaw {
    #[serde(default)]
    call: Vec<ScenarioCallRaw>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioCallRaw {
    server: String,
    tool: String,
    kind: ResponseKindRaw,
    #[serde(default)]
    response: Option<Value>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ResponseKindRaw {
    Valid,
    Invalid,
    Error,
}

/// Matches a script's own `call_mcp` invocations, in the order they
/// happen, against a [`Scenario`]'s own declared call sequence. Interior
/// mutability (a plain `Mutex`, never contended in practice since Rhai
/// evaluation is synchronous and a script's own calls happen strictly one
/// at a time) so this can be held behind the same `&self` shape
/// [`contextos_workflows::caller::McpCaller`] requires.
pub struct ScenarioMatcher {
    scenario: Scenario,
    next_position: Mutex<usize>,
}

impl ScenarioMatcher {
    #[must_use]
    pub fn new(scenario: Scenario) -> Self {
        Self {
            scenario,
            next_position: Mutex::new(0),
        }
    }

    /// How many declared calls the script has not made. A dry run that ends with any left over has not exercised the whole
    /// scenario.
    #[must_use]
    pub fn unconsumed(&self) -> usize {
        let position = *self
            .next_position
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.scenario.calls.len().saturating_sub(position)
    }

    /// Consumes and returns the next declared call's own response,
    /// verifying it matches `server`/`tool`.
    ///
    /// # Errors
    ///
    /// Returns [`ScenarioError::Mismatch`] when the next declared call
    /// names a different `server`/`tool`, or [`ScenarioError::Exhausted`]
    /// when every declared call has already been consumed.
    pub fn next_call(&self, server: &str, tool: &str) -> Result<ScenarioResponse, ScenarioError> {
        let mut position_guard = self
            .next_position
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let position = *position_guard;
        let Some(expected) = self.scenario.calls.get(position) else {
            return Err(ScenarioError::Exhausted { position });
        };
        if expected.server != server || expected.tool != tool {
            return Err(ScenarioError::Mismatch {
                position,
                expected_server: expected.server.clone(),
                expected_tool: expected.tool.clone(),
                actual_server: server.to_owned(),
                actual_tool: tool.to_owned(),
            });
        }
        *position_guard += 1;
        Ok(expected.response.clone())
    }
}

#[cfg(test)]
#[path = "dry_run_scenario_test.rs"]
mod tests;
