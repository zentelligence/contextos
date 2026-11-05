//! The code of a saved vault job, read from the release its job file names in the vault's package store.
//!
//! Source in `registry/` is edited freely and changes nothing that runs: a validation, a version, a dry run, and a run
//! all read the release, verified, with the library lock recorded for that release when a job on it first passed
//! validation. Only that first validation chooses libraries; every later read takes the lock and resolves nothing.

use contextos_workflows::{
    JobFileRelease, JobFileText, JobFingerprint, JobsState, LibraryLock, PackageKind, PackageName, RunFailure,
    StoreKind, SuppliedInputs, ValidatedInputs, ValidationCode, VersionBasis, WorkflowFailureKind,
};
use semver::Version;
use serde::Deserialize;

use crate::jobs::{ResolvedEntity, WorkflowFolder, declarations_from_text, declared_entity_of};
use crate::mcp_client::{McpCallError, McpClient};
use crate::scheduler::{StateError, read_state};
use crate::vault_package_store::VaultStoreError;
use crate::vault_release_snapshot::{
    ReleaseFailure, ReleaseRequest, ReleaseSnapshot, open_release, read_opened_release,
};

/// The `version` and `predecessor` of a job file, read without checking the rest of it.
#[derive(Deserialize)]
struct ReleaseKeys {
    version: String,
    #[serde(default)]
    predecessor: Option<String>,
}

/// A job file that does not parse, or names no exact release.
#[derive(Debug)]
pub(crate) struct NoNamedRelease;

/// The release a job file names, with its predecessor.
pub(crate) struct NamedJobRelease(pub(crate) JobFileRelease);

impl TryFrom<&str> for NamedJobRelease {
    type Error = NoNamedRelease;

    fn try_from(job_toml: &str) -> Result<Self, NoNamedRelease> {
        let keys: ReleaseKeys = toml::from_str(job_toml).map_err(|_| NoNamedRelease)?;
        JobFileRelease::try_from(JobFileText {
            version: &keys.version,
            predecessor: keys.predecessor.as_deref(),
        })
        .map(Self)
        .map_err(|_| NoNamedRelease)
    }
}

/// Which job's code to read.
pub(crate) struct CodeRequest<'a> {
    pub vault: &'a str,
    /// The workflow's folder: its source package's, which names the package in the vault's store, and its own.
    pub folder: WorkflowFolder<'a>,
    pub job_toml: &'a str,
    /// The job state the lock is taken from, when the caller holds one. Otherwise the state is read.
    pub state: Option<&'a JobsState>,
}

/// The code of one job, read and verified.
pub(crate) struct ReadCode {
    pub snapshot: ReleaseSnapshot,
    pub release: JobFileRelease,
    /// The slug of the workflow of the release, which keys the job's state.
    pub workflow_slug: String,
    package: String,
}

impl ReadCode {
    /// The job's version: the store kind, the release and its digest, the job file, and the locked libraries.
    pub(crate) fn version(&self, job_toml: &str) -> JobFingerprint {
        self.snapshot.basis.version_in(StoreKind::Vault, job_toml)
    }

    /// What the version was made from, for the state change that keeps a passing validation.
    pub(crate) fn made_from(&self) -> VersionBasis {
        VersionBasis {
            package: self.package.clone(),
            release: self.release.version.as_semver().clone(),
            lock: self.snapshot.lock.clone(),
            predecessor: self.release.predecessor.clone(),
        }
    }
}

/// What reading a job's code found.
pub(crate) enum CodeRead {
    Read(Box<ReadCode>),
    /// The job file names no exact release, which validation reports against the file.
    NoRelease,
    /// The release, or a library release in its lock, could not be read whole.
    Failed(Box<ReleaseFailure>),
    /// The vault's store cannot be located from this session.
    Unlocated(VaultStoreError),
    /// The job state, which holds the library lock, could not be read.
    StateUnreadable(String),
}

