//! What a job page's actions answer with, for a vault job and a shared job alike.
//!
//! Each responder takes the job's [`JobStore`], so a vault page and a shared page differ only in how they build one.
//! Every action goes through [`perform`], so a page cannot allow what a command refuses. Each answers with the
//! refreshed status region (`job_status.html`) for the page to swap in, with the outcome inside it: a refusal is an
//! alert in a `200`, which is what makes HTMX swap it (`docs/http-routing-response-contract.md`), and it keeps its
//! `workflow/` code and says what would allow the change. Only an unknown command, an unknown job, and a store that
//! cannot be reached are anything else.

use askama::Template;
use std::fmt::Write as _;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use contextos_workflows::{Applied, JobSchedule, JobState, SuspensionReason};
use serde::Deserialize;
use thiserror::Error;
use time::OffsetDateTime;

use crate::job_operation::{
    JobStore, OperationError, OperationOutcome, OperatorRequest, Performed, StoreError, perform_retaining,
};
use crate::job_runs_view::{JobRunsInput, JobRunsView};
use crate::job_status::{JobStatusInput, JobStatusView, StatusMessage};
use crate::page_runs::PageRunKind;
use crate::routes::vault::{html_response, not_found, unreachable};
use crate::run_claim::ClaimState;
use crate::run_control::{RunControl, RunRefusal, StartedRun};

/// The body of a `PATCH` of a job: exactly one thing to change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JobUpdate {
    /// Whether the job is a favourite. Only a vault job has favourites.
    Favourite(bool),
    /// A schedule to set, or an empty one to clear.
    Schedule(String),
}

#[derive(Debug, Error)]
#[error("a job update names exactly one of `favourite` or `schedule`")]
pub struct JobUpdateError;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JobUpdateRaw {
    #[serde(default)]
    favourite: Option<bool>,
    #[serde(default)]
    schedule: Option<String>,
}

impl TryFrom<&[u8]> for JobUpdate {
    type Error = JobUpdateError;

