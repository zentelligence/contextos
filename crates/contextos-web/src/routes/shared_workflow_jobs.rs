//! One shared job and what an operator does to it: `GET /shared/workflows/{{package}}/{{workflow}}/jobs/{{job}}` (its status,
//! actions, and run log), `PATCH` (a schedule), `DELETE` (a retired job's state) with `GET .../confirm-delete`, and
//! `PUT .../execute/{{command}}`, `POST .../runs` (a run by hand, live or dry), and
//! `PUT .../runs/{{run}}/execute/cancel`.
//!
//! Every action is a thin caller of [`crate::job_pages`], over a [`SharedJobStore`], so a shared job behaves as a vault job
//! does and a page cannot allow what a command refuses. A shared job has no favourites.

use std::sync::Arc;

use askama::Template;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use contextos_workflows::{JobLifecycle, JobState};

use crate::job_chain::RunHistory;
use crate::job_operation_shared::SharedJobStore;
use crate::job_pages::{self, JobUpdate};
use crate::job_release_view::{JobReleaseInput, JobReleaseView};
use crate::job_runs_view::{JobRunsInput, JobRunsView};
use crate::job_status::{JobStatusInput, JobStatusView};
use crate::page_job::PageJob;
use crate::rendering::page::BreadcrumbSegment;
use crate::routes::shared_workflows::{
    PageError, SharedRoots, SharedRoutesState, card, job_href, key_of, page_failure, respond, workflow_href,
};
use crate::routes::vault::{not_found, unreachable};
use crate::routes::workflow_views::{Fact, JobCard, JobDetail, render_job};
use crate::run_control::{RunActivity, RunControl};
use crate::shared_claim::SharedJobKey;
use crate::shared_index::{SharedIndexedJob, index_shared};
use crate::shared_runs::{ChainSources, recent_chain_runs};
use crate::shared_state::SharedStateStore;
use crate::workflow_releases_stores::SharedReleases;

/// How many runs the log shows.
const LOG_RUNS: usize = 20;

/// What a shared job's page is made of, read from the host.
struct Gathered {
    card: JobCard,
    facts: Vec<Fact>,
    registers: bool,
    stored: Option<JobState>,
    history: RunHistory,
    history_unavailable: bool,
    /// The host's state file as read for the page.
    jobs_state: contextos_workflows::JobsState,
}

/// Reads what the page needs for the job `key`, or `None` when it has no file and nothing is stored for it. A job has a page
/// when its file is there or something is stored for it, so a retired job whose file was deleted can still be re-activated or
/// deleted from here.
///
/// Blocking: reads the package store, the job files, the state file, and run records, so call it from a blocking thread.
fn gather(roots: &SharedRoots, key: &SharedJobKey) -> Result<Option<Gathered>, PageError> {
    let indexed = index_shared(&roots.store_root, &roots.jobs_root)?;
    let state = SharedStateStore::new(roots.state_root.clone()).read()?;
    let found = indexed
        .into_iter()
        .find(|workflow| workflow.package == key.package && workflow.slug == key.workflow)
        .and_then(|workflow| workflow.jobs.into_iter().find(|job| job.key == *key));
    let stored = state.job(&key.package, &key.workflow, &key.job).cloned();
    let Some(job) = found.or_else(|| {
        stored.as_ref().map(|_| SharedIndexedJob {
            key: key.clone(),
            slug: key.job.clone(),
            loaded: None,
        })
    }) else {
        return Ok(None);
    };
    let history = recent_chain_runs(
        &ChainSources {
            jobs_root: &roots.jobs_root,
            store_root: &roots.store_root,
            state: SharedStateStore::new(roots.state_root.clone()),
            runs_root: &roots.runs_root,
        },
        key,
        LOG_RUNS,
    );
    let history_unavailable = history.is_err();
    let history = history.unwrap_or_default();
    let mut card = card(&job, &state, &roots.runs_root);
    if history_unavailable {
        card.state = "unknown";
        "History unavailable".clone_into(&mut card.last_run);
    }
    let mut facts = vec![
        Fact {
            label: "Package",
            value: key.package.clone(),
        },
        Fact {
            label: "Workflow",
            value: key.workflow.clone(),
        },
    ];
    if let Some(loaded) = &job.loaded {
        facts.push(Fact {
            label: "Release",
            value: loaded.release.version.as_semver().to_string(),
        });
        facts.push(Fact {
            label: "Timeout",
            value: format!("{}s", loaded.timeout_secs),
        });
    }
    Ok(Some(Gathered {
        registers: job.loaded.is_some(),
        card,
        facts,
        stored,
        history,
        history_unavailable,
        jobs_state: state,
    }))
}

