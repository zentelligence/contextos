//! Authoring a vault job file from the command line: create, show, replace, and list.
//!
//! A vault job is one file at `registry/workflows/{package}/{workflow}/jobs/{job}.toml`, written only through the vault's primary
//! MCP server, never through a host path. Each command applies the same load, the same 64 KiB limit, the same refusals,
//! and the same release rules by stage as its shared form, so a job authored here is checked exactly as a placed one is.
//!
//! Every write is made by the server with the conflict check it gives: a create is refused when anything exists at the
//! path, and a replace when the file no longer holds what was read. The job state is read, never written, so a state
//! change made between the checks and the write is seen by the next pass, which validates the file as it then stands.
//! Whether a release is installed is read from its manifest in the vault's package store; reading a release to verify
//! it is the package engine's, and validation does that.

use contextos_core::{ContentHash, UnixText};
use contextos_workflows::package::WORKFLOWS_FILE_NAME;
use contextos_workflows::{
    InstalledReleases, JobLifecycle, JobPlace, Presence, ReleaseKinds, ReleaseRefusal, SourceText, StoreKind,
    ValidationCode, WorkflowsFile, check_new_job, check_replacement,
};
use semver::Version;

use crate::jobs::{
    JobLoadOutcome, RegisteredJob, RegisteredWorkflow, discover_workflows, is_url_safe_slug, job_from_text,
    list_job_files,
};
use crate::mcp_client::{McpCallError, McpClient};
use crate::shared_claim::SharedJobKey;
use crate::shared_job_authoring::{
    Authored, AuthoredOutcome, AuthoringError, AuthoringWarning, Digested, JobListing, ListingProblem, ShownJob,
    require_usable, stems,
};
use crate::shared_jobs::{
    JobFileEntry, JobFilesFor, MAX_JOB_MANIFEST_BYTES, SharedJobFile, UnusableReason, find_job_files,
};
use crate::store_retention::{RetentionWarning, retain_vault};
use crate::successor_plan::{StandingScope, standing_successors};
use crate::vault_content::{AttachedRead, read_attached};
use crate::vault_relative_path::VaultRelativePath;
use crate::vault_write::{ALREADY_EXISTS, CONFLICT, WriteOutcome, write_text};

/// The folder a vault keeps a workflow's releases under, and the file that says one is installed.
const PACKAGES_FOLDER: &str = ".contextos/packages";

/// The authoring commands over one vault's jobs, through its primary server.
///
/// A job is addressed by its package, its workflow's slug, and the job's key, as a shared job is.
pub struct VaultJobAuthoring<'a> {
    /// A session to the vault's primary server, which every read and write goes through.
    pub client: &'a McpClient,
    /// The vault's name.
    pub vault: &'a str,
    /// The names of the servers `web.toml` configures, which a workflow may name in `[mcp_servers]` and `[mcp_tools]`.
    pub known_servers: &'a [String],
}

/// An outage of the vault's server: the request is refused and nothing is known to have been written.
fn unreachable(_: McpCallError) -> AuthoringError {
    AuthoringError::JobStoreUnavailable {
        action: "the vault could not be reached",
        kind: None,
    }
}

/// An answer to whether a release is installed, already known, for the pure rules to ask.
struct KnownPresence(Presence);

impl InstalledReleases for KnownPresence {
    fn presence(&self, _package: &str, _workflow: &str, _release: &Version) -> Presence {
        self.0
    }
}

/// A workflow and the jobs filed under it.
struct Located {
    workflow: RegisteredWorkflow,
}

impl Located {
    /// The package the workflow belongs to, which is its name in the vault's package store.
    fn package(&self) -> &str {
        &self.workflow.package
    }

    /// The key a job of this workflow has in results: its package, the workflow, the job.
    fn key(&self, job: &str) -> SharedJobKey {
        SharedJobKey {
            package: self.workflow.package.clone(),
            workflow: self.workflow.slug.clone(),
            job: job.to_owned(),
        }
    }

    fn path(&self, stem: &str) -> String {
        format!("registry/workflows/{}/jobs/{stem}.toml", self.workflow.folder())
    }
}