    fn try_from(body: &[u8]) -> Result<Self, JobUpdateError> {
        let raw: JobUpdateRaw = serde_json::from_slice(body).map_err(|_| JobUpdateError)?;
        match (raw.favourite, raw.schedule) {
            (Some(favourite), None) => Ok(Self::Favourite(favourite)),
            (None, Some(schedule)) => Ok(Self::Schedule(schedule)),
            _ => Err(JobUpdateError),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SuspendBody {
    reason: String,
}

fn alert(text: impl Into<String>) -> StatusMessage {
    StatusMessage {
        ok: false,
        text: text.into(),
    }
}

impl From<&OperationOutcome> for StatusMessage {
    fn from(outcome: &OperationOutcome) -> Self {
        match outcome {
            OperationOutcome::Validated { result, lifecycle } if result.passed => Self {
                ok: true,
                text: format!("Validation passed. The job is {lifecycle}."),
            },
            OperationOutcome::Validated { .. } => {
                alert("workflow/validation-failed: validation failed. The failures are listed below.")
            }
            OperationOutcome::Changed(Applied::To(state)) => Self {
                ok: true,
                text: format!("The job is now {state}."),
            },
            OperationOutcome::Enabled(report) => Self {
                ok: true,
                text: format!("The job is now enabled. {report}"),
            },
            OperationOutcome::Changed(Applied::Removed) => Self {
                ok: true,
                text: "The job's stored state was deleted.".to_owned(),
            },
        }
    }
}

impl From<&Performed> for StatusMessage {
    /// What the request came to, then each warning raised after it: the request succeeded, so the message does too.
    fn from(performed: &Performed) -> Self {
        let mut message = Self::from(&performed.outcome);
        for warning in &performed.warnings {
            let _ = write!(message.text, " {warning}");
        }
        message
    }
}

impl From<&OperationError> for StatusMessage {
    fn from(error: &OperationError) -> Self {
        // A resume that did not validate lists each failure in the status region, so its message is only the headline.
        let text = error.to_string();
        let headline = match error {
            OperationError::NotValid { .. } => text.lines().next().unwrap_or_default().to_owned(),
            _ => text,
        };
        alert(headline)
    }
}

/// A store that could not be read or written is a failure of the host, not of the request.
fn store_failure(error: &StoreError) -> Response {
    tracing::warn!(%error, "a job page could not reach the job's state");
    unreachable()
}

/// The status region for the job as it is stored now, with `message` inside it, and beside it the runs region, because
/// what a state allows decides whether a run may be started. A runs region that cannot be made is left out and logged,
/// so the outcome of the action is still told.
async fn respond<S: JobStore + RunControl + Sync>(
    store: &S,
    base_path: &str,
    message: Option<StatusMessage>,
) -> Response {
    let stored = match store.stored().await {
        Ok(stored) => stored,
        Err(error) => return store_failure(&error),
    };
    let zone = store.schedule_zone();
    // The record of the run a suspension names is looked for only when it names one.
    let run_link = match stored
        .as_ref()
        .and_then(JobState::suspension)
        .and_then(|record| record.run)
    {
        Some(number) => store.record_link(number).await,
        None => None,
    };
    let view = JobStatusView::from(JobStatusInput {
        base_path,
        stored: stored.as_ref(),
        message,
        zone: &zone,
        run_link,
    });
    let status = match view.render() {
        Ok(html) => html,
        Err(error) => return render_failure("the job status", &error),
    };
    let runs = match runs_html(store, base_path, None, true).await {
        Ok(html) => html,
        Err(refusal) => {
            tracing::warn!(%refusal, "a job's runs could not be shown beside its status");
            String::new()
        }
    };
    html_response(format!("{status}{runs}"))
}

fn render_failure(what: &str, error: &askama::Error) -> Response {
    tracing::warn!(%error, "{what} could not be rendered");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("{what} could not be rendered"),
    )
        .into_response()
}

/// The runs region for the job as it is now, with `message` inside it. `beside_another` is for a region sent after the
/// one an action replaces, so it replaces the one on the page by itself.
async fn runs_html<S: JobStore + RunControl + Sync>(
    store: &S,
    base_path: &str,
    message: Option<StatusMessage>,
    beside_another: bool,
) -> Result<String, RunRefusal> {
    let lifecycle = store.lifecycle().await.map_err(|error| {
        tracing::warn!(%error, "a job's state could not be read to show its runs");
        RunRefusal::Unavailable("read the job's state")
    })?;
    let activity = store.activity().await?;
    let history = store.history().await.map_err(|refusal| {
        tracing::warn!(%refusal, "a job's run history could not be read");
    });
    JobRunsView::from(JobRunsInput {
        base_path,
        lifecycle,
        activity,
        history: history.ok(),
        message,
        beside_another,
    })
    .render()
    .map_err(|error| {
        tracing::warn!(%error, "a job's runs could not be rendered");
        RunRefusal::Unavailable("show the job's runs")
    })
}

/// The runs region for the job as it is now, with `message` inside it, as the answer to a request about a run.
async fn respond_runs<S: JobStore + RunControl + Sync>(
    store: &S,
    base_path: &str,
    message: Option<StatusMessage>,
) -> Response {
    match runs_html(store, base_path, message, false).await {
        Ok(html) => html_response(html),
        Err(refusal) => {
            tracing::warn!(%refusal, "a job page could not show the job's runs");
            unreachable()
        }
    }
}

/// Whether the job exists, as an early answer when it does not, so no action runs for a name that is nothing.
async fn missing<S: JobStore + RunControl + Sync>(store: &S) -> Option<Response> {
    match store.exists().await {
        Ok(true) => None,
        Ok(false) => Some(not_found()),
        Err(error) => Some(store_failure(&error)),
    }
}

/// A request the page could not understand, answered with the status region and an alert saying why, so it is shown
/// where the operator acted. A job that does not exist is not found.
pub(crate) async fn invalid_request<S: JobStore + RunControl + Sync>(
    store: &S,
    base_path: &str,
    text: String,
) -> Response {
    if let Some(response) = missing(store).await {
        return response;
    }
    respond(store, base_path, Some(alert(text))).await
}

/// `PUT {job}/execute/{command}`: validates, resumes, retires, or re-activates the job, or suspends it with the reason
/// in `body`.
pub(crate) async fn execute<S: JobStore + RunControl + Sync>(
    store: &S,
    base_path: &str,
    command: &str,
    body: &[u8],
    now: OffsetDateTime,
) -> Response {
    let request = match command {
        "validate" => Ok(OperatorRequest::Validate),
        "resume" => Ok(OperatorRequest::Resume),
        "retire" => Ok(OperatorRequest::Retire),
        "reactivate" => Ok(OperatorRequest::Reactivate),
        "suspend" => suspension(body),
        _ => return not_found(),
    };
    if let Some(response) = missing(store).await {
        return response;
    }
    let message = match request {
        Ok(request) => match perform_retaining(store, request, now).await {
            Ok(performed) => StatusMessage::from(&performed),
            Err(error) => StatusMessage::from(&error),
        },
        Err(text) => alert(text),
    };
    respond(store, base_path, Some(message)).await
}

fn suspension(body: &[u8]) -> Result<OperatorRequest, &'static str> {
    const NEEDS_REASON: &str = "A suspension needs a reason.";
    let body: SuspendBody = serde_json::from_slice(body).map_err(|_| NEEDS_REASON)?;
    SuspensionReason::try_from(body.reason.as_str())
        .map(OperatorRequest::Suspend)
        .map_err(|_| NEEDS_REASON)
}

/// `PATCH {job}` with a schedule: sets it, which enables a registered job and replaces the schedule of an enabled or
/// suspended one, and an empty one clears it.
pub(crate) async fn set_schedule<S: JobStore + RunControl + Sync>(
    store: &S,
    base_path: &str,
    expression: &str,
    now: OffsetDateTime,
) -> Response {
    if let Some(response) = missing(store).await {
        return response;
    }
    let expression = expression.trim();
    let request = if expression.is_empty() {
        Ok(OperatorRequest::ClearSchedule)
    } else {
        JobSchedule::try_from(expression)
            .map(OperatorRequest::Schedule)
            .map_err(|error| error.to_string())
    };
    let message = match request {
        Ok(request) => match perform_retaining(store, request, now).await {
            Ok(performed) => StatusMessage::from(&performed),
            Err(error) => StatusMessage::from(&error),
        },
        Err(text) => alert(text),
    };
    respond(store, base_path, Some(message)).await
}

/// The body of a `POST` of a run: whether it is a dry run, which reads and never writes. An empty body is a live run.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartBody {
    #[serde(default)]
    dry_run: bool,
}

