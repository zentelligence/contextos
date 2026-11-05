//! Static validation of a vault job: everything that can be checked without running it.
//!
//! The script is compiled and never evaluated, so validation has no effect on a vault or a server. Every failure found
//! is reported, each with its stable code and the file it concerns, and the job's version is worked out from the same
//! reads so that the result can be stored against it. The validator reads the workflow manifest, the entry script, and
//! the job file, reusing the loaders discovery uses, so a job validates exactly as it would register.

use contextos_core::TeamRoster;
use contextos_packages::PackageStore as _;
use contextos_workflows::call_rules::CallRules;
use contextos_workflows::workflow_definition::{WorkflowDefinition, WorkflowText};
use contextos_workflows::{
    Actor, Applied, JobFingerprint, ModuleOwner, ScriptSyntaxError, StoredValidation, Transition, Unverifiable,
    ValidationCode, ValidationFailure, ValidationReport, VersionBasis, WorkflowScript, check_script,
};
use time::OffsetDateTime;

use crate::job_checks::{Declarations, OwnerCheck, check_declarations};
use crate::team::{RosterLoadError, load_roster};

use crate::job_lifecycle::NotRegistered;
use crate::job_lifecycle::{LifecycleChange, LifecycleError, change_lifecycle};
use crate::jobs::{
    JobLoadOutcome, RegisteredJob, RegisteredWorkflow, WorkflowFolder, declarations_from_text, failure,
    is_url_safe_slug, list_job_files, load_job, unconfigured_server,
};
use crate::mcp_client::{McpCallError, McpClient};
use crate::scheduler::{read_state, write_state};
use crate::vault_content::{AttachedRead, read_attached};
use crate::vault_job_release::{CodeRead, CodeRequest, NamedJobRelease, ReleaseFinding, read_code};

pub use crate::job_pass::{ObservedJob, PassOutcome, PassReport, validation_pass, validation_pass_for};

/// What validating one job found.
#[derive(Clone, Debug)]
pub struct JobValidation {
    /// The job's version, when all three of its files could be read. Facts are recorded against it.
    pub fingerprint: Option<JobFingerprint>,
    pub report: ValidationReport,
    /// Whether the job file says authoring is finished.
    pub ready: bool,
    /// Whether a release was removed, replaced, or changed while the job's code was being read. The report then holds
    /// only what could not be checked, and the job has no version for this read: a change on the host caught mid-read,
    /// from which nothing about the job follows and a result must not be kept.
    pub release_changed_while_read: bool,
    /// Whether the job's own package, or a library of it, is gone or damaged: the report then fails the package, which
    /// names it, and nothing else about the job. An `enabled` job is suspended as unavailable for it, not as invalid.
    pub package_unavailable: bool,
    /// What the version was made from: the release the job names and its library lock. Carried to the state change that
    /// keeps a passing result, so a release's first lock is recorded in the same write that validates the job.
    pub made_from: Option<VersionBasis>,
    /// The exact release the job file names, whether or not it is installed. `None` when the file names none.
    pub named_release: Option<semver::Version>,
    /// The slug of the workflow of the release the job names, which keys the job's state. `None` when the release could
    /// not be read, in which case the caller falls back to the slug the source folder is tracked under.
    pub workflow_slug: Option<String>,
}

impl JobLocation {
    /// Where the job's workflow is kept.
    pub(crate) fn folder(&self) -> WorkflowFolder<'_> {
        WorkflowFolder {
            package: &self.package,
            directory: &self.directory,
        }
    }
}

impl JobValidation {
    /// A result that has no version: the job's code was not read, because its file names no release or the release changed
    /// while it was read when `release_changed_while_read`.
    pub(crate) const fn without_version(
        report: ValidationReport,
        ready: bool,
        release_changed_while_read: bool,
    ) -> Self {
        Self {
            fingerprint: None,
            report,
            ready,
            release_changed_while_read,
            package_unavailable: false,
            made_from: None,
            named_release: None,
            workflow_slug: None,
        }
    }

