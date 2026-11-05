//! A job for a workflow installed in the host package store.
//!
//! It is one file at `{jobs root}/{package}/{workflow}/{job}.toml`, beside the package store. It needs no vault and no
//! MCP server to be loaded, holds no path or credential, and defines no schedule.

use std::io;
use std::path::{Path, PathBuf};

use contextos_workflows::{JobFileRelease, JobFileText, JobReleaseError, LogLevel};
use serde::Deserialize;
use serde_json::{Map, Value};
use thiserror::Error;

use crate::bounded_read::{BoundedReadError, read_text_within};
pub use crate::shared_claim::SharedJobKey;

/// A shared job manifest is a few lines of TOML; anything larger is not one.
pub const MAX_JOB_MANIFEST_BYTES: u64 = 64 * 1024;

/// One loaded shared job.
#[derive(Clone, Debug, PartialEq)]
pub struct SharedJob {
    pub key: SharedJobKey,
    pub name: String,
    /// How long a run of the job may take. Required: a job never takes its workflow's default.
    pub timeout_secs: u64,
    /// The release the job runs and, for a successor, the job it succeeds.
    pub release: JobFileRelease,
    /// The values the run supplies, not yet validated against the workflow's declaration.
    pub inputs: Map<String, Value>,
    /// The minimum log level this job sets, overriding its workflow's and the host's.
    pub log_level: Option<LogLevel>,
    /// Whether the job file says authoring is finished, which lets validation run automatically and grants nothing
    /// else.
    pub ready: bool,
}

/// A job manifest's text and the identity its file path gives it.
pub struct SharedJobSource<'a> {
    pub key: SharedJobKey,
    pub text: &'a str,
}

/// `deny_unknown_fields`: any other key, an `owner` or a `schedule` included, fails that job's load.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SharedJobRaw {
    name: String,
    #[serde(default)]
    slug: Option<String>,
    /// Required: a job states how long it may run.
    timeout_secs: std::num::NonZeroU64,
    /// Required: the exact release of the package the job runs.
    version: String,
    /// The key of the job of the same workflow this one succeeds.
    #[serde(default)]
    predecessor: Option<String>,
    #[serde(default)]
    inputs: Map<String, Value>,
    #[serde(default)]
    log_level: Option<LogLevel>,
    #[serde(default)]
    ready: bool,
}

#[derive(Debug, Error)]
pub enum SharedJobError {
    #[error(
        "no shared job {} exists",
        crate::trailing_path::Trailing { path: std::path::Path::new(path), count: 3 }
    )]
    NotFound { path: String },
    #[error("the job file could not be read: {0}")]
    Io(#[from] io::Error),
    #[error("the job file is unusable: {0}")]
    Unusable(#[source] BoundedReadError),
    #[error("the job manifest does not match the required schema: {0}")]
    Schema(String),
    #[error("name must not be empty")]
    BlankName,
    #[error("{value:?} is not a usable slug: expected lowercase letters, digits, and hyphens")]
    InvalidSlug { value: String },
    #[error("{0}")]
    InvalidRelease(#[from] JobReleaseError),
}

impl TryFrom<SharedJobSource<'_>> for SharedJob {
    type Error = SharedJobError;

    fn try_from(source: SharedJobSource<'_>) -> Result<Self, SharedJobError> {
        let raw: SharedJobRaw =
            toml::from_str(source.text).map_err(|error| SharedJobError::Schema(schema_message(&error, source.text)))?;
        if raw.name.trim().is_empty() {
            return Err(SharedJobError::BlankName);
        }
        let mut key = source.key;
        if let Some(slug) = raw.slug {
            key.job = slug;
        }
        require_job_slug(&key.job)?;
        let release = JobFileRelease::try_from(JobFileText {
            version: &raw.version,
            predecessor: raw.predecessor.as_deref(),
        })?;
        Ok(Self {
            key,
            name: raw.name,
            timeout_secs: raw.timeout_secs.get(),
            release,
            inputs: raw.inputs,
            log_level: raw.log_level,
            ready: raw.ready,
        })
    }
}

/// The folder name, below a workflow's folder, that holds its job files.
pub(crate) const JOBS_FOLDER: &str = "jobs";

/// The folder that holds a workflow's job files on the host: `{jobs_root}/{package}/{workflow}/jobs`.
pub(crate) fn job_folder(jobs_root: &Path, package: &str, workflow: &str) -> PathBuf {
    jobs_root.join(package).join(workflow).join(JOBS_FOLDER)
}

/// The file of the job `key` names: `{jobs_root}/{package}/{workflow}/jobs/{stem}.toml`.
pub(crate) fn job_file_path(jobs_root: &Path, key: &SharedJobKey, stem: &str) -> PathBuf {
    job_folder(jobs_root, &key.package, &key.workflow).join(format!("{stem}.toml"))
}

/// Reads the job `key` names from `jobs_root`.
///
/// # Errors
///
/// Returns [`SharedJobError::InvalidSlug`] when a component of `key` could leave `jobs_root`,
/// [`SharedJobError::NotFound`] when there is no such file, and the manifest's own validation errors otherwise.
pub fn load_shared_job(jobs_root: &Path, key: &SharedJobKey) -> Result<SharedJob, SharedJobError> {
    load_shared_job_file(jobs_root, key).map(|(job, _)| job)
}

/// Reads the job `key` names from `jobs_root`, with the text of its file, which a job's version is taken from.
///
/// # Errors
///
/// As [`load_shared_job`].
pub fn load_shared_job_file(jobs_root: &Path, key: &SharedJobKey) -> Result<(SharedJob, String), SharedJobError> {
    for component in [&key.package, &key.workflow] {
        require_slug(component)?;
    }
    require_job_slug(&key.job)?;
    let path = job_file_path(jobs_root, key, &key.job);
    let text = match read_text_within(&path, MAX_JOB_MANIFEST_BYTES) {
        Ok(text) => text,
        Err(BoundedReadError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Err(SharedJobError::NotFound {
                path: format!("{}/{}/{}", key.package, key.workflow, key.job),
            });
        }
        Err(BoundedReadError::Io(error)) => return Err(error.into()),
        Err(other) => return Err(SharedJobError::Unusable(other)),
    };
    let job = SharedJob::try_from(SharedJobSource {
        key: key.clone(),
        text: &text,
    })?;
    Ok((job, text))
}

/// The parser's own message for `error`, with the line and column it points at when it knows them. The source line is
/// never repeated, so the message carries nothing from the file beyond what the parser's message names.
pub(crate) fn schema_message(error: &toml::de::Error, text: &str) -> String {
    match error.span().and_then(|span| text.get(..span.start)) {
        Some(before) => {
            let line = before.matches('\n').count().saturating_add(1);
            let column = before
                .rsplit('\n')
                .next()
                .map_or(0, |current| current.chars().count())
                .saturating_add(1);
            format!("{} (line {line}, column {column})", error.message())
        }
        None => error.message().to_owned(),
    }
}

/// One job file as it sits in the store, before it is checked: the key its path gives it, and its text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SharedJobFile {
    pub key: SharedJobKey,
    pub text: String,
}

