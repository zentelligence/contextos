//! One job of a vault's workflow, and what an operator does to it: `GET /{{vault_name}}/workflows/{{package}}/{{workflow}}/jobs/{{job}}`
//! (its status, actions, and run log), `PATCH` (a favourite, or a schedule), `DELETE` (a retired job's state) with
//! `GET .../confirm-delete`, `PUT .../execute/{{command}}`, and `POST .../runs` (a run by hand).
//!
//! Every action is a thin caller of [`crate::job_pages`], which holds what each answers with over a job's
//! [`JobStore`](crate::job_operation::JobStore), so a vault job and a shared job behave alike and a page cannot allow what a
//! command refuses.

use std::sync::Arc;

use askama::Template;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use contextos_workflows::{JobLifecycle, JobState};

use crate::job_index::{IndexedJob, WorkflowIndex, index_workflows};
use crate::job_operation::JobStore;
use crate::job_operation_vault::VaultJobStore;
use crate::job_pages::{self, JobUpdate};
use crate::job_release_view::{JobReleaseInput, JobReleaseView};
use crate::job_runs_view::{JobRunsInput, JobRunsView};
use crate::job_status::{JobStatusInput, JobStatusView};
use crate::jobs::{RegisteredJob, is_url_safe_slug};
use crate::mcp_client::McpCallError;
use crate::page_job::PageJob;
use crate::rendering::page;
use crate::routes::vault::{html_response, not_found, unreachable};
use crate::routes::workflow_views::{Fact, JobCard, JobDetail, render_job};
use crate::routes::workflows::{
    CardInput, WorkflowRoutesState, ensure_vault, nav_with_appearance, read_job_state, registry_error_response,
};
use crate::run_control::{RunActivity, RunControl};
use crate::run_control_vault::VaultRunControl;
use crate::scheduler;
use crate::workflow_releases_stores::VaultReleases;

/// Whether a URL's workflow and job could be the slugs a job is tracked under. Anything else names no job, and keeping it
/// out of the page's own links keeps a decoded `?`, `#`, `%`, or `/` from reaching them.
fn names_a_job(package: &str, workflow_slug: &str, slug: &str) -> bool {
    is_url_safe_slug(package) && is_url_safe_slug(workflow_slug) && is_url_safe_slug(slug)
}

/// The facts a vault job's page lists beside its run state, which it has only when it registers.
fn facts_of(job: Option<&RegisteredJob>) -> Vec<Fact> {
    job.map_or_else(Vec::new, |job| {
        vec![
            Fact {
                label: "Script",
                value: job.entry.clone(),
            },
            Fact {
                label: "Timeout",
                value: format!("{}s", job.timeout_secs),
            },
            Fact {
                label: "May call",
                value: job.calls.summary(),
            },
        ]
    })
}

fn job_path(vault_name: &str, package: &str, workflow_slug: &str, slug: &str) -> String {
    crate::urls::VaultUrls::from(vault_name).job(package, workflow_slug, slug)
}

/// The runs of one job.
fn control<'a>(
    state: &'a WorkflowRoutesState,
    vault_name: &'a str,
    package: &'a str,
    workflow_slug: &'a str,
    slug: &'a str,
) -> VaultRunControl<'a> {
    VaultRunControl {
        clients: &state.clients,
        host: &state.host,
        vault_name,
        package,
        workflow_slug,
        job_slug: slug,
        runs: &state.page_runs,
        clock: &state.clock,
    }
}

/// What every action of a job needs before it is carried out: the URL names a job, the vault exists, and the server its
/// state is reached through is there. Anything else is the response to send.
async fn reach(
    state: &WorkflowRoutesState,
    vault_name: &str,
    package: &str,
    workflow_slug: &str,
    slug: &str,
) -> Result<Arc<crate::mcp_client::McpClient>, Box<Response>> {
    if !names_a_job(package, workflow_slug, slug) {
        return Err(Box::new(not_found()));
    }
    let client = state
        .client()
        .map_err(|error| Box::new(registry_error_response(error)))?;
    ensure_vault(&client, vault_name).await?;
    Ok(client)
}

