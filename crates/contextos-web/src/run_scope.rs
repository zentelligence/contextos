//! What a vault-registered job's run may reach.
//!
//! A run is confined to its own vault and to the calls its workflow permits. The checks run in a wrapper around
//! the host's caller, so a refused call is never sent.

use std::sync::Arc;

use contextos_workflows::call_rules::CallRules;
use contextos_workflows::{McpCaller, ScopedMcpCaller, VaultScope};

/// The reach of one run of a job registered in `home_vault`.
pub(crate) struct RunScope<'a> {
    pub primary_server: &'a str,
    pub home_vault: &'a str,
    /// Every vault the primary server reports.
    pub reported_vaults: Vec<String>,
    /// What the workflow's `[mcp_servers]` and `[mcp_tools]` lists permit.
    pub calls: CallRules,
}

impl RunScope<'_> {
    /// Wraps `inner` so only calls within this scope reach it.
    pub(crate) fn apply(self, inner: Arc<dyn McpCaller>) -> Arc<dyn McpCaller> {
        let vaults = VaultScope::new(
            self.primary_server.to_owned(),
            self.reported_vaults,
            vec![self.home_vault.to_owned()],
        );
        Arc::new(ScopedMcpCaller::new(inner, self.calls).with_vault_scope(vaults))
    }
}

#[cfg(test)]
#[path = "run_scope_test.rs"]
mod tests;
