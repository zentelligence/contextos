//! `GET /{{vault_name}}/packages`: the library releases a vault's package store holds, each with the workflow releases
//! locked to it, so a person can see which workflow releases still run on an older library release. The page only reads the
//! store, its state, and its jobs; installing and removing are the command line's and the API's.

use askama::Template;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;

use crate::job_operation::{OperatorRequest, perform_retaining};
use crate::job_operation_vault::VaultJobStore;
use crate::library_dependents::{
    Carried, Command, Dependent, DependentsBlock, DependentsInput, Picked, carry_out, dependents_of, element_id,
    library_named,
};
use crate::library_status::{check_libraries, unchecked_libraries};
use crate::library_views::{LibrariesInput, LibrariesSection};
use crate::mcp_client::McpClient;
use crate::package_op_error::PackageOpError;
use crate::package_standing::StoreStanding;
use crate::package_stores::VaultPackagesOf;
use crate::rendering::page;
use crate::routes::vault::{html_response, not_found, unreachable};
use crate::routes::workflow_views::{Scope, ScopeTab};
use crate::routes::workflows::{
    WorkflowRoutesState, WorkflowsQuery, ensure_vault, nav_with_appearance, registry_error_response,
};
use crate::shared_claim::SharedJobKey;
use crate::shared_job_authoring::JobListing;
use crate::vault_job_authoring::VaultJobAuthoring;

/// What the library section is made from: the store's standing and the vault's jobs.
async fn world_of(
    state: &WorkflowRoutesState,
    client: &McpClient,
    vault: &str,
) -> Result<(StoreStanding, Vec<JobListing>), PackageOpError> {
    let standing = VaultPackagesOf { client, vault }.standing().await?;
    let known_servers = state.clients.names();
    let jobs = VaultJobAuthoring {
        client,
        vault,
        known_servers: &known_servers,
    }
    .list()
    .await
    .map_err(|_| PackageOpError::Host {
        reason: "the vault's jobs could not be listed",
    })?;
    Ok((standing, jobs))
}

#[derive(Template)]
#[template(path = "vault_packages.html")]
struct VaultPackagesPage {
    /// The tabs to the vault's workflows and the shared workflows.
    scope: Vec<ScopeTab>,
    libraries: LibrariesSection,
}

/// `GET /{{vault_name}}/packages`.
pub async fn page(
    State(state): State<WorkflowRoutesState>,
    Path(vault_name): Path<String>,
    Query(query): Query<WorkflowsQuery>,
    headers: HeaderMap,
) -> Response {
    let client = match state.client() {
        Ok(client) => client,
        Err(error) => return registry_error_response(error),
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let urls = crate::urls::VaultUrls::from(&*vault_name);
    let libraries = match world_of(&state, &client, &vault_name).await {
        Ok((standing, jobs)) => {
            let statuses = match client.vault_packages(&vault_name).await {
                Ok(engine) => check_libraries(&engine, &standing).await,
                Err(error) => {
                    tracing::warn!(vault = %vault_name, %error, "a vault's package store could not be opened to check its libraries");
                    unchecked_libraries(
                        &standing,
                        "the vault's package store could not be opened to check the release",
                    )
                }
            };
            LibrariesSection::from(LibrariesInput {
                standing: &standing,
                workflow_href: &|package, workflow| urls.workflow(package, workflow),
                statuses: &statuses,
                jobs: &jobs,
                release_href: &|name, version| format!("{}/{name}/versions/{version}", urls.packages()),
                job_href: &|key| urls.job(&key.package, &key.workflow, &key.job),
            })
        }
        Err(error) => {
            tracing::warn!(vault = %vault_name, code = error.code(), "a vault's library locks could not be read for its packages page");
            LibrariesSection::unreadable(&format!("The library locks could not be read ({}).", error.code()))
        }
    };
    let Ok(fragment) = (VaultPackagesPage {
        scope: Scope::VaultLibraries(&vault_name).tabs(&[]),
        libraries,
    })
    .render() else {
        tracing::warn!("a vault's packages page could not be rendered");
        return unreachable();
    };
    if headers.contains_key("hx-request") {
        html_response(fragment)
    } else {
        let mut nav = nav_with_appearance(
            &state,
            &client,
            &vault_name,
            "Workflows",
            query.entity(),
            &urls.packages(),
            crate::routes::sidebar_wide_from_headers(&headers),
        )
        .await;
        nav.page_title = Some(crate::routes::shared_workflows::screen_title(
            "Library Releases",
            "library",
        ));
        html_response(page::render_page(&nav, "Workflows", &fragment))
    }
}

/// `PUT /{vault_name}/packages/{name}/versions/{version}/execute/{command}`: validates every job on the workflow releases
/// that lock the library release (`validate-dependents`), or resumes the picked ones (`resume-dependents`), each through
/// the operation a job's own page uses, and answers with the library release's jobs block, one result per job.
pub async fn execute(
    State(state): State<WorkflowRoutesState>,
    Path((vault_name, name, version, command)): Path<(String, String, String, String)>,
    body: Bytes,
) -> Response {
    let Ok(command) = Command::try_from(command.as_str()) else {
        return not_found();
    };
    let client = match state.client() {
        Ok(client) => client,
        Err(error) => return registry_error_response(error),
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let (standing, jobs) = match world_of(&state, &client, &vault_name).await {
        Ok(world) => world,
        Err(error) => {
            tracing::warn!(vault = %vault_name, code = error.code(), "a vault's library locks could not be read for a library action");
            return unreachable();
        }
    };
    let Some(library) = library_named(&standing, &name, &version) else {
        return not_found();
    };
    let urls = crate::urls::VaultUrls::from(&*vault_name);
    let job_href = |key: &SharedJobKey| urls.job(&key.package, &key.workflow, &key.job);
    let dependents = dependents_of(&standing, &jobs, &name, &library.release.version);
    let known_servers = state.clients.names();
    let now = state.host.clock.instant().await;
    let run = |key: SharedJobKey, request: OperatorRequest| {
        let (client, known_servers, vault_name, state) = (&client, &known_servers, &vault_name, &state);
        async move {
            let store = VaultJobStore {
                client,
                vault_name,
                known_mcp_servers: known_servers,
                package: &key.package,
                workflow_slug: &key.workflow,
                job_slug: &key.job,
                claims_root: state.host.claims_root.as_deref(),
            };
            perform_retaining(&store, request, now).await
        }
    };
    let carried = match Picked::try_from(&*body) {
        Ok(picked) => carry_out(command, &dependents, &picked, &job_href, run).await,
        Err(()) => Carried::unreadable_pick(),
    };
    // What the actions did can move a job off this library release, so the jobs are read again.
    let dependents: Vec<Dependent> = match world_of(&state, &client, &vault_name).await {
        Ok((standing, jobs)) => dependents_of(&standing, &jobs, &name, &library.release.version),
        Err(_) => dependents,
    };
    let id = element_id(&name, &version);
    let block = DependentsBlock::from(DependentsInput {
        id: format!("{id}-jobs"),
        base: format!("{}/{name}/versions/{version}", urls.packages()),
        dependents: &dependents,
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
