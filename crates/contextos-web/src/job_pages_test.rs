//! The page responders over an in-memory job: what each action answers with, and what it leaves in the state.

use axum::body::to_bytes;
use axum::http::StatusCode;
use contextos_workflows::JobLifecycle;

use super::*;
use crate::job_operation_fake::{FakeJob, Finding, NOW, StartAnswer, TestResult};
use crate::page_runs::PageRunKind;

const BASE: &str = "/harbour/workflows/wf/jobs/job";
const LIST: &str = "/harbour/workflows/wf";

async fn body(response: Response) -> Result<(StatusCode, axum::http::HeaderMap, String), Box<dyn std::error::Error>> {
    let (parts, body) = response.into_parts();
    let bytes = to_bytes(body, usize::MAX).await?;
    Ok((
        parts.status,
        parts.headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    ))
}

async fn run(job: &FakeJob, command: &str, payload: &str) -> Result<(StatusCode, String), Box<dyn std::error::Error>> {
    let (status, _, text) = body(execute(job, BASE, command, payload.as_bytes(), NOW).await).await?;
    Ok((status, text))
}

// ---------------------------------------------------------------------
// Execute
// ---------------------------------------------------------------------

#[tokio::test]
async fn validating_answers_with_the_refreshed_status_and_says_what_happened() -> TestResult {
    let job = FakeJob::new("()");

    let (status, html) = run(&job, "validate", "").await?;

    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("id=\"job-status\""), "{html}");
    assert!(
        html.contains("Validation passed") && html.contains("validated"),
        "{html}"
    );
    assert!(html.contains("role=\"status\""), "{html}");
    assert_eq!(job.lifecycle_now(), JobLifecycle::Validated);
    Ok(())
}

#[tokio::test]
async fn a_failed_validation_is_an_alert_and_the_failures_are_listed() -> TestResult {
    let job = FakeJob::new("()");
    job.finds(Finding::Fails);

    let (status, html) = run(&job, "validate", "").await?;

    assert_eq!(status, StatusCode::OK, "an HTMX swap needs a 200 to happen at all");
    assert!(
        html.contains("role=\"alert\"") && html.contains("workflow/validation-failed"),
        "{html}"
    );
    assert!(
        html.contains("validation/script-syntax") && html.contains("sync.rhai"),
        "{html}"
    );
    Ok(())
}

#[tokio::test]
async fn a_refused_command_is_an_alert_with_its_code_the_state_and_what_would_allow_it_and_changes_nothing()
-> TestResult {
    let job = FakeJob::new("()");
    let before = job.state();

    let (status, html) = run(&job, "suspend", "{\"reason\": \"paused\"}").await?;

    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("role=\"alert\""), "{html}");
    assert!(
        html.contains("workflow/transition-refused") && html.contains("draft"),
        "{html}"
    );
    assert!(html.contains("enabled"), "what would allow it:\n{html}");
    assert_eq!(job.state(), before);
    Ok(())
}

#[tokio::test]
async fn suspending_needs_a_reason_and_stores_the_one_given() -> TestResult {
    let job = FakeJob::new("()");
    job.register()?;
    job.step(
        contextos_workflows::Actor::Operator,
        &contextos_workflows::Transition::Enable(crate::job_operation_fake::schedule("0 3 * * *")?, job.current()),
    )?;

    let (_, missing) = run(&job, "suspend", "{}").await?;
    let (_, blank) = run(&job, "suspend", "{\"reason\": \"   \"}").await?;
    let (_, garbled) = run(&job, "suspend", "not json").await?;
    for html in [&missing, &blank, &garbled] {
        assert!(
            html.contains("role=\"alert\"") && html.contains("needs a reason"),
            "{html}"
        );
    }
    assert_eq!(job.lifecycle_now(), JobLifecycle::Enabled);

    let (_, done) = run(&job, "suspend", "{\"reason\": \"the upstream API is down\"}").await?;

    assert!(done.contains("the upstream API is down"), "{done}");
    assert_eq!(job.lifecycle_now(), JobLifecycle::Suspended);
    Ok(())
}

