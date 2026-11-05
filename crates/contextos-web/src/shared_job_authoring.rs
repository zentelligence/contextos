//! Authoring a shared job file from the command line: create, show, replace, and list.
//!
//! A shared job is one file at `{jobs root}/{package}/{workflow}/{job}.toml`. This module is the one writer of that
//! file for a command-line caller, so a job authored here is checked exactly as a hand-placed one is, and every check
//! and write happens under the host state's lock, the lock every lifecycle change takes.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use contextos_core::ContentHash;
use contextos_fs::FsError;
use contextos_fs::host_file::{FileReplaceObserver, NoFileFaults, create_file_observed, replace_file_atomically};
use contextos_fs::is_link_or_reparse_point;
use contextos_workflows::{
    InstalledReleases, JobPlace, JobsState, Presence, ReleaseKinds, ReleaseRefusal, StoreKind, SuccessorRefusal,
    ValidationCode, check_new_job, check_replacement,
};
use semver::Version;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::bounded_read::{BoundedReadError, read_text_within};
use crate::package_store;
use crate::saved_jobs::SavedJobs;
use crate::shared_claim::SharedJobKey;
use crate::shared_jobs::{
    JobFilesFor, MAX_JOB_MANIFEST_BYTES, SharedJob, SharedJobError, SharedJobFile, SharedJobSource, UnusableReason,
    find_job_files, list_shared_job_entries, list_shared_job_files, require_slug,
};
use crate::shared_state::{SharedStateError, SharedStateStore};
use crate::store_retention::{RetentionWarning, retain_host};
use crate::successor_plan::{StandingScope, standing_successors};

/// What a create or replace did to the job file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthoredOutcome {
    /// A new file now holds the bytes given.
    Created,
    /// An existing file now holds the bytes given.
    Replaced,
    /// The file already held exactly the bytes given, and was not written.
    Unchanged,
}

impl AuthoredOutcome {
    /// The word the outcome is reported as.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Replaced => "replaced",
            Self::Unchanged => "unchanged",
        }
    }
}

/// A non-fatal finding about a job that was stored.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthoringWarning {
    /// The job's package or workflow is not installed, so the job cannot be validated yet.
    PackageUnavailable { package: String, workflow: String },
    /// The package store could not be read, so whether the package and workflow are installed is not known.
    PackageUnchecked { package: String, workflow: String },
    /// Retention could not finish after the job file was replaced. The file is replaced all the same.
    RetentionFailed(RetentionWarning),
}

impl AuthoringWarning {
    /// The stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::PackageUnavailable { .. } | Self::PackageUnchecked { .. } => "workflow/package-unavailable",
            Self::RetentionFailed(_) => RetentionWarning::CODE,
        }
    }
}

/// What a create or replace came to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Authored {
    pub outcome: AuthoredOutcome,
    pub key: SharedJobKey,
    /// The hash of the bytes the file holds now.
    pub hash: ContentHash,
    pub warnings: Vec<AuthoringWarning>,
}

/// A job file as `show` reports it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShownJob {
    pub key: SharedJobKey,
    /// The file's content, exactly as stored.
    pub content: String,
    /// The hash of the file's bytes, the form `replace` expects.
    pub hash: ContentHash,
}

