//! Why a package store command or request was refused or failed, each with a stable code: listing, inspecting, removing,
//! and rebuilding the ledger of a vault's store or the host's.

use std::fmt;

use contextos_packages::PackagesError;
use contextos_workflows::package::PackageError;
use contextos_workflows::{KeepReason, ReleaseRef};
use thiserror::Error;

use crate::mcp_client::McpCallError;
use crate::package_bundle::BundleError;
use crate::package_graph::UnresolvableRelease;
use crate::package_keep::ReasonText;
use crate::package_retention::RetentionError;
use crate::package_store::{PackageStoreError, describe};
use crate::shared_state::SharedStateError;
use crate::vault_package_store::VaultStoreError;

/// The reasons that keep a release, as the sentence that follows its name.
#[derive(Debug)]
pub struct Reasons<'a>(pub &'a [KeepReason]);

impl fmt::Display for Reasons<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, reason) in self.0.iter().enumerate() {
            if index > 0 {
                formatter.write_str("; ")?;
            }
            write!(formatter, "{}", ReasonText(reason))?;
        }
        Ok(())
    }
}

/// A refusal or failure of a package store operation. Nothing is changed unless the operation says it was.
#[derive(Debug, Error)]
pub enum PackageOpError {
    /// A package name that cannot name one.
    #[error("{value:?} is not a usable package name: {reason}")]
    Name { value: String, reason: String },
    /// The web configuration cannot be used.
    #[error("the web configuration cannot be used ({reason}), so nothing was done; fix or remove it")]
    ConfigInvalid { reason: String },
    /// No configured server could be connected, so the vault cannot be reached.
    #[error("{reason}, so the vault's package store cannot be reached; check the configured server and try again")]
    NotConnected { reason: &'static str },
    /// The primary server could not be asked.
    #[error("the vault's server could not be asked: {0}")]
    Unreachable(#[from] McpCallError),
    /// The vault's package store cannot be located.
    #[error(transparent)]
    Vault(#[from] VaultStoreError),
    /// The package store refused or failed.
    #[error(transparent)]
    Packages(#[from] PackagesError),
    /// A rule keeps the release, so it was not removed.
    #[error(
        "{release} is in use and was not removed: {}; removing it anyway needs force",
        Reasons(.reasons)
    )]
    InUse {
        release: ReleaseRef,
        /// Every reason that refuses the removal.
        reasons: Vec<KeepReason>,
    },
    /// What keeps releases (the job state, the job files) could not be read, so nothing was decided or changed.
    #[error("what keeps releases could not be read, so nothing was changed: {0}; fix or restore it and try again")]
    KeepUnreadable(#[from] RetentionError),
    /// The bundle is larger than the limit.
    #[error("the bundle is {size} bytes, larger than the {limit} byte limit")]
    BundleTooLarge { size: usize, limit: usize },
    /// The package folder is larger than the limit.
    #[error("the package folder holds {size} bytes, more than the {limit} byte limit")]
    FolderTooLarge { size: u64, limit: usize },
    /// The bundle is malformed or does not match its manifest.
    #[error(transparent)]
    Bundle(#[from] BundleError),
    /// The package's own manifest or identity is refused.
    #[error(transparent)]
    Manifest(#[from] PackageError),
    /// The install would leave releases unable to resolve their libraries.
    #[error("the install would leave releases unable to resolve their libraries: {}", describe(.releases))]
    Unresolvable { releases: Vec<UnresolvableRelease> },
    /// The host's package store could not be read or written.
    #[error("the package store could not be read or written: {0}")]
    StoreIo(#[from] std::io::Error),
    /// A release could not be read as the operation needed.
    #[error(transparent)]
    Release(Box<PackageStoreError>),
    /// The host's data folder could not be determined, or a blocking operation did not finish.
    #[error("{reason}")]
    Host { reason: &'static str },
}

impl From<PackageStoreError> for PackageOpError {
    /// Each refusal of the host's store under the variant that carries its code. A failure that is about one release
    /// being read is kept whole as [`PackageOpError::Release`].
    fn from(error: PackageStoreError) -> Self {
        match error {
            PackageStoreError::Packages(error) => Self::Packages(error),
            PackageStoreError::StoreLocked => Self::Packages(PackagesError::StoreLocked),
            PackageStoreError::State(error) => Self::KeepUnreadable(RetentionError::HostState(error)),
            PackageStoreError::BundleTooLarge { size, limit } => Self::BundleTooLarge { size, limit },
            PackageStoreError::FolderTooLarge { size, limit } => Self::FolderTooLarge { size, limit },
            PackageStoreError::Bundle(error) => Self::Bundle(error),
            PackageStoreError::Package(error) => Self::Manifest(error),
            PackageStoreError::Unresolvable { releases } => Self::Unresolvable { releases },
            PackageStoreError::Io(error) => Self::StoreIo(error),
            PackageStoreError::AppDataUnavailable => Self::Host {
                reason: "the host's data folder could not be determined",
            },
            other @ (PackageStoreError::NotFound { .. }
            | PackageStoreError::ReleaseDamaged { .. }
            | PackageStoreError::ReleaseUnverifiable { .. }
            | PackageStoreError::ReleaseChangedWhileRead { .. }
            | PackageStoreError::NoSuchWorkflow { .. }
            | PackageStoreError::WorkflowInvalid { .. }
            | PackageStoreError::EntryUnreadable { .. }
            | PackageStoreError::ModuleUnreadable(_)) => Self::Release(Box::new(other)),
        }
    }
}

impl From<SharedStateError> for PackageOpError {
    fn from(error: SharedStateError) -> Self {
        Self::KeepUnreadable(RetentionError::HostState(error))
    }
}

impl PackageOpError {
    /// The stable machine-readable code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Name { .. } => contextos_packages::codes::INVALID_NAME,
            Self::ConfigInvalid { .. } => "workflow/config-invalid",
            Self::NotConnected { .. } | Self::Unreachable(_) => "workflow/server-unavailable",
            Self::Vault(error) => error.code(),
            Self::Packages(error) => error.code(),
            Self::InUse { .. } => "packages/in-use",
            Self::KeepUnreadable(_) => "packages/state-unreadable",
            Self::BundleTooLarge { .. } => "packages/bundle-too-large",
            Self::FolderTooLarge { .. } => "packages/folder-too-large",
            Self::Bundle(_) => "packages/invalid-bundle",
            Self::Manifest(PackageError::DuplicateIdentity { .. }) => "packages/duplicate-identity",
            Self::Manifest(_) => "packages/invalid-manifest",
            Self::Unresolvable { .. } => "packages/dependency-unsatisfied",
            Self::StoreIo(_) | Self::Release(_) | Self::Host { .. } => contextos_packages::codes::STORE_UNAVAILABLE,
        }
    }
}

#[cfg(test)]
#[path = "package_op_error_test.rs"]
mod tests;
