//! An index of every job file in a vault, whether or not the job registers.
//!
//! Discovery lists only the jobs that load, which is right for the scheduler and wrong for an operator: a draft whose job
//! file fails validation would appear nowhere, and could not be validated or deleted from a page. The index walks the same
//! folders and reads each file's tracked slug, the one its state is kept under, and attaches the loaded job when there is
//! one. A workflow whose own manifest is invalid is listed with none of its jobs loaded, because a job loads against its
//! workflow.

use contextos_workflows::ValidationFailure;

use crate::job_discovery::{PackageFileStanding, read_package_folders, unlisted_refusal};
use crate::job_validation::{read_text, tracked_slug};
use crate::jobs::{
    JobLoadOutcome, RegisteredJob, RegisteredWorkflow, WorkflowFolder, WorkflowLoadOutcome, list_job_files, load_job,
    load_workflow,
};
use crate::mcp_client::{McpCallError, McpClient};

/// One workflow folder, with every job file under it.
#[derive(Debug)]
pub struct IndexedWorkflow {
    /// The slug the workflow is tracked under: its folder's name.
    pub slug: String,
    /// The package folder under `registry/workflows/` that holds the workflow's folder.
    pub package: String,
    /// The workflow, when the package lists it and its manifest is valid.
    pub loaded: Option<RegisteredWorkflow>,
    /// Why the workflow does not register, when it does not: the package does not list it or its files are not as they
    /// must be.
    pub refusal: Option<ValidationFailure>,
    /// Every job file, in slug order.
    pub jobs: Vec<IndexedJob>,
}

/// One job file.
#[derive(Debug)]
pub struct IndexedJob {
    /// The slug the job is tracked under: the one its file names, or its file's stem.
    pub slug: String,
    /// The file's name within the workflow's `jobs/` folder.
    pub file: String,
    /// The job, when it registers.
    pub loaded: Option<RegisteredJob>,
}

/// A folder under `registry/workflows/` that is not a workflow package, and why.
#[derive(Debug)]
pub struct RefusedPackage {
    pub package: String,
    pub refusal: ValidationFailure,
}

/// What a vault's `registry/workflows/` holds.
#[derive(Debug, Default)]
pub struct WorkflowIndex {
    /// Every workflow folder of every package, in package and slug order.
    pub workflows: Vec<IndexedWorkflow>,
    /// Every package folder that is not a package, in name order.
    pub refused_packages: Vec<RefusedPackage>,
}

/// Indexes every workflow folder of every package under the vault's `registry/workflows/`, in package and slug order.
/// Jobs are found by listing the job files where they live, so a job file is indexed whether or not its workflow can be
/// read; its workflow is the folder it sits in. A folder that is neither a listed workflow nor holds a workflow file or a
/// job file is not a workflow and is left out. A package folder that is not a package, a workflow folder that is not
/// listed, a listed workflow that is invalid, and a workflow whose name an earlier package holds are each kept, with the
/// reason, so an operator sees what was not registered. Two files that name one slug in a workflow are listed once, by
/// the first.
///
/// # Errors
///
/// Returns [`McpCallError::Unreachable`] when the MCP transport itself fails.
pub async fn index_workflows(
    client: &McpClient,
    vault_name: &str,
    known_mcp_servers: &[String],
) -> Result<WorkflowIndex, McpCallError> {
    let packages = read_package_folders(client, vault_name).await?;
    if packages.is_empty() {
        return Ok(WorkflowIndex::default());
    }
    let team = crate::team::load_team(client, vault_name).await;
    let mut index = WorkflowIndex::default();
    for package in packages {
        let source = match &package.file {
            PackageFileStanding::Listed(source) => Some(source.as_ref()),
            PackageFileStanding::Refused(found) => {
                index.refused_packages.push(RefusedPackage {
                    package: package.name.clone(),
                    refusal: found.clone(),
                });
                None
            }
        };
        for folder in package.folders() {
            let (loaded, refusal) = match source {
                Some(source) if source.lists(folder.directory) => {
                    match load_workflow(client, vault_name, source, folder.directory, known_mcp_servers).await? {
                        WorkflowLoadOutcome::Valid(workflow) => (Some(*workflow), None),
                        WorkflowLoadOutcome::Invalid(found) => (None, Some(found)),
                    }
                }
                Some(_) => (None, Some(unlisted_refusal(folder.directory))),
                None => (None, None),
            };
            let jobs = index_jobs(client, vault_name, folder, loaded.as_ref(), &team).await?;
            let holds_workflow_file = match source {
                Some(source) => {
                    !source.lists(folder.directory)
                        && !source
                            .unlisted_workflows(client, vault_name, &[folder.directory.to_owned()])
                            .await?
                            .is_empty()
                }
                None => false,
            };
            let listed = source.is_some_and(|source| source.lists(folder.directory));
            if jobs.is_empty() && !listed && !holds_workflow_file {
                continue;
            }
            index.workflows.push(IndexedWorkflow {
                slug: folder.directory.to_owned(),
                package: folder.package.to_owned(),
                loaded,
                refusal,
                jobs,
            });
        }
    }
    index
        .workflows
        .sort_by(|a, b| (&a.package, &a.slug).cmp(&(&b.package, &b.slug)));
    Ok(index)
}

async fn index_jobs(
    client: &McpClient,
    vault_name: &str,
    folder: WorkflowFolder<'_>,
    workflow: Option<&RegisteredWorkflow>,
    team: &[crate::team::TeamMember],
) -> Result<Vec<IndexedJob>, McpCallError> {
    let mut jobs: Vec<IndexedJob> = Vec::new();
    for file in list_job_files(client, vault_name, folder).await? {
        let Some(text) = read_text(
            client,
            &format!("{vault_name}://registry/workflows/{folder}/jobs/{file}"),
        )
        .await?
        else {
            continue;
        };
        let stem = file.strip_suffix(".toml").unwrap_or(&file);
        let Some(slug) = tracked_slug(&text, stem) else {
            continue;
        };
        if jobs.iter().any(|job| job.slug == slug) {
            tracing::warn!(vault = vault_name, workflow = %folder, slug = %slug, "two job files name one slug, so only the first is listed");
            continue;
        }
        let loaded = match workflow {
            Some(workflow) => match load_job(client, vault_name, workflow, &file, team).await? {
                JobLoadOutcome::Valid(job) => Some(*job),
                JobLoadOutcome::Invalid(_) => None,
            },
            None => None,
        };
        jobs.push(IndexedJob { slug, file, loaded });
    }
    jobs.sort_by(|a, b| a.slug.cmp(&b.slug));
    Ok(jobs)
}
