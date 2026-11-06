//! The host's shared workflows and the jobs under them: `GET /shared/workflows` lists every workflow of every installed package with
//! its jobs, filterable by state, and `GET /shared/workflows/{{package}}/{{workflow}}` shows one. A job, and what an operator
//! does to it, is in [`crate::routes::shared_workflow_jobs`].
//!
//! A shared workflow belongs to a package, not to a vault, so these pages take no vault and read the host's package store,
//! job files, state, and run records. The hierarchy is the one a vault's workflows have, workflows then jobs then runs, with
//! the package standing where the vault does in the URL, and the views are the same ones
//! ([`crate::routes::workflow_views`]). Every job file is listed, whether or not it loads.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use contextos_workflows::{Clock, JobsState, SystemClock};
use serde::Deserialize;
use thiserror::Error;

use crate::config::WorkflowsConfig;
use crate::jobs::is_url_safe_slug;
use crate::mcp_client::{McpClient, McpClientSet};
use crate::page_runs::PageRuns;
use crate::release_views::{ReleasesInput, ReleasesSection};
use crate::rendering::page::{self, BreadcrumbSegment, PageTitle};
use crate::rendering::shell::{self, HostScreen};
use crate::routes::vault::{error_response, html_response, not_found, unreachable};
use crate::routes::workflow_views::{
    CardParts, Fact, JobCard, Link, Problem, Scope, StateCounts, StateFilter, WorkflowDetail, WorkflowEntry,
    WorkflowsList, render_detail, render_list, state_tiles,
};
use crate::run_control_shared::{SharedRunControl, SharedRunHost};
use crate::scheduler::{RunKind, run_allowed};
use crate::shared_claim::SharedJobKey;
use crate::shared_index::{SharedIndexError, SharedIndexedJob, SharedWorkflow, index_shared};
use crate::shared_runs::recent_shared_runs;
use crate::shared_state::{SharedStateError, SharedStateStore};
use crate::shared_validation::SharedValidationHost;
use crate::view_favourites::WORKFLOWS;
use crate::workflow_releases_stores::SharedReleases;

/// Where the host keeps what the shared pages read, under the platform app-data directory.
#[derive(Clone, Debug)]
pub struct SharedRoots {
    pub store_root: PathBuf,
    pub jobs_root: PathBuf,
    pub runs_root: PathBuf,
    pub state_root: PathBuf,
    /// Where each job's claim, active-run descriptor, and cancel request are kept.
    pub claims_root: PathBuf,
}

/// What a shared page needs: the host's roots, the MCP sessions a validation reads, the clock a change is stamped with, and
/// `web.toml`'s own path (read fresh per request for the shell's appearance).
#[derive(Clone)]
pub struct SharedRoutesState {
    pub(crate) roots: Arc<SharedRoots>,
    pub(crate) clients: Arc<McpClientSet>,
    pub(crate) state: SharedStateStore,
    pub(crate) clock: Arc<dyn Clock>,
    /// The same local clock, read asynchronously by a page so the server's zone is resolved before a stamp is taken.
    pub(crate) vault_time: Arc<crate::vault_time::VaultTime>,
    /// What this process has started from pages, which a page shows while it runs and after it ends.
    pub(crate) page_runs: Arc<PageRuns>,
    /// What a run started from a page needs of the host.
    pub(crate) run_host: SharedRunHost,
    web_config_path: Arc<PathBuf>,
}

impl SharedRoutesState {
    #[must_use]
    pub fn new(roots: SharedRoots, clients: Arc<McpClientSet>, web_config_path: Arc<PathBuf>) -> Self {
        let state = SharedStateStore::new(roots.state_root.clone());
        let workflows = WorkflowsConfig::default();
        // Read synchronously by runs and stamps; the server's zone is resolved by the first page that needs it, and
        // this host's zone stands in until then.
        let vault_time = Arc::new(crate::vault_time::VaultTime::from(crate::vault_time::VaultTimeInput {
            clock: Arc::new(SystemClock),
            clients: Arc::clone(&clients),
        }));
        let clock: Arc<dyn Clock> = vault_time.clone();
        Self {
            run_host: SharedRunHost {
                clients: Arc::clone(&clients),
                store_root: roots.store_root.clone(),
                jobs_root: roots.jobs_root.clone(),
                claims_root: roots.claims_root.clone(),
                runs_root: roots.runs_root.clone(),
                state: state.clone(),
                run_retention: workflows.run_retention(),
                log_level: workflows.log_level,
            },
            state,
            roots: Arc::new(roots),
            clients,
            clock,
            vault_time,
            page_runs: Arc::new(PageRuns::default()),
            web_config_path,
        }
    }