fn store<'a>(
    client: &'a crate::mcp_client::McpClient,
    known_mcp_servers: &'a [String],
    claims_root: Option<&'a std::path::Path>,
    vault_name: &'a str,
    package: &'a str,
    workflow_slug: &'a str,
    slug: &'a str,
) -> VaultJobStore<'a> {
    VaultJobStore {
        client,
        vault_name,
        known_mcp_servers,
        package,
        workflow_slug,
        job_slug: slug,
        claims_root,
    }
}

/// The address of the record of the run a job's suspension names, when it names one and the record is found.
async fn suspended_run_link(control: &VaultRunControl<'_>, stored: Option<&JobState>) -> Option<String> {
    let number = stored.and_then(JobState::suspension).and_then(|record| record.run)?;
    control.record_link(number).await
}

/// The indexed job `slug` of the workflow `workflow_slug`, when the index holds one.
fn job_of(indexed: WorkflowIndex, package: &str, workflow_slug: &str, slug: &str) -> Option<IndexedJob> {
    indexed
        .workflows
        .into_iter()
        .find(|workflow| workflow.package == package && workflow.slug == workflow_slug)
        .and_then(|workflow| workflow.jobs.into_iter().find(|job| job.slug == slug))
}

/// The job's status region as the page first draws it, with no outcome in it.
async fn status_region(job_runs: &VaultRunControl<'_>, base: &str, stored: Option<&JobState>) -> String {
    let zone = job_runs.schedule_zone();
    JobStatusView::from(JobStatusInput {
        base_path: base,
        stored,
        message: None,
        zone: &zone,
        run_link: suspended_run_link(job_runs, stored).await,
    })
    .render()
    .unwrap_or_else(|_| "<p>The job's status could not be rendered.</p>".to_owned())
}

/// The job's release section: the release it names, its lock, the jobs either side of it, and what is wrong with its file,
/// or the sentence that says the releases could not be read.
async fn release_section(
    client: &crate::mcp_client::McpClient,
    vault_name: &str,
    known_servers: &[String],
    (package, workflow, slug): (&str, &str, &str),
    job_state: &contextos_workflows::JobsState,
) -> String {
    let reader = VaultReleases {
        client,
        vault: vault_name,
        known_servers,
    };
    match reader.report_with_jobs(package, workflow).await {
        Ok((report, listings)) => {
            let urls = crate::urls::VaultUrls::from(vault_name);
            let href = |job: &str| urls.job(package, workflow, job);
            JobReleaseView::from(JobReleaseInput {
                package,
                workflow,
                job: slug,
                listings: &listings,
                report: &report,
                state: job_state,
                job_href: &href,
            })
            .render()
            .unwrap_or_else(|_| "<p>The job's release could not be rendered.</p>".to_owned())
        }
        Err(error) => {
            tracing::warn!(
                vault = vault_name,
                code = error.code(),
                "a job's release could not be read for its page"
            );
            format!(
                "<p role=\"alert\">The job's release could not be read ({}).</p>",
                error.code()
            )
        }
    }
}