#[tokio::test]
async fn resuming_a_job_whose_validation_fails_is_refused_with_the_failures_and_it_stays_suspended() -> TestResult {
    let job = FakeJob::new("()");
    job.suspend_after_enabling()?;
    job.finds(Finding::Fails);

    let (status, html) = run(&job, "resume", "").await?;

    assert_eq!(status, StatusCode::OK);
    assert!(
        html.contains("role=\"alert\"") && html.contains("workflow/validation-failed"),
        "{html}"
    );
    assert!(
        html.contains("validation/script-syntax"),
        "the failures are listed:\n{html}"
    );
    assert_eq!(job.lifecycle_now(), JobLifecycle::Suspended);
    Ok(())
}

#[tokio::test]
async fn retiring_and_reactivating_follow_the_state() -> TestResult {
    let job = FakeJob::new("()");
    job.suspend_after_enabling()?;

    let (_, retired) = run(&job, "retire", "").await?;
    assert!(retired.contains("The job is now retired"), "{retired}");
    assert!(retired.contains("execute/reactivate"), "{retired}");

    let (_, draft) = run(&job, "reactivate", "").await?;
    assert!(draft.contains("The job is now draft"), "{draft}");
    Ok(())
}

#[tokio::test]
async fn a_command_that_does_not_exist_is_not_found() -> TestResult {
    let job = FakeJob::new("()");

    let (status, _) = run(&job, "explode", "").await?;

    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn a_job_that_does_not_exist_is_not_found_for_every_action() -> TestResult {
    let job = FakeJob::new("()");
    job.vanish();

    for command in ["validate", "resume", "retire"] {
        let (status, _) = run(&job, command, "").await?;
        assert_eq!(status, StatusCode::NOT_FOUND, "{command}");
    }
    let (status, _, _) = body(delete(&job, BASE, LIST, NOW).await).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = body(set_schedule(&job, BASE, "0 3 * * *", NOW).await).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = body(confirm_delete(&job, BASE).await).await?;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a confirmation for nothing is not offered"
    );
    Ok(())
}

// ---------------------------------------------------------------------
// Schedule
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_schedule_enables_a_registered_job_and_an_empty_one_clears_it() -> TestResult {
    let job = FakeJob::new("()");
    job.register()?;

    let (_, _, enabled) = body(set_schedule(&job, BASE, "0 3 * * *", NOW).await).await?;
    assert!(
        enabled.contains("The job is now enabled") && enabled.contains("0 3 * * *"),
        "{enabled}"
    );
    assert_eq!(job.lifecycle_now(), JobLifecycle::Enabled);

    let (_, _, cleared) = body(set_schedule(&job, BASE, "  ", NOW).await).await?;
    assert!(cleared.contains("The job is now registered"), "{cleared}");
    assert_eq!(job.lifecycle_now(), JobLifecycle::Registered);
    Ok(())
}

#[tokio::test]
async fn a_schedule_that_is_not_cron_is_an_alert_naming_it_and_changes_nothing() -> TestResult {
    let job = FakeJob::new("()");
    job.register()?;
    let before = job.state();

    let (status, _, html) = body(set_schedule(&job, BASE, "every tuesday", NOW).await).await?;

    assert_eq!(status, StatusCode::OK);
    assert!(
        html.contains("role=\"alert\"") && html.contains("every tuesday") && html.contains("cron"),
        "{html}"
    );
    assert_eq!(job.state(), before);
    Ok(())
}

#[tokio::test]
async fn a_schedule_for_a_job_whose_files_changed_since_validation_is_refused() -> TestResult {
    let job = FakeJob::new("()");
    job.register()?;
    job.edit_files("edited()");

    let (_, _, html) = body(set_schedule(&job, BASE, "0 3 * * *", NOW).await).await?;

    assert!(
        html.contains("role=\"alert\"") && html.contains("validate it again"),
        "{html}"
    );
    assert_eq!(job.lifecycle_now(), JobLifecycle::Registered);
    Ok(())
}

// ---------------------------------------------------------------------
// Delete
// ---------------------------------------------------------------------

#[tokio::test]
async fn the_confirmation_names_what_is_deleted_and_what_is_not_and_offers_a_way_back() -> TestResult {
    let job = FakeJob::new("()");

    let (status, _, html) = body(confirm_delete(&job, BASE).await).await?;

    assert_eq!(status, StatusCode::OK);
    assert!(html.contains(&format!("hx-delete=\"{BASE}\"")), "{html}");
    assert!(
        html.contains("hx-target=\"#job-status\""),
        "an error lands in the status region:\n{html}"
    );
    assert!(html.contains("files") && html.contains("untouched"), "{html}");
    assert!(
        html.contains(&format!("href=\"{BASE}\"")),
        "cancelling is a plain link back:\n{html}"
    );
    assert!(!html.contains("<html") && !html.contains("<script"), "{html}");
    Ok(())
}

