//! Finding what a vault's `registry/workflows/` holds: the packages, their workflows, and the jobs saved beneath them.
//!
//! A vault package is a folder `registry/workflows/{package}/` with a `workflows.toml` that lists its workflows, and each
//! workflow is a folder of its own, `{package}/{workflow}/`, with a `workflow.toml`. A job is found by listing the job
//! files where they live, and its workflow is the folder it sits in.

use std::collections::HashSet;

use contextos_workflows::{PackageName, SourceText, ValidationCode, ValidationFailure, WorkflowName, WorkflowsFile};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::jobs::{
    JobLoadOutcome, RegisteredJob, RegisteredWorkflow, ToolErrorResult, declarations_from_text, failure, load_job,
    unconfigured_server,
};
use crate::mcp_client::{McpCallError, McpClient};
use crate::source_read::{SourceRead, read_source, unexpected_answer};

#[derive(Debug, Deserialize)]
struct DirEntryResult {
    name: String,
    kind: String,
}

#[derive(Debug, Deserialize)]
struct ListDirectoryResult {
    entries: Vec<DirEntryResult>,
}

/// Discovers every workflow of every package under `vault_name`'s `registry/workflows/`, then the jobs saved in each
/// valid workflow's own `jobs/` folder, flattened into one list. A package folder that is not a package (a bad name, no
/// `workflows.toml`, or one that does not parse) registers nothing; a listed workflow that is invalid, a folder holding a
/// `workflow.toml` that `workflows.toml` does not list, and none of their jobs are discovered. Each is logged with `tracing::warn!` and shown by the
/// workflow index ([`crate::job_index::index_workflows`]) with its reason. `registry/workflows/` itself not existing yet is
/// not an error.
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the MCP transport itself
/// fails.
pub async fn discover_jobs(
    client: &McpClient,
    vault_name: &str,
    known_mcp_servers: &[String],
) -> Result<Vec<RegisteredJob>, McpCallError> {
    let workflows = discover_workflows(client, vault_name, known_mcp_servers).await?;
    if workflows.is_empty() {
        return Ok(Vec::new());
    }
    let team = crate::team::load_team(client, vault_name).await;
    let mut jobs = Vec::new();
    for workflow in &workflows {
        jobs.extend(discover_jobs_for_workflow(client, vault_name, workflow, &team).await?);
    }
    jobs.sort_by(|a, b| {
        (a.package.as_str(), a.workflow_slug.as_str(), a.slug.as_str()).cmp(&(
            b.package.as_str(),
            b.workflow_slug.as_str(),
            b.slug.as_str(),
        ))
    });
    Ok(jobs)
}

/// Discovers and validates every workflow of every package under `vault_name`'s `registry/workflows/`.
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the MCP transport itself
/// fails.
pub async fn discover_workflows(
    client: &McpClient,
    vault_name: &str,
    known_mcp_servers: &[String],
) -> Result<Vec<RegisteredWorkflow>, McpCallError> {
    let mut workflows = Vec::new();
    for folder in read_package_folders(client, vault_name).await? {
        let package = folder.name.clone();
        let source = match folder.file {
            PackageFileStanding::Refused(found) => {
                tracing::warn!(vault = vault_name, package = %package, reason = %found.message, "package registration failed");
                continue;
            }
            PackageFileStanding::Listed(source) => source,
        };
        for unlisted in source
            .unlisted_workflows(client, vault_name, &folder.workflow_folders)
            .await?
        {
            tracing::warn!(
                vault = vault_name,
                package = %package,
                workflow = %unlisted,
                "workflow registration failed: the folder holds a workflow.toml that workflows.toml does not list"
            );
        }
        for directory in source.file.workflows() {
            match load_workflow(client, vault_name, &source, directory, known_mcp_servers).await? {
                WorkflowLoadOutcome::Invalid(found) => {
                    tracing::warn!(vault = vault_name, package = %package, dir = %directory, reason = %found.message, "workflow registration failed");
                }
                WorkflowLoadOutcome::Valid(workflow) => workflows.push(*workflow),
            }
        }
    }
    workflows.sort_by(|a, b| (&a.package, &a.slug).cmp(&(&b.package, &b.slug)));
    Ok(workflows)
}

/// A workflow's folder under `registry/workflows/`: its package's folder and its own, which together are the path its
/// files are kept under.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkflowFolder<'a> {
    pub package: &'a str,
    pub directory: &'a str,
}

impl std::fmt::Display for WorkflowFolder<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}/{}", self.package, self.directory)
    }
}

