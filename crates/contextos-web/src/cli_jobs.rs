//! The command-line side of the operator operation: what `contextos-web jobs validate`, `schedule`, `suspend`, `resume`,
//! `retire`, `reactivate`, and `delete` do once their arguments are parsed.
//!
//! Every command builds the store for its kind of job and calls [`perform_retaining`], which holds the rules.
//! This module turns what came back into the text a terminal shows, and into an error, with its `workflow/` code in the
//! message, when the request was refused or a validation failed, so the process exits unsuccessfully.

use std::fmt::Write as _;
use std::path::Path;

use contextos_workflows::{Applied, JobLifecycle};
use serde::Serialize;
use thiserror::Error;
use time::OffsetDateTime;

use crate::enabling_report::EnablingReport;
use crate::job_operation::{OperationError, OperationOutcome, OperatorRequest, Performed, perform_retaining};
use crate::job_operation_shared::SharedJobStore;
use crate::job_operation_vault::VaultJobStore;
use crate::mcp_client::McpClientSet;
use crate::shared_claim::SharedJobKey;
use crate::shared_state::SharedStateStore;
use crate::shared_validation::SharedValidationHost;
use crate::store_retention::RetentionWarning;

/// What a command came to: the text a terminal shows and, for an enabling, what it did beyond the job.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Operated {
    /// The sentence a terminal shows.
    #[serde(rename = "message")]
    pub text: String,
    /// What an enabling retired and moved, when the command enabled a job.
    pub enabling: Option<EnablingReport>,
    /// Retention could not finish after the command. The command was carried out all the same.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<RetentionWarning>,
}

#[derive(Debug, Error)]
pub enum CliJobsError {
    #[error(transparent)]
    Operation(#[from] OperationError),
    /// A validation the operator asked for did not pass. Its text lists every failure.
    #[error("{0}")]
    ValidationFailed(String),
    #[error("the primary MCP server {0:?} is not connected, and a vault job's state is reached through it")]
    NoPrimaryServer(String),
}

/// Whether a request needs the MCP servers `web.toml` configures, because it validates. The rest read and write state
/// alone, so a shared job can be suspended or retired on a host whose servers are down or not configured at all.
#[must_use]
pub const fn needs_servers(request: &OperatorRequest) -> bool {
    matches!(request, OperatorRequest::Validate | OperatorRequest::Resume)
}

/// Where a vault job's state and files are reached.
pub struct VaultScope<'a> {
    pub clients: &'a McpClientSet,
    pub primary_server: &'a str,
    /// Every server `web.toml` configures.
    pub known_servers: &'a [String],
    pub vault: &'a str,
    /// Where the host keeps run claims, when it has a claims directory.
    pub claims_root: Option<&'a Path>,
}

/// Carries out `request` for the vault job `package`/`workflow`/`job`, and says what came of it.
///
/// # Errors
///
/// Returns [`CliJobsError::Operation`] when the request was refused or could not be carried out,
/// [`CliJobsError::ValidationFailed`] when a requested validation did not pass, and [`CliJobsError::NoPrimaryServer`]
/// when the server a vault's state is reached through is not connected.
pub async fn operate_vault(
    scope: &VaultScope<'_>,
    package: &str,
    workflow: &str,
    job: &str,
    request: OperatorRequest,
    now: OffsetDateTime,
) -> Result<Operated, CliJobsError> {
    let client = scope
        .clients
        .get(scope.primary_server)
        .ok_or_else(|| CliJobsError::NoPrimaryServer(scope.primary_server.to_owned()))?;
    let store = VaultJobStore {
        client: &client,
        vault_name: scope.vault,
        known_mcp_servers: scope.known_servers,
        package,
        workflow_slug: workflow,
        job_slug: job,
        claims_root: scope.claims_root,
    };
    let performed = perform_retaining(&store, request, now).await?;
    operated(
        performed,
        &format!("job {package}/{workflow}/{job} of vault {}", scope.vault),
    )
}

/// Where a shared job's state and files are, and what validating it needs.
pub struct SharedScope<'a> {
    pub state: &'a SharedStateStore,
    pub jobs_root: &'a Path,
    pub validation: &'a SharedValidationHost<'a>,
    /// Where the host keeps run claims.
    pub claims_root: &'a Path,
}

/// Carries out `request` for the shared job `key`, and says what came of it.
///
/// # Errors
///
/// Returns [`CliJobsError::Operation`] when the request was refused or could not be carried out, and
/// [`CliJobsError::ValidationFailed`] when a requested validation did not pass.
pub async fn operate_shared(
    scope: &SharedScope<'_>,
    key: &SharedJobKey,
    request: OperatorRequest,
    now: OffsetDateTime,
) -> Result<Operated, CliJobsError> {
    let store = SharedJobStore {
        state: scope.state,
        jobs_root: scope.jobs_root,
        validation: scope.validation,
        key,
        claims_root: scope.claims_root,
    };
    let performed = perform_retaining(&store, request, now).await?;
    operated(
        performed,
        &format!("shared job {}/{}/{}", key.package, key.workflow, key.job),
    )
}

/// What a command came to, from the outcome of its request about `label`.
fn operated(performed: Performed, label: &str) -> Result<Operated, CliJobsError> {
    let enabling = match &performed.outcome {
        OperationOutcome::Enabled(report) => Some(report.as_ref().clone()),
        OperationOutcome::Validated { .. } | OperationOutcome::Changed(_) => None,
    };
    let mut text = describe(performed.outcome, label)?;
    for warning in &performed.warnings {
        let _ = write!(text, "\nwarning {warning}");
    }
    Ok(Operated {
        text,
        enabling,
        warnings: performed.warnings,
    })
}

/// The text a terminal shows for `outcome` of a request about `label`, or the error a failed validation is.
fn describe(outcome: OperationOutcome, label: &str) -> Result<String, CliJobsError> {
    match outcome {
        OperationOutcome::Validated { result, lifecycle } if result.passed => {
            let mut text = format!("Validation passed for {label}. It is {lifecycle}.");
            if lifecycle == JobLifecycle::Validated {
                text.push_str(" Dry-run it next with `contextos-web jobs dry-run`, which registers it once the dry run completes.");
            }
            Ok(text)
        }
        OperationOutcome::Validated { result, .. } => {
            let mut text = format!("workflow/validation-failed: validation failed for {label}");
            if !result.unverifiable.is_empty() {
                text.push_str("; some checks were unverifiable, so the job does not pass");
            }
            for failure in &result.failures {
                let _ = write!(text, "\n  {}: {}: {}", failure.code, failure.artefact, failure.message);
            }
            for check in &result.unverifiable {
                let _ = write!(text, "\n  {check} could not be checked");
            }
            Err(CliJobsError::ValidationFailed(text))
        }
        OperationOutcome::Changed(Applied::To(state)) => Ok(format!("{label} is now {state}.")),
        OperationOutcome::Enabled(report) => Ok(format!("{label} is now enabled. {report}")),
        OperationOutcome::Changed(Applied::Removed) => Ok(format!(
            "{label} was deleted from the job state. Its files, if any are left, are untouched."
        )),
    }
}

#[cfg(test)]
#[path = "cli_jobs_test.rs"]
mod tests;