#[tokio::test]
async fn deleting_a_retired_job_sends_the_browser_to_the_list_with_hx_redirect() -> TestResult {
    let job = FakeJob::new("()");
    job.suspend_after_enabling()?;
    job.step(
        contextos_workflows::Actor::Operator,
        &contextos_workflows::Transition::Retire,
    )?;

    let (status, headers, _) = body(delete(&job, BASE, LIST, NOW).await).await?;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers.get("hx-redirect").and_then(|value| value.to_str().ok()),
        Some(LIST)
    );
    assert!(job.state().job("pkg", "wf", "job").is_none());
    Ok(())
}

#[tokio::test]
async fn deleting_a_job_that_is_not_retired_is_an_alert_in_the_status_region_and_changes_nothing() -> TestResult {
    let job = FakeJob::new("()");
    job.register()?;
    let before = job.state();

    let (status, headers, html) = body(delete(&job, BASE, LIST, NOW).await).await?;

    assert_eq!(status, StatusCode::OK);
    assert!(headers.get("hx-redirect").is_none());
    assert!(html.contains("role=\"alert\"") && html.contains("retired"), "{html}");
    assert_eq!(job.state(), before);
    Ok(())
}

// ---------------------------------------------------------------------
// The body of an update
// ---------------------------------------------------------------------

#[test]
fn an_update_is_exactly_one_of_a_favourite_or_a_schedule() {
    assert_eq!(
        JobUpdate::try_from(&b"{\"favourite\": true}"[..]).ok(),
        Some(JobUpdate::Favourite(true))
    );
    assert_eq!(
        JobUpdate::try_from(&b"{\"schedule\": \"0 3 * * *\"}"[..]).ok(),
        Some(JobUpdate::Schedule("0 3 * * *".to_owned()))
    );
    for refused in [
        &b"{}"[..],
        &b"{\"favourite\": true, \"schedule\": \"0 3 * * *\"}"[..],
        &b"{\"state\": \"enabled\"}"[..],
        &b"not json"[..],
        &b""[..],
    ] {
        assert!(
            JobUpdate::try_from(refused).is_err(),
            "{}",
            String::from_utf8_lossy(refused)
        );
    }
}