    /// Adds to the report what the rules about releases and the link find in the job file against `state`, each as
    /// `validation/artefact-invalid` on `version` or `predecessor`: the same rules keeping a result applies, and with the
    /// same effect on the result, which no longer carries what it was made from once a rule fails.
    ///
    /// A file edited away from the release or predecessor its job was enabled with is the fault, whatever else is
    /// wrong, so the result is no longer one of an unavailable package: an edit to a release that is not installed fails
    /// validation, and only a job whose own release is gone is unavailable.
    pub(crate) fn apply_release_rules(
        &mut self,
        state: &contextos_workflows::JobsState,
        package: &str,
        (workflow, job): (&str, &str),
    ) {
        let findings = match (&self.made_from, &self.named_release) {
            (Some(basis), _) => state.release_rule_findings(package, (workflow, job), self.fingerprint.as_ref(), basis),
            (None, Some(named)) => state
                .release_edit(package, (workflow, job), named)
                .map(|message| contextos_workflows::ReleaseRuleFindings {
                    failures: vec![contextos_workflows::ReleaseRuleFailure {
                        artefact: "version",
                        message,
                    }],
                    edited_record: true,
                })
                .unwrap_or_default(),
            (None, None) => return,
        };
        if !findings.failures.is_empty() {
            self.made_from = None;
        }
        if findings.edited_record {
            self.package_unavailable = false;
        }
        self.report.failures.extend(
            findings
                .failures
                .into_iter()
                .map(|found| failure(ValidationCode::ArtefactInvalid, found.artefact, found.message)),
        );
    }

    /// This result as the job state stores it, made at `at`.
    #[must_use]
    pub fn stored(&self, at: OffsetDateTime) -> StoredValidation {
        let mut stored = StoredValidation::from(contextos_workflows::ValidationRecord {
            report: &self.report,
            fingerprint: self.fingerprint.clone(),
            at,
        });
        stored.made_from.clone_from(&self.made_from);
        stored
    }
}

/// Validates the job in `job_file` under the workflow in `folder`.
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the MCP transport itself fails. Anything wrong with the job is in the
/// report, not an error.
pub async fn validate_job(
    client: &McpClient,
    vault_name: &str,
    folder: WorkflowFolder<'_>,
    job_file: &str,
    known_mcp_servers: &[String],
    owner: OwnerCheck<'_>,
) -> Result<JobValidation, McpCallError> {
    validate_job_with_state(client, vault_name, (folder, job_file), known_mcp_servers, owner, None).await
}

