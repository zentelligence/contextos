//! Static validation of a shared job: everything that can be checked without running it.
//!
//! The script is compiled and never evaluated, and a tool is checked by listing a server's tools and nothing else, so
//! validation has no effect on a vault or a server. Every failure found is reported with its stable code and the file it
//! concerns, and a check that needs a server that cannot be reached is reported as unverifiable rather than as a defect in
//! the job. The job's version is worked out from the same reads, so the result can be stored against it.

use std::collections::BTreeMap;
use std::path::Path;

use contextos_workflows::call_rules::ToolRef;
use contextos_workflows::{LibraryLock, PackageName, VersionBasis};
use contextos_workflows::{
    SuppliedInputs, Unverifiable, ValidatedInputs, ValidationCode, ValidationReport, WorkflowDefinition, WorkflowScript,
};
use semver::Version;

use crate::job_validation::{JobValidation, check_syntax, names_ready};
use crate::jobs::failure;
use crate::library_checks::library_tool_failures;
use crate::mcp_client::McpClientSet;
use crate::package_store::PackageStoreError;
use crate::shared_claim::SharedJobKey;
use crate::shared_jobs::{SharedJob, SharedJobFile, SharedJobSource};
use crate::shared_runner::{reported_vaults, require_vaults_configured};
use crate::shared_snapshot::{SharedSnapshot, SnapshotFailure, SnapshotRequest, SnapshotResult, read_snapshot};
use crate::shared_version::NamedRelease;

/// What validating a shared job needs from its host.
pub struct SharedValidationHost<'a> {
    pub store_root: &'a Path,
    pub clients: &'a McpClientSet,
    /// The server that reports the configured vaults.
    pub primary_server: &'a str,
    /// Every server `web.toml` configures, whether or not it is connected.
    pub known_servers: &'a [String],
}

/// What the one snapshot of the job's code gave: the workflow whose declarations can be checked, and its code or the
/// file that could not be read and why.
struct Resolved {
    definition: WorkflowDefinition,
    script: Result<WorkflowScript, ScriptFailure>,
    /// The tools each library release declares in its `library.toml`.
    library_tools: BTreeMap<PackageName, Vec<ToolRef>>,
}

/// Why the job's code could not be read, with the code and artefact it is reported against.
struct ScriptFailure {
    code: ValidationCode,
    artefact: String,
    message: String,
}

/// Validates the shared job in `file`.
///
/// A failure of any kind is in the returned report. The job has a version whenever its job file names a release that
/// resolves, whether or not the job is otherwise valid, so a result can be stored against it.
pub async fn validate_shared_job(host: &SharedValidationHost<'_>, file: &SharedJobFile) -> JobValidation {
    validate_shared_job_against(host, file, None).await
}

