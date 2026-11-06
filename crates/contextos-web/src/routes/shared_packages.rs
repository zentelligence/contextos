//! `GET /shared/packages`: the packages installed on the host, each release with the modules it exports and the installed
//! releases that depend on it, and the releases that are installed and cannot be read, each reported for itself. The page
//! only reads the host's package store; installing and removing are the command line's and the API's.

use std::sync::Arc;

use askama::Template;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;

use crate::host_store::HostStore;
use crate::job_operation::{OperatorRequest, perform_retaining};
use crate::job_operation_shared::SharedJobStore;
use crate::library_dependents::{
    Carried, Command, Dependent, DependentsBlock, DependentsInput, Picked, carry_out, dependents_of, element_id,
    library_named,
};
use crate::library_status::check_libraries;
use crate::library_views::{LibrariesInput, LibrariesSection};
use crate::package_listing::PackageListing;
use crate::package_op_error::PackageOpError;
use crate::package_standing::StoreStanding;
use crate::package_store::{self, UnreadableRelease};
use crate::package_stores::SharedPackages;
use crate::routes::shared_workflows::{
    SharedRoutesState, crumb, job_href, respond_titled, screen_title, vault_names, workflow_href,
};
use crate::routes::vault::{html_response, not_found, unreachable};
use crate::routes::workflow_views::{Scope, ScopeTab};
use crate::saved_jobs::SavedJobs;
use crate::shared_claim::SharedJobKey;
use crate::shared_job_authoring::{JobListing, SharedJobAuthoring};
use crate::shared_state::SharedStateStore;

/// One readable release as a row of the table.
struct PackageRow {
    name: String,
    version: String,
    exports: Vec<String>,
    required_by: Vec<String>,
}

#[derive(Template)]
#[template(path = "packages_list.html")]
struct PackagesPage {
    /// The tabs to the shared workflows and back to each vault.
    scope: Vec<ScopeTab>,
    rows: Vec<PackageRow>,
    unreadable: Vec<UnreadableRelease>,
    /// The library releases and the workflow releases locked to each.
    libraries: LibrariesSection,
}

impl From<PackageListing> for PackagesPage {
    fn from(listing: PackageListing) -> Self {
        Self {
            scope: Vec::new(),
            rows: listing
                .releases()
                .iter()
                .map(|release| PackageRow {
                    name: release.identity.name.to_string(),
                    version: release.identity.version.to_string(),
                    exports: release.exports.clone(),
                    required_by: release.required_by.iter().map(ToString::to_string).collect(),
                })
                .collect(),
            unreadable: listing.unreadable,
            libraries: LibrariesSection::unreadable("The library locks have not been read."),
        }
    }
}

/// The host's package store, with the jobs and state that say what keeps its releases.
fn packages_of(state: &SharedRoutesState) -> SharedPackages {
    SharedPackages {
        store_root: state.roots.store_root.clone(),
        jobs: SavedJobs {
            jobs_root: state.roots.jobs_root.clone(),
            state: SharedStateStore::new(state.roots.state_root.clone()),
        },
    }
}

/// What the library section is made from: the store's standing and the host's jobs.
async fn world_of(state: &SharedRoutesState) -> Result<(StoreStanding, Vec<JobListing>), PackageOpError> {
    let standing = packages_of(state).standing().await?;
    let authoring = SharedJobAuthoring {
        jobs_root: state.roots.jobs_root.clone(),
        store_root: state.roots.store_root.clone(),
        state: SharedStateStore::new(state.roots.state_root.clone()),
    };
    // Listing the jobs folder blocks, so it runs on a thread that may.
    let jobs = tokio::task::spawn_blocking(move || authoring.list())
        .await
        .map_err(|_| PackageOpError::Host {
            reason: "listing the host's jobs did not finish",
        })?
        .map_err(|_| PackageOpError::Host {
            reason: "the host's jobs could not be listed",
        })?;
    Ok((standing, jobs))
}

fn release_href(name: &str, version: &str) -> String {
    format!("/shared/packages/{name}/versions/{version}")
}

