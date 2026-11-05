//! Why a report of a workflow's releases could not be made, each with a stable code.

use contextos_packages::PackagesError;
use thiserror::Error;

use crate::mcp_client::McpCallError;
use crate::shared_job_authoring::AuthoringError;
use crate::vault_package_store::VaultStoreError;

/// A refusal or failure of `workflows releases`. Nothing is changed in any case: the command only reads.
#[derive(Debug, Error)]
pub enum WorkflowReleasesError {
    /// A package or workflow name that cannot name one.
    #[error("{value:?} is not a usable {what} name: {reason}")]
    Name {
        what: &'static str,
        value: String,
        reason: String,
    },
    /// The web configuration cannot be used.
    #[error("the web configuration cannot be used ({reason}), so nothing was read; fix or remove it")]
    ConfigInvalid { reason: String },
    /// No configured server could be connected, so the vault cannot be reached.
    #[error("{reason}, so the vault's releases cannot be read; check the configured server and try again")]
    NotConnected { reason: &'static str },
    /// The primary server could not be asked.
    #[error("the vault's server could not be asked: {0}")]
    Unreachable(#[from] McpCallError),
    /// The vault's package store cannot be located.
    #[error(transparent)]
    Store(#[from] VaultStoreError),
    /// The package store failed.
    #[error(transparent)]
    Packages(#[from] PackagesError),
    /// The store's jobs could not be listed.
    #[error(transparent)]
    Jobs(#[from] AuthoringError),
    /// The job state could not be read.
    #[error("the job state could not be read: {reason}")]
    State { reason: String },
    /// The host's data folder could not be determined, or a blocking read did not finish.
    #[error("{reason}")]
    Host { reason: &'static str },
}

impl WorkflowReleasesError {
    /// The stable machine-readable code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Name { .. } => contextos_packages::codes::INVALID_NAME,
            Self::ConfigInvalid { .. } => "workflow/config-invalid",
            Self::NotConnected { .. } | Self::Unreachable(_) => "workflow/server-unavailable",
            Self::Store(error) => error.code(),
            Self::Packages(error) => error.code(),
            Self::Jobs(error) => error.code(),
            Self::State { .. } | Self::Host { .. } => "workflow/state-unavailable",
        }
    }
}

#[cfg(test)]
#[path = "workflow_releases_error_test.rs"]
mod tests;