/// As [`validate_shared_job`], for a job on a workflow release whose library `lock` is recorded: the libraries are the
/// locked releases and nothing is resolved. `None` is for a release no job has passed validation on, whose libraries
/// are resolved and reported in the result as the lock to record.
pub async fn validate_shared_job_against(
    host: &SharedValidationHost<'_>,
    file: &SharedJobFile,
    lock: Option<&LibraryLock>,
) -> JobValidation {
    let mut report = ValidationReport::default();
    let job_file = job_file_name(&file.key.job);
    let key = &file.key;
    let job = match SharedJob::try_from(SharedJobSource {
        key: key.clone(),
        text: &file.text,
    }) {
        Ok(job) => Some(job),
        Err(error) => {
            report
                .failures
                .push(failure(ValidationCode::ArtefactInvalid, &job_file, error.to_string()));
            None
        }
    };
    let ready = job.as_ref().map_or_else(|| names_ready(&file.text), |job| job.ready);
    // The release to read is the one the job file names, whether or not the rest of the file loads, so every failure is
    // still reported. A file that names no exact release has no release to read and no version: the failure above is the
    // whole result.
    let named = job.as_ref().map_or_else(
        || NamedRelease::try_from(file.text.as_str()).ok().map(|named| named.0),
        |job| Some(job.release.version.as_semver().clone()),
    );
    let Some(named) = named else {
        return JobValidation::without_version(report, ready, false);
    };
    // The version, the checks, and the code they read all come from this one snapshot of the store.
    let read = read_for(host.store_root, key, &named, lock).await;
    if let Err(failure) = &read
        && failure.release_changed()
    {
        report.unverifiable.push(Unverifiable {
            check: "the job's code".to_owned(),
            reason: failure.error.to_string(),
        });
        return JobValidation::without_version(report, ready, true);
    }
    let basis = match &read {
        Ok(snapshot) => Some(&snapshot.basis),
        Err(failure) => failure.basis.as_ref(),
    };
    let fingerprint = basis.map(|basis| basis.version(&file.text));
    // What the version was made from, for the write that keeps a passing result: the lock the snapshot was read with,
    // which is the recorded one, or the one this validation resolved as the release's first.
    let made_from = read.as_ref().ok().map(|snapshot| VersionBasis {
        package: key.package.clone(),
        release: named.clone(),
        lock: snapshot.lock.clone(),
        predecessor: job.as_ref().and_then(|job| job.release.predecessor.clone()),
    });

    let manifest_invalid = read.as_ref().is_err_and(|failure| failure.release_damaged());
    match read.map(resolved).or_else(|failure| partly_resolved(&failure)) {
        Err(message) => {
            let code = if manifest_invalid {
                ValidationCode::ArtefactInvalid
            } else {
                ValidationCode::ArtefactMissing
            };
            report.failures.push(failure(code, &key.package, message));
        }
        Ok(resolved) => {
            check_script(&resolved, &mut report).await;
            check_effects(host, &resolved.definition, &mut report).await;
            if let Some(job) = &job {
                check_inputs(host, &resolved.definition, job, &job_file, &mut report).await;
            }
        }
    }
    // A package that is gone, or a library of it that is gone, makes the package unavailable. A damaged one is an invalid
    // artefact like any other, so an enabled job is suspended as having failed validation, as a vault job is.
    let package_unavailable = report
        .failures
        .iter()
        .any(|failure| failure.code == ValidationCode::ArtefactMissing && failure.artefact == key.package);
    JobValidation {
        fingerprint,
        report,
        ready,
        release_changed_while_read: false,
        package_unavailable,
        made_from,
        named_release: Some(named),
        workflow_slug: None,
    }
}

/// Whether the validation found a release that changed while the job's code was being read, which is a change on the
/// host caught mid-read and not a fact about the job, so its result must not be kept.
///
/// A package or a library that is gone is not this: the job has no version, as it has none when its own package is not
/// installed, and it is handled the same way.
#[must_use]
pub fn changed_on_host(validation: &JobValidation) -> bool {
    validation.release_changed_while_read
}

/// The file a shared job's own settings are in.
fn job_file_name(job: &str) -> String {
    format!("{job}.toml")
}

/// Reads the job's snapshot off the async executor.
async fn read_for(
    store_root: &Path,
    key: &SharedJobKey,
    version: &Version,
    lock: Option<&LibraryLock>,
) -> SnapshotResult<SharedSnapshot> {
    let package = PackageName::try_from(key.package.as_str()).map_err(|error| {
        Box::new(SnapshotFailure {
            identity: None,
            basis: None,
            definition: None,
            error: error.into(),
        })
    })?;
    read_snapshot(SnapshotRequest {
        store_root,
        package,
        version,
        workflow: &key.workflow,
        lock,
    })
    .await
}

/// A snapshot that was read, as the checks take it.
fn resolved(snapshot: SharedSnapshot) -> Resolved {
    Resolved {
        definition: snapshot.definition,
        script: Ok(snapshot.script),
        library_tools: snapshot.library_tools,
    }
}

/// A snapshot that could not be read in full. When the workflow was found, its declarations can still be checked, and
/// the file or resolution that failed is reported against the file it concerns. Otherwise there is nothing to check
/// but the failure, whose message says which step failed.
fn partly_resolved(failure: &SnapshotFailure) -> Result<Resolved, String> {
    let Some(definition) = failure.definition.clone() else {
        return Err(failure.error.to_string());
    };
    // A library that does not resolve concerns the package whose dependencies name it.
    let named_package = || {
        failure
            .identity
            .as_ref()
            .map_or_else(|| definition.folder.clone(), |identity| identity.name.to_string())
    };
    let (code, artefact) = match (&failure.error, failure.dependency_code()) {
        (_, Some(code)) => (code, named_package()),
        (PackageStoreError::EntryUnreadable { entry, .. }, None) => (ValidationCode::ArtefactMissing, entry.clone()),
        (PackageStoreError::ModuleUnreadable(unreadable), None) => {
            (ValidationCode::ArtefactMissing, unreadable.artefact())
        }
        (_, None) => (ValidationCode::ArtefactMissing, named_package()),
    };
    let message = failure.error.to_string();
    Ok(Resolved {
        definition,
        script: Err(ScriptFailure {
            code,
            artefact,
            message,
        }),
        library_tools: BTreeMap::new(),
    })
}