/// [`validate_job`] for a caller that holds the job state, which the job's library lock is taken from, so a pass that
/// validates many jobs does not read it for each.
///
/// # Errors
///
/// As [`validate_job`].
pub(crate) async fn validate_job_with_state(
    client: &McpClient,
    vault_name: &str,
    (folder, job_file): (WorkflowFolder<'_>, &str),
    known_mcp_servers: &[String],
    owner: OwnerCheck<'_>,
    state: Option<&contextos_workflows::JobsState>,
) -> Result<JobValidation, McpCallError> {
    let job_text = read_text(
        client,
        &format!("{vault_name}://registry/workflows/{folder}/jobs/{job_file}"),
    )
    .await?;
    let mut report = ValidationReport::default();
    // Whatever release the job names, its source folder must be a listed workflow of a workflow package, or the vault's
    // source layout is wrong and the job says so.
    if let Some(found) = crate::job_discovery::source_refusal(client, vault_name, folder).await? {
        report.failures.push(found);
    }
    let mut ready = job_text.as_deref().is_some_and(names_ready);

    // The workflow's declarations, and the code they describe, come from the release the job file names and never from
    // source, so editing or deleting source changes nothing here.
    let mut code = ReleaseCode::default();
    let mut workflow_text = None;
    let mut declared = None;
    let mut job_checked = false;
    if let Some(job_text) = &job_text {
        let read = read_code(
            client,
            &CodeRequest {
                vault: vault_name,
                folder,
                job_toml: job_text,
                state,
            },
        )
        .await?;
        if let CodeRead::Read(read) = &read {
            workflow_text = Some(read.snapshot.workflow_toml.clone());
            match declarations_from_text(&read.snapshot.workflow_toml, folder) {
                Err(found) => report.failures.push(found),
                Ok(workflow) => match unconfigured_server(&workflow.calls, known_mcp_servers) {
                    Some(found) => report.failures.push(found),
                    None => declared = Some(workflow),
                },
            }
            job_checked = true;
        } else if matches!(read, CodeRead::NoRelease) {
            // No release is named, so the job file fails before its inputs are looked at, whatever the workflow declares.
            declared = Some(RegisteredWorkflow::without_declarations(
                folder.package,
                folder.directory,
            ));
            job_checked = true;
        }
        code = check_release_code(read, job_text, workflow_text.as_deref(), &mut report).await;
    } else {
        report.failures.push(failure(
            ValidationCode::ArtefactMissing,
            job_file,
            "job manifest could not be read",
        ));
    }
    if job_checked && let Some(workflow) = &declared {
        let team = crate::team::load_team(client, vault_name).await;
        match load_job(client, vault_name, workflow, job_file, &team).await? {
            JobLoadOutcome::Invalid(found) => report.failures.push(found),
            JobLoadOutcome::Valid(job) => ready = job.ready,
        }
    }
    check_declarations(
        client,
        vault_name,
        &Declarations {
            job_file,
            workflow_text: workflow_text.as_deref(),
            job_text: job_text.as_deref(),
        },
        owner,
        &mut report,
    )
    .await?;
    Ok(JobValidation {
        release_changed_while_read: code.unverifiable,
        package_unavailable: code.removed,
        fingerprint: code.fingerprint,
        made_from: code.made_from,
        named_release: job_text
            .as_deref()
            .and_then(|text| NamedJobRelease::try_from(text).ok())
            .map(|named| named.0.version.as_semver().clone()),
        workflow_slug: code.workflow_slug,
        report,
        ready,
    })
}

/// What reading a job's release came to, beyond the failures it added to the report.
#[derive(Default)]
struct ReleaseCode {
    fingerprint: Option<JobFingerprint>,
    made_from: Option<VersionBasis>,
    workflow_slug: Option<String>,
    /// The release could not be verified, so nothing about the job follows from this read.
    unverifiable: bool,
    /// The release, or a library release in its lock, is gone.
    removed: bool,
}

/// Checks the code in the release `read` found, and reports what is wrong with the release or the code. A job whose
/// release cannot be read has no version.
async fn check_release_code(
    read: CodeRead,
    job_toml: &str,
    workflow_toml: Option<&str>,
    report: &mut ValidationReport,
) -> ReleaseCode {
    let mut found = ReleaseCode::default();
    let unverifiable = |report: &mut ValidationReport, reason: String| {
        report.unverifiable.push(Unverifiable {
            check: "the job's code".to_owned(),
            reason,
        });
    };
    match read {
        CodeRead::Read(read) => {
            let entry = read.snapshot.definition.entry.clone();
            report
                .failures
                .extend(check_syntax(&entry, &read.snapshot.script).await);
            report.failures.extend(
                crate::library_checks::library_tool_failures(
                    &read.snapshot.script,
                    &entry,
                    &read.snapshot.library_tools,
                    &workflow_calls(workflow_toml),
                )
                .await,
            );
            found.fingerprint = Some(read.version(job_toml));
            found.made_from = Some(read.made_from());
            found.workflow_slug = Some(read.workflow_slug);
        }
        CodeRead::NoRelease => {}
        CodeRead::Failed(failed) => {
            let finding = ReleaseFinding::from(failed.as_ref());
            if let Some((code, artefact)) = finding.failure {
                report.failures.push(failure(code, &artefact, finding.message));
            } else {
                unverifiable(report, finding.message);
                found.unverifiable = true;
            }
            found.removed = finding.removed;
        }
        CodeRead::Unlocated(error) => {
            unverifiable(report, error.to_string());
            found.unverifiable = true;
        }
        CodeRead::StateUnreadable(reason) => {
            unverifiable(report, format!("the job state could not be read: {reason}"));
            found.unverifiable = true;
        }
    }
    found
}

/// Compiles `script`, whose entry script is `entry`, without running it, off the async executor, and reports each file's
/// first problem with its position, against the module it is in or else the entry script.
pub(crate) async fn check_syntax(entry: &str, script: &WorkflowScript) -> Vec<ValidationFailure> {
    let root = script.root().clone();
    let script = script.clone();
    let checked = tokio::task::spawn_blocking(move || check_script(&script))
        .await
        .unwrap_or_else(|join_error| {
            vec![ScriptSyntaxError {
                module: None,
                owner: None,
                message: format!("the syntax check did not complete: {join_error}"),
                line: None,
                position: None,
            }]
        });
    checked
        .into_iter()
        .map(|error| {
            let place = match (error.line, error.position) {
                (Some(line), Some(column)) => format!("line {line}, column {column}: "),
                (Some(line), None) => format!("line {line}: "),
                _ => String::new(),
            };
            // A module of a library is named with the library, as it is imported, so it is not taken for a module of the
            // job's own package that has the same path.
            let artefact = match (&error.module, &error.owner) {
                (None, _) => entry.to_owned(),
                (Some(module), Some(owner)) if *owner != root => match owner {
                    ModuleOwner::Package(package) => format!("{package}:{module}"),
                    ModuleOwner::Workflow => module.to_string(),
                },
                (Some(module), _) => module.to_string(),
            };
            failure(
                ValidationCode::ScriptSyntax,
                &artefact,
                format!("{place}{}", error.message),
            )
        })
        .collect()
}

pub(crate) async fn read_text(client: &McpClient, vault_path: &str) -> Result<Option<String>, McpCallError> {
    Ok(match read_attached(client, vault_path).await? {
        AttachedRead::Text(read) => Some(read.text),
        AttachedRead::Refused { .. } | AttachedRead::NotText => None,
    })
}

/// The call rules a workflow manifest declares, read without checking the rest of it. A manifest that cannot be read
/// declares none, and is reported as the failure it is elsewhere.
pub(crate) fn workflow_calls(workflow_toml: Option<&str>) -> CallRules {
    workflow_toml
        .and_then(|text| {
            WorkflowDefinition::try_from(WorkflowText {
                text,
                folder: "workflow",
            })
            .ok()
        })
        .map(|declared| declared.calls)
        .unwrap_or_default()
}

/// The `entry` a workflow manifest names, read without checking the rest of it, so a script can be compiled even when
/// something else in the manifest is wrong.
pub(crate) fn entry_named(workflow_toml: &str) -> Option<String> {
    toml::from_str::<toml::Table>(workflow_toml)
        .ok()?
        .get("entry")?
        .as_str()
        .map(str::to_owned)
}

/// Whether a job file says `ready = true`, read without checking the rest of it.
pub(crate) fn names_ready(job_toml: &str) -> bool {
    toml::from_str::<toml::Table>(job_toml)
        .ok()
        .and_then(|table| table.get("ready").and_then(toml::Value::as_bool))
        .unwrap_or(false)
}

/// Keeps what validation found, against the version it was made for or against none when the job's files could not all
/// be read, as asked for by `by`, and applies what it decides about the job's state: a `validated` or `registered` job
/// whose files changed since goes back to `draft`, and a `draft` that passed moves to `validated` for that version, with
/// its release's library lock, its identity, and its link to a predecessor recorded in the same change. Returns what
/// keeping the result did.
pub(crate) fn record_result_kept(
    state: &mut contextos_workflows::JobsState,
    package: &str,
    workflow_slug: &str,
    job_slug: &str,
    validation: &JobValidation,
    by: Actor,
    now: OffsetDateTime,
) -> contextos_workflows::ValidationKept {
    state.keep_validation(
        package,
        workflow_slug,
        job_slug,
        validation.stored(now).made_by(by),
        now,
    )
}

/// Whether a validation result was kept in the vault's job state.
#[derive(Debug, Eq, PartialEq)]
pub enum Recording {
    Recorded,
    /// The result was deliberately not kept: the team could not be used, so the owner check was unverifiable, and a
    /// result made without it is reported and never stored.
    Withheld,
    /// The result was not kept, and the reason says why: the state could not be read, another writer changed it in
    /// between, or the job has no slug a state entry could be kept under.
    NotRecorded {
        conflict: bool,
        reason: String,
    },
}

/// Where a job's files are in the vault.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobLocation {
    /// The package folder under `registry/workflows/` that holds the job's workflow.
    pub package: String,
    /// The job's workflow folder within its package.
    pub directory: String,
    /// The job file's name within that folder's `jobs/` folder.
    pub file: String,
}

