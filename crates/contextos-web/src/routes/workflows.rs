//! A vault's workflows and the jobs under them: `GET /{{vault_name}}/workflows` lists every workflow with
//! its jobs, filterable by state, and `GET /{{vault_name}}/workflows/{{workflow}}` shows one. A job, and what an operator
//! does to it, is in [`crate::routes::workflow_jobs`].
//!
//! The hierarchy is workflows, then jobs, then runs, and the URLs follow it. Every job file is listed, whether or not the
//! job registers, so a draft that fails validation can be seen and validated. Discovery runs fresh on every request, unlike
//! [`crate::routes::apps`]'s own per-vault cache: the scheduler ([`crate::scheduler`]) already re-discovers every poll
//! regardless, so a second, page-level cache would only risk disagreeing with what the scheduler itself is about to act on.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use contextos_workflows::{Clock, JobLifecycle, JobState, JobsState};
use serde::Deserialize;

use crate::job_index::{IndexedJob, IndexedWorkflow, index_workflows};
use crate::job_runs::{RunSummary, run_history};
use crate::mcp_client::{McpCallError, McpClient, McpClientSet};
use crate::page_runs::PageRuns;
use crate::release_views::{ReleasesInput, ReleasesSection};
use crate::rendering::page;
use crate::rendering::shell::{self, ActiveScreen};
use crate::routes::vault::{error_response, html_response, not_found, server_not_configured, unreachable};
use crate::routes::workflow_views::{
    CardParts, Fact, JobCard, Link, Problem, Scope, StateCounts, StateFilter, WorkflowDetail, WorkflowEntry,
    WorkflowsList, render_detail, render_list, state_tiles,
};
use crate::run_settings::HostContext;
use crate::scheduler::{self, RunKind, run_allowed};
use crate::view_favourites;
use crate::workflow_releases_stores::VaultReleases;

/// Shared state a workflow or job route needs: the MCP sessions (whose primary server every vault operation is issued
/// against, and whose configured names a manifest's own `mcp_servers` allow-list is validated against, both read per
/// request so a server added or removed in `/settings/` counts at once), the clock a change is stamped with, and
/// `web.toml`'s own path (read fresh per request for the nav shell's appearance).
#[derive(Clone)]
pub struct WorkflowRoutesState {
    pub(crate) clients: Arc<McpClientSet>,
    pub(crate) host: HostContext,
    pub(crate) clock: Arc<dyn Clock>,
    /// What this process has started from pages, which a page shows while it runs and after it ends.
    pub(crate) page_runs: Arc<PageRuns>,
    web_config_path: Arc<PathBuf>,
}

impl WorkflowRoutesState {
    #[must_use]
    pub fn new(clients: Arc<McpClientSet>, host: HostContext, web_config_path: Arc<PathBuf>) -> Self {
        let clock: Arc<dyn Clock> = host.clock.clone();
        Self {
            clients,
            host,
            clock,
            page_runs: Arc::new(PageRuns::default()),
            web_config_path,
        }
    }

    pub(crate) fn client(&self) -> Result<Arc<McpClient>, RegistryError> {
        self.clients.primary().ok_or(RegistryError::ServerNotConfigured)
    }
}

#[derive(Clone, Copy)]
pub(crate) enum RegistryError {
    ServerNotConfigured,
    Unreachable,
}

impl From<McpCallError> for RegistryError {
    fn from(_source: McpCallError) -> Self {
        Self::Unreachable
    }
}

pub(crate) fn registry_error_response(error: RegistryError) -> Response {
    match error {
        RegistryError::ServerNotConfigured => server_not_configured(),
        RegistryError::Unreachable => unreachable(),
    }
}

pub(crate) async fn ensure_vault(client: &McpClient, vault_name: &str) -> Result<(), Box<Response>> {
    match crate::apps::vault_exists(client, vault_name).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(Box::new(not_found())),
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => Err(Box::new(unreachable())),
    }
}

/// The vault's job state. A state file that cannot be read or does not parse is a failure, because treating it as empty would
/// show every job as a draft with the draft's actions, which is not what is stored.
pub(crate) async fn read_job_state(client: &McpClient, vault_name: &str) -> Result<JobsState, Box<Response>> {
    match scheduler::read_state(client, vault_name).await {
        Ok(snapshot) => Ok(snapshot.value),
        Err(error) => {
            tracing::warn!(vault = vault_name, %error, "the job state could not be read, so no job page was drawn");
            Err(Box::new(unreachable()))
        }
    }
}