    /// The `[workflows]` defaults from `web.toml` that a run started from a page uses: how long its records are kept, and the
    /// log level.
    #[must_use]
    pub fn with_workflows(mut self, workflows: &WorkflowsConfig) -> Self {
        self.run_host.run_retention = workflows.run_retention();
        self.run_host.log_level = workflows.log_level;
        self
    }

    /// The runs of the shared job `key`.
    pub(crate) fn run_control<'a>(&'a self, key: &'a SharedJobKey) -> SharedRunControl<'a> {
        SharedRunControl {
            key,
            host: &self.run_host,
            runs: &self.page_runs,
            clock: &self.clock,
        }
    }

    /// What validating a shared job reads, with the servers as `servers` found them for this request.
    pub(crate) fn validation_host<'a>(&'a self, servers: &'a CurrentServers) -> SharedValidationHost<'a> {
        SharedValidationHost {
            store_root: &self.roots.store_root,
            clients: &self.clients,
            primary_server: &servers.primary,
            known_servers: &servers.known,
        }
    }

    /// The servers as they are configured now, read once for a request.
    pub(crate) fn current_servers(&self) -> CurrentServers {
        CurrentServers {
            primary: self.clients.primary_name().unwrap_or_default(),
            known: self.clients.names(),
        }
    }
}

/// The primary server and every configured server's name, read together for one request, since `/settings/` can change
/// them while the process runs.
pub(crate) struct CurrentServers {
    primary: String,
    known: Vec<String>,
}