/// Finds the files of the job tracked as `job_slug` under the workflow `workflow_slug` of the package `package`, whether
/// or not the job would register. A job that cannot be registered is still found, because validating it is how an operator
/// learns why.
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the MCP transport itself fails.
pub async fn locate_job(
    client: &McpClient,
    vault_name: &str,
    package: &str,
    workflow_slug: &str,
    job_slug: &str,
) -> Result<Option<JobLocation>, McpCallError> {
    if !is_url_safe_slug(package) || !is_url_safe_slug(workflow_slug) {
        return Ok(None);
    }
    let folder = WorkflowFolder {
        package,
        directory: workflow_slug,
    };
    for file in list_job_files(client, vault_name, folder).await? {
        let Some(job_text) = read_text(
            client,
            &format!("{vault_name}://registry/workflows/{folder}/jobs/{file}"),
        )
        .await?
        else {
            continue;
        };
        let stem = file.strip_suffix(".toml").unwrap_or(&file);
        if tracked_slug(&job_text, stem).as_deref() == Some(job_slug) {
            return Ok(Some(JobLocation {
                package: package.to_owned(),
                directory: workflow_slug.to_owned(),
                file,
            }));
        }
    }
    Ok(None)
}

/// Validates one job now, whether or not its job file says it is ready, and records the result: a `draft` job that
/// passes becomes `validated`, and a failure is stored and shown with the job left as it was. Returns the result and
/// whether it was recorded, which an operator's request needs to know, because a result that was not recorded cannot
/// allow anything.
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the MCP transport itself fails.
pub async fn validate_and_record(
    client: &McpClient,
    vault_name: &str,
    known_mcp_servers: &[String],
    location: &JobLocation,
    now: OffsetDateTime,
) -> Result<(JobValidation, Recording), McpCallError> {
    let (folder, job_file) = (location.folder(), location.file.as_str());
    let team = loaded_team(client, vault_name).await?;
    let owner_unverifiable = matches!(team, LoadedTeam::Unavailable(_));
    // The first passing validation of a workflow release chooses its library releases and records them, so the vault's
    // store lock is held from before the choice to after the record: retention cannot remove a chosen release in
    // between. When the lock cannot be had the result is still reported, and says it was not recorded.
    let engine = client.vault_packages(vault_name).await.ok();
    let held = match &engine {
        Some(engine) => Some(engine.lock().await),
        None => None,
    };
    let unlocked = match &held {
        Some(Err(error)) => Some(Recording::NotRecorded {
            conflict: matches!(error, contextos_packages::PackagesError::StoreLocked),
            reason: error.to_string(),
        }),
        Some(Ok(_)) | None => None,
    };
    // A validation that passed first on its workflow release while another job's validation recorded that release's
    // library lock is repeated against the recorded lock, which this one is then held to.
    let mut repeats = 0;
    loop {
        let validation = validate_job(client, vault_name, folder, job_file, known_mcp_servers, team.check()).await?;
        let job_text = read_text(
            client,
            &format!("{vault_name}://registry/workflows/{folder}/jobs/{job_file}"),
        )
        .await?;
        if owner_unverifiable || validation.release_changed_while_read {
            // The owner check or the release could not be verified, so the result is incomplete for a reason that is not
            // the job's. It is reported and nothing is stored, so no state changes on the strength of it and nothing is
            // held against the job.
            return Ok((validation, Recording::Withheld));
        }
        let not_recorded = |reason: &str| Recording::NotRecorded {
            conflict: false,
            reason: reason.to_owned(),
        };
        let Some(job_text) = job_text else {
            return Ok((validation, not_recorded("the job's files could not be read")));
        };
        // The workflow is the folder the job file sits in, matched exactly.
        let workflow_slug = is_url_safe_slug(folder.directory).then(|| folder.directory.to_owned());
        let (Some(workflow_slug), Some(job_slug)) = (
            workflow_slug,
            tracked_slug(&job_text, job_file.strip_suffix(".toml").unwrap_or(job_file)),
        ) else {
            return Ok((
                validation,
                not_recorded("the job has no slug a state entry can be kept under"),
            ));
        };
        if let Some(recording) = unlocked {
            return Ok((validation, recording));
        }
        let mut snapshot = match read_state(client, vault_name).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let reason = format!("the job state could not be read: {error}");
                tracing::warn!(
                    vault = vault_name,
                    "the job state could not be read, so the validation was not recorded"
                );
                return Ok((validation, not_recorded(&reason)));
            }
        };
        // What is reported is the result with the rules about releases and the link applied, as keeping it applies them.
        let mut reported = validation.clone();
        reported.apply_release_rules(&snapshot.value, folder.package, (&workflow_slug, &job_slug));
        let kept = record_result_kept(
            &mut snapshot.value,
            folder.package,
            &workflow_slug,
            &job_slug,
            &validation,
            Actor::Operator,
            now,
        );
        if kept.lock == contextos_workflows::LockOutcome::Conflict && repeats < MAX_LOCK_REPEATS {
            repeats += 1;
            continue;
        }
        let recording = match write_state(client, vault_name, &snapshot).await {
            Ok(()) => Recording::Recorded,
            Err(error) => {
                tracing::warn!(
                    vault = vault_name,
                    "the job state changed or could not be written, so the validation was not recorded"
                );
                Recording::NotRecorded {
                    conflict: error.is_conflict(),
                    reason: error.to_string(),
                }
            }
        };
        return Ok((reported, recording));
    }
}