/// A vault package read from source: its folder's name and what its `workflows.toml` declares.
#[derive(Clone, Debug)]
pub(crate) struct SourcePackage {
    pub name: String,
    pub file: WorkflowsFile,
}

/// A package folder under `registry/workflows/`, and what it is.
pub(crate) struct PackageFolder {
    pub name: String,
    pub file: PackageFileStanding,
    /// The folders directly beneath it that may hold a workflow or its jobs: those named as a workflow may be.
    pub workflow_folders: Vec<String>,
}

impl PackageFolder {
    /// The workflow folders of the package.
    pub(crate) fn folders(&self) -> impl Iterator<Item = WorkflowFolder<'_>> {
        self.workflow_folders.iter().map(|directory| WorkflowFolder {
            package: &self.name,
            directory,
        })
    }
}

/// What a package folder's `workflows.toml` makes of it.
pub(crate) enum PackageFileStanding {
    /// The folder is not a workflow package, for the reason given, which names the folder name or the file.
    Refused(ValidationFailure),
    Listed(Box<SourcePackage>),
}

/// Reads the `workflows.toml` of the package folder `name`: a name that is not a package name, a file that is not there,
/// one the server will not hand over, and one that does not parse each refuse the folder, for the reason that is true.
///
/// # Errors
///
/// As [`crate::source_read::read_source`]: an outage is an error and never a refusal of the folder.
pub(crate) async fn read_package_file(
    client: &McpClient,
    vault_name: &str,
    name: &str,
) -> Result<PackageFileStanding, McpCallError> {
    if let Err(error) = PackageName::try_from(name) {
        return Ok(PackageFileStanding::Refused(failure(
            ValidationCode::ArtefactInvalid,
            name,
            format!("{error}, so the folder is not a package"),
        )));
    }
    let path = format!("{vault_name}://registry/workflows/{name}/workflows.toml");
    let text = match read_source(client, &path).await? {
        SourceRead::Text(text) => text,
        SourceRead::Missing => {
            return Ok(PackageFileStanding::Refused(failure(
                ValidationCode::ArtefactMissing,
                "workflows.toml",
                format!("the folder {name} has no workflows.toml, so it is not a workflow package"),
            )));
        }
        SourceRead::Unusable(reason) => {
            return Ok(PackageFileStanding::Refused(failure(
                ValidationCode::ArtefactInvalid,
                "workflows.toml",
                format!("workflows.toml {reason}"),
            )));
        }
    };
    Ok(match WorkflowsFile::try_from(SourceText(&text)) {
        Ok(file) => PackageFileStanding::Listed(Box::new(SourcePackage {
            name: name.to_owned(),
            file,
        })),
        Err(error) => PackageFileStanding::Refused(failure(
            ValidationCode::ArtefactInvalid,
            "workflows.toml",
            error.to_string(),
        )),
    })
}

/// Every package folder under `vault_name`'s `registry/workflows/`, in name order, with what each is and the folders in it
/// that may hold workflows or jobs. The one rule for which folders those are, whoever asks.
///
/// # Errors
///
/// As [`read_package_file`].
pub(crate) async fn read_package_folders(
    client: &McpClient,
    vault_name: &str,
) -> Result<Vec<PackageFolder>, McpCallError> {
    let mut folders = Vec::new();
    for name in list_registry_directories(client, vault_name, "registry/workflows").await? {
        let file = read_package_file(client, vault_name, &name).await?;
        let mut workflow_folders = Vec::new();
        if PackageName::try_from(name.as_str()).is_ok() {
            for directory in
                list_registry_directories(client, vault_name, &format!("registry/workflows/{name}")).await?
            {
                if WorkflowName::try_from(directory.as_str()).is_ok() {
                    workflow_folders.push(directory);
                }
            }
            // A workflow the package lists is looked at whether or not its folder is there, so a listing with no folder
            // behind it is seen and not skipped.
            if let PackageFileStanding::Listed(source) = &file {
                for listed in source.file.workflows() {
                    if !workflow_folders.contains(listed) {
                        workflow_folders.push(listed.clone());
                    }
                }
            }
        }
        folders.push(PackageFolder {
            name,
            file,
            workflow_folders,
        });
    }
    folders.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(folders)
}

impl SourcePackage {
    /// Whether the package lists the workflow in `directory`.
    pub(crate) fn lists(&self, directory: &str) -> bool {
        self.file.workflows().iter().any(|listed| listed == directory)
    }

