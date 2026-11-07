//! A dry-run `McpCaller` implementation: backs a workflow script's own `call_mcp`
//! calls during `contextos-web jobs run --dry-run`, engaging a
//! [`ScenarioMatcher`](crate::dry_run_scenario::ScenarioMatcher) instead
//! of a real MCP tool invocation - no real call, mutating or otherwise,
//! ever reaches the network in dry-run mode.
//!
//! Every call is still validated against the *real* target tool's own
//! declared schema, fetched live via `tools/list` against the actual
//! configured session (`mcp_client::McpClient::list_all_tools`): this is
//! exactly the development value dry-run exists to provide, and is only
//! possible because `contextos-web` already holds a live connection - a
//! script author editing in isolation has no other way to know a real,
//! current tool schema, least of all a third-party server's own.
//!
//! `contextos-workflows` itself needs no change for any of this: dry-run
//! is expressed entirely as which [`contextos_workflows::caller::McpCaller`]
//! implementation is supplied, invisible to the script and to the pure
//! execution crate.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use contextos_workflows::{McpCallFailure, McpCaller, WorkflowFailureKind};
use rmcp::model::Tool;
use serde_json::{Map, Value};
use tokio::sync::Mutex;

use crate::dry_run_scenario::{ScenarioError, ScenarioMatcher, ScenarioResponse};
use crate::mcp_client::McpClientSet;

/// Backs a dry-run job execution: every `call_mcp` the script makes is
/// checked against `matcher`'s own declared scenario and the real target
/// tool's own live schema, never against a real invocation.
pub struct DryRunMcpCaller {
    clients: Arc<McpClientSet>,
    matcher: Arc<ScenarioMatcher>,
    tool_cache: Mutex<HashMap<String, Vec<Tool>>>,
}

impl DryRunMcpCaller {
    #[must_use]
    pub fn new(clients: Arc<McpClientSet>, matcher: ScenarioMatcher) -> Self {
        Self::sharing(clients, Arc::new(matcher))
    }

    /// As [`DryRunMcpCaller::new`], over a matcher the caller keeps a handle to, so it can ask afterwards whether the whole
    /// scenario was used.
    #[must_use]
    pub fn sharing(clients: Arc<McpClientSet>, matcher: Arc<ScenarioMatcher>) -> Self {
        Self {
            clients,
            matcher,
            tool_cache: Mutex::new(HashMap::new()),
        }
    }

    async fn tool_definition(&self, server: &str, tool: &str) -> Result<Tool, McpCallFailure> {
        let mut cache = self.tool_cache.lock().await;
        if !cache.contains_key(server) {
            let Some(client) = self.clients.get(server) else {
                let message = if self.clients.is_configured(server) {
                    "MCP server is not connected; /server/settings/ shows why and connects it again"
                } else {
                    "MCP server is not configured"
                };
                return Err(McpCallFailure {
                    kind: WorkflowFailureKind::ServerUnavailable,
                    server: server.to_owned(),
                    tool: tool.to_owned(),
                    message: message.to_owned(),
                });
            };
            let tools = client.list_all_tools().await.map_err(|source| McpCallFailure {
                kind: WorkflowFailureKind::ServerUnavailable,
                server: server.to_owned(),
                tool: tool.to_owned(),
                message: source.to_string(),
            })?;
            cache.insert(server.to_owned(), tools);
        }
        cache
            .get(server)
            .and_then(|tools| tools.iter().find(|candidate| candidate.name == tool))
            .cloned()
            .ok_or_else(|| McpCallFailure {
                kind: WorkflowFailureKind::ToolError,
                server: server.to_owned(),
                tool: tool.to_owned(),
                message: format!("{server:?} advertises no tool named {tool:?}"),
            })
    }
}

impl McpCaller for DryRunMcpCaller {
    fn call<'a>(
        &'a self,
        server: &'a str,
        tool: &'a str,
        arguments: Map<String, Value>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, McpCallFailure>> + Send + 'a>> {
        Box::pin(async move {
            let response = self
                .matcher
                .next_call(server, tool)
                .map_err(|error| scenario_failure(server, tool, &error))?;

            if let ScenarioResponse::Error(message) = response {
                return Err(McpCallFailure {
                    kind: WorkflowFailureKind::ToolError,
                    server: server.to_owned(),
                    tool: tool.to_owned(),
                    message,
                });
            }

            let definition = self.tool_definition(server, tool).await?;
            let arguments_value = Value::Object(arguments);
            let input_schema = Value::Object(definition.input_schema.as_ref().clone());
            validate_against_schema(&arguments_value, &input_schema).map_err(|message| McpCallFailure {
                kind: WorkflowFailureKind::ToolError,
                server: server.to_owned(),
                tool: tool.to_owned(),
                message: format!(
                    "the script's own call arguments do not match {tool:?}'s declared input schema: {message}"
                ),
            })?;

            match response {
                ScenarioResponse::Valid(value) => {
                    if let Some(output_schema) = &definition.output_schema {
                        let output_schema_value = Value::Object(output_schema.as_ref().clone());
                        validate_against_schema(&value, &output_schema_value).map_err(|message| McpCallFailure {
                            kind: WorkflowFailureKind::ResultUnreadable,
                            server: server.to_owned(),
                            tool: tool.to_owned(),
                            message: format!(
                                "this scenario's own mocked response for {tool:?} does not match its declared output \
                                 schema: {message}"
                            ),
                        })?;
                    }
                    Ok(value)
                }
                ScenarioResponse::Invalid(value) => Ok(value),
                ScenarioResponse::Error(_) => unreachable!("handled above"),
            }
        })
    }
}

fn scenario_failure(server: &str, tool: &str, error: &ScenarioError) -> McpCallFailure {
    McpCallFailure {
        kind: WorkflowFailureKind::ToolError,
        server: server.to_owned(),
        tool: tool.to_owned(),
        message: error.to_string(),
    }
}

/// Validates `value` against `schema` (a JSON Schema document), returning
/// a human-readable message naming the first violation found.
fn validate_against_schema(value: &Value, schema: &Value) -> Result<(), String> {
    jsonschema::validate(schema, value).map_err(|error| error.to_string())
}

#[cfg(test)]
#[path = "dry_run_caller_test.rs"]
mod tests;