fn crumbs(key: &SharedJobKey) -> Vec<BreadcrumbSegment> {
    vec![
        BreadcrumbSegment {
            label: "shared".to_owned(),
            href: Some("/shared".to_owned()),
        },
        BreadcrumbSegment {
            label: key.package.clone(),
            href: None,
        },
        BreadcrumbSegment {
            label: key.workflow.clone(),
            href: Some(workflow_href(&key.package, &key.workflow)),
        },
        BreadcrumbSegment {
            label: key.job.clone(),
            href: None,
        },
    ]
}

/// The job's release section: the release it names, its lock, the jobs either side of it, and what is wrong with its file,
/// or the sentence that says the releases could not be read.
async fn release_section(
    state: &SharedRoutesState,
    key: &SharedJobKey,
    jobs_state: &contextos_workflows::JobsState,
) -> String {
    let reader = SharedReleases {
        store_root: state.roots.store_root.clone(),
        jobs_root: state.roots.jobs_root.clone(),
        state: SharedStateStore::new(state.roots.state_root.clone()),
    };
    match reader.report_with_jobs(&key.package, &key.workflow).await {
        Ok((report, listings)) => {
            let href = |job: &str| format!("{}/jobs/{job}", workflow_href(&key.package, &key.workflow));
            JobReleaseView::from(JobReleaseInput {
                package: &key.package,
                workflow: &key.workflow,
                job: &key.job,
                listings: &listings,
                report: &report,
                state: jobs_state,
                job_href: &href,
            })
            .render()
            .unwrap_or_else(|_| "<p>The job's release could not be rendered.</p>".to_owned())
        }
        Err(error) => {
            tracing::warn!(
                code = error.code(),
                "a shared job's release could not be read for its page"
            );
            format!(
                "<p role=\"alert\">The job's release could not be read ({}).</p>",
                error.code()
            )
        }
    }
}