impl VaultJobAuthoring<'_> {
    /// Creates the job file for `job` of `workflow` of `package` with exactly `bytes`, never overwriting.
    ///
    /// # Errors
    ///
    /// Returns the [`AuthoringError`] that says why the request was refused.
    pub async fn create(
        &self,
        package: &str,
        workflow: &str,
        job: &str,
        bytes: &[u8],
    ) -> Result<Authored, AuthoringError> {
        let asked = SharedJobKey {
            package: package.to_owned(),
            workflow: workflow.to_owned(),
            job: job.to_owned(),
        };
        require_usable(&asked)?;
        let stored = unix_text(text_within_limit(bytes)?);
        let (text, bytes) = (stored.as_str(), stored.as_bytes());
        let Some(located) = self.find(package, workflow).await? else {
            // The file is checked as far as it can be without the workflow, so a bad file is reported as one first.
            let release = check_without_workflow(package, workflow, job, text)?;
            return Err(ReleaseRefusal::NotInstalled {
                package: package.to_owned(),
                workflow: workflow.to_owned(),
                version: release.version.as_semver().clone(),
            }
            .into());
        };
        let loaded = check_text(&located.workflow, job, job, text)?;
        let key = located.key(job);
        let hash = ContentHash::from(Digested(bytes));
        let entries = self.entries(&located).await?;
        if let Some(existing) = identical_or_different(&entries, job, bytes) {
            return existing.map(|()| Authored {
                outcome: AuthoredOutcome::Unchanged,
                key,
                hash,
                warnings: Vec::new(),
            });
        }
        let JobFilesFor { tracked, .. } = find_job_files(entries.clone(), &key);
        if !tracked.is_empty() {
            return Err(AuthoringError::JobKeyTaken {
                key: key.to_string(),
                files: stems(&tracked),
            });
        }
        let state = self.state().await?;
        if state
            .job(&located.workflow.package, &located.workflow.slug, job)
            .is_some()
        {
            return Err(AuthoringError::JobStateRemains { key: key.to_string() });
        }
        Self::require_no_standing_successor(&located, &state, &entries, job)?;
        let presence = self.presence(&located, loaded.release.version.as_semver()).await;
        check_new_job(
            &KnownPresence(presence),
            &state,
            JobPlace {
                package: located.package(),
                workflow: &located.workflow.slug,
            },
            &loaded.release,
        )?;
        self.write(&located, job, text, None).await?;
        tracing::info!(vault = %self.vault, package = %located.workflow.package, workflow = %located.workflow.slug, job = %job, "vault job created");
        Ok(Authored {
            outcome: AuthoredOutcome::Created,
            key,
            hash,
            warnings: Vec::new(),
        })
    }

    /// Reports the job file for `job` of `workflow` of `package` and its hash.
    ///
    /// # Errors
    ///
    /// Returns the [`AuthoringError`] that says why the request was refused.
    pub async fn show(&self, package: &str, workflow: &str, job: &str) -> Result<ShownJob, AuthoringError> {
        let asked = SharedJobKey {
            package: package.to_owned(),
            workflow: workflow.to_owned(),
            job: job.to_owned(),
        };
        require_usable(&asked)?;
        let located = self.require(&asked).await?;
        let key = located.key(job);
        let entries = self.entries(&located).await?;
        let file = the_file_tracked_as(entries, &key)?;
        Ok(ShownJob {
            key,
            hash: ContentHash::from(Digested(file.text.as_bytes())),
            content: file.text,
        })
    }

    /// Replaces the job file for `job` of `workflow` of `package` with exactly `bytes`, when it still has the hash `expected`.
    ///
    /// # Errors
    ///
    /// Returns the [`AuthoringError`] that says why the request was refused.
    pub async fn replace(
        &self,
        package: &str,
        workflow: &str,
        job: &str,
        bytes: &[u8],
        expected: &ContentHash,
    ) -> Result<Authored, AuthoringError> {
        let asked = SharedJobKey {
            package: package.to_owned(),
            workflow: workflow.to_owned(),
            job: job.to_owned(),
        };
        require_usable(&asked)?;
        let stored = unix_text(text_within_limit(bytes)?);
        let (text, bytes) = (stored.as_str(), stored.as_bytes());
        let located = self.require(&asked).await?;
        let key = located.key(job);
        let entries = self.entries(&located).await?;
        let file = the_file_tracked_as(entries, &key)?;
        // The file written is the tracked one, whatever its name, so that is the name the new bytes are checked as, and
        // they must keep the job's key.
        let replacement = check_text(&located.workflow, &file.key.job, job, text)?;
        let hash = ContentHash::from(Digested(bytes));
        let current = ContentHash::from(Digested(file.text.as_bytes()));
        if &current != expected {
            return Err(AuthoringError::JobChanged {
                key: key.to_string(),
                current: <&str>::from(&current).to_owned(),
            });
        }
        let presence = self.presence(&located, replacement.release.version.as_semver()).await;
        let warnings = match presence {
            Presence::Installed => Vec::new(),
            Presence::Missing => vec![AuthoringWarning::PackageUnavailable {
                package: key.package.clone(),
                workflow: key.workflow.clone(),
            }],
            Presence::Unreadable => vec![AuthoringWarning::PackageUnchecked {
                package: key.package.clone(),
                workflow: key.workflow.clone(),
            }],
        };
        if file.text.as_bytes() == bytes {
            return Ok(Authored {
                outcome: AuthoredOutcome::Unchanged,
                key,
                hash,
                warnings,
            });
        }
        let state = self.state().await?;
        let before = match job_from_text(&located.workflow, &format!("{}.toml", file.key.job), &file.text, &[]) {
            JobLoadOutcome::Valid(before) => Some(before),
            JobLoadOutcome::Invalid(_) => None,
        };
        check_replacement(
            &KnownPresence(presence),
            &state,
            JobPlace {
                package: located.package(),
                workflow: &located.workflow.slug,
            },
            state.lifecycle_of(&located.workflow.package, &located.workflow.slug, job),
            before.as_ref().map(|before| &before.release),
            &replacement.release,
        )?;
        self.write(&located, &file.key.job, text, Some(&current)).await?;
        tracing::info!(vault = %self.vault, package = %located.workflow.package, workflow = %located.workflow.slug, job = %job, "vault job replaced");
        let mut warnings = warnings;
        // A changed `version` can leave a release nothing keeps. A retention that cannot finish is a warning: the file is replaced.
        if before.is_none_or(|before| before.release.version != replacement.release.version)
            && let Err(error) = retain_vault(self.client, self.vault).await
        {
            warnings.push(AuthoringWarning::RetentionFailed(RetentionWarning::raised(
                StoreKind::Vault,
                &error,
            )));
        }
        Ok(Authored {
            outcome: AuthoredOutcome::Replaced,
            key,
            hash,
            warnings,
        })
    }

    /// Lists every job file of the vault and every stored state entry once.
    ///
    /// # Errors
    ///
    /// Returns the [`AuthoringError`] that says why the listing could not be made.
    pub async fn list(&self) -> Result<Vec<JobListing>, AuthoringError> {
        let workflows = discover_workflows(self.client, self.vault, self.known_servers)
            .await
            .map_err(unreachable)?;
        let state = self.state().await?;
        let mut rows = Vec::new();
        for workflow in workflows {
            let located = Located { workflow };
            let entries = self.entries(&located).await?;
            for entry in &entries {
                rows.push(listing_of(&located, entry, &state));
            }
        }
        mark_shared_keys(&mut rows);
        fill_successors(&mut rows);
        for (package, workflow_slug, job_slug) in state.recorded_jobs() {
            let Some(stored) = state.job(package, workflow_slug, job_slug) else {
                continue;
            };
            if rows
                .iter()
                .any(|row| row.key.package == package && row.key.workflow == workflow_slug && row.key.job == job_slug)
            {
                continue;
            }
            rows.push(JobListing {
                key: SharedJobKey {
                    package: package.to_owned(),
                    workflow: workflow_slug.to_owned(),
                    job: job_slug.to_owned(),
                },
                file: None,
                name: None,
                lifecycle: state.lifecycle_of(package, workflow_slug, job_slug),
                schedule: stored.schedule().map(str::to_owned),
                ready: None,
                version: None,
                release_kind: None,
                predecessor: None,
                successor: None,
                replaced_by: None,
                hash: None,
                problems: vec![ListingProblem {
                    code: "workflow/job-not-found".to_owned(),
                    message: "stored state has no job file".to_owned(),
                }],
                suspension: stored.suspension().cloned(),
            });
        }
        crate::shared_job_authoring::fill_replacements(&mut rows, &state);
        rows.sort_by(|a, b| {
            (&a.key.package, &a.key.workflow, &a.key.job, &a.file).cmp(&(
                &b.key.package,
                &b.key.workflow,
                &b.key.job,
                &b.file,
            ))
        });
        Ok(rows)
    }

    /// The registered workflow `workflow` of the package `package`, when the vault has one.
    async fn find(&self, package: &str, workflow: &str) -> Result<Option<Located>, AuthoringError> {
        let workflows = discover_workflows(self.client, self.vault, self.known_servers)
            .await
            .map_err(unreachable)?;
        Ok(workflows
            .into_iter()
            .find(|candidate| candidate.package == package && candidate.slug == workflow)
            .map(|workflow| Located { workflow }))
    }

    /// As [`Self::find`], and a workflow the vault does not have is a job that is not found.
    async fn require(&self, asked: &SharedJobKey) -> Result<Located, AuthoringError> {
        self.find(&asked.package, &asked.workflow)
            .await?
            .ok_or_else(|| AuthoringError::JobNotFound { key: asked.to_string() })
    }

    /// Every file in the workflow's `jobs` folder, as read.
    async fn entries(&self, located: &Located) -> Result<Vec<JobFileEntry>, AuthoringError> {
        let names = list_job_files(self.client, self.vault, located.workflow.folder())
            .await
            .map_err(unreachable)?;
        let mut entries = Vec::with_capacity(names.len());
        for name in names {
            let stem = name.strip_suffix(".toml").unwrap_or(&name).to_owned();
            let key = located.key(&stem);
            if !is_url_safe_slug(&stem) {
                entries.push(JobFileEntry::Misnamed { key });
                continue;
            }
            let path = format!("{}://{}", self.vault, located.path(&stem));
            entries.push(match read_attached(self.client, &path).await.map_err(unreachable)? {
                AttachedRead::Text(read)
                    if u64::try_from(read.text.len()).is_ok_and(|length| length <= MAX_JOB_MANIFEST_BYTES) =>
                {
                    JobFileEntry::Usable(SharedJobFile { key, text: read.text })
                }
                AttachedRead::Text(_) => JobFileEntry::Unusable {
                    key,
                    reason: UnusableReason::TooLarge,
                },
                AttachedRead::NotText => JobFileEntry::Unusable {
                    key,
                    reason: UnusableReason::NotText,
                },
                AttachedRead::Refused { .. } => JobFileEntry::Unusable {
                    key,
                    reason: UnusableReason::Unreadable(std::io::ErrorKind::Other),
                },
            });
        }
        Ok(entries)
    }

    /// The vault's job state as it is now.
    async fn state(&self) -> Result<contextos_workflows::JobsState, AuthoringError> {
        crate::scheduler::read_state(self.client, self.vault)
            .await
            .map(|snapshot| snapshot.value)
            .map_err(|error| match error {
                crate::scheduler::StateError::Persistence(crate::vault_state::FavouriteError::Malformed) => {
                    AuthoringError::StateMalformed {
                        reason: "the vault's job state does not parse".to_owned(),
                    }
                }
                _ => AuthoringError::StateUnavailable {
                    action: "the vault's job state could not be read",
                    kind: None,
                },
            })
    }

    /// Whether the release is in the vault's package store and holds the workflow, as the host's store judges it: the
    /// release's manifest is there and its `workflows.toml` lists the workflow.
    async fn presence(&self, located: &Located, release: &Version) -> Presence {
        let folder = format!("{}://{PACKAGES_FOLDER}/{}/{release}", self.vault, located.package());
        match Self::read_release_file(self.client, &format!("{folder}/manifest.toml")).await {
            Ok(Some(_)) => {}
            Ok(None) => return Presence::Missing,
            Err(unreadable) => return unreadable,
        }
        let listed = match Self::read_release_file(self.client, &format!("{folder}/{WORKFLOWS_FILE_NAME}")).await {
            Ok(Some(text)) => text,
            // A release with no `workflows.toml` is a library, which holds no workflow.
            Ok(None) => return Presence::Missing,
            Err(unreadable) => return unreadable,
        };
        match WorkflowsFile::try_from(SourceText(&listed)) {
            Ok(file) if file.workflows().iter().any(|name| name == &located.workflow.slug) => Presence::Installed,
            Ok(_) => Presence::Missing,
            Err(_) => Presence::Unreadable,
        }
    }

    /// The text of one file of a stored release: `Ok(None)` when it is not there and `Err` when it could not be read.
    async fn read_release_file(client: &McpClient, path: &str) -> Result<Option<String>, Presence> {
        match read_attached(client, path).await {
            Ok(AttachedRead::Text(attached)) => Ok(Some(attached.text)),
            Ok(AttachedRead::Refused { code }) if code.as_deref() == Some("path/not-found") => Ok(None),
            Ok(AttachedRead::Refused { .. } | AttachedRead::NotText) | Err(_) => Err(Presence::Unreadable),
        }
    }

    /// Refuses a key that a standing successor of the workflow names, by the one rule successor making shares.
    fn require_no_standing_successor(
        located: &Located,
        state: &contextos_workflows::JobsState,
        entries: &[JobFileEntry],
        job: &str,
    ) -> Result<(), AuthoringError> {
        let files = entries.iter().filter_map(|entry| match entry {
            JobFileEntry::Usable(file) => Some(file),
            JobFileEntry::Unusable { .. } | JobFileEntry::Misnamed { .. } => None,
        });
        let successors = standing_successors(
            state,
            StandingScope {
                package: &located.workflow.package,
                workflow: &located.workflow.slug,
            },
            files,
            job,
        );
        if successors.is_empty() {
            Ok(())
        } else {
            Err(AuthoringError::JobKeyTaken {
                key: located.key(job).to_string(),
                files: format!("its successor {}", successors.join(", ")),
            })
        }
    }

    /// Writes `text` as the file named `stem`, creating it when `replacing` is `None` and otherwise replacing the file
    /// that held `replacing`.
    async fn write(
        &self,
        located: &Located,
        stem: &str,
        text: &str,
        replacing: Option<&ContentHash>,
    ) -> Result<(), AuthoringError> {
        let Ok(path) = VaultRelativePath::try_from(located.path(stem).as_str()) else {
            return Err(AuthoringError::JobKeyInvalid { part: "job" });
        };
        let outcome = write_text(self.client, self.vault, &path, text, replacing.map(<&str>::from))
            .await
            .map_err(unreachable)?;
        match outcome {
            WriteOutcome::Written => Ok(()),
            WriteOutcome::Refused { code } => Err(match code.as_deref() {
                Some(ALREADY_EXISTS) => AuthoringError::JobExists {
                    key: located.key(stem).to_string(),
                },
                Some(CONFLICT) => AuthoringError::JobChanged {
                    key: located.key(stem).to_string(),
                    current: "unknown: the file changed while the request was made".to_owned(),
                },
                _ => AuthoringError::JobStoreUnavailable {
                    action: "the vault refused the write",
                    kind: None,
                },
            }),
        }
    }
}