/// Why an authoring request was refused. Nothing was written in any case, and no message holds a host path.
#[derive(Debug, Error)]
pub enum AuthoringError {
    /// The bytes do not make a valid job file. `cause` is the `validation/` code a hand-placed file would get.
    #[error("workflow/job-file-invalid: the job file is not valid ({cause}): {message}")]
    JobFileInvalid { cause: &'static str, message: String },
    #[error(
        "workflow/job-key-invalid: the {part} is not a usable name: use lowercase letters, digits, and hyphens, and not a reserved device name, and a job is not named current, previous, or future"
    )]
    JobKeyInvalid { part: &'static str },
    #[error(
        "workflow/job-key-mismatch: the file would be tracked as the job {slug:?}, but the job asked for is {job:?}; keep a `slug` line naming {job:?} when the file's name is not {job:?}.toml"
    )]
    JobKeyMismatch { job: String, slug: String },
    #[error("workflow/job-exists: the job {key} already has a file with different content")]
    JobExists { key: String },
    /// A successor's new key is the key of a job that already has a file.
    #[error(
        "workflow/job-exists: the successor's key {key} already belongs to a job; delete that job if it is retired, or give the successor another key (the one-job form of `jobs successor` takes the key to use)"
    )]
    SuccessorKeyHeld { key: String },
    #[error("workflow/job-key-taken: the job {key} is already the key of {files}")]
    JobKeyTaken { key: String, files: String },
    #[error(
        "workflow/job-state-remains: the job {key} has stored state and no file; delete that state first, retiring the job if it is not already retired"
    )]
    JobStateRemains { key: String },
    /// The web configuration the command was started with is there and cannot be used. `reason` names its class of
    /// failure and never a path or a value.
    #[error(
        "workflow/config-invalid: the web configuration cannot be used ({reason}), so nothing was done; fix or remove it"
    )]
    ConfigInvalid { reason: String },
    /// The release the file names is not installed, could not be looked for, or is not one a job may name or the job a stage that does
    /// not allow it. Carries its own code.
    #[error("{0}")]
    ReleaseRefused(#[from] ReleaseRefusal),
    /// A successor may not be made, or may not name the release asked for. Carries its own code.
    #[error("{0}")]
    SuccessorRefused(#[from] SuccessorRefusal),
    /// Another operation holds the vault's package store lock, which a vault successor is written under.
    #[error("packages/store-locked: another operation holds the package store's lock; try again when it finishes")]
    StoreBusy,
    #[error("workflow/job-changed: the job {key} changed since it was read; its hash is now {current}")]
    JobChanged { key: String, current: String },
    #[error("workflow/job-not-found: there is no job file for {key}")]
    JobNotFound { key: String },
    /// The jobs folder, a job file, or the input could not be read or written, or a link stands where a folder or file
    /// belongs. `kind` is what the filesystem reported, when it reported anything. Never holds a path.
    #[error("workflow/job-store-unavailable: {action}{suffix}", suffix = KindSuffix(*kind))]
    JobStoreUnavailable {
        action: &'static str,
        kind: Option<io::ErrorKind>,
    },
    /// The shared job state could not be read or written. `kind` is what the filesystem reported. Never holds a path.
    #[error("workflow/state-unavailable: {action}{suffix}", suffix = KindSuffix(*kind))]
    StateUnavailable {
        action: &'static str,
        kind: Option<io::ErrorKind>,
    },
    /// The shared job state file does not parse and is not repaired. `reason` is the parser's message, which holds no
    /// content from the file.
    #[error("workflow/state-unavailable: the shared job state is not valid and is not repaired: {reason}")]
    StateMalformed { reason: String },
}

/// The ` (kind)` that ends a message, or nothing when the filesystem reported no kind.
struct KindSuffix(Option<io::ErrorKind>);

impl std::fmt::Display for KindSuffix {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(kind) => write!(formatter, " ({kind})"),
            None => Ok(()),
        }
    }
}

impl AuthoringError {
    /// The stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::JobFileInvalid { .. } => "workflow/job-file-invalid",
            Self::JobKeyInvalid { .. } => "workflow/job-key-invalid",
            Self::JobKeyMismatch { .. } => "workflow/job-key-mismatch",
            Self::JobExists { .. } | Self::SuccessorKeyHeld { .. } => "workflow/job-exists",
            Self::JobKeyTaken { .. } => "workflow/job-key-taken",
            Self::JobStateRemains { .. } => "workflow/job-state-remains",
            Self::ConfigInvalid { .. } => "workflow/config-invalid",
            Self::ReleaseRefused(refusal) => refusal.code(),
            Self::SuccessorRefused(refusal) => refusal.code(),
            Self::StoreBusy => "packages/store-locked",
            Self::JobChanged { .. } => "workflow/job-changed",
            Self::JobNotFound { .. } => "workflow/job-not-found",
            Self::JobStoreUnavailable { .. } => "workflow/job-store-unavailable",
            Self::StateUnavailable { .. } | Self::StateMalformed { .. } => "workflow/state-unavailable",
        }
    }

    /// The `validation/` code a refused file carries, when it is a file that was refused.
    #[must_use]
    pub const fn cause(&self) -> Option<&'static str> {
        match self {
            Self::JobFileInvalid { cause, .. } => Some(*cause),
            _ => None,
        }
    }

    pub(crate) fn invalid_file(message: impl Into<String>) -> Self {
        Self::JobFileInvalid {
            cause: ValidationCode::ArtefactInvalid.as_str(),
            message: message.into(),
        }
    }

    pub(crate) fn unavailable(action: &'static str, error: &io::Error) -> Self {
        Self::JobStoreUnavailable {
            action,
            kind: Some(error.kind()),
        }
    }

    /// A refusal that names no filesystem failure, only what stands in the way.
    pub(crate) const fn blocked(action: &'static str) -> Self {
        Self::JobStoreUnavailable { action, kind: None }
    }

    /// The refusal for a file that is there and cannot be used, which a hand-placed file would fail to load for too.
    pub(crate) fn unusable(reason: UnusableReason) -> Self {
        Self::invalid_file(format!("the job file is {reason}"))
    }
}