impl SharedJobFile {
    /// The key the job is tracked under in the host state: the slug its manifest names, or the file stem when it names
    /// none. `None` when the manifest names a slug that is not URL-safe, so a job that cannot be loaded never gets a
    /// state entry of its own.
    #[must_use]
    pub fn tracked_key(&self) -> Option<SharedJobKey> {
        let named = toml::from_str::<toml::Table>(&self.text)
            .ok()
            .and_then(|table| table.get("slug").and_then(toml::Value::as_str).map(str::to_owned));
        let mut key = self.key.clone();
        if let Some(slug) = named {
            require_slug(&slug).ok()?;
            key.job = slug;
        }
        Some(key)
    }
}

/// Why a regular `*.toml` file in the jobs folder cannot be used as a job file. Never holds content or a path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnusableReason {
    /// Larger than [`MAX_JOB_MANIFEST_BYTES`].
    TooLarge,
    /// Not UTF-8 text.
    NotText,
    /// The file could not be read.
    Unreadable(io::ErrorKind),
}

impl std::fmt::Display for UnusableReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => formatter.write_str("too large"),
            Self::NotText => formatter.write_str("not UTF-8"),
            Self::Unreadable(kind) => write!(formatter, "unreadable: {kind}"),
        }
    }
}

/// One regular `*.toml` file as it sits in the jobs folder, whether or not it can be used.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JobFileEntry {
    /// A file with a URL-safe name whose text was read.
    Usable(SharedJobFile),
    /// A file with a URL-safe name that cannot be used. `key` is the key its name gives it.
    Unusable { key: SharedJobKey, reason: UnusableReason },
    /// A file whose stem is not a lowercase slug, so it can never be a job. `key.job` is the stem as found.
    Misnamed { key: SharedJobKey },
}

/// Every regular `*.toml` file under `jobs_root`, as `{package}/{workflow}/{file}.toml`, in path order, including the
/// ones that cannot be used, each with the reason. Folders whose name is not URL-safe and anything at another depth are
/// left out. A store with no `jobs_root` has no jobs.
///
/// # Errors
///
/// Returns the filesystem error when a directory that exists cannot be listed.
pub fn list_shared_job_entries(jobs_root: &Path) -> io::Result<Vec<JobFileEntry>> {
    let mut found = Vec::new();
    for package in plain_entries(jobs_root, EntryKind::Directory)? {
        let package_dir = jobs_root.join(&package);
        for workflow in plain_entries(&package_dir, EntryKind::Directory)? {
            let workflow_dir = job_folder(jobs_root, &package, &workflow);
            for file in plain_entries(&workflow_dir, EntryKind::File)? {
                let Some(job) = file.strip_suffix(".toml") else {
                    continue;
                };
                let key = SharedJobKey {
                    package: package.clone(),
                    workflow: workflow.clone(),
                    job: job.to_owned(),
                };
                if require_job_slug(job).is_err() {
                    found.push(JobFileEntry::Misnamed { key });
                    continue;
                }
                found.push(
                    match read_text_within(&workflow_dir.join(&file), MAX_JOB_MANIFEST_BYTES) {
                        Ok(text) => JobFileEntry::Usable(SharedJobFile { key, text }),
                        Err(error) => JobFileEntry::Unusable {
                            key,
                            reason: UnusableReason::from(&error),
                        },
                    },
                );
            }
        }
    }
    Ok(found)
}

