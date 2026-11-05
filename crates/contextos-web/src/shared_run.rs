//! What a run of a workflow from the host package store needs before it can start.
//!
//! The release is chosen once, here, so an install during the run cannot change the script it executes. Every failure
//! is one that ends the run before any MCP call.

use std::path::Path;
use std::sync::Arc;

use contextos_workflows::PackageName;
use contextos_workflows::call_rules::CallRules;
use contextos_workflows::{
    CommandLineInputs, DeclaredOutputs, InputOverrides, LibraryLock, LogLevel, OverriddenInputs, PackageError,
    PackageIdentity, SuppliedInputs, ValidatedInputs, WorkflowFailureKind, WorkflowFiles, WorkflowScript,
};
use semver::Version;
use serde_json::{Map, Value};
use thiserror::Error;

use crate::package_files::InstalledPackageFiles;
use crate::package_store::PackageStoreError;
use crate::release_basis::ReleaseBasis;
use crate::shared_snapshot::{SharedSnapshot, SnapshotFailure, SnapshotRequest};

/// The largest workflow entry script a run will read.
pub const MAX_SCRIPT_BYTES: u64 = 1024 * 1024;

/// Input values as a run receives them.
pub enum RunValues {
    /// Already structured, as a saved job's `[inputs]` table holds them.
    Structured(Map<String, Value>),
    /// `name=value` arguments from a command line, read by each input's declared kind.
    CommandLine(Vec<String>),
    /// A saved job's values with `name=value` arguments given for this run only, which replace the job's own.
    Overridden {
        job_values: Map<String, Value>,
        arguments: Vec<String>,
    },
}

/// Which workflow to run, and with which values.
pub struct SharedRunRequest<'a> {
    pub store_root: &'a Path,
    pub package: &'a str,
    pub workflow: &'a str,
    /// The exact release to run, which a job's file or a one-off run's command line names; never the newest.
    pub version: &'a Version,
    /// The library lock recorded for the workflow release, or `None` for one no job has passed validation on.
    pub lock: Option<&'a LibraryLock>,
    pub values: RunValues,
}

/// A snapshot that was already read, and the values to run it with, for a run that must not read the store again: the
/// scheduler reads the snapshot before it claims a tick, and the run it starts executes that same snapshot.
pub struct SnapshotRun {
    pub snapshot: SharedSnapshot,
    pub values: RunValues,
}

/// A shared run that is ready to start.
#[derive(Debug)]
pub struct SharedRun {
    pub identity: PackageIdentity,
    /// The library releases this run resolved, in name order.
    pub libraries: Vec<PackageIdentity>,
    /// The releases this run resolved, which its version is made from.
    pub basis: ReleaseBasis,
    /// The entry script and every module of the package and its libraries, read once with the releases this run
    /// resolved, so what a validation read is what runs.
    pub script: WorkflowScript,
    pub inputs: ValidatedInputs,
    /// The input names given on the command line for this run, in declaration order. Never their values.
    pub overridden: Vec<String>,
    /// The outputs the workflow declares, which its script sets with `set_output`.
    pub outputs: DeclaredOutputs,
    /// The calls the workflow declares it may make; nothing else is issued.
    pub calls: CallRules,
    /// The minimum log level the workflow's package sets for it, if any.
    pub log_level: Option<LogLevel>,
    /// The files the package declares, read from the release this run resolved, so an install during the run cannot
    /// change what the script reads either.
    pub files: WorkflowFiles,
}

impl SharedRun {
    /// The version of the job whose job file is `job_toml`, made from the releases this run resolved, so what a run
    /// settles is about the version it ran, even when another release is installed while it starts.
    #[must_use]
    pub fn version(&self, job_toml: &str) -> contextos_workflows::JobFingerprint {
        self.basis.version(job_toml)
    }
}