/// The state file's own text names its path, so only what failed is kept.
impl From<SharedStateError> for AuthoringError {
    fn from(error: SharedStateError) -> Self {
        match error {
            SharedStateError::Malformed { reason, .. } => Self::StateMalformed { reason },
            SharedStateError::Io(error) => Self::StateUnavailable {
                action: "the shared job state could not be read or written",
                kind: Some(error.kind()),
            },
            SharedStateError::Serialise(_) => Self::StateUnavailable {
                action: "the shared job state could not be serialised",
                kind: None,
            },
        }
    }
}

/// A write failure, without the path the filesystem error carries.
impl From<FsError> for AuthoringError {
    fn from(error: FsError) -> Self {
        let kind = match &error {
            FsError::CreateTemporary { source, .. }
            | FsError::WriteTemporary { source, .. }
            | FsError::FlushTemporary { source, .. }
            | FsError::PersistTemporary { source, .. }
            | FsError::AtomicWriteInterrupted { source, .. } => Some(source.kind()),
            _ => None,
        };
        Self::JobStoreUnavailable {
            action: "the job file could not be written",
            kind,
        }
    }
}

/// The SHA-256 of some bytes, as the hash `ContextOS` MCP reports for a file.
pub(crate) struct Digested<'a>(pub(crate) &'a [u8]);

impl From<Digested<'_>> for ContentHash {
    fn from(bytes: Digested<'_>) -> Self {
        Self::from(<[u8; 32]>::from(Sha256::digest(bytes.0)))
    }
}

/// Names that Windows reserves for devices, whatever their extension, so a job authored here is portable.
fn is_device_name(part: &str) -> bool {
    let numbered = |prefix: &str| {
        part.strip_prefix(prefix)
            .is_some_and(|digit| digit.len() == 1 && digit.chars().all(|c| ('1'..='9').contains(&c)))
    };
    matches!(part, "con" | "prn" | "aux" | "nul") || numbered("com") || numbered("lpt")
}

/// Each part of `key` is a slug and none is a device name, so the key can only name a path inside the jobs folder.
pub(crate) fn require_usable(key: &SharedJobKey) -> Result<(), AuthoringError> {
    for (part, value) in [
        ("package", &key.package),
        ("workflow", &key.workflow),
        ("job", &key.job),
    ] {
        if require_slug(value).is_err() || is_device_name(value) {
            return Err(AuthoringError::JobKeyInvalid { part });
        }
    }
    if contextos_workflows::is_reserved_job_slug(&key.job) {
        return Err(AuthoringError::JobKeyInvalid { part: "job" });
    }
    Ok(())
}