async fn check_script(resolved: &Resolved, report: &mut ValidationReport) {
    let entry = &resolved.definition.entry;
    match &resolved.script {
        Ok(script) => {
            report.failures.extend(check_syntax(entry, script).await);
            report.failures.extend(
                library_tool_failures(script, entry, &resolved.library_tools, &resolved.definition.calls).await,
            );
        }
        Err(unreadable) => {
            report.failures.push(failure(
                unreadable.code,
                &unreadable.artefact,
                unreadable.message.clone(),
            ));
        }
    }
}

/// Checks that every server an allow list of the workflow names is configured, and that each tool its `[mcp_tools]`
/// allow list names is offered by that server, found by listing the server's tools. A server that is configured but
/// cannot be asked is unverifiable.
async fn check_effects(
    host: &SharedValidationHost<'_>,
    definition: &WorkflowDefinition,
    report: &mut ValidationReport,
) {
    for server in definition.calls.servers_named_by_allow_lists() {
        if !host.known_servers.iter().any(|known| known == server) {
            report.failures.push(failure(
                ValidationCode::ServerNotConfigured,
                &definition.folder,
                format!("the workflow names server {server:?} in an allow list, and it is not configured in web.toml"),
            ));
            continue;
        }
        let named_tools: Vec<&str> = definition
            .calls
            .lists()
            .tool_allow
            .iter()
            .filter(|entry| entry.server == server)
            .map(|entry| entry.tool.as_str())
            .collect();
        if named_tools.is_empty() {
            continue;
        }
        let Some(client) = host.clients.get(server) else {
            report.unverifiable.push(Unverifiable {
                check: format!("tools of {server}"),
                reason: format!("server {server:?} is not connected"),
            });
            continue;
        };
        let offered = match client.list_all_tools().await {
            Ok(tools) => tools,
            Err(error) => {
                report.unverifiable.push(Unverifiable {
                    check: format!("tools of {server}"),
                    reason: error.to_string(),
                });
                continue;
            }
        };
        for tool in named_tools {
            if !offered.iter().any(|offered| offered.name == tool) {
                report.failures.push(failure(
                    ValidationCode::ToolNotFound,
                    &definition.folder,
                    format!("the workflow allows tool {tool:?}, which server {server:?} does not offer"),
                ));
            }
        }
    }
}

/// Checks the job's values against the workflow's declared inputs, and every `vault` input against the vaults the primary
/// server reports.
async fn check_inputs(
    host: &SharedValidationHost<'_>,
    definition: &WorkflowDefinition,
    job: &SharedJob,
    job_file: &str,
    report: &mut ValidationReport,
) {
    let inputs = match ValidatedInputs::try_from(SuppliedInputs {
        declared: definition.inputs.fields(),
        values: job.inputs.clone(),
    }) {
        Ok(inputs) => inputs,
        Err(error) => {
            report
                .failures
                .push(failure(ValidationCode::InputInvalid, job_file, error.to_string()));
            return;
        }
    };
    if inputs.vault_names().is_empty() {
        return;
    }
    if host.clients.get(host.primary_server).is_none() {
        report.unverifiable.push(Unverifiable {
            check: "vault inputs".to_owned(),
            reason: format!("the primary server {:?} is not connected", host.primary_server),
        });
        return;
    }
    match reported_vaults(host.clients, host.primary_server).await {
        Err(unreadable) => report.unverifiable.push(Unverifiable {
            check: "vault inputs".to_owned(),
            reason: unreadable.message,
        }),
        Ok(reported) => {
            if let Err(missing) = require_vaults_configured(&inputs, &reported) {
                report
                    .failures
                    .push(failure(ValidationCode::InputInvalid, job_file, missing.message));
            }
        }
    }
}