/// Reads the code of the job in `request`.
///
/// # Errors
///
/// Returns the call error when the server could not be asked, because then nothing about the code is known.
pub(crate) async fn read_code(client: &McpClient, request: &CodeRequest<'_>) -> Result<CodeRead, McpCallError> {
    let Ok(NamedJobRelease(release)) = NamedJobRelease::try_from(request.job_toml) else {
        return Ok(CodeRead::NoRelease);
    };
    let version: &Version = release.version.as_semver();
    let engine = match client.vault_packages(request.vault).await {
        Ok(engine) => engine,
        Err(unlocated) => return Ok(CodeRead::Unlocated(unlocated)),
    };
    let package = match PackageName::try_from(request.folder.package) {
        Ok(package) => package,
        Err(error) => return Ok(CodeRead::Failed(Box::new(ReleaseFailure::Libraries(error)))),
    };
    // The workflow's slug keys its library lock, and is known only from the release, so the release is opened first.
    let opened = match open_release(&engine, &package, version).await {
        Ok(opened) => opened,
        Err(failure) => return Ok(CodeRead::Failed(failure)),
    };
    let workflow_slug = match opened.workflow_of(request.folder.directory) {
        Ok(definition) => definition.folder,
        Err(failure) => return Ok(CodeRead::Failed(failure)),
    };
    let read_here;
    let state = if let Some(state) = request.state {
        state
    } else {
        read_here = match read_state(client, request.vault).await {
            Ok(snapshot) => snapshot,
            Err(StateError::Mcp(outage)) => return Err(outage),
            Err(other) => return Ok(CodeRead::StateUnreadable(other.to_string())),
        };
        &read_here.value
    };
    let lock: Option<LibraryLock> = state
        .library_lock(request.folder.package, &workflow_slug, version)
        .cloned();
    let snapshot = read_opened_release(
        &engine,
        opened,
        &ReleaseRequest {
            package: &package,
            release: version,
            workflow: request.folder.directory,
            lock: lock.as_ref(),
        },
    )
    .await;
    Ok(match snapshot {
        Ok(snapshot) => CodeRead::Read(Box::new(ReadCode {
            snapshot,
            release,
            workflow_slug,
            package: request.folder.package.to_owned(),
        })),
        Err(failure) => CodeRead::Failed(failure),
    })
}

/// How a release that could not be read is reported by validation.
pub(crate) struct ReleaseFinding {
    /// The failure to report, or none when the release could not be verified: nothing about the job follows then.
    pub failure: Option<(ValidationCode, String)>,
    pub message: String,
    /// Whether the release, or one in its lock, is gone, which suspends an `enabled` job as unavailable.
    pub removed: bool,
}

impl From<&ReleaseFailure> for ReleaseFinding {
    fn from(failure: &ReleaseFailure) -> Self {
        let message = failure.to_string();
        let (reported, removed) = match failure {
            ReleaseFailure::Removed { release } => {
                (Some((ValidationCode::ArtefactMissing, release.name.to_string())), true)
            }
            ReleaseFailure::Damaged { release, .. } | ReleaseFailure::LockDigestChanged { release, .. } => {
                (Some((ValidationCode::ArtefactInvalid, release.name.to_string())), false)
            }
            ReleaseFailure::Incomplete { .. } | ReleaseFailure::Unverifiable { .. } => (None, false),
            ReleaseFailure::NoSuchWorkflow { workflow, .. } => (
                Some((ValidationCode::ArtefactMissing, format!("{workflow}/workflow.toml"))),
                false,
            ),
            ReleaseFailure::WorkflowInvalid { workflow, .. } => (
                Some((ValidationCode::ArtefactInvalid, format!("{workflow}/workflow.toml"))),
                false,
            ),
            ReleaseFailure::PackageFileInvalid {
                release,
                file,
                is_library,
                ..
            } => {
                let artefact = if *is_library {
                    format!("{}:{file}", release.name)
                } else {
                    (*file).to_owned()
                };
                (Some((ValidationCode::ArtefactInvalid, artefact)), false)
            }
            ReleaseFailure::Libraries(error) => (
                Some((
                    ValidationCode::try_from(error).unwrap_or(ValidationCode::ArtefactMissing),
                    "libraries".to_owned(),
                )),
                false,
            ),
            ReleaseFailure::Module(unreadable) => {
                (Some((ValidationCode::ArtefactMissing, unreadable.artefact())), false)
            }
            ReleaseFailure::Entry { entry, .. } => (Some((ValidationCode::ArtefactMissing, entry.clone())), false),
            ReleaseFailure::Declarations { file, .. } => (Some((ValidationCode::ArtefactMissing, file.clone())), false),
        };
        Self {
            failure: reported,
            message,
            removed,
        }
    }
}

/// A saved job's code read for a run, with the job file text its version is made with.
pub(crate) struct ReadForRun {
    pub code: ReadCode,
    pub job_toml: String,
}