/// Checks `bytes` exactly as loading a placed file named by `source` does, and that the job the file would then be
/// tracked as, which its own `slug` decides, is `wanted`.
fn check_job_file(source: &SharedJobKey, wanted: &SharedJobKey, bytes: &[u8]) -> Result<SharedJob, AuthoringError> {
    if u64::try_from(bytes.len()).map_or(true, |length| length > MAX_JOB_MANIFEST_BYTES) {
        return Err(AuthoringError::invalid_file(format!(
            "the job file is larger than the {MAX_JOB_MANIFEST_BYTES} byte limit"
        )));
    }
    let text =
        std::str::from_utf8(bytes).map_err(|_| AuthoringError::invalid_file("the job file is not UTF-8 text"))?;
    let job = SharedJob::try_from(SharedJobSource {
        key: source.clone(),
        text,
    })
    .map_err(|error| AuthoringError::from(&error))?;
    if job.key.job == wanted.job {
        Ok(job)
    } else {
        Err(AuthoringError::JobKeyMismatch {
            job: wanted.job.clone(),
            slug: job.key.job,
        })
    }
}

impl From<&SharedJobError> for AuthoringError {
    fn from(error: &SharedJobError) -> Self {
        Self::invalid_file(error.to_string())
    }
}

/// Reads a job file or standard input, never holding more than the manifest limit and one byte more.
///
/// # Errors
///
/// Returns [`AuthoringError::JobFileInvalid`] when the input is larger than the limit, and
/// [`AuthoringError::JobStoreUnavailable`] when the reader fails.
pub fn read_job_bytes(reader: impl Read) -> Result<Vec<u8>, AuthoringError> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_JOB_MANIFEST_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| AuthoringError::unavailable("the job input could not be read", &error))?;
    if u64::try_from(bytes.len()).map_or(true, |length| length > MAX_JOB_MANIFEST_BYTES) {
        return Err(AuthoringError::invalid_file(format!(
            "the job file is larger than the {MAX_JOB_MANIFEST_BYTES} byte limit"
        )));
    }
    Ok(bytes)
}

/// Reads the TOML a command was given: standard input for `-`, otherwise the file at `source`, bounded as
/// [`read_job_bytes`] is.
///
/// # Errors
///
/// As [`read_job_bytes`], and [`AuthoringError::JobStoreUnavailable`] when the file cannot be opened. No message names
/// the path.
pub fn read_job_input(source: &Path) -> Result<Vec<u8>, AuthoringError> {
    if source == Path::new("-") {
        read_job_bytes(io::stdin().lock())
    } else {
        let file = fs::File::open(source)
            .map_err(|error| AuthoringError::unavailable("the job file could not be opened", &error))?;
        read_job_bytes(file)
    }
}

/// The authoring commands over one host's jobs folder, package store, and state.
#[derive(Clone, Debug)]
pub struct SharedJobAuthoring {
    pub jobs_root: PathBuf,
    pub store_root: PathBuf,
    pub state: SharedStateStore,
}

impl SharedJobAuthoring {
    /// Creates the job file for `key` with exactly `bytes`, never overwriting.
    ///
    /// Blocking: waits for the state lock, so call it from a blocking thread.
    ///
    /// # Errors
    ///
    /// Returns the [`AuthoringError`] that says why the request was refused.
    pub fn create(&self, key: &SharedJobKey, bytes: &[u8]) -> Result<Authored, AuthoringError> {
        self.create_observed(key, bytes, &NoFileFaults)
    }

    /// As [`SharedJobAuthoring::create`], reporting each step of the file's creation to `observer`, which a test uses
    /// to put another writer's file in place at the one moment the no-replace move must be what refuses.
    ///
    /// Blocking: waits for the state lock, so call it from a blocking thread.
    ///
    /// # Errors
    ///
    /// As [`SharedJobAuthoring::create`].
    pub fn create_observed(
        &self,
        key: &SharedJobKey,
        bytes: &[u8],
        observer: &dyn FileReplaceObserver,
    ) -> Result<Authored, AuthoringError> {
        self.create_checked(key, bytes, observer, None)
    }

    fn create_checked(
        &self,
        key: &SharedJobKey,
        bytes: &[u8],
        observer: &dyn FileReplaceObserver,
        kinds: Option<&dyn ReleaseKinds>,
    ) -> Result<Authored, AuthoringError> {
        require_usable(key)?;
        let job = check_job_file(key, key, bytes)?;
        let authored =
            self.locked(|jobs| self.create_locked(jobs, kinds.unwrap_or(jobs), key, (bytes, &job), observer))?;
        log_written(key, authored.outcome, "shared job created");
        Ok(authored)
    }