/// How many times a validation is repeated against a library lock another job's validation recorded first.
const MAX_LOCK_REPEATS: u8 = 3;

/// The vault's team as a validation asked for by hand sees it: a roster, or the reason the owner cannot be checked.
enum LoadedTeam {
    Roster(Box<TeamRoster>),
    Unavailable(String),
}

impl LoadedTeam {
    fn check(&self) -> OwnerCheck<'_> {
        match self {
            Self::Roster(roster) => OwnerCheck::Roster(roster),
            Self::Unavailable(reason) => OwnerCheck::Unavailable(reason),
        }
    }
}

/// Reads the vault's team strictly. A team that cannot be used is not an error here: the owner check is unverifiable,
/// which the validation reports without recording a defect against the job.
async fn loaded_team(client: &McpClient, vault_name: &str) -> Result<LoadedTeam, McpCallError> {
    Ok(match load_roster(client, vault_name).await {
        Ok(roster) => LoadedTeam::Roster(Box::new(roster)),
        Err(RosterLoadError::Unreachable(error)) => return Err(error),
        Err(RosterLoadError::Unreadable { .. }) => {
            LoadedTeam::Unavailable("the vault's team file could not be read".to_owned())
        }
        Err(RosterLoadError::Invalid(error)) => LoadedTeam::Unavailable(format!(
            "the vault's team cannot be used for jobs ({}: {})",
            error.code(),
            error.rule().as_str()
        )),
    })
}