fn started_message(started: &StartedRun) -> StatusMessage {
    let kind = if started.is_dry() { "dry" } else { "live" };
    let text = match started.run_id {
        Some(run_id) => format!("The {kind} run R-{run_id} was started."),
        None => format!("The {kind} run was started."),
    };
    StatusMessage { ok: true, text }
}

/// What a request to start a run came to: whether a run was started, and the runs region to answer with.
pub(crate) struct StartOutcome {
    pub(crate) started: bool,
    pub(crate) response: Response,
}

/// `POST {job}/runs`: starts a run on a task of its own and answers at once with the runs region, which follows the run
/// until it ends. A live run is refused while another run of the job is active, and a dry run is not, because a dry run
/// takes no claim. Everything refused is an alert in a `200`, so it is swapped in where the operator acted.
pub(crate) async fn start_run<S: JobStore + RunControl + Sync>(
    store: &S,
    base_path: &str,
    body: &[u8],
) -> StartOutcome {
    if let Some(response) = missing(store).await {
        return StartOutcome {
            started: false,
            response,
        };
    }
    let kind = match start_kind(body) {
        Ok(kind) => kind,
        Err(text) => {
            return StartOutcome {
                started: false,
                response: respond_runs(store, base_path, Some(alert(text))).await,
            };
        }
    };
    let (started, message) = match begin(store, kind).await {
        Ok(started) => (true, started_message(&started)),
        Err(refusal) => (false, alert(refusal.to_string())),
    };
    StartOutcome {
        started,
        response: respond_runs(store, base_path, Some(message)).await,
    }
}

fn start_kind(body: &[u8]) -> Result<PageRunKind, &'static str> {
    const UNDERSTOOD: &str = "A run is started with an empty body, or one that names only `dry_run` as true or false.";
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(PageRunKind::Live);
    }
    let body: StartBody = serde_json::from_slice(body).map_err(|_| UNDERSTOOD)?;
    Ok(if body.dry_run {
        PageRunKind::Dry
    } else {
        PageRunKind::Live
    })
}