    fn create_locked(
        &self,
        jobs: &JobsState,
        kinds: &dyn ReleaseKinds,
        key: &SharedJobKey,
        (bytes, job): (&[u8], &SharedJob),
        observer: &dyn FileReplaceObserver,
    ) -> Result<Authored, AuthoringError> {
        let hash = ContentHash::from(Digested(bytes));
        let warnings = Vec::new();
        self.require_plain_folders(key)?;
        let path = self.file_path(key, &key.job);
        match fs::symlink_metadata(&path) {
            Ok(found) if found.is_file() && !is_link_or_reparse_point(&found) => {
                return match read_text_within(&path, MAX_JOB_MANIFEST_BYTES) {
                    Ok(existing) if existing.as_bytes() == bytes => Ok(Authored {
                        outcome: AuthoredOutcome::Unchanged,
                        key: key.clone(),
                        hash,
                        warnings,
                    }),
                    Ok(_) | Err(BoundedReadError::TooLarge { .. } | BoundedReadError::NotText { .. }) => {
                        Err(AuthoringError::JobExists { key: key.to_string() })
                    }
                    Err(BoundedReadError::Io(error)) => Err(AuthoringError::unavailable(
                        "the existing job file could not be read",
                        &error,
                    )),
                };
            }
            Ok(_) => {
                return Err(AuthoringError::blocked(
                    "a link or folder stands where the job file belongs, so nothing was written",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(AuthoringError::unavailable("the job file could not be checked", &error)),
        }
        let tracked = self.files_tracked_as(key)?;
        if !tracked.is_empty() {
            return Err(AuthoringError::JobKeyTaken {
                key: key.to_string(),
                files: stems(&tracked),
            });
        }
        if jobs.job(&key.package, &key.workflow, &key.job).is_some() {
            return Err(AuthoringError::JobStateRemains { key: key.to_string() });
        }
        self.require_no_standing_successor(jobs, key)?;
        check_new_job(
            &HostCatalogue(&self.store_root),
            kinds,
            JobPlace {
                package: &key.package,
                workflow: &key.workflow,
            },
            &job.release,
        )?;
        self.make_folders(key)?;
        match create_file_observed(&path, bytes, observer) {
            Ok(()) => Ok(Authored {
                outcome: AuthoredOutcome::Created,
                key: key.clone(),
                hash,
                warnings,
            }),
            // Another writer's file arrived between the check and the move, which the filesystem refused to replace.
            Err(FsError::PersistTemporary { source, .. }) if source.kind() == io::ErrorKind::AlreadyExists => {
                Err(AuthoringError::JobExists { key: key.to_string() })
            }
            Err(error) => Err(error.into()),
        }
    }

    /// As [`SharedJobAuthoring::create`], asking `kinds` whether a release is current, previous, or future.
    ///
    /// # Errors
    ///
    /// As [`SharedJobAuthoring::create`].
    #[cfg(test)]
    pub(crate) fn create_with_kinds(
        &self,
        key: &SharedJobKey,
        bytes: &[u8],
        kinds: &dyn ReleaseKinds,
    ) -> Result<Authored, AuthoringError> {
        self.create_checked(key, bytes, &NoFileFaults, Some(kinds))
    }

    /// As [`SharedJobAuthoring::replace`], asking `kinds` whether a release is current, previous, or future.
    ///
    /// # Errors
    ///
    /// As [`SharedJobAuthoring::replace`].
    #[cfg(test)]
    pub(crate) fn replace_with_kinds(
        &self,
        key: &SharedJobKey,
        bytes: &[u8],
        expected: &ContentHash,
        kinds: &dyn ReleaseKinds,
    ) -> Result<Authored, AuthoringError> {
        self.replace_checked(key, bytes, expected, Some(kinds))
    }

    /// Reports the job file for `key` and its hash.
    ///
    /// Blocking: waits for the state lock, so call it from a blocking thread.
    ///
    /// # Errors
    ///
    /// Returns the [`AuthoringError`] that says why the request was refused.
    pub fn show(&self, key: &SharedJobKey) -> Result<ShownJob, AuthoringError> {
        require_usable(key)?;
        self.locked(|_jobs| {
            let file = self.the_file_tracked_as(key)?;
            Ok(ShownJob {
                key: key.clone(),
                hash: ContentHash::from(Digested(file.text.as_bytes())),
                content: file.text,
            })
        })
    }

    /// Replaces the job file for `key` with exactly `bytes`, when it still has the hash `expected`.
    ///
    /// Blocking: waits for the state lock, so call it from a blocking thread.
    ///
    /// # Errors
    ///
    /// Returns the [`AuthoringError`] that says why the request was refused.
    pub fn replace(
        &self,
        key: &SharedJobKey,
        bytes: &[u8],
        expected: &ContentHash,
    ) -> Result<Authored, AuthoringError> {
        self.replace_checked(key, bytes, expected, None)
    }

    fn replace_checked(
        &self,
        key: &SharedJobKey,
        bytes: &[u8],
        expected: &ContentHash,
        kinds: Option<&dyn ReleaseKinds>,
    ) -> Result<Authored, AuthoringError> {
        require_usable(key)?;
        let replacement = check_job_file(key, key, bytes)?;
        let authored = self.locked(|jobs| {
            let hash = ContentHash::from(Digested(bytes));
            let warnings = self.warnings(key, replacement.release.version.as_semver());
            self.require_plain_folders(key)?;
            let file = match self.the_file_tracked_as(key) {
                Err(not_found @ AuthoringError::JobNotFound { .. }) => {
                    // A link at the job's own name is never listed as a file, so it is named here for what it is.
                    Self::require_not_a_link(&self.file_path(key, &key.job))?;
                    return Err(not_found);
                }
                other => other?,
            };
            // The file written is the tracked one, whatever its name, so that is the path checked for a link and the
            // name the new bytes are checked as, which must keep the job's key.
            Self::require_not_a_link(&self.file_path(key, &file.key.job))?;
            check_job_file(&file.key, key, bytes)?;
            let current = ContentHash::from(Digested(file.text.as_bytes()));
            if &current != expected {
                return Err(AuthoringError::JobChanged {
                    key: key.to_string(),
                    current: <&str>::from(&current).to_owned(),
                });
            }
            let mut version_changed = false;
            let outcome = if file.text.as_bytes() == bytes {
                AuthoredOutcome::Unchanged
            } else {
                let before = SharedJob::try_from(SharedJobSource {
                    key: file.key.clone(),
                    text: &file.text,
                })
                .ok();
                version_changed = before
                    .as_ref()
                    .is_none_or(|job| job.release.version != replacement.release.version);
                check_replacement(
                    &HostCatalogue(&self.store_root),
                    kinds.unwrap_or(jobs),
                    JobPlace {
                        package: &key.package,
                        workflow: &key.workflow,
                    },
                    jobs.lifecycle_of(&key.package, &key.workflow, &key.job),
                    before.as_ref().map(|job| &job.release),
                    &replacement.release,
                )?;
                replace_file_atomically(&self.file_path(key, &file.key.job), bytes)?;
                AuthoredOutcome::Replaced
            };
            Ok((
                Authored {
                    outcome,
                    key: key.clone(),
                    hash,
                    warnings,
                },
                version_changed,
            ))
        })?;
        let (authored, version_changed) = authored;
        log_written(key, authored.outcome, "shared job replaced");
        Ok(if version_changed {
            self.retained(authored)
        } else {
            authored
        })
    }

    /// `authored`, with the warning retention raises when a file that was replaced left a release nothing keeps and
    /// retention could not remove it. A file replaced with what it held names nothing new, so retention is not run.
    fn retained(&self, mut authored: Authored) -> Authored {
        if authored.outcome == AuthoredOutcome::Replaced {
            let saved = SavedJobs {
                jobs_root: self.jobs_root.clone(),
                state: self.state.clone(),
            };
            if let Err(error) = retain_host(&self.store_root, &saved) {
                authored
                    .warnings
                    .push(AuthoringWarning::RetentionFailed(RetentionWarning::raised(
                        StoreKind::Host,
                        &error,
                    )));
            }
        }
        authored
    }

    /// Runs `change` with the state as it is under the lock, which it never alters.
    fn locked<T>(&self, change: impl FnOnce(&JobsState) -> Result<T, AuthoringError>) -> Result<T, AuthoringError> {
        self.state.update(|jobs| change(jobs))?
    }

    /// The warning for a release that is not installed, which the job cannot validate against yet, or for a package
    /// store that could not be read, which leaves it unknown whether it is.
    fn warnings(&self, key: &SharedJobKey, release: &Version) -> Vec<AuthoringWarning> {
        let package = key.package.clone();
        let workflow = key.workflow.clone();
        match HostCatalogue(&self.store_root).presence(&key.package, &key.workflow, release) {
            Presence::Installed => Vec::new(),
            Presence::Missing => vec![AuthoringWarning::PackageUnavailable { package, workflow }],
            Presence::Unreadable => vec![AuthoringWarning::PackageUnchecked { package, workflow }],
        }
    }

    fn file_path(&self, key: &SharedJobKey, stem: &str) -> PathBuf {
        crate::shared_jobs::job_file_path(&self.jobs_root, key, stem)
    }

    /// Refuses a link, a reparse point, or a non-folder where the package or workflow folder is, so nothing is ever
    /// read or written through one. A folder that does not exist yet is fine.
    fn require_plain_folders(&self, key: &SharedJobKey) -> Result<(), AuthoringError> {
        for (part, path) in [
            ("package", self.jobs_root.join(&key.package)),
            ("workflow", self.jobs_root.join(&key.package).join(&key.workflow)),
            (
                "jobs",
                crate::shared_jobs::job_folder(&self.jobs_root, &key.package, &key.workflow),
            ),
        ] {
            match fs::symlink_metadata(&path) {
                Ok(found) if found.is_dir() && !is_link_or_reparse_point(&found) => {}
                Ok(_) => {
                    return Err(AuthoringError::blocked(match part {
                        "package" => "a link or file stands where the package folder belongs, so nothing was written",
                        "workflow" => "a link or file stands where the workflow folder belongs, so nothing was written",
                        _ => "a link or file stands where the jobs folder belongs, so nothing was written",
                    }));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(error) => {
                    return Err(AuthoringError::unavailable(
                        "the jobs folder could not be checked",
                        &error,
                    ));
                }
            }
        }
        Ok(())
    }

    fn require_not_a_link(path: &Path) -> Result<(), AuthoringError> {
        match fs::symlink_metadata(path) {
            Ok(found) if is_link_or_reparse_point(&found) => Err(AuthoringError::blocked(
                "a link stands where the job file belongs, so nothing was written",
            )),
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(AuthoringError::unavailable("the job file could not be checked", &error)),
        }
    }

    /// Makes the jobs folder and the package and workflow folders beneath it, each only when missing and never
    /// through a link.
    fn make_folders(&self, key: &SharedJobKey) -> Result<(), AuthoringError> {
        fs::create_dir_all(&self.jobs_root)
            .map_err(|error| AuthoringError::unavailable("the jobs folder could not be made", &error))?;
        let package = self.jobs_root.join(&key.package);
        let workflow = package.join(&key.workflow);
        let jobs = crate::shared_jobs::job_folder(&self.jobs_root, &key.package, &key.workflow);
        for folder in [package, workflow, jobs] {
            match fs::create_dir(&folder) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(AuthoringError::unavailable("a job folder could not be made", &error));
                }
            }
        }
        // Revalidated now that the folders exist, at the point of the write.
        self.require_plain_folders(key)
    }

    /// Refuses a key that a standing successor of the workflow names, by the one rule successor making shares, since
    /// the key then belongs to the chain that job continues.
    fn require_no_standing_successor(&self, jobs: &JobsState, key: &SharedJobKey) -> Result<(), AuthoringError> {
        let files = list_shared_job_files(&self.jobs_root)
            .map_err(|error| AuthoringError::unavailable("the jobs folder could not be listed", &error))?;
        let successors = standing_successors(
            jobs,
            StandingScope {
                package: &key.package,
                workflow: &key.workflow,
            },
            &files,
            &key.job,
        );
        if successors.is_empty() {
            Ok(())
        } else {
            Err(AuthoringError::JobKeyTaken {
                key: key.to_string(),
                files: format!("its successor {}", successors.join(", ")),
            })
        }
    }

    /// Every job file whose tracked key is `key`.
    fn files_tracked_as(&self, key: &SharedJobKey) -> Result<Vec<SharedJobFile>, AuthoringError> {
        let files = list_shared_job_files(&self.jobs_root)
            .map_err(|error| AuthoringError::unavailable("the jobs folder could not be listed", &error))?;
        Ok(files
            .into_iter()
            .filter(|file| file.tracked_key().as_ref() == Some(key))
            .collect())
    }

    /// The one job file `key` is tracked under. A key with no usable file is refused as an unusable file when one is
    /// there under the key's own name, and as not found otherwise.
    fn the_file_tracked_as(&self, key: &SharedJobKey) -> Result<SharedJobFile, AuthoringError> {
        let entries = list_shared_job_entries(&self.jobs_root)
            .map_err(|error| AuthoringError::unavailable("the jobs folder could not be listed", &error))?;
        let JobFilesFor { mut tracked, unusable } = find_job_files(entries, key);
        let not_found = || {
            unusable.map_or_else(
                || AuthoringError::JobNotFound { key: key.to_string() },
                AuthoringError::unusable,
            )
        };
        // A file under the key's own name that cannot be used is a second claimant on the key whenever a usable file is
        // tracked under it, so neither can be chosen between.
        if unusable.is_some() && !tracked.is_empty() {
            return Err(AuthoringError::JobKeyTaken {
                key: key.to_string(),
                files: format!("{} and {}.toml", stems(&tracked), key.job),
            });
        }
        match tracked.len() {
            0 => Err(not_found()),
            1 => tracked.pop().ok_or_else(not_found),
            _ => Err(AuthoringError::JobKeyTaken {
                key: key.to_string(),
                files: stems(&tracked),
            }),
        }
    }
}

/// The host package store as the one question authoring asks of it: whether a release of a workflow is installed.
struct HostCatalogue<'a>(&'a Path);

impl InstalledReleases for HostCatalogue<'_> {
    fn presence(&self, package: &str, workflow: &str, release: &Version) -> Presence {
        let Ok(inventory) = package_store::inventory(self.0) else {
            return Presence::Unreadable;
        };
        let installed = inventory.releases.iter().any(|candidate| {
            candidate.manifest.identity.name.as_str() == package
                && candidate.manifest.identity.version == *release
                && candidate.kind.workflows().iter().any(|listed| listed == workflow)
        });
        if installed {
            Presence::Installed
        } else if inventory
            .unreadable
            .iter()
            .any(|damaged| damaged.name == package && damaged.version == release.to_string())
        {
            // A release of that version which cannot be read may be the one with the workflow.
            Presence::Unreadable
        } else {
            Presence::Missing
        }
    }
}