impl From<&BoundedReadError> for UnusableReason {
    fn from(error: &BoundedReadError) -> Self {
        match error {
            BoundedReadError::TooLarge { .. } => Self::TooLarge,
            BoundedReadError::NotText { .. } => Self::NotText,
            BoundedReadError::Io(error) => Self::Unreadable(error.kind()),
        }
    }
}

/// The files that matter to one job key: every usable file tracked under it, and the unusable file its own name gives
/// it, if there is one.
#[derive(Debug)]
pub struct JobFilesFor {
    pub tracked: Vec<SharedJobFile>,
    pub unusable: Option<UnusableReason>,
}

/// Picks out of `entries` the files `key` is tracked under, and the unusable file named `{key.job}.toml`, which has no
/// manifest to say it is tracked under anything else.
#[must_use]
pub fn find_job_files(entries: Vec<JobFileEntry>, key: &SharedJobKey) -> JobFilesFor {
    let mut found = JobFilesFor {
        tracked: Vec::new(),
        unusable: None,
    };
    for entry in entries {
        match entry {
            JobFileEntry::Usable(file) if file.tracked_key().as_ref() == Some(key) => found.tracked.push(file),
            JobFileEntry::Unusable { key: named, reason } if &named == key => found.unusable = Some(reason),
            JobFileEntry::Usable(_) | JobFileEntry::Unusable { .. } | JobFileEntry::Misnamed { .. } => {}
        }
    }
    found
}

/// Every job file under `jobs_root` that can be a job, as `{package}/{workflow}/{job}.toml`, in path order. A file too
/// large to be a manifest, one that cannot be read, a name that is not URL-safe, and anything at another depth is left
/// out, because none of them can be a job; [`list_shared_job_entries`] reports them. A store with no `jobs_root` has no
/// jobs.
///
/// # Errors
///
/// Returns the filesystem error when a directory that exists cannot be listed.
pub fn list_shared_job_files(jobs_root: &Path) -> io::Result<Vec<SharedJobFile>> {
    Ok(list_shared_job_entries(jobs_root)?
        .into_iter()
        .filter_map(|entry| match entry {
            JobFileEntry::Usable(file) => Some(file),
            JobFileEntry::Unusable { .. } | JobFileEntry::Misnamed { .. } => None,
        })
        .collect())
}

/// `files` without any whose tracked key another file also has, which would give two files with different versions one
/// state entry, each pass taking the other's version for a new one. Both are set aside and a warning names the key, so
/// the operator renames one; neither is validated or scheduled meanwhile.
#[must_use]
pub fn without_clashing_keys(files: Vec<SharedJobFile>) -> Vec<SharedJobFile> {
    let mut seen: std::collections::BTreeMap<(String, String, String), usize> = std::collections::BTreeMap::new();
    for file in &files {
        if let Some(key) = file.tracked_key() {
            *seen.entry((key.package, key.workflow, key.job)).or_default() += 1;
        }
    }
    files
        .into_iter()
        .filter(|file| {
            let Some(key) = file.tracked_key() else {
                return true;
            };
            let clash = seen
                .get(&(key.package.clone(), key.workflow.clone(), key.job.clone()))
                .is_some_and(|count| *count > 1);
            if clash {
                tracing::warn!(package = %key.package, workflow = %key.workflow, job = %key.job, file = %file.key.job, "two shared job files share one job key, so this one is not tracked until one is renamed");
            }
            !clash
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq)]
enum EntryKind {
    Directory,
    File,
}

/// The names of the directories directly inside `directory` that are URL-safe, or of the regular `*.toml` files in it
/// whatever their stem, sorted. A directory that does not exist has none. A link is neither.
fn plain_entries(directory: &Path, kind: EntryKind) -> io::Result<Vec<String>> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let wanted = match kind {
            EntryKind::Directory => file_type.is_dir() && require_slug(&name).is_ok(),
            EntryKind::File => file_type.is_file() && name.strip_suffix(".toml").is_some(),
        };
        if wanted {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

/// As [`require_slug`], and not a word the state file uses for a workflow's own entries, which no job may be named.
pub(crate) fn require_job_slug(value: &str) -> Result<(), SharedJobError> {
    require_slug(value)?;
    if contextos_workflows::is_reserved_job_slug(value) {
        return Err(SharedJobError::InvalidSlug {
            value: value.to_owned(),
        });
    }
    Ok(())
}

pub(crate) fn require_slug(value: &str) -> Result<(), SharedJobError> {
    let usable = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if usable {
        Ok(())
    } else {
        Err(SharedJobError::InvalidSlug {
            value: value.to_owned(),
        })
    }
}

#[cfg(test)]
#[path = "shared_jobs_test.rs"]
mod tests;