/// `bytes` as text, within the manifest limit.
fn text_within_limit(bytes: &[u8]) -> Result<&str, AuthoringError> {
    if u64::try_from(bytes.len()).map_or(true, |length| length > MAX_JOB_MANIFEST_BYTES) {
        return Err(AuthoringError::invalid_file(format!(
            "the job file is larger than the {MAX_JOB_MANIFEST_BYTES} byte limit"
        )));
    }
    std::str::from_utf8(bytes).map_err(|_| AuthoringError::invalid_file("the job file is not UTF-8 text"))
}

/// `text` as the vault stores it, with unix line endings, which is what is checked, hashed, compared, and written.
fn unix_text(text: &str) -> String {
    String::from(UnixText::from(text))
}

/// Checks `text` as loading a file named `stem` does, and that the job the file would then be tracked as is `wanted`.
fn check_text(
    workflow: &RegisteredWorkflow,
    stem: &str,
    wanted: &str,
    text: &str,
) -> Result<Box<RegisteredJob>, AuthoringError> {
    match job_from_text(workflow, &format!("{stem}.toml"), text, &[]) {
        JobLoadOutcome::Invalid(failure) => Err(AuthoringError::JobFileInvalid {
            cause: failure.code.as_str(),
            message: failure.message,
        }),
        JobLoadOutcome::Valid(job) if job.slug == wanted => Ok(job),
        JobLoadOutcome::Valid(job) => Err(AuthoringError::JobKeyMismatch {
            job: wanted.to_owned(),
            slug: job.slug,
        }),
    }
}