/// Starts a run of `kind`. A live run is refused here while the job's claim is held, so the request is not even made,
/// and again where the claim is taken, which is the rule that holds across processes.
async fn begin<S: RunControl + Sync>(store: &S, kind: PageRunKind) -> Result<StartedRun, RunRefusal> {
    if kind == PageRunKind::Live && matches!(store.activity().await?.claim, ClaimState::Held(_)) {
        return Err(RunRefusal::Busy);
    }
    store.start(kind).await
}

/// `PUT {job}/runs/{run}/execute/cancel`: asks the active run to stop. It stops at its next step, and the status
/// follows it until it has. A run that is not active, because it ended or is another's, is an alert and stops nothing.
pub(crate) async fn cancel_run<S: JobStore + RunControl + Sync>(store: &S, base_path: &str, run: &str) -> Response {
    let Some(run_id) = run_number(run) else {
        return not_found();
    };
    if let Some(response) = missing(store).await {
        return response;
    }
    let message = match store.cancel(run_id).await {
        Ok(()) => StatusMessage {
            ok: true,
            text: format!("Run R-{run_id} was asked to stop. It stops at its next step."),
        },
        Err(refusal) => alert(refusal.to_string()),
    };
    respond_runs(store, base_path, Some(message)).await
}

/// `GET {job}/runs`: the runs region as the job's runs are now, which the region asks for again while a run is active,
/// so it follows the run to its end. Asked for by HTMX only, because it is a fragment.
pub(crate) async fn list_runs<S: JobStore + RunControl + Sync>(store: &S, base_path: &str) -> Response {
    if let Some(response) = missing(store).await {
        return response;
    }
    respond_runs(store, base_path, None).await
}

/// The number in a run's identifier, which is `R-` and then the number and nothing else.
fn run_number(run: &str) -> Option<u64> {
    let digits = run.strip_prefix("R-")?;
    (!digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| digits.parse().ok())
        .flatten()
}

#[derive(Template)]
#[template(path = "job_deleted.html")]
struct Deleted<'a> {
    text: &'a str,
    list_path: &'a str,
}

#[derive(Template)]
#[template(path = "job_confirm_delete.html")]
struct ConfirmDelete<'a> {
    base_path: &'a str,
    removes_file: bool,
}

/// `GET {job}/confirm-delete`: the confirmation fragment, whose button is the `DELETE`. There is nothing to confirm for
/// a job that does not exist.
pub(crate) async fn confirm_delete<S: JobStore + RunControl + Sync>(store: &S, base_path: &str) -> Response {
    if let Some(response) = missing(store).await {
        return response;
    }
    match (ConfirmDelete {
        base_path,
        removes_file: store.removes_files_on_delete(),
    })
    .render()
    {
        Ok(html) => html_response(html),
        Err(error) => {
            tracing::warn!(%error, "a delete confirmation could not be rendered");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "the confirmation could not be rendered",
            )
                .into_response()
        }
    }
}

/// `DELETE {job}`: deletes a retired job's stored state, and a shared job's file with it, and sends the browser to
/// `list_path`. Anything the table refuses is an alert in the status region the confirmation's button targets.
pub(crate) async fn delete<S: JobStore + RunControl + Sync>(
    store: &S,
    base_path: &str,
    list_path: &str,
    now: OffsetDateTime,
) -> Response {
    if let Some(response) = missing(store).await {
        return response;
    }
    match perform_retaining(store, OperatorRequest::Delete, now).await {
        Ok(performed) if performed.warnings.is_empty() => {
            (StatusCode::OK, [("hx-redirect", list_path.to_owned())], "").into_response()
        }
        // A redirect would lose the warning, and the job's page is gone, so the status region says what was done and
        // where the jobs are.
        Ok(performed) => {
            let view = Deleted {
                text: &StatusMessage::from(&performed).text,
                list_path,
            };
            match view.render() {
                Ok(html) => html_response(html),
                Err(error) => render_failure("the deleted job's status", &error),
            }
        }
        Err(error) => respond(store, base_path, Some(StatusMessage::from(&error))).await,
    }
}

#[cfg(test)]
#[path = "job_pages_test.rs"]
mod tests;