/// What a vault job's card is built from.
pub(crate) struct CardInput<'a> {
    pub(crate) vault_name: &'a str,
    pub(crate) package: &'a str,
    pub(crate) workflow_slug: &'a str,
    pub(crate) job: &'a IndexedJob,
    pub(crate) stored: Option<&'a JobState>,
    pub(crate) latest: Option<&'a RunSummary>,
}

impl From<CardInput<'_>> for JobCard {
    fn from(input: CardInput<'_>) -> Self {
        let CardInput {
            vault_name,
            package,
            workflow_slug,
            job,
            stored,
            latest,
        } = input;
        let href = crate::urls::VaultUrls::from(vault_name).job(package, workflow_slug, &job.slug);
        let name = job
            .loaded
            .as_ref()
            .map_or_else(|| job.slug.clone(), |loaded| loaded.name.clone());
        Self::from(CardParts {
            slug: &job.slug,
            favourite_toggle_html: crate::rendering::favourite_toggle::render(
                &href,
                &name,
                stored.is_some_and(|state| state.favourite),
            ),
            name,
            href,
            registers: job.loaded.is_some(),
            can_run: job.loaded.is_some()
                && run_allowed(
                    stored.map_or_else(JobLifecycle::default, JobState::lifecycle),
                    RunKind::Live,
                ),
            stored,
            owner: job.loaded.as_ref().and_then(|loaded| loaded.owner.as_ref()),
            latest,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct WorkflowsQuery {
    #[serde(default)]
    entity: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

impl WorkflowsQuery {
    /// The Command Centre entity switcher's own selection.
    pub(crate) fn entity(&self) -> Option<&str> {
        self.entity.as_deref()
    }
}

/// One job's card, with its stored state and its latest run; a history that cannot be read shows as unknown.
async fn job_card(
    client: &McpClient,
    vault_name: &str,
    package: &str,
    workflow_slug: &str,
    job: &IndexedJob,
    stored: Option<&JobState>,
) -> JobCard {
    let result = run_history(client, vault_name, package, workflow_slug, &job.slug, 1).await;
    let mut card = JobCard::from(CardInput {
        vault_name,
        package,
        workflow_slug,
        job,
        stored,
        latest: result.as_ref().ok().and_then(|runs| runs.first()),
    });
    if result.is_err() {
        card.state = "unknown";
        "History unavailable".clone_into(&mut card.last_run);
    }
    card
}

/// Builds the entry of one indexed workflow: its cards, each with the stored state and the latest run, and only those the
/// filter admits.
async fn workflow_entry(
    client: &McpClient,
    vault_name: &str,
    workflow: &IndexedWorkflow,
    job_state: &JobsState,
    filter: StateFilter,
) -> WorkflowEntry {
    let mut jobs = Vec::new();
    for job in &workflow.jobs {
        let stored = job_state.job(&workflow.package, &workflow.slug, &job.slug);
        let lifecycle = stored.map_or_else(JobLifecycle::default, JobState::lifecycle);
        if !filter.admits(lifecycle) {
            continue;
        }
        jobs.push(job_card(client, vault_name, &workflow.package, &workflow.slug, job, stored).await);
    }
    let loaded = workflow.loaded.as_ref();
    let facts = loaded.map_or_else(Vec::new, |loaded| {
        vec![
            Fact {
                label: "Script",
                value: loaded.entry.clone(),
            },
            Fact {
                label: "Timeout",
                value: format!("{}s", loaded.timeout_secs),
            },
            Fact {
                label: "May call",
                value: loaded.calls.summary(),
            },
        ]
    });
    WorkflowEntry {
        name: loaded.map_or_else(|| workflow.slug.clone(), |loaded| loaded.name.clone()),
        href: crate::urls::VaultUrls::from(vault_name).workflow(&workflow.package, &workflow.slug),
        problem: loaded.is_none().then(|| Problem {
            badge: "Does not register",
            detail: workflow.refusal.as_ref().map_or_else(
                || {
                    "This workflow does not register, so none of its jobs can run. Validate one of its jobs to see why."
                        .to_owned()
                },
                |refusal| {
                    format!(
                        "This workflow does not register, so none of its jobs can run: {}",
                        refusal.message
                    )
                },
            ),
        }),
        facts,
        jobs,
    }
}

/// Every job of the vault's workflows that runs on a schedule, as a card with its latest run, for the Overview's Inputs
/// panel. `None` when the workflows or their stored state could not be read.
pub(crate) async fn scheduled_job_cards(
    client: &McpClient,
    vault_name: &str,
    server_names: &[String],
) -> Option<Vec<JobCard>> {
    let indexed = index_workflows(client, vault_name, server_names).await.ok()?;
    let job_state = read_job_state(client, vault_name).await.ok()?;
    let mut cards = Vec::new();
    for workflow in &indexed.workflows {
        for job in &workflow.jobs {
            let stored = job_state.job(&workflow.package, &workflow.slug, &job.slug);
            if stored.and_then(JobState::schedule).is_none() {
                continue;
            }
            cards.push(job_card(client, vault_name, &workflow.package, &workflow.slug, job, stored).await);
        }
    }
    Some(cards)
}

/// `GET /{{vault_name}}/workflows`: lists every workflow with its jobs, or only the jobs in the state `?state=` names.
pub async fn list(
    State(state): State<WorkflowRoutesState>,
    Path(vault_name): Path<String>,
    Query(query): Query<WorkflowsQuery>,
    headers: HeaderMap,
) -> Response {
    let Ok(filter) = StateFilter::try_from(query.state.as_deref()) else {
        return error_response(StatusCode::BAD_REQUEST, "workflows/unknown-state-filter");
    };
    let client = match state.client() {
        Ok(client) => client,
        Err(error) => return registry_error_response(error),
    };
    if let Err(response) = ensure_vault(&client, &vault_name).await {
        return *response;
    }
    let indexed = match index_workflows(&client, &vault_name, &state.clients.names()).await {
        Ok(indexed) => indexed,
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => return unreachable(),
    };
    let job_state = match read_job_state(&client, &vault_name).await {
        Ok(job_state) => job_state,
        Err(response) => return *response,
    };
    let mut workflows = Vec::with_capacity(indexed.workflows.len() + indexed.refused_packages.len());
    for workflow in &indexed.workflows {
        let entry = workflow_entry(&client, &vault_name, workflow, &job_state, filter).await;
        // A filter hides a workflow with no job in it, which would only be an empty heading.
        if filter == StateFilter::All || !entry.jobs.is_empty() {
            workflows.push(entry);
        }
    }
    let list_path = crate::urls::VaultUrls::from(&*vault_name).workflows();
    // A folder that is not a package has no jobs to filter, so only the unfiltered list shows it.
    if filter == StateFilter::All {
        workflows.extend(indexed.refused_packages.iter().map(|refused| WorkflowEntry {
            name: refused.package.clone(),
            href: list_path.clone(),
            problem: Some(Problem {
                badge: "Not a package",
                detail: format!(
                    "The folder {} is not a workflow package: {}",
                    refused.package, refused.refusal.message
                ),
            }),
            facts: Vec::new(),
            jobs: Vec::new(),
        }));
    }
    let counts: StateCounts = indexed
        .workflows
        .iter()
        .flat_map(|workflow| {
            workflow.jobs.iter().map(|job| {
                job_state
                    .job(&workflow.package, &workflow.slug, &job.slug)
                    .map_or_else(JobLifecycle::default, JobState::lifecycle)
            })
        })
        .collect();
    let fragment = render_list(&WorkflowsList {
        scope: Scope::VaultWorkflows(&vault_name).tabs(&[]),
        filter_value: filter.value(),
        tiles: state_tiles(&list_path, filter, &counts),
        workflows,
        notices: crate::team_notice::team_notice(&client, &vault_name)
            .await
            .into_iter()
            .collect(),
    });
    if headers.contains_key("hx-request") {
        html_response(fragment)
    } else {
        // The breadcrumb suffix here is a label shared with the job detail page, so the title bar is chosen explicitly.
        let entity_base_path = format!("{list_path}/");
        let (mut nav, page_title) = tokio::join!(
            nav_with_appearance(
                &state,
                &client,
                &vault_name,
                "Workflows",
                query.entity.as_deref(),
                &entity_base_path,
                crate::routes::sidebar_wide_from_headers(&headers),
            ),
            shell::page_title(&client, &vault_name, &view_favourites::WORKFLOWS),
        );
        nav.page_title = Some(page_title);
        html_response(page::render_page(&nav, "Workflows", &fragment))
    }
}

/// `GET /{{vault_name}}/workflows/{{package}}/{{workflow}}`: one workflow, its entry script, timeout, and servers, and its jobs.
pub async fn detail(
    State(state): State<WorkflowRoutesState>,
    Path((vault_name, package, workflow_slug)): Path<(String, String, String)>,
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
    let indexed = match index_workflows(&client, &vault_name, &state.clients.names()).await {
        Ok(indexed) => indexed,
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => return unreachable(),
    };
    let Some(workflow) = indexed
        .workflows
        .iter()
        .find(|workflow| workflow.package == package && workflow.slug == workflow_slug)
    else {
        return not_found();
    };
    let job_state = match read_job_state(&client, &vault_name).await {
        Ok(job_state) => job_state,
        Err(response) => return *response,
    };
    let entry = workflow_entry(&client, &vault_name, workflow, &job_state, StateFilter::All).await;
    let releases = releases_section(&client, &vault_name, &state.clients.names(), (&package, &workflow_slug)).await;
    let fragment = render_detail(&WorkflowDetail {
        back: Link {
            href: crate::urls::VaultUrls::from(&*vault_name).workflows(),
            label: "All workflows",
        },
        workflow: entry,
        releases,
    });
    if headers.contains_key("hx-request") {
        html_response(fragment)
    } else {
        let nav = nav_with_appearance(
            &state,
            &client,
            &vault_name,
            "Workflows",
            query.entity.as_deref(),
            &crate::urls::VaultUrls::from(&*vault_name).workflow(&package, &workflow_slug),
            crate::routes::sidebar_wide_from_headers(&headers),
        )
        .await;
        html_response(page::render_page(&nav, "Workflows", &fragment))
    }
}

/// A workflow's releases with the jobs on each, or the sentence that says they could not be read.
async fn releases_section(
    client: &McpClient,
    vault_name: &str,
    known_servers: &[String],
    (package, workflow): (&str, &str),
) -> ReleasesSection {
    let reader = VaultReleases {
        client,
        vault: vault_name,
        known_servers,
    };
    match reader.report_with_jobs(package, workflow).await {
        Ok((report, jobs)) => {
            let urls = crate::urls::VaultUrls::from(vault_name);
            let href = |job: &str| urls.job(package, workflow, job);
            ReleasesSection::from(ReleasesInput {
                report: &report,
                jobs: &jobs,
                job_href: &href,
            })
        }
        Err(error) => {
            tracing::warn!(
                vault = vault_name,
                code = error.code(),
                "a workflow's releases could not be read for its page"
            );
            ReleasesSection::unreadable(&format!("The releases could not be read ({}).", error.code()))
        }
    }
}

pub(crate) async fn nav_with_appearance(
    state: &WorkflowRoutesState,
    client: &McpClient,
    vault_name: &str,
    breadcrumb_suffix: &str,
    entity: Option<&str>,
    entity_base_path: &str,
    sidebar_wide: bool,
) -> page::NavData {
    let web_config_path = Arc::clone(&state.web_config_path);
    let server_names = state.clients.names();
    let (mut nav, appearance, mcp_status) = tokio::join!(
        Box::pin(shell::build_nav(
            client,
            ActiveScreen::Jobs,
            Some(vault_name),
            Some(breadcrumb_suffix),
            entity,
            Some(entity_base_path),
            &server_names,
        )),
        async move {
            tokio::task::spawn_blocking(move || crate::config::current_appearance(&web_config_path))
                .await
                .unwrap_or_default()
        },
        shell::mcp_status(&state.clients),
    );
    nav.appearance = appearance;
    nav.sidebar_wide = sidebar_wide;
    nav.mcp_status = mcp_status;
    nav
}

#[cfg(test)]
#[path = "workflows_test.rs"]
mod tests;