/// The checks that need no workflow, for a request that names one the vault does not have: schedule, syntax, schema, and
/// the release fields. The inputs are not checked, because the workflow that declares them is not there.
fn check_without_workflow(
    package: &str,
    slug: &str,
    stem: &str,
    text: &str,
) -> Result<contextos_workflows::JobFileRelease, AuthoringError> {
    let detached = RegisteredWorkflow {
        package: package.to_owned(),
        slug: slug.to_owned(),
        name: String::new(),
        entry: String::new(),
        timeout_secs: crate::jobs::DEFAULT_TIMEOUT_SECS,
        entity: None,
        calls: contextos_workflows::call_rules::CallRules::default(),
        inputs: contextos_workflows::DeclaredInputs::default(),
        outputs: contextos_workflows::DeclaredOutputs::default(),
        log_level: None,
        templates: contextos_workflows::DeclaredFiles::default(),
        modules: contextos_workflows::DeclaredModules::default(),
        libraries: Vec::new(),
    };
    match job_from_text(&detached, &format!("{stem}.toml"), text, &[]) {
        JobLoadOutcome::Valid(job) if job.slug == stem => Ok(job.release),
        JobLoadOutcome::Valid(job) => Err(AuthoringError::JobKeyMismatch {
            job: stem.to_owned(),
            slug: job.slug,
        }),
        JobLoadOutcome::Invalid(failure) if failure.code == ValidationCode::InputInvalid => {
            // The release fields are read from the text on their own, as the inputs are all that failed.
            let table: toml::Table = toml::from_str(text).unwrap_or_default();
            let version = table.get("version").and_then(toml::Value::as_str).unwrap_or_default();
            let predecessor = table.get("predecessor").and_then(toml::Value::as_str);
            contextos_workflows::JobFileRelease::try_from(contextos_workflows::JobFileText { version, predecessor })
                .map_err(|error| AuthoringError::invalid_file(error.to_string()))
        }
        JobLoadOutcome::Invalid(failure) => Err(AuthoringError::JobFileInvalid {
            cause: failure.code.as_str(),
            message: failure.message,
        }),
    }
}