#[derive(Debug, Error)]
pub(crate) enum PageError {
    #[error(transparent)]
    Index(#[from] SharedIndexError),
    #[error(transparent)]
    State(#[from] SharedStateError),
}

pub(crate) fn workflow_href(package: &str, workflow: &str) -> String {
    format!("/shared/workflows/{package}/{workflow}")
}

pub(crate) fn job_href(key: &SharedJobKey) -> String {
    format!("{}/jobs/{}", workflow_href(&key.package, &key.workflow), key.job)
}

/// The key a URL names, or `None` when a component is not a URL-safe slug, which every job file's own key is, so it cannot
/// name a job and cannot put a decoded `?`, `#`, `%`, or `/` into a link.
pub(crate) fn key_of(package: &str, workflow: &str, job: &str) -> Option<SharedJobKey> {
    [package, workflow, job]
        .iter()
        .all(|name| is_url_safe_slug(name))
        .then(|| SharedJobKey {
            package: package.to_owned(),
            workflow: workflow.to_owned(),
            job: job.to_owned(),
        })
}

/// One shared workflow's entry, with the package and slug it is found by.
pub(crate) struct SharedEntry {
    pub(crate) package: String,
    pub(crate) slug: String,
    pub(crate) entry: WorkflowEntry,
}

/// One shared job's card, with what is stored for it and its latest run.
pub(crate) fn card(job: &SharedIndexedJob, state: &JobsState, runs_root: &std::path::Path) -> JobCard {
    let latest = recent_shared_runs(runs_root, &job.key, 1)
        .ok()
        .and_then(|runs| runs.into_iter().next());
    JobCard::from(CardParts {
        slug: &job.slug,
        name: job
            .loaded
            .as_ref()
            .map_or_else(|| job.slug.clone(), |loaded| loaded.name.clone()),
        href: job_href(&job.key),
        registers: job.loaded.is_some(),
        can_run: job.loaded.is_some()
            && run_allowed(
                state.lifecycle_of(&job.key.package, &job.key.workflow, &job.key.job),
                RunKind::Live,
            ),
        stored: state.job(&job.key.package, &job.key.workflow, &job.key.job),
        owner: None,
        latest: latest.as_ref(),
        favourite_toggle_html: String::new(),
    })
}

fn entry(workflow: &SharedWorkflow, state: &JobsState, runs_root: &std::path::Path) -> SharedEntry {
    let jobs = workflow.jobs.iter().map(|job| card(job, state, runs_root)).collect();
    let facts = vec![
        Fact {
            label: "Package",
            value: workflow.package.clone(),
        },
        Fact {
            label: "Release",
            value: workflow
                .release
                .as_ref()
                .map_or_else(|| "Not installed".to_owned(), ToString::to_string),
        },
        Fact {
            label: "Script",
            value: workflow.entry.clone().unwrap_or_else(|| "Not installed".to_owned()),
        },
    ];
    SharedEntry {
        package: workflow.package.clone(),
        slug: workflow.slug.clone(),
        entry: WorkflowEntry {
            name: format!("{} / {}", workflow.package, workflow.slug),
            href: workflow_href(&workflow.package, &workflow.slug),
            problem: workflow.release.is_none().then_some(Problem {
                badge: "Not installed",
                detail: "No installed package provides this workflow, so none of its jobs can run. Install the package again, or retire the jobs.".to_owned(),
            }),
            facts,
            jobs,
        },
    }
}

/// Every shared workflow with its jobs, newest state and latest run read from the host.
///
/// Blocking: reads the package store, the job files, the state file, and run records, so call it from a blocking thread.
pub(crate) fn entries(roots: &SharedRoots) -> Result<Vec<SharedEntry>, PageError> {
    let indexed = index_shared(&roots.store_root, &roots.jobs_root)?;
    let state = SharedStateStore::new(roots.state_root.clone()).read()?;
    Ok(indexed
        .iter()
        .filter(|workflow| {
            // A name that cannot be part of a URL has no page, and so no link to a dead one. It cannot have jobs either,
            // because a job file is found only under slug-safe names.
            let addressable = is_url_safe_slug(&workflow.package) && is_url_safe_slug(&workflow.slug);
            if !addressable {
                tracing::warn!(package = %workflow.package, workflow = %workflow.slug, "a shared workflow is not listed: its package or workflow name is not a URL-safe slug");
            }
            addressable
        })
        .map(|workflow| entry(workflow, &state, &roots.runs_root))
        .collect())
}

/// What a shared page's shell is: the sidebar of the active vault, with this page's own breadcrumb.
pub(crate) async fn shell_nav(
    state: &SharedRoutesState,
    client: &McpClient,
    breadcrumb: Vec<BreadcrumbSegment>,
    headers: &HeaderMap,
) -> page::NavData {
    let web_config_path = Arc::clone(&state.web_config_path);
    let server_names = state.clients.names();
    let active_vault = crate::active_vault::read_from_headers(headers);
    let (mut nav, appearance, mcp_status) = tokio::join!(
        Box::pin(shell::build_host_nav(
            client,
            HostScreen::Shared,
            active_vault.as_deref(),
            &server_names,
        )),
        async move {
            tokio::task::spawn_blocking(move || crate::config::current_appearance(&web_config_path))
                .await
                .unwrap_or_default()
        },
        shell::mcp_status(&state.clients),
    );
    nav.breadcrumb = breadcrumb;
    nav.appearance = appearance;
    nav.sidebar_wide = crate::routes::sidebar_wide_from_headers(headers);
    nav.mcp_status = mcp_status;
    nav
}

/// The configured vaults' names, for the tabs that lead back to them, or none with no session to ask.
pub(crate) async fn vault_names(state: &SharedRoutesState) -> Vec<String> {
    match crate::routes::primary_session(&state.clients) {
        Ok(client) => shell::vault_names(&client).await,
        Err(_) => Vec::new(),
    }
}

/// The title bar of a shared screen, which has no favourite: favourites belong to a vault.
pub(crate) fn screen_title(label: &str, icon: &'static str) -> PageTitle {
    PageTitle {
        icon,
        label: label.to_owned(),
        actions_html: String::new(),
        download_html: String::new(),
        favourite_toggle_html: String::new(),
    }
}

pub(crate) fn crumb(label: &str, href: Option<String>) -> BreadcrumbSegment {
    BreadcrumbSegment {
        label: label.to_owned(),
        href,
    }
}

/// A page for a full navigation, or the fragment alone for an HTMX request.
pub(crate) async fn respond(
    state: &SharedRoutesState,
    headers: &HeaderMap,
    breadcrumb: Vec<BreadcrumbSegment>,
    fragment: String,
) -> Response {
    respond_titled(state, headers, breadcrumb, fragment, None).await
}

/// [`respond`], with the title bar of a screen that has one.
pub(crate) async fn respond_titled(
    state: &SharedRoutesState,
    headers: &HeaderMap,
    breadcrumb: Vec<BreadcrumbSegment>,
    fragment: String,
    title: Option<PageTitle>,
) -> Response {
    if headers.contains_key("hx-request") {
        return html_response(fragment);
    }
    let client = match crate::routes::primary_session(&state.clients) {
        Ok(client) => client,
        Err(response) => return *response,
    };
    let mut nav = shell_nav(state, &client, breadcrumb, headers).await;
    nav.page_title = title;
    html_response(page::render_page(&nav, "Workflows", &fragment))
}

pub(crate) fn page_failure(error: &PageError) -> Response {
    tracing::warn!(%error, "a shared workflows page could not read the host");
    unreachable()
}

#[derive(Debug, Default, Deserialize)]
pub struct SharedQuery {
    #[serde(default)]
    state: Option<String>,
}

/// `all`, with only the jobs `filter` admits.
fn admitted(all: Vec<SharedEntry>, filter: StateFilter) -> Vec<SharedEntry> {
    all.into_iter()
        .map(|mut shared| {
            shared.entry.jobs.retain(|job| filter.admits(job.lifecycle));
            shared
        })
        // A filter hides a workflow with no job in it, which would only be an empty heading.
        .filter(|shared| filter == StateFilter::All || !shared.entry.jobs.is_empty())
        .collect()
}

/// `GET /shared/workflows`: lists every shared workflow with its jobs, or only the jobs in the state `?state=` names.
pub async fn list(
    State(state): State<SharedRoutesState>,
    Query(query): Query<SharedQuery>,
    headers: HeaderMap,
) -> Response {
    let Ok(filter) = StateFilter::try_from(query.state.as_deref()) else {
        return error_response(StatusCode::BAD_REQUEST, "workflows/unknown-state-filter");
    };
    let roots = Arc::clone(&state.roots);
    let gathered = tokio::task::spawn_blocking(move || entries(&roots)).await;
    let all = match gathered {
        Ok(Ok(all)) => all,
        Ok(Err(error)) => return page_failure(&error),
        Err(error) => {
            tracing::warn!(%error, "the shared workflows could not be gathered");
            return unreachable();
        }
    };
    let counts: StateCounts = all
        .iter()
        .flat_map(|shared| shared.entry.jobs.iter().map(|job| job.lifecycle))
        .collect();
    let workflows: Vec<WorkflowEntry> = admitted(all, filter).into_iter().map(|shared| shared.entry).collect();
    let fragment = render_list(&WorkflowsList {
        scope: Scope::SharedWorkflows.tabs(&vault_names(&state).await),
        filter_value: filter.value(),
        tiles: state_tiles(crate::urls::SHARED_WORKFLOWS, filter, &counts),
        workflows,
        notices: Vec::new(),
    });
    respond_titled(
        &state,
        &headers,
        vec![crumb("Shared workflows", None)],
        fragment,
        Some(screen_title("Shared workflows", WORKFLOWS.icon)),
    )
    .await
}

/// `GET /shared/workflows/{{package}}/{{workflow}}`: one shared workflow, the release it comes from, and its jobs.
pub async fn workflow(
    State(state): State<SharedRoutesState>,
    Path((package, workflow)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if !is_url_safe_slug(&package) || !is_url_safe_slug(&workflow) {
        return not_found();
    }
    let roots = Arc::clone(&state.roots);
    let gathered = tokio::task::spawn_blocking(move || entries(&roots)).await;
    let all = match gathered {
        Ok(Ok(all)) => all,
        Ok(Err(error)) => return page_failure(&error),
        Err(error) => {
            tracing::warn!(%error, "the shared workflows could not be gathered");
            return unreachable();
        }
    };
    let Some(found) = all
        .into_iter()
        .find(|shared| shared.package == package && shared.slug == workflow)
    else {
        return not_found();
    };
    let releases = SharedReleases {
        store_root: state.roots.store_root.clone(),
        jobs_root: state.roots.jobs_root.clone(),
        state: SharedStateStore::new(state.roots.state_root.clone()),
    };
    let releases = match releases.report_with_jobs(&package, &workflow).await {
        Ok((report, jobs)) => {
            let href = |job: &str| workflow_href(&package, &workflow) + "/jobs/" + job;
            ReleasesSection::from(ReleasesInput {
                report: &report,
                jobs: &jobs,
                job_href: &href,
            })
        }
        Err(error) => {
            tracing::warn!(
                code = error.code(),
                "a shared workflow's releases could not be read for its page"
            );
            ReleasesSection::unreadable(&format!("The releases could not be read ({}).", error.code()))
        }
    };
    let fragment = render_detail(&WorkflowDetail {
        back: Link {
            href: crate::urls::SHARED_WORKFLOWS.to_owned(),
            label: "All shared workflows",
        },
        workflow: found.entry,
        releases,
    });
    respond(
        &state,
        &headers,
        vec![
            crumb("Shared workflows", Some(crate::urls::SHARED_WORKFLOWS.to_owned())),
            crumb(&package, None),
            crumb(&workflow, None),
        ],
        fragment,
    )
    .await
}

#[cfg(test)]
#[path = "shared_workflows_test.rs"]
mod tests;
