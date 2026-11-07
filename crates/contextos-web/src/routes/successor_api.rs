//! `PUT /api/v1/{vault}/workflows/{package}/{workflow}/jobs/{job}/execute/successor` makes one successor, and `POST
//! /api/v1/{vault}/workflows/{package}/{workflow}/jobs/execute/successor` makes one for every job in `enabled` or
//! `suspended` on the current release. The host's workflows use `shared` in place of the vault's name.
//!
//! Each is a thin caller of [`crate::successor_stores`], so the API and the command line decide alike, and answers with
//! the report the command prints under `--format json`. A refusal is the JSON error document of
//! `docs/http-routing-response-contract.md`, with each refusal under `refusals`; nothing is written when any is made.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use contextos_workflows::SuccessorTarget;
use serde::Deserialize;

use crate::routes::shared_workflows::SharedRoutesState;
use crate::routes::workflows::WorkflowRoutesState;
use crate::shared_state::SharedStateStore;
use crate::successor_plan::SuccessorScope;
use crate::successor_report::{SuccessorJson, SuccessorReport};
use crate::successor_service::SuccessorRequest;
use crate::successor_stores::{SharedSuccessors, VaultSuccessors};

#[cfg(test)]
#[path = "successor_api_test.rs"]
mod tests;

/// The body of the request for one successor.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OneBody {
    new_job: String,
    #[serde(default)]
    to: Option<String>,
}

/// The body of the request for every job's successor.
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct AllBody {
    #[serde(default)]
    to: Option<String>,
}

/// The release a body names, `future` when it names none.
fn target_of(to: Option<&str>) -> Result<SuccessorTarget, Box<Response>> {
    to.map_or(Ok(SuccessorTarget::Future), |to| {
        SuccessorTarget::try_from(to).map_err(|error| Box::new(bad_request(&error.to_string())))
    })
}

/// The JSON error document, with `refusals` left to the caller.
fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    let document = serde_json::json!({ "error": { "code": code, "message": message } });
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        document.to_string(),
    )
        .into_response()
}

fn bad_request(message: &str) -> Response {
    error_response(StatusCode::BAD_REQUEST, "workflow/request-invalid", message)
}

/// The body of a request for one successor, or the bad-request response for why it is not one.
fn one_request((package, workflow, job): (&str, &str, &str), body: &Bytes) -> Result<SuccessorRequest, Box<Response>> {
    let parsed: OneBody = serde_json::from_slice(body).map_err(|error| Box::new(bad_request(&error.to_string())))?;
    let to = target_of(parsed.to.as_deref())?;
    Ok(SuccessorRequest {
        package: package.to_owned(),
        workflow: workflow.to_owned(),
        scope: SuccessorScope::One {
            job: job.to_owned(),
            new_job: parsed.new_job,
        },
        to,
    })
}

/// The body of a request for every job's successor; an empty body asks for the defaults.
fn all_request((package, workflow): (&str, &str), body: &Bytes) -> Result<SuccessorRequest, Box<Response>> {
    let parsed: AllBody = if body.iter().all(u8::is_ascii_whitespace) {
        AllBody::default()
    } else {
        serde_json::from_slice(body).map_err(|error| Box::new(bad_request(&error.to_string())))?
    };
    let to = target_of(parsed.to.as_deref())?;
    Ok(SuccessorRequest {
        package: package.to_owned(),
        workflow: workflow.to_owned(),
        scope: SuccessorScope::All,
        to,
    })
}

/// The status a refusal code has.
fn status_of(code: &str) -> StatusCode {
    match code {
        "workflow/job-not-found" | "workflow/package-unavailable" => StatusCode::NOT_FOUND,
        "workflow/job-file-invalid" | "workflow/job-key-invalid" | "workflow/job-key-mismatch" => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
        "workflow/transition-refused"
        | "workflow/job-exists"
        | "workflow/job-key-taken"
        | "workflow/job-state-remains"
        | "workflow/job-changed"
        | "packages/store-locked" => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// The report as the response: 200 when nothing was refused, otherwise the status of its refusal.
fn respond(report: &SuccessorReport) -> Response {
    let status = report.refusal_code().map_or(StatusCode::OK, status_of);
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        SuccessorJson(report).to_string(),
    )
        .into_response()
}

/// A vault the primary server knows, and the session to it.
pub(super) async fn reach_vault(
    state: &WorkflowRoutesState,
    vault: &str,
) -> Result<std::sync::Arc<crate::mcp_client::McpClient>, Box<Response>> {
    let Ok(client) = state.client() else {
        return Err(Box::new(error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "workflow/server-unavailable",
            "no server is configured to reach the vault through",
        )));
    };
    match crate::apps::vault_exists(&client, vault).await {
        Ok(true) => Ok(client),
        Ok(false) => Err(Box::new(error_response(
            StatusCode::NOT_FOUND,
            "vault/not-found",
            "there is no such vault",
        ))),
        Err(_) => Err(Box::new(error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "workflow/server-unavailable",
            "the vault's server could not be reached",
        ))),
    }
}

async fn make_in_vault(
    state: &WorkflowRoutesState,
    vault: &str,
    request: Result<SuccessorRequest, Box<Response>>,
) -> Response {
    let request = match request {
        Ok(request) => request,
        Err(response) => return *response,
    };
    let client = match reach_vault(state, vault).await {
        Ok(client) => client,
        Err(response) => return *response,
    };
    let known_servers = state.clients.names();
    respond(
        &VaultSuccessors {
            client: &client,
            vault,
            known_servers: &known_servers,
        }
        .make(&request)
        .await,
    )
}

/// `PUT /api/v1/{vault}/workflows/{package}/{workflow}/jobs/{job}/execute/successor`.
pub async fn vault_one(
    State(state): State<WorkflowRoutesState>,
    Path((vault, package, workflow, job)): Path<(String, String, String, String)>,
    body: Bytes,
) -> Response {
    make_in_vault(&state, &vault, one_request((&package, &workflow, &job), &body)).await
}

/// `POST /api/v1/{vault}/workflows/{package}/{workflow}/jobs/execute/successor`.
pub async fn vault_all(
    State(state): State<WorkflowRoutesState>,
    Path((vault, package, workflow)): Path<(String, String, String)>,
    body: Bytes,
) -> Response {
    make_in_vault(&state, &vault, all_request((&package, &workflow), &body)).await
}

async fn make_on_host(state: &SharedRoutesState, request: Result<SuccessorRequest, Box<Response>>) -> Response {
    let request = match request {
        Ok(request) => request,
        Err(response) => return *response,
    };
    respond(
        &SharedSuccessors {
            jobs_root: state.roots.jobs_root.clone(),
            store_root: state.roots.store_root.clone(),
            state: SharedStateStore::new(state.roots.state_root.clone()),
        }
        .make(&request)
        .await,
    )
}

/// `PUT /api/v1/server/workflows/{package}/{workflow}/jobs/{job}/execute/successor`.
pub async fn shared_one(
    State(state): State<SharedRoutesState>,
    Path((package, workflow, job)): Path<(String, String, String)>,
    body: Bytes,
) -> Response {
    make_on_host(&state, one_request((&package, &workflow, &job), &body)).await
}

/// `POST /api/v1/server/workflows/{package}/{workflow}/jobs/execute/successor`.
pub async fn shared_all(
    State(state): State<SharedRoutesState>,
    Path((package, workflow)): Path<(String, String)>,
    body: Bytes,
) -> Response {
    make_on_host(&state, all_request((&package, &workflow), &body)).await
}
