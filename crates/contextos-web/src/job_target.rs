//! The job a command line names: `<vault> <package> <workflow> <job>`, or with `--shared` `<package> <workflow> <job>`.
//!
//! A command takes the words after its name as they were typed, because the number of words is what tells a vault job
//! from a shared one, and this turns them into the key of the job, or refuses a count that fits neither.

use clap::CommandFactory;
use contextos_web::shared_claim::SharedJobKey;
use contextos_workflows::JobKey;
use thiserror::Error;

/// A job in a vault, or on the host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum JobTarget {
    Vault { vault: String, key: JobKey },
    Shared(SharedJobKey),
}

/// The words after a command did not name a job.
#[derive(Debug, Error)]
#[error("a {kind} is named by {expected}, but {found} words were given")]
pub(crate) struct TargetError {
    kind: &'static str,
    expected: &'static str,
    found: usize,
}

impl TargetError {
    /// A mistake in the number of words after a command: `found` words were given where `expected` names `kind`.
    pub(crate) const fn new(kind: &'static str, expected: &'static str, found: usize) -> Self {
        Self { kind, expected, found }
    }

    /// Reports the mistake as the usage error it is and ends the process with the status a command line that was not
    /// understood has.
    pub(crate) fn exit(&self) -> ! {
        crate::Cli::command()
            .error(clap::error::ErrorKind::WrongNumberOfValues, self)
            .exit()
    }
}

impl TryFrom<(&[String], bool)> for JobTarget {
    type Error = TargetError;

    fn try_from((words, shared): (&[String], bool)) -> Result<Self, Self::Error> {
        match (shared, words) {
            (true, [package, workflow, job]) => Ok(Self::Shared(SharedJobKey {
                package: package.clone(),
                workflow: workflow.clone(),
                job: job.clone(),
            })),
            (false, [vault, package, workflow, job]) => Ok(Self::Vault {
                vault: vault.clone(),
                key: JobKey {
                    package: package.clone(),
                    workflow: workflow.clone(),
                    job: job.clone(),
                },
            }),
            (true, _) => Err(TargetError {
                kind: "shared job",
                expected: "<package> <workflow> <job>",
                found: words.len(),
            }),
            (false, _) => Err(TargetError {
                kind: "vault job",
                expected: "<vault> <package> <workflow> <job>",
                found: words.len(),
            }),
        }
    }
}

/// The package store a command works on: a vault's, or the host's with `--shared`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PackageHome {
    Vault(String),
    Shared,
}

/// A package store alone: `<vault>`, or nothing with `--shared`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoreTarget(pub(crate) PackageHome);

/// One release in a package store: `<vault> <package> <version>`, or with `--shared` `<package> <version>`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReleaseTarget {
    pub(crate) home: PackageHome,
    pub(crate) package: String,
    pub(crate) version: String,
}

/// A bundle to install and the package store it goes into: `<vault> <bundle>`, or with `--shared` `<bundle>`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BundleTarget {
    pub(crate) home: PackageHome,
    pub(crate) bundle: std::path::PathBuf,
}

impl TryFrom<(&[String], bool)> for BundleTarget {
    type Error = TargetError;

    fn try_from((words, shared): (&[String], bool)) -> Result<Self, Self::Error> {
        match (shared, words) {
            (true, [bundle]) => Ok(Self {
                home: PackageHome::Shared,
                bundle: bundle.into(),
            }),
            (false, [vault, bundle]) => Ok(Self {
                home: PackageHome::Vault(vault.clone()),
                bundle: bundle.into(),
            }),
            (true, _) => Err(TargetError::new("shared bundle", "<bundle>", words.len())),
            (false, _) => Err(TargetError::new("vault bundle", "<vault> <bundle>", words.len())),
        }
    }
}

impl TryFrom<(&[String], bool)> for StoreTarget {
    type Error = TargetError;

    fn try_from((words, shared): (&[String], bool)) -> Result<Self, Self::Error> {
        match (shared, words) {
            (true, []) => Ok(Self(PackageHome::Shared)),
            (false, [vault]) => Ok(Self(PackageHome::Vault(vault.clone()))),
            (true, _) => Err(TargetError::new("shared package store", "no words", words.len())),
            (false, _) => Err(TargetError::new("vault package store", "<vault>", words.len())),
        }
    }
}

impl TryFrom<(&[String], bool)> for ReleaseTarget {
    type Error = TargetError;

    fn try_from((words, shared): (&[String], bool)) -> Result<Self, Self::Error> {
        match (shared, words) {
            (true, [package, version]) => Ok(Self {
                home: PackageHome::Shared,
                package: package.clone(),
                version: version.clone(),
            }),
            (false, [vault, package, version]) => Ok(Self {
                home: PackageHome::Vault(vault.clone()),
                package: package.clone(),
                version: version.clone(),
            }),
            (true, _) => Err(TargetError::new("shared release", "<package> <version>", words.len())),
            (false, _) => Err(TargetError::new(
                "vault release",
                "<vault> <package> <version>",
                words.len(),
            )),
        }
    }
}

/// A workflow in a vault, or in a package installed on the host: `<vault> <package> <workflow>`, or with `--shared`
/// `<package> <workflow>`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WorkflowTarget {
    Vault {
        vault: String,
        package: String,
        workflow: String,
    },
    Shared {
        package: String,
        workflow: String,
    },
}

impl TryFrom<(&[String], bool)> for WorkflowTarget {
    type Error = TargetError;

    fn try_from((words, shared): (&[String], bool)) -> Result<Self, Self::Error> {
        match (shared, words) {
            (true, [package, workflow]) => Ok(Self::Shared {
                package: package.clone(),
                workflow: workflow.clone(),
            }),
            (false, [vault, package, workflow]) => Ok(Self::Vault {
                vault: vault.clone(),
                package: package.clone(),
                workflow: workflow.clone(),
            }),
            (true, _) => Err(TargetError {
                kind: "shared workflow",
                expected: "<package> <workflow>",
                found: words.len(),
            }),
            (false, _) => Err(TargetError {
                kind: "vault workflow",
                expected: "<vault> <package> <workflow>",
                found: words.len(),
            }),
        }
    }
}

#[cfg(test)]
#[path = "job_target_test.rs"]
mod tests;