    /// The folders of `candidates` that hold a `workflow.toml` and are not listed in `workflows.toml`.
    pub(crate) async fn unlisted_workflows(
        &self,
        client: &McpClient,
        vault_name: &str,
        candidates: &[String],
    ) -> Result<Vec<String>, McpCallError> {
        let mut unlisted = Vec::new();
        for directory in candidates {
            if self.lists(directory) {
                continue;
            }
            if exists_in_package(client, vault_name, &self.name, &format!("{directory}/workflow.toml")).await? {
                unlisted.push(directory.clone());
            }
        }
        Ok(unlisted)
    }
}

/// Why the source of the workflow in `folder` is not registered because of its package, nothing when the package is a
/// package and lists the workflow. The package standing a job's validation is held to, whatever release the job names.
///
/// # Errors
///
/// As [`read_package_file`].
pub(crate) async fn source_refusal(
    client: &McpClient,
    vault_name: &str,
    folder: WorkflowFolder<'_>,
) -> Result<Option<ValidationFailure>, McpCallError> {
    Ok(match read_package_file(client, vault_name, folder.package).await? {
        PackageFileStanding::Refused(found) => Some(found),
        PackageFileStanding::Listed(source) if source.lists(folder.directory) => None,
        PackageFileStanding::Listed(_) => Some(unlisted_refusal(folder.directory)),
    })
}

/// The refusal of a folder that a package's `workflows.toml` does not list.
pub(crate) fn unlisted_refusal(directory: &str) -> ValidationFailure {
    failure(
        ValidationCode::ArtefactInvalid,
        "workflows.toml",
        format!(
            "workflows.toml does not list the workflow {directory}, so the folder {directory} is not a workflow of the package"
        ),
    )
}

/// The entries directly under `path` in `vault_name`. A folder that does not exist has none: only a `path/not-found`
/// refusal says so. Any other refusal, and an answer that is not a listing, leave what the folder holds unknown, which is
/// an outage and not an empty folder: reading it as empty would report every library, workflow, or job in it missing.
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the transport itself fails, and [`McpCallError::Refused`] when the server
/// answered with something that is not the folder's listing.
async fn list_entries(client: &McpClient, vault_name: &str, path: &str) -> Result<Vec<DirEntryResult>, McpCallError> {
    let mut args = Map::new();
    args.insert("path".to_owned(), Value::String(format!("{vault_name}://{path}")));
    let result = client.call_tool("fs_list_directory".to_owned(), args).await?;
    let refused = || unexpected_answer(client);
    if result.is_error == Some(true) {
        return match ToolErrorResult::from(&result).code.as_deref() {
            Some("path/not-found") => Ok(Vec::new()),
            _ => Err(refused()),
        };
    }
    let Ok(listing) = result.into_typed::<ListDirectoryResult>() else {
        return Err(refused());
    };
    Ok(listing.entries)
}

/// The folders directly under `path` in `vault_name`; see [`list_entries`] for what an empty answer and an error mean.
///
/// # Errors
///
/// As [`list_entries`].
pub(crate) async fn list_registry_directories(
    client: &McpClient,
    vault_name: &str,
    path: &str,
) -> Result<Vec<String>, McpCallError> {
    Ok(list_entries(client, vault_name, path)
        .await?
        .into_iter()
        .filter(|entry| entry.kind == "dir")
        .map(|entry| entry.name)
        .collect())
}

pub(crate) enum WorkflowLoadOutcome {
    Invalid(ValidationFailure),
    Valid(Box<RegisteredWorkflow>),
}