/// `GET /shared/workflows/{{package}}/{{workflow}}/jobs/{{job}}`: the job's status and actions and its most recent runs.
pub async fn detail(
    State(state): State<SharedRoutesState>,
    Path((package, workflow, job)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let Some(key) = key_of(&package, &workflow, &job) else {
        return not_found();
    };
    let roots = Arc::clone(&state.roots);
    let owned = key.clone();
    let gathered = tokio::task::spawn_blocking(move || gather(&roots, &owned)).await;
    let gathered = match gathered {
        Ok(Ok(Some(gathered))) => gathered,
        Ok(Ok(None)) => return not_found(),
        Ok(Err(error)) => return page_failure(&error),
        Err(error) => {
            tracing::warn!(%error, "a shared job could not be gathered");
            return unreachable();
        }
    };
    let base = job_href(&key);
    let activity = state.run_control(&key).activity().await.unwrap_or_else(|refusal| {
        tracing::warn!(%refusal, "a shared job page could not see what the job's runs are doing");
        RunActivity::default()
    });
    let zone = state.run_control(&key).schedule_zone();
    let status_html = JobStatusView::from(JobStatusInput {
        base_path: &base,
        stored: gathered.stored.as_ref(),
        message: None,
        zone: &zone,
        run_link: None,
    })
    .render()
    .unwrap_or_else(|_| "<p>The job's status could not be rendered.</p>".to_owned());
    let runs_html = JobRunsView::from(JobRunsInput {
        base_path: &base,
        lifecycle: gathered
            .stored
            .as_ref()
            .map_or_else(JobLifecycle::default, JobState::lifecycle),
        activity,
        history: (!gathered.history_unavailable).then_some(gathered.history),
        message: None,
        beside_another: false,
    })
    .render()
    .unwrap_or_else(|_| "<p>The job's runs could not be rendered.</p>".to_owned());
    let release_html = release_section(&state, &key, &gathered.jobs_state).await;
    let fragment = render_job(&JobDetail {
        workflow_href: workflow_href(&key.package, &key.workflow),
        registers: gathered.registers,
        facts: gathered.facts,
        job: gathered.card,
        status_html,
        release_html,
        runs_html,
        notices: Vec::new(),
    });
    respond(&state, &headers, crumbs(&key), fragment).await
}

/// `PATCH /shared/workflows/{{package}}/{{workflow}}/jobs/{{job}}`: sets the job's schedule, or clears it when empty. A shared
/// job has no favourites.
pub async fn update(
    State(state): State<SharedRoutesState>,
    Path((package, workflow, job)): Path<(String, String, String)>,
    body: Bytes,
) -> Response {
    let Some(key) = key_of(&package, &workflow, &job) else {
        return not_found();
    };
    let servers = state.current_servers();
    let host = state.validation_host(&servers);
    let store = SharedJobStore {
        state: &state.state,
        jobs_root: &state.roots.jobs_root,
        validation: &host,
        key: &key,
        claims_root: &state.roots.claims_root,
    };
    let runs = state.run_control(&key);
    let job = PageJob {
        store: &store,
        control: &runs,
    };
    let base = job_href(&key);
    match JobUpdate::try_from(&body[..]) {
        Ok(JobUpdate::Schedule(expression)) => {
            job_pages::set_schedule(&job, &base, &expression, state.vault_time.instant().await).await
        }
        Ok(JobUpdate::Favourite(_)) => {
            job_pages::invalid_request(&job, &base, "A shared job has no favourites.".to_owned()).await
        }
        Err(error) => job_pages::invalid_request(&job, &base, error.to_string()).await,
    }
}

/// `PUT /shared/workflows/{{package}}/{{workflow}}/jobs/{{job}}/execute/{{command}}`: validates, suspends, resumes, retires,
/// or re-activates the job, and answers with its refreshed status.
pub async fn execute(
    State(state): State<SharedRoutesState>,
    Path((package, workflow, job, command)): Path<(String, String, String, String)>,
    body: Bytes,
) -> Response {
    let Some(key) = key_of(&package, &workflow, &job) else {
        return not_found();
    };
    let servers = state.current_servers();
    let host = state.validation_host(&servers);
    let store = SharedJobStore {
        state: &state.state,
        jobs_root: &state.roots.jobs_root,
        validation: &host,
        key: &key,
        claims_root: &state.roots.claims_root,
    };
    let runs = state.run_control(&key);
    let job = PageJob {
        store: &store,
        control: &runs,
    };
    job_pages::execute(&job, &job_href(&key), &command, &body, state.vault_time.instant().await).await
}

/// `GET .../confirm-delete`: the confirmation whose button is the `DELETE`, for a job that exists.
pub async fn confirm_delete(
    State(state): State<SharedRoutesState>,
    Path((package, workflow, job)): Path<(String, String, String)>,
) -> Response {
    let Some(key) = key_of(&package, &workflow, &job) else {
        return not_found();
    };
    let servers = state.current_servers();
    let host = state.validation_host(&servers);
    let store = SharedJobStore {
        state: &state.state,
        jobs_root: &state.roots.jobs_root,
        validation: &host,
        key: &key,
        claims_root: &state.roots.claims_root,
    };
    let runs = state.run_control(&key);
    let job = PageJob {
        store: &store,
        control: &runs,
    };
    job_pages::confirm_delete(&job, &job_href(&key)).await
}

/// `DELETE /shared/workflows/{{package}}/{{workflow}}/jobs/{{job}}`: deletes a retired job's stored state and sends the
/// browser to its workflow.
pub async fn delete(
    State(state): State<SharedRoutesState>,
    Path((package, workflow, job)): Path<(String, String, String)>,
) -> Response {
    let Some(key) = key_of(&package, &workflow, &job) else {
        return not_found();
    };
    let servers = state.current_servers();
    let host = state.validation_host(&servers);
    let store = SharedJobStore {
        state: &state.state,
        jobs_root: &state.roots.jobs_root,
        validation: &host,
        key: &key,
        claims_root: &state.roots.claims_root,
    };
    let runs = state.run_control(&key);
    let job = PageJob {
        store: &store,
        control: &runs,
    };
    job_pages::delete(
        &job,
        &job_href(&key),
        &workflow_href(&key.package, &key.workflow),
        state.vault_time.instant().await,
    )
    .await
}

/// `POST /shared/workflows/{{package}}/{{workflow}}/jobs/{{job}}/runs`: starts a run outside the job's own schedule, live or,
/// when the body asks for it, a dry run. A live run is refused while another run of the job is active, in any process, and a
/// dry run never is.
pub async fn start_run(
    State(state): State<SharedRoutesState>,
    Path((package, workflow, job)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(key) = key_of(&package, &workflow, &job) else {
        return not_found();
    };
    let servers = state.current_servers();
    let host = state.validation_host(&servers);
    let store = SharedJobStore {
        state: &state.state,
        jobs_root: &state.roots.jobs_root,
        validation: &host,
        key: &key,
        claims_root: &state.roots.claims_root,
    };
    let runs = state.run_control(&key);
    let job = PageJob {
        store: &store,
        control: &runs,
    };
    let base = job_href(&key);
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

/// `PUT /shared/workflows/{{package}}/{{workflow}}/jobs/{{job}}/runs/{{run}}/execute/cancel`: asks the active run to stop,
/// whichever process started it. It stops at its next step.
pub async fn cancel_run(
    State(state): State<SharedRoutesState>,
    Path((package, workflow, job, run)): Path<(String, String, String, String)>,
) -> Response {
    let Some(key) = key_of(&package, &workflow, &job) else {
        return not_found();
    };
    let servers = state.current_servers();
    let host = state.validation_host(&servers);
    let store = SharedJobStore {
        state: &state.state,
        jobs_root: &state.roots.jobs_root,
        validation: &host,
        key: &key,
        claims_root: &state.roots.claims_root,
    };
    let runs = state.run_control(&key);
    let job = PageJob {
        store: &store,
        control: &runs,
    };
    job_pages::cancel_run(&job, &job_href(&key), &run).await
}

/// `GET /shared/workflows/{{package}}/{{workflow}}/jobs/{{job}}/runs`: the job's runs region as it is now, which the region asks
/// for again while a run is active. It is a fragment, so a browser that navigates to it is sent to the job's page.
pub async fn list_runs(
    State(state): State<SharedRoutesState>,
    Path((package, workflow, job)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let Some(key) = key_of(&package, &workflow, &job) else {
        return not_found();
    };
    if !headers.contains_key("hx-request") {
        return Redirect::to(&job_href(&key)).into_response();
    }
    let servers = state.current_servers();
    let host = state.validation_host(&servers);
    let store = SharedJobStore {
        state: &state.state,
        jobs_root: &state.roots.jobs_root,
        validation: &host,
        key: &key,
        claims_root: &state.roots.claims_root,
    };
    let runs = state.run_control(&key);
    let job = PageJob {
        store: &store,
        control: &runs,
    };
    job_pages::list_runs(&job, &job_href(&key)).await
}