#[tokio::test]
async fn every_answer_is_a_fragment_and_never_a_page() -> TestResult {
    let job = FakeJob::new("()");
    job.register()?;
    for response in [
        execute(&job, BASE, "validate", b"", NOW).await,
        execute(&job, BASE, "retire", b"", NOW).await,
        set_schedule(&job, BASE, "0 3 * * *", NOW).await,
        confirm_delete(&job, BASE).await,
    ] {
        let (_, _, html) = body(response).await?;
        assert!(
            !html.contains("<html")
                && !html.contains("<body")
                && !html.contains("<head>")
                && !html.contains("<!doctype"),
            "{html}"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Starting and stopping a run
// ---------------------------------------------------------------------

async fn start(job: &FakeJob, payload: &str) -> Result<(StatusCode, String), Box<dyn std::error::Error>> {
    let (status, _, text) = body(start_run(job, BASE, payload.as_bytes()).await.response).await?;
    Ok((status, text))
}

async fn stop(job: &FakeJob, run: &str) -> Result<(StatusCode, String), Box<dyn std::error::Error>> {
    let (status, _, text) = body(cancel_run(job, BASE, run).await).await?;
    Ok((status, text))
}

#[tokio::test]
async fn starting_a_run_starts_a_live_run_and_answers_with_the_runs_saying_so() -> TestResult {
    let job = FakeJob::new("()");
    job.register()?;

    let (status, html) = start(&job, "").await?;

    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("id=\"job-runs\""), "{html}");
    assert!(
        html.contains("role=\"status\"") && html.contains("run R-5 was started"),
        "{html}"
    );
    assert_eq!(job.started(), [PageRunKind::Live]);
    Ok(())
}

#[tokio::test]
async fn a_body_that_asks_for_a_dry_run_starts_a_dry_run() -> TestResult {
    let job = FakeJob::new("()");

    let (status, html) = start(&job, r#"{"dry_run": true}"#).await?;

    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("dry run R-5 was started"), "{html}");
    assert_eq!(job.started(), [PageRunKind::Dry]);
    Ok(())
}

#[tokio::test]
async fn a_body_that_says_it_is_not_a_dry_run_starts_a_live_run() -> TestResult {
    let job = FakeJob::new("()");
    job.register()?;

    start(&job, r#"{"dry_run": false}"#).await?;

    assert_eq!(job.started(), [PageRunKind::Live]);
    Ok(())
}

#[tokio::test]
async fn a_body_naming_anything_else_is_an_alert_and_starts_nothing() -> TestResult {
    let job = FakeJob::new("()");
    job.register()?;

    for payload in [r#"{"dry_run": true, "extra": 1}"#, r#"{"dry_run": "yes"}"#, "not json"] {
        let (status, html) = start(&job, payload).await?;

        assert_eq!(status, StatusCode::OK, "{payload}");
        assert!(html.contains("role=\"alert\""), "{payload}: {html}");
    }
    assert!(
        job.started().is_empty(),
        "`job.started()` should be empty, got {:?}",
        job.started()
    );
    Ok(())
}

#[tokio::test]
async fn a_live_run_is_refused_while_a_run_is_active_whatever_the_host_would_allow() -> TestResult {
    let job = FakeJob::new("()");
    job.register()?;
    job.runs_live(3);

    let (status, html) = start(&job, "").await?;

    assert_eq!(status, StatusCode::OK, "an HTMX swap needs a 200 to happen at all");
    assert!(
        html.contains("role=\"alert\"") && html.contains("workflow/job-busy"),
        "{html}"
    );
    assert!(
        html.contains("active run in progress") || html.contains("run is active"),
        "{html}"
    );
    assert!(job.started().is_empty(), "the page does not even ask");
    Ok(())
}

#[tokio::test]
async fn a_run_that_is_still_starting_also_blocks_a_live_run() -> TestResult {
    let job = FakeJob::new("()");
    job.register()?;
    job.is_starting();

    let (_, html) = start(&job, "").await?;

    assert!(html.contains("workflow/job-busy"), "{html}");
    assert!(
        job.started().is_empty(),
        "`job.started()` should be empty, got {:?}",
        job.started()
    );
    Ok(())
}

#[tokio::test]
async fn a_dry_run_is_allowed_while_a_live_run_is_active() -> TestResult {
    let job = FakeJob::new("()");
    job.register()?;
    job.runs_live(3);

    let (_, html) = start(&job, r#"{"dry_run": true}"#).await?;

    assert!(html.contains("role=\"status\""), "{html}");
    assert_eq!(job.started(), [PageRunKind::Dry]);
    Ok(())
}

#[tokio::test]
async fn a_start_the_job_refuses_is_an_alert_with_its_reason() -> TestResult {
    let job = FakeJob::new("()");
    job.answers_start_with(StartAnswer::NotRunnable(
        "workflow/job-not-runnable: the job is draft, so a live run is not available".to_owned(),
    ));

    let (status, html) = start(&job, "").await?;

    assert_eq!(status, StatusCode::OK);
    assert!(
        html.contains("workflow/job-not-runnable") && html.contains("draft"),
        "{html}"
    );
    assert!(html.contains("role=\"alert\""), "{html}");
    Ok(())
}

#[tokio::test]
async fn a_run_that_could_not_start_and_a_host_that_could_not_be_used_are_alerts_too() -> TestResult {
    let job = FakeJob::new("()");
    job.register()?;
    for answer in [
        StartAnswer::CouldNotStart("no scenario named \"happy\" exists".to_owned()),
        StartAnswer::HostUnavailable,
    ] {
        job.answers_start_with(answer);

        let (status, html) = start(&job, "").await?;

        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("role=\"alert\""), "{html}");
    }
    Ok(())
}

#[tokio::test]
async fn starting_a_run_of_a_job_that_does_not_exist_is_not_found_and_starts_nothing() -> TestResult {
    let job = FakeJob::new("()");
    job.vanish();

    let (status, _) = start(&job, "").await?;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        job.started().is_empty(),
        "`job.started()` should be empty, got {:?}",
        job.started()
    );
    Ok(())
}

#[tokio::test]
async fn asking_the_active_run_to_stop_says_it_was_asked() -> TestResult {
    let job = FakeJob::new("()");
    job.runs_live(7);

    let (status, html) = stop(&job, "R-7").await?;

    assert_eq!(status, StatusCode::OK);
    assert!(
        html.contains("role=\"status\"") && html.contains("R-7") && html.contains("asked to stop"),
        "{html}"
    );
    assert_eq!(job.cancelled(), [7]);
    Ok(())
}

#[tokio::test]
async fn asking_a_run_that_is_not_active_to_stop_is_an_alert_and_stops_nothing() -> TestResult {
    let job = FakeJob::new("()");
    job.runs_live(8);

    let (status, html) = stop(&job, "R-7").await?;

    assert_eq!(status, StatusCode::OK);
    assert!(
        html.contains("role=\"alert\"") && html.contains("R-7") && html.contains("not active"),
        "{html}"
    );
    assert!(
        job.cancelled().is_empty(),
        "`job.cancelled()` should be empty, got {:?}",
        job.cancelled()
    );
    Ok(())
}

#[tokio::test]
async fn a_run_that_is_not_a_run_number_is_not_found_and_stops_nothing() -> TestResult {
    let job = FakeJob::new("()");
    job.runs_live(7);

    for run in [
        "7",
        "R-",
        "R-x",
        "r-7",
        "R-7/../x",
        "R--1",
        "R-+7",
        "R- 7",
        "R-99999999999999999999999",
    ] {
        let (status, _) = stop(&job, run).await?;
        assert_eq!(status, StatusCode::NOT_FOUND, "{run}");
    }
    assert!(
        job.cancelled().is_empty(),
        "`job.cancelled()` should be empty, got {:?}",
        job.cancelled()
    );
    Ok(())
}

#[tokio::test]
async fn stopping_a_run_of_a_job_that_does_not_exist_is_not_found() -> TestResult {
    let job = FakeJob::new("()");
    job.vanish();

    let (status, _) = stop(&job, "R-7").await?;

    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

// ---------------------------------------------------------------------
// The runs region beside an action's status
// ---------------------------------------------------------------------

#[tokio::test]
async fn an_action_answers_with_the_status_and_beside_it_the_runs_region() -> TestResult {
    let job = FakeJob::new("()");

    let (_, html) = run(&job, "validate", "").await?;

    assert!(
        html.contains("id=\"job-status\"") && html.contains("id=\"job-runs\""),
        "{html}"
    );
    assert!(
        html.find("id=\"job-status\"") < html.find("id=\"job-runs\""),
        "the status is what the action replaced, so it comes first: {html}"
    );
    assert!(html.contains("hx-swap-oob=\"outerHTML\""), "{html}");
    Ok(())
}

#[tokio::test]
async fn the_runs_region_beside_an_action_follows_the_state_the_action_set() -> TestResult {
    let job = FakeJob::new("()");
    job.suspend_after_enabling()?;

    let (_, before) = run(&job, "validate", "").await?;
    let (_, after) = run(&job, "retire", "").await?;

    assert!(before.contains("Run now") && before.contains("Dry run"), "{before}");
    assert!(
        !after.contains("Run now") && !after.contains("Dry run"),
        "a retired job cannot be run: {after}"
    );
    Ok(())
}

#[tokio::test]
async fn the_runs_region_beside_an_action_shows_an_active_run() -> TestResult {
    let job = FakeJob::new("()");
    job.suspend_after_enabling()?;
    job.runs_live(9);

    let (_, html) = run(&job, "validate", "").await?;

    assert!(html.contains("R-9") && html.contains("Cancel run"), "{html}");
    Ok(())
}

#[tokio::test]
async fn listing_the_runs_answers_with_the_runs_region_alone() -> TestResult {
    let job = FakeJob::new("()");
    job.register()?;
    job.runs_live(4);

    let (status, _, html) = body(list_runs(&job, BASE).await).await?;

    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("id=\"job-runs\"") && html.contains("R-4"), "{html}");
    assert!(
        !html.contains("id=\"job-status\"") && !html.contains("hx-swap-oob"),
        "it replaces itself where it is: {html}"
    );
    Ok(())
}

#[tokio::test]
async fn listing_the_runs_of_a_job_that_does_not_exist_is_not_found() -> TestResult {
    let job = FakeJob::new("()");
    job.vanish();

    let (status, _, _) = body(list_runs(&job, BASE).await).await?;

    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn a_log_that_cannot_be_read_is_said_so_and_does_not_stop_the_region() -> TestResult {
    let job = FakeJob::new("()");
    job.register()?;
    job.has_history(None);

    let (status, _, html) = body(list_runs(&job, BASE).await).await?;

    assert_eq!(status, StatusCode::OK);
    assert!(
        html.contains("Run history is unavailable") && html.contains("Run now"),
        "{html}"
    );
    Ok(())
}

#[tokio::test]
async fn a_dry_run_this_process_started_is_shown_as_in_progress_and_followed() -> TestResult {
    let job = FakeJob::new("()");
    job.register()?;
    job.page_has(crate::page_runs::PageRunsOfJob {
        running: vec![(PageRunKind::Dry, NOW)],
        last_ended: None,
    });

    let (_, _, html) = body(list_runs(&job, BASE).await).await?;

    assert!(
        html.contains("A dry run is in progress.") && html.contains("hx-trigger=\"every 3s\""),
        "{html}"
    );
    assert!(html.contains("Run now"), "a dry run does not stop a live run: {html}");
    Ok(())
}

#[tokio::test]
async fn a_change_of_state_never_stops_a_run_that_is_active() -> TestResult {
    let job = FakeJob::new("()");
    job.suspend_after_enabling()?;
    job.runs_live(7);

    run(&job, "retire", "").await?;
    run(&job, "reactivate", "").await?;

    assert!(
        job.cancelled().is_empty(),
        "only an operator's request to stop a run stops it"
    );
    Ok(())
}

#[test]
fn the_status_message_of_an_enabling_names_the_predecessor_retired_and_how_current_and_previous_moved() {
    use crate::enabling_report::{EnablingReport, VersionMove};
    let report = EnablingReport {
        schedule: "0 5 * * *".to_owned(),
        retired: Some("nightly".to_owned()),
        current: Some(VersionMove {
            from: Some("1.0.0".to_owned()),
            to: Some("2.0.0".to_owned()),
        }),
        previous: Some(VersionMove {
            from: None,
            to: Some("1.0.0".to_owned()),
        }),
    };

    let message = StatusMessage::from(&OperationOutcome::Enabled(Box::new(report)));

    assert!(message.ok);
    assert_eq!(
        message.text,
        "The job is now enabled. Its schedule is 0 5 * * *. Its predecessor nightly was retired. Current moved from 1.0.0 to 2.0.0; previous moved from none to 1.0.0."
    );
}

#[tokio::test]
async fn retiring_a_job_runs_retention_and_a_retention_that_fails_is_a_warning_beside_the_success() -> TestResult {
    let job = FakeJob::new("()");
    job.suspend_after_enabling()?;
    job.fails_retention();

    let (status, html) = run(&job, "retire", "").await?;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(job.lifecycle_now(), JobLifecycle::Retired);
    assert_eq!(job.retentions(), 1);
    assert!(html.contains("The job is now retired."), "{html}");
    assert!(html.contains("packages/retention-failed"), "{html}");
    assert!(html.contains("role=\"status\""), "{html}");
    Ok(())
}

#[tokio::test]
async fn deleting_a_job_whose_retention_fails_shows_the_warning_beside_a_way_back_to_the_list() -> TestResult {
    let job = FakeJob::new("()");
    job.suspend_after_enabling()?;
    run(&job, "retire", "").await?;
    job.fails_retention();

    let (status, headers, html) = body(delete(&job, BASE, LIST, NOW).await).await?;

    assert_eq!(status, StatusCode::OK);
    assert!(
        headers.get("hx-redirect").is_none(),
        "the warning would be lost in the redirect"
    );
    assert!(job.state().job("pkg", "wf", "job").is_none());
    assert!(
        html.contains("The job&#39;s stored state was deleted.")
            || html.contains("The job's stored state was deleted."),
        "{html}"
    );
    assert!(html.contains("packages/retention-failed"), "{html}");
    assert!(html.contains(&format!("href=\"{LIST}\"")), "{html}");
    Ok(())
}

#[tokio::test]
async fn a_schedule_set_on_a_registered_job_runs_retention() -> TestResult {
    let job = FakeJob::new("()");
    job.register()?;

    let response = set_schedule(&job, BASE, "0 3 * * *", NOW).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(job.retentions(), 1);
    Ok(())
}

#[tokio::test]
async fn deleting_a_retired_job_runs_retention() -> TestResult {
    let job = FakeJob::new("()");
    job.suspend_after_enabling()?;
    run(&job, "retire", "").await?;
    let before = job.retentions();

    let response = delete(&job, BASE, LIST, NOW).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(job.retentions(), before + 1);
    Ok(())
}