/// Loads the workflow `dir_name` that `package` lists: its `workflow.toml`, its entry script beneath its own folder, and
/// the package's modules and templates, which are relative to the package.
pub(crate) async fn load_workflow(
    client: &McpClient,
    vault_name: &str,
    package: &SourcePackage,
    dir_name: &str,
    known_mcp_servers: &[String],
) -> Result<WorkflowLoadOutcome, McpCallError> {
    let source = package;
    let artefact = format!("{dir_name}/workflow.toml");
    let manifest = format!("{vault_name}://registry/workflows/{}/{artefact}", source.name);
    let read = match read_source(client, &manifest).await? {
        SourceRead::Text(text) => text,
        SourceRead::Missing => {
            return Ok(WorkflowLoadOutcome::Invalid(failure(
                ValidationCode::ArtefactMissing,
                &artefact,
                format!("workflows.toml lists the workflow {dir_name}, which has no {artefact}"),
            )));
        }
        SourceRead::Unusable(reason) => {
            return Ok(WorkflowLoadOutcome::Invalid(failure(
                ValidationCode::ArtefactInvalid,
                &artefact,
                format!("{artefact} {reason}"),
            )));
        }
    };
    let workflow = match declarations_from_text(
        &read,
        WorkflowFolder {
            package: &source.name,
            directory: dir_name,
        },
    ) {
        Ok(workflow) => workflow.declaring(&source.file),
        Err(found) => return Ok(WorkflowLoadOutcome::Invalid(found)),
    };
    if let Some(found) = unconfigured_server(&workflow.calls, known_mcp_servers) {
        return Ok(WorkflowLoadOutcome::Invalid(found));
    }
    let entry = format!("{dir_name}/{}", workflow.entry);
    if !exists_in_package(client, vault_name, &source.name, &entry).await? {
        return Ok(WorkflowLoadOutcome::Invalid(failure(
            ValidationCode::ArtefactMissing,
            &entry,
            format!("entry file {:?} does not exist", workflow.entry),
        )));
    }
    // A template is read live from source when a run reads it, and the release fixes only its declared path, so a
    // template that is gone does not make the workflow invalid: it fails the run that reads it.
    for module in workflow.modules.entries() {
        if !exists_in_package(client, vault_name, &source.name, module.as_str()).await? {
            return Ok(WorkflowLoadOutcome::Invalid(failure(
                ValidationCode::ArtefactMissing,
                module.as_str(),
                format!("module {:?} declared in workflows.toml does not exist", module.as_str()),
            )));
        }
    }
    Ok(WorkflowLoadOutcome::Valid(Box::new(workflow)))
}

/// Whether `relative`, a file or folder within the package's folder, exists.
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the transport itself fails.
pub(crate) async fn exists_in_package(
    client: &McpClient,
    vault_name: &str,
    package: &str,
    relative: &str,
) -> Result<bool, McpCallError> {
    let mut args = Map::new();
    args.insert(
        "path".to_owned(),
        Value::String(format!("{vault_name}://registry/workflows/{package}/{relative}")),
    );
    let result = client.call_tool("fs_get_file_info".to_owned(), args).await?;
    Ok(result.is_error != Some(true))
}

/// Discovers and validates every job saved under `workflow`'s own
/// `registry/workflows/{{package}}/{{workflow}}/jobs/` directory. A job file
/// failing schema validation, an invalid `schedule`, or one naming an
/// `owner` absent from `.contextos/team.toml` fails registration for that
/// job specifically without affecting any other job under the same or a
/// different workflow. The `jobs/` directory itself not existing yet is
/// not an error (a workflow may be manual-invocation-only, with no saved
/// job at all).
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the MCP transport itself
/// fails.
pub async fn discover_jobs_for_workflow(
    client: &McpClient,
    vault_name: &str,
    workflow: &RegisteredWorkflow,
    team: &[crate::team::TeamMember],
) -> Result<Vec<RegisteredJob>, McpCallError> {
    let files = list_job_files(client, vault_name, workflow.folder()).await?;
    let mut jobs = Vec::with_capacity(files.len());
    let mut seen_slugs = HashSet::new();
    for file_name in files {
        match load_job(client, vault_name, workflow, &file_name, team).await? {
            JobLoadOutcome::Invalid(failure) => {
                tracing::warn!(
                    vault = vault_name,
                    workflow = %workflow.slug,
                    file = %file_name,
                    reason = %failure.message,
                    "job registration failed"
                );
            }
            JobLoadOutcome::Valid(job) => {
                if seen_slugs.insert(job.slug.clone()) {
                    jobs.push(*job);
                } else {
                    tracing::warn!(
                        vault = vault_name,
                        workflow = %workflow.slug,
                        slug = %job.slug,
                        "job registration failed: duplicate slug within this workflow"
                    );
                }
            }
        }
    }
    jobs.sort_by(|a, b| a.slug.cmp(&b.slug));
    Ok(jobs)
}

/// The job files (`*.toml`) of a workflow's `jobs` folder. A workflow with no `jobs` folder has no jobs; a listing the
/// server refuses for any other reason, or answers with something that is not a listing, is an outage and changes
/// nothing.
///
/// # Errors
///
/// As [`list_entries`].
pub(crate) async fn list_job_files(
    client: &McpClient,
    vault_name: &str,
    folder: WorkflowFolder<'_>,
) -> Result<Vec<String>, McpCallError> {
    Ok(
        list_entries(client, vault_name, &format!("registry/workflows/{folder}/jobs"))
            .await?
            .into_iter()
            .filter(|entry| {
                entry.kind == "file"
                    && std::path::Path::new(&entry.name)
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("toml"))
            })
            .map(|entry| entry.name)
            .collect(),
    )
}

#[cfg(test)]
#[path = "job_discovery_test.rs"]
mod tests;