/// The host's library releases with what each checks as, the workflow releases locked to each, and the jobs on them, or why
/// they could not be read.
async fn libraries_of(state: &SharedRoutesState) -> LibrariesSection {
    match world_of(state).await {
        Ok((standing, jobs)) => {
            let statuses = check_libraries(HostStore::new(&state.roots.store_root).engine(), &standing).await;
            LibrariesSection::from(LibrariesInput {
                standing: &standing,
                workflow_href: &|package, workflow| workflow_href(package, workflow),
                statuses: &statuses,
                jobs: &jobs,
                release_href: &release_href,
                job_href: &job_href,
            })
        }
        Err(error) => {
            tracing::warn!(
                code = error.code(),
                "the host's library locks could not be read for the packages page"
            );
            LibrariesSection::unreadable(&format!("The library locks could not be read ({}).", error.code()))
        }
    }
}

/// `PUT /shared/packages/{name}/versions/{version}/execute/{command}`: validates every job on the workflow releases that
/// lock the library release (`validate-dependents`), or resumes the picked ones (`resume-dependents`), each through the
/// operation a job's own page uses, and answers with the library release's jobs block, one result per job.
pub async fn execute(
    State(state): State<SharedRoutesState>,
    Path((name, version, command)): Path<(String, String, String)>,
    body: Bytes,
) -> Response {
    let Ok(command) = Command::try_from(command.as_str()) else {
        return not_found();
    };
    let (standing, jobs) = match world_of(&state).await {
        Ok(world) => world,
        Err(error) => {
            tracing::warn!(
                code = error.code(),
                "the host's library locks could not be read for a library action"
            );
            return unreachable();
        }
    };
    let Some(library) = library_named(&standing, &name, &version) else {
        return not_found();
    };
    let dependents = dependents_of(&standing, &jobs, &name, &library.release.version);
    let picked = Picked::try_from(&*body);
    let servers = state.current_servers();
    let host = state.validation_host(&servers);
    let now = state.vault_time.instant().await;
    let run = |key: SharedJobKey, request: OperatorRequest| {
        let (state, host) = (&state, &host);
        async move {
            let store = SharedJobStore {
                state: &state.state,
                jobs_root: &state.roots.jobs_root,
                validation: host,
                key: &key,
                claims_root: &state.roots.claims_root,
            };
            perform_retaining(&store, request, now).await
        }
    };
    let carried = match picked {
        Ok(picked) => carry_out(command, &dependents, &picked, &job_href, run).await,
        Err(()) => Carried::unreadable_pick(),
    };
    // What the actions did can move a job off this library release, so the jobs are read again.
    let dependents = match world_of(&state).await {
        Ok((standing, jobs)) => dependents_of(&standing, &jobs, &name, &library.release.version),
        Err(_) => dependents,
    };
    block_response(&name, &version, &dependents, carried)
}

fn block_response(name: &str, version: &str, dependents: &[Dependent], carried: Carried) -> Response {
    let id = element_id(name, version);
    let block = DependentsBlock::from(DependentsInput {
        id: format!("{id}-jobs"),
        base: release_href(name, version),
        dependents,
        href: &job_href,
        carried,
    });
    match block.render() {
        Ok(fragment) => html_response(fragment),
        Err(error) => {
            tracing::warn!(%error, "a library release's jobs could not be rendered");
            unreachable()
        }
    }
}

/// `GET /shared/packages`.
pub async fn list(State(state): State<SharedRoutesState>, headers: HeaderMap) -> Response {
    let roots = Arc::clone(&state.roots);
    let read = tokio::task::spawn_blocking(move || package_store::inventory(&roots.store_root)).await;
    let inventory = match read {
        Ok(Ok(inventory)) => inventory,
        Ok(Err(error)) => {
            tracing::warn!(%error, "the package store could not be read for the packages page");
            return unreachable();
        }
        Err(error) => {
            tracing::warn!(%error, "the package store could not be read for the packages page");
            return unreachable();
        }
    };
    let mut page = PackagesPage::from(PackageListing::from(inventory));
    page.libraries = libraries_of(&state).await;
    page.scope = Scope::SharedLibraries.tabs(&vault_names(&state).await);
    let fragment = match page.render() {
        Ok(fragment) => fragment,
        Err(error) => {
            tracing::warn!(%error, "the packages page could not be rendered");
            return unreachable();
        }
    };
    respond_titled(
        &state,
        &headers,
        vec![
            crumb("Shared workflows", Some(crate::urls::SHARED_WORKFLOWS.to_owned())),
            crumb("Library releases", None),
        ],
        fragment,
        Some(screen_title("Library releases", "library")),
    )
    .await
}
