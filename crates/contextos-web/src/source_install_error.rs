//! Why installing a vault's source into its package store was refused or failed, each with a stable code.

use std::fmt::Write as _;

use contextos_packages::PackagesError;
use contextos_workflows::{ValidationCode, ValidationFailure};
use thiserror::Error;

use crate::mcp_client::McpCallError;
use crate::vault_package_store::VaultStoreError;

/// A refusal or failure of an install from source. Nothing is installed when it is returned, except where a variant says
/// otherwise.
#[derive(Debug, Error)]
pub enum SourceInstallError {
    /// The source fails a check validation makes. Each failure carries its own validation code and the file it concerns.
    #[error("{}", describe(.failures))]
    Invalid { failures: Vec<ValidationFailure> },
    /// The primary server could not be asked, so nothing is known about the source.
    #[error("the vault's server could not be asked, so its source was not read: {0}")]
    Unreachable(#[from] McpCallError),
    /// The web configuration cannot be used, or names no server to reach the vault through.
    #[error("the web configuration cannot be used ({reason}), so nothing was done; fix or remove it")]
    ConfigInvalid { reason: String },
    /// No configured server could be connected, so the vault cannot be reached.
    #[error("{reason}, so the vault's source cannot be read; check the configured server and try again")]
    NotConnected { reason: &'static str },
    /// The vault's package store cannot be located.
    #[error(transparent)]
    Store(#[from] VaultStoreError),
    /// The package store refused the release or failed.
    #[error(transparent)]
    Packages(#[from] PackagesError),
    /// A library requirement is satisfied by no release installed now.
    #[error("{reason}; install a release of the library that satisfies it, or change the requirement")]
    DependencyUnsatisfied { reason: String },
    /// The name belongs to the other kind of package in this vault's store.
    #[error(
        "{name} is already a {held} in this vault's package store, so the {wanted} source registry/{wanted_folder}/{name} \
         cannot be installed under the same name; rename one of the two source folders"
    )]
    NameTaken {
        name: String,
        held: &'static str,
        wanted: &'static str,
        wanted_folder: &'static str,
    },
    /// An install at a version found the source no longer matches the release recorded under it.
    #[error("{reason}")]
    SourceChanged { reason: String },
    /// An install at a version names a release the ledger has never recorded.
    #[error("{package} {version} has never been installed from this vault, so there is nothing to restore")]
    NotInstalled { package: String, version: String },
    /// The job state could not be read or written.
    #[error("the vault's job state could not be {action}: {reason}")]
    State { action: &'static str, reason: String },
    /// Another writer changed the job state while the release was being recorded, and each repeat met the same.
    #[error(
        "the vault's job state kept changing, so the new release was not recorded as future; run the install again"
    )]
    StateChanged,
}

fn describe(failures: &[ValidationFailure]) -> String {
    let mut text = String::new();
    for failure in failures {
        if !text.is_empty() {
            text.push_str("; ");
        }
        let _ = write!(
            text,
            "{} {}: {}",
            failure.code.as_str(),
            failure.artefact,
            failure.message
        );
    }
    text
}

impl SourceInstallError {
    /// The stable machine-readable code. For [`Self::Invalid`] it is the first failure's validation code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid { failures } => failures
                .first()
                .map_or("validation/artefact-invalid", |failure| failure.code.as_str()),
            Self::Unreachable(_) | Self::NotConnected { .. } => "workflow/server-unavailable",
            Self::ConfigInvalid { .. } => "workflow/config-invalid",
            Self::Store(error) => error.code(),
            Self::Packages(error) => error.code(),
            Self::DependencyUnsatisfied { .. } => "packages/dependency-unsatisfied",
            Self::NameTaken { .. } | Self::SourceChanged { .. } => contextos_packages::codes::DUPLICATE_IDENTITY,
            Self::NotInstalled { .. } => contextos_packages::codes::NOT_FOUND,
            Self::State { .. } => "workflow/state-unavailable",
            Self::StateChanged => "workflow/state-changed",
        }
    }
}

impl SourceInstallError {
    /// A refusal with the one validation failure `code` for `artefact`.
    pub fn refusing(code: ValidationCode, artefact: &str, message: impl Into<String>) -> Self {
        Self::Invalid {
            failures: vec![ValidationFailure {
                code,
                artefact: artefact.to_owned(),
                message: message.into(),
            }],
        }
    }
}

#[cfg(test)]
#[path = "source_install_error_test.rs"]
mod tests;