/// `GET /{{vault_name}}/workflows/{{package}}/{{workflow}}/jobs/{{job}}`: the job's status and actions, its script, timeout and
/// servers when it registers, and its most recent runs.
pub async fn detail(
    State(state): State<WorkflowRoutesState>,
    Path((vault_name, package, workflow_slug, slug)): Path<(String, String, String, String)>,
    Query(query): Query<crate::routes::workflows::WorkflowsQuery>,
    headers: HeaderMap,
) -> Response {
    if !names_a_job(&package, &workflow_slug, &slug) {
        return not_found();
    }
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
    let found = job_of(indexed, &package, &workflow_slug, &slug);
    let job_state = match read_job_state(&client, &vault_name).await {
        Ok(job_state) => job_state,
        Err(response) => return *response,
    };
    let stored = job_state.job(&package, &workflow_slug, &slug);
    // A job has a page when its file is there or something is stored for it, so a retired job whose files were deleted can
    // still be re-activated or deleted from here.
    let Some(indexed) = found.or_else(|| {
        stored.map(|_| IndexedJob {
            slug: slug.clone(),
            file: String::new(),
            loaded: None,
        })
    }) else {
        return not_found();
    };
    let job_runs = control(&state, &vault_name, &package, &workflow_slug, &slug);
    let history = job_runs.history().await;
    let history_unavailable = history.is_err();
    let history = history.unwrap_or_default();
    let mut card = JobCard::from(CardInput {
        vault_name: &vault_name,
        package: &package,
        workflow_slug: &workflow_slug,
        job: &indexed,
        stored,
        // The latest run of this job itself, not of the chain it is in, though the chain has newer runs.
        latest: history.own_latest.as_ref(),
    });
    if history_unavailable {
        card.state = "unknown";
        "History unavailable".clone_into(&mut card.last_run);
    }
    let base = job_path(&vault_name, &package, &workflow_slug, &slug);
    let activity = job_runs.activity().await.unwrap_or_else(|refusal| {
        tracing::warn!(%refusal, "a job page could not see what the job's runs are doing");
        RunActivity::default()
    });
    let lifecycle = stored.map_or_else(JobLifecycle::default, JobState::lifecycle);
    let status_html = status_region(&job_runs, &base, stored).await;
    let runs_html = JobRunsView::from(JobRunsInput {
        base_path: &base,
        lifecycle,
        activity,
        history: (!history_unavailable).then_some(history),
        message: None,
        beside_another: false,
    })
    .render()
    .unwrap_or_else(|_| "<p>The job's runs could not be rendered.</p>".to_owned());
    let loaded = indexed.loaded.as_ref();
    let release_html = release_section(
        &client,
        &vault_name,
        &state.clients.names(),
        (&package, &workflow_slug, &slug),
        &job_state,
    )
    .await;
    let fragment = render_job(&JobDetail {
        workflow_href: crate::urls::VaultUrls::from(&*vault_name).workflow(&package, &workflow_slug),
        registers: loaded.is_some(),
        facts: facts_of(loaded),
        job: card,
        status_html,
        release_html,
        runs_html,
        notices: crate::team_notice::job_notices(&client, &vault_name, loaded).await,
    });
    if headers.contains_key("hx-request") {
        html_response(fragment)
    } else {
        let nav = nav_with_appearance(
            &state,
            &client,
            &vault_name,
            "Workflows",
            query.entity(),
            &base,
            crate::routes::sidebar_wide_from_headers(&headers),
        )
        .await;
        html_response(page::render_page(&nav, "Workflows", &fragment))
    }
}