/// Logs a create or replace at INFO only when bytes were written; an unchanged file is a DEBUG note at most.
fn log_written(key: &SharedJobKey, outcome: AuthoredOutcome, message: &'static str) {
    if outcome == AuthoredOutcome::Unchanged {
        tracing::debug!(package = %key.package, workflow = %key.workflow, job = %key.job, "shared job already held these bytes, so nothing was written");
    } else {
        tracing::info!(
            package = %key.package, workflow = %key.workflow, job = %key.job, outcome = ?outcome, "{message}"
        );
    }
}

/// The file names of `files`, for a message.
pub(crate) fn stems(files: &[SharedJobFile]) -> String {
    files
        .iter()
        .map(|file| format!("{}.toml", file.key.job))
        .collect::<Vec<_>>()
        .join(" and ")
}

#[path = "shared_job_listing.rs"]
mod listing;
#[path = "shared_successor.rs"]
mod successors;
pub(crate) use listing::fill_replacements;
pub use listing::{JobListing, ListingProblem};

#[cfg(test)]
#[path = "shared_job_authoring_test.rs"]
mod tests;

#[cfg(test)]
#[path = "shared_job_authoring_files_test.rs"]
mod files_tests;

#[cfg(test)]
#[path = "shared_job_authoring_show_test.rs"]
mod show_tests;

#[cfg(test)]
#[path = "shared_job_authoring_retention_test.rs"]
mod retention_tests;