/// When the file at the job's own name exists: `Some(Ok(()))` if it holds exactly `bytes`, `Some(Err(..))` if it holds
/// something else or cannot be read, `None` if there is none.
fn identical_or_different(entries: &[JobFileEntry], job: &str, bytes: &[u8]) -> Option<Result<(), AuthoringError>> {
    entries.iter().find_map(|entry| match entry {
        JobFileEntry::Usable(file) if file.key.job == job => Some(if file.text.as_bytes() == bytes {
            Ok(())
        } else {
            Err(AuthoringError::JobExists {
                key: file.key.to_string(),
            })
        }),
        JobFileEntry::Unusable { key, .. } if key.job == job => {
            Some(Err(AuthoringError::JobExists { key: key.to_string() }))
        }
        JobFileEntry::Usable(_) | JobFileEntry::Unusable { .. } | JobFileEntry::Misnamed { .. } => None,
    })
}

/// The one job file `key` is tracked under, or the refusal for none or several.
fn the_file_tracked_as(entries: Vec<JobFileEntry>, key: &SharedJobKey) -> Result<SharedJobFile, AuthoringError> {
    let JobFilesFor { mut tracked, unusable } = find_job_files(entries, key);
    let not_found = || {
        unusable.map_or_else(
            || AuthoringError::JobNotFound { key: key.to_string() },
            AuthoringError::unusable,
        )
    };
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

/// One file's row.
fn listing_of(located: &Located, entry: &JobFileEntry, state: &contextos_workflows::JobsState) -> JobListing {
    let problem_row = |key: &SharedJobKey, code: &str, message: String| JobListing {
        key: key.clone(),
        file: None,
        name: None,
        lifecycle: state.lifecycle_of(&located.workflow.package, &located.workflow.slug, &key.job),
        schedule: None,
        ready: None,
        version: None,
        release_kind: None,
        predecessor: None,
        successor: None,
        replaced_by: None,
        hash: None,
        problems: vec![ListingProblem {
            code: code.to_owned(),
            message,
        }],
        suspension: state
            .job(&located.workflow.package, &located.workflow.slug, &key.job)
            .and_then(|stored| stored.suspension())
            .cloned(),
    };
    match entry {
        JobFileEntry::Unusable { key, reason } => problem_row(
            key,
            ValidationCode::ArtefactInvalid.as_str(),
            format!("the file is {reason}"),
        ),
        JobFileEntry::Misnamed { key } => problem_row(
            key,
            "workflow/job-key-invalid",
            "the file name is not a usable job name: use lowercase letters, digits, and hyphens".to_owned(),
        ),
        JobFileEntry::Usable(file) => {
            let loaded = job_from_text(&located.workflow, &format!("{}.toml", file.key.job), &file.text, &[]);
            let hash = Some(ContentHash::from(Digested(file.text.as_bytes())));
            match loaded {
                JobLoadOutcome::Valid(job) => {
                    let key = located.key(&job.slug);
                    JobListing {
                        file: (file.key.job != job.slug).then(|| file.key.job.clone()),
                        name: Some(job.name.clone()),
                        lifecycle: state.lifecycle_of(&located.workflow.package, &located.workflow.slug, &job.slug),
                        schedule: state
                            .job(&located.workflow.package, &located.workflow.slug, &job.slug)
                            .and_then(|stored| stored.schedule())
                            .map(str::to_owned),
                        ready: Some(job.ready),
                        release_kind: Some(state.release_kind(
                            located.package(),
                            &located.workflow.slug,
                            job.release.version.as_semver(),
                        )),
                        version: Some(job.release.version.as_semver().clone()),
                        predecessor: job.release.predecessor.as_ref().map(|key| key.as_str().to_owned()),
                        successor: None,
                        replaced_by: None,
                        hash,
                        problems: Vec::new(),
                        suspension: state
                            .job(&located.workflow.package, &located.workflow.slug, &job.slug)
                            .and_then(|stored| stored.suspension())
                            .cloned(),
                        key,
                    }
                }
                JobLoadOutcome::Invalid(failure) => {
                    let key = file.tracked_key().unwrap_or_else(|| file.key.clone());
                    JobListing {
                        file: (file.key.job != key.job).then(|| file.key.job.clone()),
                        hash,
                        ..problem_row(&key, failure.code.as_str(), failure.message)
                    }
                }
            }
        }
    }
}

/// Marks each row whose key another row shares.
fn mark_shared_keys(rows: &mut [JobListing]) {
    for row_index in 0..rows.len() {
        let others: Vec<String> = rows
            .iter()
            .enumerate()
            .filter(|(index, other)| *index != row_index && other.key == rows[row_index].key)
            .map(|(_, other)| other.file.clone().unwrap_or_else(|| other.key.job.clone()))
            .collect();
        if let Some(row) = rows.get_mut(row_index)
            && !others.is_empty()
        {
            row.problems.push(ListingProblem {
                code: "workflow/job-key-taken".to_owned(),
                message: format!("the job key is shared with {}", others.join(", ")),
            });
        }
    }
}

/// Names, on each row, the job of the same workflow that names it as its predecessor and has not been retired.
fn fill_successors(rows: &mut [JobListing]) {
    let successors: Vec<(SharedJobKey, String)> = rows
        .iter()
        .filter(|row| row.lifecycle != JobLifecycle::Retired)
        .filter_map(|row| {
            let predecessor = row.predecessor.as_ref()?;
            Some((
                SharedJobKey {
                    package: row.key.package.clone(),
                    workflow: row.key.workflow.clone(),
                    job: predecessor.clone(),
                },
                row.key.job.clone(),
            ))
        })
        .collect();
    for row in rows.iter_mut() {
        row.successor = successors
            .iter()
            .find(|(predecessor, _)| *predecessor == row.key)
            .map(|(_, successor)| successor.clone());
    }
}

#[path = "vault_successor.rs"]
mod successors;