#[derive(Debug, Error)]
pub enum SharedRunError {
    #[error("the package or release is unavailable: {0}")]
    Unavailable(String),
    /// A release was removed, replaced, or changed while the snapshot was being read: a change on the host caught
    /// mid-read, so the run does not start and nothing about the job follows from it.
    #[error("the release changed while it was read, so the run did not start; try again ({0})")]
    ReleaseChanged(String),
    #[error("the inputs are invalid: {0}")]
    InputInvalid(#[source] PackageError),
}

impl SharedRunError {
    /// The failure kind the run ends with.
    #[must_use]
    pub const fn kind(&self) -> WorkflowFailureKind {
        match self {
            Self::Unavailable(_) | Self::ReleaseChanged(_) => WorkflowFailureKind::PackageUnavailable,
            Self::InputInvalid(_) => WorkflowFailureKind::InputInvalid,
        }
    }
}

impl From<PackageStoreError> for SharedRunError {
    fn from(source: PackageStoreError) -> Self {
        Self::Unavailable(source.to_string())
    }
}

impl From<Box<SnapshotFailure>> for SharedRunError {
    fn from(failure: Box<SnapshotFailure>) -> Self {
        if failure.release_changed() {
            Self::ReleaseChanged(failure.to_string())
        } else {
            Self::Unavailable(failure.to_string())
        }
    }
}

impl TryFrom<SharedRunRequest<'_>> for SharedRun {
    type Error = SharedRunError;

    fn try_from(request: SharedRunRequest<'_>) -> Result<Self, SharedRunError> {
        let workflow = request.workflow;
        if workflow.is_empty() || matches!(workflow, "." | "..") || workflow.contains(['/', '\\', '\0']) {
            return Err(SharedRunError::Unavailable(
                "a workflow name must be a single plain name".to_owned(),
            ));
        }
        let package =
            PackageName::try_from(request.package).map_err(|error| SharedRunError::Unavailable(error.to_string()))?;
        let snapshot = SharedSnapshot::read(&SnapshotRequest {
            store_root: request.store_root,
            package,
            version: request.version,
            workflow: request.workflow,
            lock: request.lock,
        })
        .map_err(SharedRunError::from)?;
        Self::try_from(SnapshotRun {
            snapshot,
            values: request.values,
        })
    }
}

impl TryFrom<SnapshotRun> for SharedRun {
    type Error = SharedRunError;

    fn try_from(read: SnapshotRun) -> Result<Self, SharedRunError> {
        let SnapshotRun { snapshot, values } = read;
        let SharedSnapshot {
            manifest,
            definition,
            release_dir,
            templates,
            libraries,
            basis,
            script,
            lock: _,
            library_tools: _,
        } = snapshot;
        let (inputs, overridden) = match values {
            RunValues::Structured(values) => (
                ValidatedInputs::try_from(SuppliedInputs {
                    declared: definition.inputs.fields(),
                    values,
                }),
                Vec::new(),
            ),
            RunValues::CommandLine(arguments) => (
                ValidatedInputs::try_from(CommandLineInputs {
                    declared: definition.inputs.fields(),
                    arguments,
                }),
                Vec::new(),
            ),
            RunValues::Overridden { job_values, arguments } => {
                match OverriddenInputs::try_from(InputOverrides {
                    declared: definition.inputs.fields(),
                    job_values,
                    arguments,
                }) {
                    Ok(merged) => (Ok(merged.inputs), merged.overridden),
                    Err(error) => (Err(error), Vec::new()),
                }
            }
        };
        let inputs = inputs.map_err(SharedRunError::InputInvalid)?;
        Ok(Self {
            identity: manifest.identity,
            libraries: libraries.into_iter().map(|library| library.identity).collect(),
            basis,
            script,
            inputs,
            overridden,
            outputs: definition.outputs,
            calls: definition.calls,
            log_level: definition.log_level,
            files: WorkflowFiles::new(templates, Arc::new(InstalledPackageFiles::from(release_dir))),
        })
    }
}

#[cfg(test)]
#[path = "shared_run_test.rs"]
mod tests;