impl ReadForRun {
    /// `job` as its release declares its workflow: the servers, templates, modules, libraries, inputs, outputs, log level,
    /// and entity come from the release's own `workflow.toml`, and the values the job holds are checked against the
    /// inputs that declares. The run is refused when they do not fit.
    ///
    /// # Errors
    ///
    /// `workflow/job-not-runnable` when the release's `workflow.toml` does not parse, or the job's input values do not fit
    /// the inputs it declares.
    pub(crate) fn declared_job(
        &self,
        job: &crate::jobs::RegisteredJob,
    ) -> Result<crate::jobs::RegisteredJob, RunFailure> {
        let refused = |why: String| {
            let kind = WorkflowFailureKind::JobNotRunnable;
            RunFailure {
                kind: Some(kind),
                message: format!("{}: {why}, so this run did not start", kind.code()),
            }
        };
        let workflow = declarations_from_text(
            &self.code.snapshot.workflow_toml,
            WorkflowFolder {
                package: &job.package,
                directory: &job.workflow_slug,
            },
        )
        .map_err(|found| {
            refused(format!(
                "the release's workflow.toml cannot be used ({})",
                found.message
            ))
        })?;
        let workflow = match &self.code.snapshot.kind {
            PackageKind::Workflows(file) => workflow.declaring(file),
            PackageKind::Library(_) => workflow,
        };
        let inputs = ValidatedInputs::try_from(SuppliedInputs {
            declared: workflow.inputs.fields(),
            values: job.inputs.values().clone(),
        })
        .map_err(|error| {
            refused(format!(
                "the job's inputs do not fit the release's declared inputs: {error}"
            ))
        })?;
        Ok(crate::jobs::RegisteredJob {
            entity: ResolvedEntity::from((
                declared_entity_of(&self.job_toml).as_deref(),
                workflow.entity.as_deref(),
            )),
            entry: workflow.entry,
            calls: workflow.calls,
            inputs,
            declared_inputs: workflow.inputs,
            outputs: workflow.outputs,
            workflow_log_level: workflow.log_level,
            templates: workflow.templates,
            modules: workflow.modules,
            libraries: workflow.libraries,
            ..job.clone()
        })
    }
}

/// Reads and verifies the release `job` names, which a run does before its script starts.
///
/// # Errors
///
/// Returns the failure the run ends with: `workflow/package-unavailable` for a release, or a locked library release,
/// that is removed, damaged, or incomplete, or a store that cannot be located; `workflow/job-not-runnable` for a job file
/// that names no exact release; `workflow/state-unavailable` when the library lock cannot be read; and
/// `workflow/server-unavailable` when the server could not be asked.
pub(crate) async fn read_job_release(
    client: &McpClient,
    vault: &str,
    job: &crate::jobs::RegisteredJob,
) -> Result<ReadForRun, contextos_workflows::RunFailure> {
    let refused = |kind: WorkflowFailureKind, why: String| RunFailure {
        kind: Some(kind),
        message: format!("{}: {why}, so this run did not start", kind.code()),
    };
    let folder = WorkflowFolder {
        package: &job.package,
        directory: &job.workflow_slug,
    };
    let job_toml = crate::job_validation::read_text(
        client,
        &format!("{vault}://registry/workflows/{folder}/jobs/{}", job.file_name),
    )
    .await
    .map_err(|outage| {
        refused(
            WorkflowFailureKind::ServerUnavailable,
            format!("the job file could not be read: {outage}"),
        )
    })?
    .ok_or_else(|| {
        refused(
            WorkflowFailureKind::JobNotRunnable,
            "the job file could not be read".to_owned(),
        )
    })?;
    let code = read_code(
        client,
        &CodeRequest {
            vault,
            folder,
            job_toml: &job_toml,
            state: None,
        },
    )
    .await
    .map_err(|outage| {
        refused(
            WorkflowFailureKind::ServerUnavailable,
            format!("the release could not be read: {outage}"),
        )
    })?;
    match code {
        CodeRead::Read(code) => Ok(ReadForRun { code: *code, job_toml }),
        CodeRead::Failed(failure) => Err(refused(WorkflowFailureKind::PackageUnavailable, failure.to_string())),
        CodeRead::Unlocated(error) => Err(refused(WorkflowFailureKind::PackageUnavailable, error.to_string())),
        CodeRead::NoRelease => Err(refused(
            WorkflowFailureKind::JobNotRunnable,
            "the job file names no exact release".to_owned(),
        )),
        CodeRead::StateUnreadable(reason) => Err(refused(
            WorkflowFailureKind::StateUnavailable,
            format!("the job state holding the library lock could not be read: {reason}"),
        )),
    }
}