/// `POST /{{vault_name}}/workflows/{{package}}/{{workflow}}/jobs/{{job}}/runs`: starts a run outside the job's own schedule, live or, when
/// the body asks for it, a dry run. A live run is refused while another run of the job is active, and runs of different
/// jobs, and dry runs, never wait for each other.
pub async fn start_run(
    State(state): State<WorkflowRoutesState>,
    Path((vault_name, package, workflow_slug, slug)): Path<(String, String, String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let client = match reach(&state, &vault_name, &package, &workflow_slug, &slug).await {
        Ok(client) => client,
        Err(response) => return *response,
    };
    let known_servers = state.clients.names();
    let job_store = store(
        &client,
        &known_servers,
        state.host.claims_root.as_deref(),
        &vault_name,
        &package,
        &workflow_slug,
        &slug,
    );
    let runs = control(&state, &vault_name, &package, &workflow_slug, &slug);
    let job = PageJob {
        store: &job_store,
        control: &runs,
    };
    let base = job_path(&vault_name, &package, &workflow_slug, &slug);
    let mut outcome = job_pages::start_run(&job, &base, &body).await;
    if headers.contains_key("hx-request") {
        return outcome.response;
    }
    // A form that is not HTMX, such as a card's Run now, is sent to the job's page, which shows the run and follows it. A
    // request that was refused says so with a conflict, as only a request nobody offered can be.
    if outcome.started {
        Redirect::to(&base).into_response()
    } else {
        if outcome.response.status() == StatusCode::OK {
            *outcome.response.status_mut() = StatusCode::CONFLICT;
        }
        outcome.response
    }
}

/// `GET /{{vault_name}}/workflows/{{package}}/{{workflow}}/jobs/{{job}}/runs`: the job's runs region as it is now, which the region asks
/// for again while a run is active. It is a fragment, so a browser that navigates to it is sent to the job's page.
pub async fn list_runs(
    State(state): State<WorkflowRoutesState>,
    Path((vault_name, package, workflow_slug, slug)): Path<(String, String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let base = job_path(&vault_name, &package, &workflow_slug, &slug);
    if !headers.contains_key("hx-request") {
        return Redirect::to(&base).into_response();
    }
    let client = match reach(&state, &vault_name, &package, &workflow_slug, &slug).await {
        Ok(client) => client,
        Err(response) => return *response,
    };
    let known_servers = state.clients.names();
    let job_store = store(
        &client,
        &known_servers,
        state.host.claims_root.as_deref(),
        &vault_name,
        &package,
        &workflow_slug,
        &slug,
    );
    let runs = control(&state, &vault_name, &package, &workflow_slug, &slug);
    let job = PageJob {
        store: &job_store,
        control: &runs,
    };
    job_pages::list_runs(&job, &base).await
}

/// `PUT /{{vault_name}}/workflows/{{package}}/{{workflow}}/jobs/{{job}}/runs/{{run}}/execute/cancel`: asks the active run to stop, whichever
/// process started it. It stops at its next step.
pub async fn cancel_run(
    State(state): State<WorkflowRoutesState>,
    Path((vault_name, package, workflow_slug, slug, run)): Path<(String, String, String, String, String)>,
) -> Response {
    let client = match reach(&state, &vault_name, &package, &workflow_slug, &slug).await {
        Ok(client) => client,
        Err(response) => return *response,
    };
    let known_servers = state.clients.names();
    let job_store = store(
        &client,
        &known_servers,
        state.host.claims_root.as_deref(),
        &vault_name,
        &package,
        &workflow_slug,
        &slug,
    );
    let runs = control(&state, &vault_name, &package, &workflow_slug, &slug);
    let job = PageJob {
        store: &job_store,
        control: &runs,
    };
    job_pages::cancel_run(&job, &job_path(&vault_name, &package, &workflow_slug, &slug), &run).await
}

/// `PATCH /{{vault_name}}/workflows/{{package}}/{{workflow}}/jobs/{{job}}`: changes exactly one thing about the job. A `favourite` sets
/// its star in `.contextos/jobs.toml` to the body's own explicit value, mirroring `routes::apps::set_favourite`'s
/// PATCH-with-explicit-value shape and self-targeted HTMX response (see
/// [`crate::rendering::favourite_toggle::FavouriteUpdate`] for why the client sends the value rather than the server
/// flipping it). A `schedule` sets the job's schedule, or clears it when empty, through the shared operation.
pub async fn update(
    State(state): State<WorkflowRoutesState>,
    Path((vault_name, package, workflow_slug, slug)): Path<(String, String, String, String)>,
    body: Bytes,
) -> Response {
    let client = match reach(&state, &vault_name, &package, &workflow_slug, &slug).await {
        Ok(client) => client,
        Err(response) => return *response,
    };
    let base = job_path(&vault_name, &package, &workflow_slug, &slug);
    let known_servers = state.clients.names();
    let job_store = store(
        &client,
        &known_servers,
        state.host.claims_root.as_deref(),
        &vault_name,
        &package,
        &workflow_slug,
        &slug,
    );
    let runs = control(&state, &vault_name, &package, &workflow_slug, &slug);
    let job = PageJob {
        store: &job_store,
        control: &runs,
    };
    match JobUpdate::try_from(&body[..]) {
        Ok(JobUpdate::Schedule(expression)) => {
            job_pages::set_schedule(&job, &base, &expression, state.host.clock.instant().await).await
        }
        Ok(JobUpdate::Favourite(favourite)) => {
            set_favourite(
                &state,
                &vault_name,
                (&package, &workflow_slug, &slug),
                favourite,
                &job_store,
            )
            .await
        }
        Err(error) => job_pages::invalid_request(&job, &base, error.to_string()).await,
    }
}

async fn set_favourite(
    state: &WorkflowRoutesState,
    vault_name: &str,
    (package, workflow_slug, slug): (&str, &str, &str),
    favourite: bool,
    job_store: &VaultJobStore<'_>,
) -> Response {
    let Ok(client) = state.client() else {
        return unreachable();
    };
    match job_store.exists().await {
        Ok(true) => {}
        Ok(false) => return not_found(),
        Err(_) => return unreachable(),
    }
    let name = match index_workflows(&client, vault_name, &state.clients.names()).await {
        Ok(indexed) => indexed
            .workflows
            .into_iter()
            .find(|workflow| workflow.package == package && workflow.slug == workflow_slug)
            .and_then(|workflow| workflow.jobs.into_iter().find(|job| job.slug == slug))
            .and_then(|job| job.loaded)
            .map_or_else(|| slug.to_owned(), |job| job.name),
        Err(McpCallError::Unreachable { .. } | McpCallError::Refused { .. }) => return unreachable(),
    };
    if let Err(error) = scheduler::set_job_favourite(&client, vault_name, package, workflow_slug, slug, favourite).await
    {
        tracing::warn!(vault = %vault_name, package = %package, workflow = %workflow_slug, job = %slug, %error, "failed to persist a job favourite update");
        return unreachable();
    }
    crate::rendering::favourite_toggle::response(&job_path(vault_name, package, workflow_slug, slug), &name, favourite)
}

/// `PUT /{{vault_name}}/workflows/{{package}}/{{workflow}}/jobs/{{job}}/execute/{{command}}`: validates, suspends, resumes, retires, or
/// re-activates the job, and answers with its refreshed status.
pub async fn execute(
    State(state): State<WorkflowRoutesState>,
    Path((vault_name, package, workflow_slug, slug, command)): Path<(String, String, String, String, String)>,
    body: Bytes,
) -> Response {
    let client = match reach(&state, &vault_name, &package, &workflow_slug, &slug).await {
        Ok(client) => client,
        Err(response) => return *response,
    };
    let known_servers = state.clients.names();
    let job_store = store(
        &client,
        &known_servers,
        state.host.claims_root.as_deref(),
        &vault_name,
        &package,
        &workflow_slug,
        &slug,
    );
    let runs = control(&state, &vault_name, &package, &workflow_slug, &slug);
    let job = PageJob {
        store: &job_store,
        control: &runs,
    };
    job_pages::execute(
        &job,
        &job_path(&vault_name, &package, &workflow_slug, &slug),
        &command,
        &body,
        state.host.clock.instant().await,
    )
    .await
}

/// `GET /{{vault_name}}/workflows/{{package}}/{{workflow}}/jobs/{{job}}/confirm-delete`: the confirmation whose button is the `DELETE`,
/// for a job that exists in a vault that exists.
pub async fn confirm_delete(
    State(state): State<WorkflowRoutesState>,
    Path((vault_name, package, workflow_slug, slug)): Path<(String, String, String, String)>,
) -> Response {
    let client = match reach(&state, &vault_name, &package, &workflow_slug, &slug).await {
        Ok(client) => client,
        Err(response) => return *response,
    };
    let known_servers = state.clients.names();
    let job_store = store(
        &client,
        &known_servers,
        state.host.claims_root.as_deref(),
        &vault_name,
        &package,
        &workflow_slug,
        &slug,
    );
    let runs = control(&state, &vault_name, &package, &workflow_slug, &slug);
    let job = PageJob {
        store: &job_store,
        control: &runs,
    };
    job_pages::confirm_delete(&job, &job_path(&vault_name, &package, &workflow_slug, &slug)).await
}

/// `DELETE /{{vault_name}}/workflows/{{package}}/{{workflow}}/jobs/{{job}}`: deletes a retired job's stored state and sends the browser
/// to its workflow.
pub async fn delete(
    State(state): State<WorkflowRoutesState>,
    Path((vault_name, package, workflow_slug, slug)): Path<(String, String, String, String)>,
) -> Response {
    let client = match reach(&state, &vault_name, &package, &workflow_slug, &slug).await {
        Ok(client) => client,
        Err(response) => return *response,
    };
    let known_servers = state.clients.names();
    let job_store = store(
        &client,
        &known_servers,
        state.host.claims_root.as_deref(),
        &vault_name,
        &package,
        &workflow_slug,
        &slug,
    );
    let runs = control(&state, &vault_name, &package, &workflow_slug, &slug);
    let job = PageJob {
        store: &job_store,
        control: &runs,
    };
    job_pages::delete(
        &job,
        &job_path(&vault_name, &package, &workflow_slug, &slug),
        &crate::urls::VaultUrls::from(&*vault_name).workflow(&package, &workflow_slug),
        state.host.clock.instant().await,
    )
    .await
}