/// Validates one job now and records the result, as [`validate_and_record`] does, for a caller that does not need to
/// know whether it was recorded.
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the MCP transport itself fails. A state file that cannot be read or
/// written is logged and the result is returned without having been recorded.
pub async fn validate_now(
    client: &McpClient,
    vault_name: &str,
    known_mcp_servers: &[String],
    folder: WorkflowFolder<'_>,
    job_file: &str,
    now: OffsetDateTime,
) -> Result<JobValidation, McpCallError> {
    let location = JobLocation {
        package: folder.package.to_owned(),
        directory: folder.directory.to_owned(),
        file: job_file.to_owned(),
    };
    Ok(
        validate_and_record(client, vault_name, known_mcp_servers, &location, now)
            .await?
            .0,
    )
}

/// What a job's release says its version is now.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum CurrentVersion {
    /// The release and its locked libraries were read whole.
    Known(JobFingerprint),
    /// The job has no version to speak of: the release is removed or damaged, or a library of its lock is, the job file
    /// names no exact release, or the release holds no such workflow. A job that depends on its version loses it.
    Gone,
    /// The release, or a library release in its lock, is incomplete or could not be verified, or the store or the job
    /// state could not be read. That says nothing about the job, so nothing about it changes.
    Unverifiable,
}

/// A job's current version, read from the release its job file names. The library lock is taken from `state` when the
/// caller holds the job state, and read otherwise.
///
/// # Errors
///
/// Returns the call error when the server could not be asked about a file, because then the version is unknown, not
/// gone, and a job must not be demoted or suspended for an outage.
pub(crate) async fn current_version(
    client: &McpClient,
    vault_name: &str,
    (folder, job_text): (WorkflowFolder<'_>, &str),
    state: Option<&contextos_workflows::JobsState>,
) -> Result<CurrentVersion, McpCallError> {
    Ok(
        match read_code(
            client,
            &CodeRequest {
                vault: vault_name,
                folder,
                job_toml: job_text,
                state,
            },
        )
        .await?
        {
            CodeRead::Read(read) => CurrentVersion::Known(read.version(job_text)),
            CodeRead::NoRelease => CurrentVersion::Gone,
            CodeRead::Failed(failed) => {
                if ReleaseFinding::from(failed.as_ref()).failure.is_some() {
                    CurrentVersion::Gone
                } else {
                    CurrentVersion::Unverifiable
                }
            }
            CodeRead::Unlocated(_) | CodeRead::StateUnreadable(_) => CurrentVersion::Unverifiable,
        },
    )
}

/// The slug a job or workflow is tracked under: the explicit `slug` its manifest names, or `fallback` when it names
/// none. `None` when the manifest names a slug that is not URL-safe, so a job that cannot be registered never gets a
/// state entry of its own.
pub(crate) fn tracked_slug(manifest: &str, fallback: &str) -> Option<String> {
    let named = toml::from_str::<toml::Table>(manifest)
        .ok()
        .and_then(|table| table.get("slug").and_then(toml::Value::as_str).map(str::to_owned));
    match named {
        Some(explicit) => is_url_safe_slug(&explicit).then_some(explicit),
        None => Some(fallback.to_owned()),
    }
}

/// What recording a completed dry run did.
#[derive(Debug, Eq, PartialEq)]
pub enum DryRunRecorded {
    /// The job was `validated` for the version that was dry-run, and is now `registered`.
    Registered,
    /// Nothing was recorded, and the reason says why: the job was not `validated`, or its version is not the one that
    /// was validated, or the state could not be written.
    NotRecorded(crate::job_lifecycle::NotRegistered),
}

/// The version of a registered job as its files stand now, or `None` when one of them cannot be read. A dry run takes
/// this before it starts, so what it records is about the version that ran.
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the MCP transport itself fails.
pub async fn job_version(
    client: &McpClient,
    vault_name: &str,
    job: &RegisteredJob,
) -> Result<Option<JobFingerprint>, McpCallError> {
    let location = JobLocation {
        package: job.package.clone(),
        directory: job.workflow_slug.clone(),
        file: job.file_name.clone(),
    };
    version_at(client, vault_name, &location).await
}

/// The version of the job at `location` as its files stand now, or `None` when one of them cannot be read.
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the MCP transport itself fails.
pub async fn version_at(
    client: &McpClient,
    vault_name: &str,
    location: &JobLocation,
) -> Result<Option<JobFingerprint>, McpCallError> {
    let folder = location.folder();
    let Some(job_text) = read_text(
        client,
        &format!("{vault_name}://registry/workflows/{folder}/jobs/{}", location.file),
    )
    .await?
    else {
        return Ok(None);
    };
    Ok(
        match current_version(client, vault_name, (folder, &job_text), None).await? {
            CurrentVersion::Known(version) => Some(version),
            CurrentVersion::Gone | CurrentVersion::Unverifiable => None,
        },
    )
}

/// Records that a dry run completed for `version`, the version the job had when the run started, which registers a job
/// that was validated for that version. Anything else, such as a job that is still a draft or one whose files changed
/// since, is reported and recorded as nothing.
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the MCP transport itself fails.
pub async fn record_dry_run(
    client: &McpClient,
    vault_name: &str,
    job: &RegisteredJob,
    version: JobFingerprint,
    now: OffsetDateTime,
) -> Result<DryRunRecorded, McpCallError> {
    let change = change_lifecycle(
        client,
        vault_name,
        LifecycleChange {
            package: &job.package,
            workflow_slug: &job.workflow_slug,
            job_slug: &job.slug,
            actor: Actor::DryRun,
            transition: Transition::DryRunCompleted(version),
            now,
        },
    )
    .await;
    Ok(match change {
        Ok(Applied::To(_) | Applied::Removed) => DryRunRecorded::Registered,
        Err(LifecycleError::Refused { source }) => DryRunRecorded::NotRecorded(NotRegistered::from(&source)),
        Err(LifecycleError::Conflict | LifecycleError::Unavailable(_)) => {
            DryRunRecorded::NotRecorded(NotRegistered::StateUnavailable)
        }
    })
}

#[cfg(test)]
#[path = "job_validation_test.rs"]
mod tests;
