//! Limits which MCP calls a workflow run may make.
//!
//! [`ScopedMcpCaller`] wraps the host's [`McpCaller`] and checks every call a script issues before it is passed on,
//! so a refused call is never sent. It depends only on the [`McpCaller`] trait, keeping this crate free of any MCP
//! transport.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{Map, Value};

use crate::call_rules::CallRules;
use crate::caller::{McpCallFailure, McpCaller, WorkflowFailureKind};

/// The vaults a run may name in a call to the primary server.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultScope {
    primary_server: String,
    reported: Vec<String>,
    permitted: Vec<String>,
}

impl VaultScope {
    /// `reported` is every vault the primary server reports, and `permitted` the ones this run may name.
    #[must_use]
    pub fn new(primary_server: String, reported: Vec<String>, permitted: Vec<String>) -> Self {
        Self {
            primary_server,
            reported,
            permitted,
        }
    }
}

impl VaultScope {
    /// The reported vault that some string in `arguments` names although this run may not, if any.
    ///
    /// A string names a vault the way the MCP server resolves one: the text before its first `://`, matched against
    /// the vault names without regard to ASCII case. Only string values are examined, at any depth in arrays and
    /// objects, and never object keys.
    fn out_of_scope_vault(&self, arguments: &Map<String, Value>) -> Option<&str> {
        arguments.values().find_map(|value| self.out_of_scope_in(value))
    }

    fn out_of_scope_in(&self, value: &Value) -> Option<&str> {
        match value {
            Value::String(text) => self.out_of_scope_named_by(text),
            Value::Array(items) => items.iter().find_map(|item| self.out_of_scope_in(item)),
            Value::Object(map) => map.values().find_map(|item| self.out_of_scope_in(item)),
            Value::Null | Value::Bool(_) | Value::Number(_) => None,
        }
    }

    fn out_of_scope_named_by(&self, text: &str) -> Option<&str> {
        let (candidate, _) = text.split_once("://")?;
        let reported = self.reported.iter().find(|name| name.eq_ignore_ascii_case(candidate))?;
        let permitted = self.permitted.iter().any(|name| name.eq_ignore_ascii_case(candidate));
        (!permitted).then_some(reported.as_str())
    }
}

/// An [`McpCaller`] that passes on only the calls its [`CallRules`] and [`VaultScope`] permit.
pub struct ScopedMcpCaller {
    inner: Arc<dyn McpCaller>,
    rules: CallRules,
    vaults: Option<VaultScope>,
}

impl ScopedMcpCaller {
    /// Wraps `inner` so that only calls within `rules` reach it.
    #[must_use]
    pub fn new(inner: Arc<dyn McpCaller>, rules: CallRules) -> Self {
        Self {
            inner,
            rules,
            vaults: None,
        }
    }

    /// Also limits which vaults a call to the scope's primary server may name.
    #[must_use]
    pub fn with_vault_scope(mut self, vaults: VaultScope) -> Self {
        self.vaults = Some(vaults);
        self
    }
}

impl McpCaller for ScopedMcpCaller {
    fn call<'a>(
        &'a self,
        server: &'a str,
        tool: &'a str,
        arguments: Map<String, Value>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, McpCallFailure>> + Send + 'a>> {
        if !self.rules.permits(server, tool) {
            let message = self.rules.refusal_for(server, tool);
            return Box::pin(async move {
                Err(McpCallFailure {
                    kind: WorkflowFailureKind::EffectDenied,
                    server: server.to_owned(),
                    tool: tool.to_owned(),
                    message,
                })
            });
        }
        if let Some(scope) = &self.vaults
            && server == scope.primary_server
            && let Some(vault) = scope.out_of_scope_vault(&arguments)
        {
            let message = format!("the call names vault {vault:?}, which is outside this run's vault scope");
            return Box::pin(async move {
                Err(McpCallFailure {
                    kind: WorkflowFailureKind::VaultOutOfScope,
                    server: server.to_owned(),
                    tool: tool.to_owned(),
                    message,
                })
            });
        }
        self.inner.call(server, tool, arguments)
    }
}

#[cfg(test)]
#[path = "scoped_caller_test.rs"]
mod tests;
