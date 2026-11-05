//! Executes a resolved run of a workflow from the host package store.
//!
//! The run is confined to the calls its workflow's `[mcp_servers]` and `[mcp_tools]` lists permit and to the vaults its own vault
//! inputs name. It touches no vault to start, claim, or finish, so it works with zero vaults configured.

use std::sync::Arc;

use contextos_workflows::{
    CallJournal, Clock, JournalledCaller, LogLevel, McpCaller, RunFailure, RunLabel, RunResult, RunSettings,
    ScopedMcpCaller, ValidatedInputs, VaultScope, WorkflowFailureKind, run_workflow_declaring,
};

use tokio_util::sync::CancellationToken;

use crate::mcp_client::McpClientSet;
use crate::rendering::shell;
use crate::run_settings::RunPlan;
use crate::shared_run::SharedRun;

/// A shared run ready to execute.
pub struct SharedExecution {
    pub run: SharedRun,
    pub timeout_secs: u64,
    /// The minimum level the script's log messages are written at, already resolved from the host, workflow, and job.
    pub log_level: LogLevel,
    /// The run's identity in the messages written for it.
    pub label: RunLabel,
    /// The host's caller, reached only through the scope checks.
    pub caller: Arc<dyn McpCaller>,
    pub primary_server: String,
    /// Every vault the primary server reports, so a name outside the run's vault inputs can be refused.
    pub reported_vaults: Vec<String>,
    /// Cancelling it stops the run.
    pub cancel: CancellationToken,
    /// Where each call is reported as it ends, so a run that dies leaves the calls it completed. A
    /// one-off run has none.
    pub journal: Option<Arc<dyn CallJournal>>,
    /// The host's clock, whose zone the run's own times are shown in.
    pub clock: Arc<dyn Clock>,
}

/// Runs the workflow to completion, or until its timeout or cancellation, and reports how it ended with the outputs its
/// script set.
pub async fn execute_shared(execution: SharedExecution) -> RunResult {
    tracing::info!(
        package = %execution.run.identity.name,
        version = %execution.run.identity.version,
        digest = %execution.run.identity.digest.as_str(),
        inputs = ?execution.run.inputs.values().keys().collect::<Vec<_>>(),
        "shared workflow run starting"
    );
    for library in &execution.run.libraries {
        tracing::info!(
            library = %library.name,
            version = %library.version,
            digest = %library.digest.as_str(),
            "shared workflow run uses a library release"
        );
    }
    let vaults = VaultScope::new(
        execution.primary_server,
        execution.reported_vaults,
        execution.run.inputs.vault_names().to_vec(),
    );
    let mut caller: Arc<dyn McpCaller> =
        Arc::new(ScopedMcpCaller::new(execution.caller, execution.run.calls).with_vault_scope(vaults));
    // Real time for the run's own engine (its timeout follows it), shown in the host's zone.
    let clock: Arc<dyn Clock> = Arc::new(crate::vault_time::LocalisedSystemClock {
        zone: Arc::clone(&execution.clock),
    });
    // Outermost, so a call the scope refuses is journalled too.
    if let Some(journal) = execution.journal {
        caller = Arc::new(JournalledCaller::new(caller, journal, Arc::clone(&clock)));
    }
    let settings = RunSettings::from(RunPlan {
        timeout_secs: execution.timeout_secs,
        log_level: execution.log_level,
        label: execution.label,
        cancel: execution.cancel,
    });
    run_workflow_declaring(
        execution.run.script,
        execution.run.inputs,
        execution.run.outputs,
        caller,
        execution.run.files,
        clock,
        settings,
    )
    .await
}

/// Refuses a run whose `vault` inputs name a vault the primary server does not report, matching names the way the
/// server does, without regard to ASCII case.
///
/// # Errors
///
/// Returns a `workflow/vault-unavailable` failure naming the first vault that is not configured.
pub(crate) fn require_vaults_configured(inputs: &ValidatedInputs, reported: &[String]) -> Result<(), RunFailure> {
    match inputs
        .vault_names()
        .iter()
        .find(|name| !reported.iter().any(|known| known.eq_ignore_ascii_case(name)))
    {
        None => Ok(()),
        Some(missing) => Err(RunFailure {
            kind: Some(WorkflowFailureKind::VaultUnavailable),
            message: format!(
                "{}: the vault {missing:?} is not configured on the primary server",
                WorkflowFailureKind::VaultUnavailable.code()
            ),
        }),
    }
}

/// Every vault the primary server reports. Without a connected primary server there is nothing to report, and no
/// call to it can succeed either.
///
/// # Errors
///
/// Returns a `workflow/server-unavailable` failure when the primary server is connected but its vaults cannot be
/// listed, because a scope that cannot be checked is refused.
pub(crate) async fn reported_vaults(clients: &McpClientSet, primary_server: &str) -> Result<Vec<String>, RunFailure> {
    let Some(client) = clients.get(primary_server) else {
        return Ok(Vec::new());
    };
    shell::configured_vault_names(&client)
        .await
        .map_err(|source| RunFailure {
            kind: Some(WorkflowFailureKind::ServerUnavailable),
            message: format!(
                "{}: the configured vaults could not be listed to scope the run: {source}",
                WorkflowFailureKind::ServerUnavailable.code()
            ),
        })
}

#[cfg(test)]
#[path = "shared_runner_test.rs"]
mod tests;
