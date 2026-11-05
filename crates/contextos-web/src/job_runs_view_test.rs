use askama::Template;
use contextos_workflows::JobLifecycle;
use time::macros::datetime;

use super::*;
use crate::page_runs::{PageRunEnded, PageRunsOfJob};
use crate::run_claim::ActiveRun;

const BASE: &str = "/harbour/workflows/ops/sync/jobs/nightly";
const NOW: time::OffsetDateTime = datetime!(2026-10-01 03:00:00 UTC);

fn render_in(
    lifecycle: JobLifecycle,
    activity: RunActivity,
    history: Option<Vec<RunSummary>>,
    beside_another: bool,
) -> Result<String, askama::Error> {
    JobRunsView::from(JobRunsInput {
        base_path: BASE,
        lifecycle,
        activity,
        history: history.map(RunHistory::from),
        message: None,
        beside_another,
    })
    .render()
}

fn render(lifecycle: JobLifecycle, activity: RunActivity) -> Result<String, askama::Error> {
    render_in(lifecycle, activity, Some(Vec::new()), false)
}

fn live(run_id: u64) -> RunActivity {
    RunActivity {
        claim: ClaimState::Held(Some(ActiveRun { run_id, started: NOW })),
        page: PageRunsOfJob::default(),
    }
}

fn dry_running() -> RunActivity {
    RunActivity {
        claim: ClaimState::Idle,
        page: PageRunsOfJob {
            running: vec![(PageRunKind::Dry, NOW)],
            last_ended: None,
        },
    }
}

fn run(number: u64, status: &str, dry_run: bool) -> RunSummary {
    RunSummary {
        path: String::new(),
        run_id: format!("R-{number}"),
        job: String::new(),
        release: String::new(),
        dry_run,
        trigger: "manual".to_owned(),
        status: status.to_owned(),
        started: "2026-10-01T03:00:00Z".to_owned(),
        completed: "2026-10-01T03:00:05Z".to_owned(),
        error: String::new(),
        code: None,
        script_started: crate::job_runs::ScriptStarted::Absent,
    }
}

const RUN_NOW: &str =
    "hx-post=\"/harbour/workflows/ops/sync/jobs/nightly/runs\" hx-target=\"#job-runs\" hx-swap=\"outerHTML\">Run now";

#[test]
fn an_idle_job_that_may_be_run_offers_run_now_and_a_dry_run_and_does_not_poll() -> Result<(), askama::Error> {
    let html = render(JobLifecycle::Registered, RunActivity::default())?;

    assert!(html.contains(RUN_NOW), "{html}");
    assert!(
        html.contains(&format!("hx-post=\"{BASE}/runs\" hx-vals='{{\"dry_run\": true}}'"))
            || html.contains("hx-vals='{\"dry_run\": true}'"),
        "{html}"
    );
    assert!(html.contains("Dry run"), "{html}");
    assert!(
        !html.contains("hx-trigger"),
        "nothing is running, so nothing is followed: {html}"
    );
    assert!(!html.contains("disabled"), "{html}");
    Ok(())
}

#[test]
fn a_draft_offers_a_dry_run_and_no_run_now() -> Result<(), askama::Error> {
    let html = render(JobLifecycle::Draft, RunActivity::default())?;

    assert!(html.contains("Dry run"), "{html}");
    assert!(!html.contains("Run now"), "{html}");
    Ok(())
}

#[test]
fn a_retired_job_offers_no_run_of_any_kind() -> Result<(), askama::Error> {
    let html = render(JobLifecycle::Retired, RunActivity::default())?;

    assert!(!html.contains("Run now") && !html.contains("Dry run"), "{html}");
    Ok(())
}

#[test]
fn an_active_run_is_shown_with_a_cancel_and_run_now_is_disabled_with_the_reason() -> Result<(), askama::Error> {
    let html = render(JobLifecycle::Enabled, live(7))?;

    assert!(html.contains("R-7"), "{html}");
    assert!(
        html.contains(&format!("hx-put=\"{BASE}/runs/R-7/execute/cancel\"")),
        "{html}"
    );
    assert!(html.contains("Cancel run"), "{html}");
    assert!(html.contains("active run in progress"), "{html}");
    assert!(html.contains("disabled"), "{html}");
    assert!(
        !html.contains(RUN_NOW),
        "Run now cannot be pressed while a run is active: {html}"
    );
    assert!(
        html.contains(&format!("hx-get=\"{BASE}/runs\" hx-trigger=\"every 3s\"")),
        "the region follows the run: {html}"
    );
    Ok(())
}

#[test]
fn a_dry_run_can_be_started_while_a_live_run_is_active() -> Result<(), askama::Error> {
    let html = render(JobLifecycle::Enabled, live(7))?;

    assert!(html.contains("Dry run"), "{html}");
    Ok(())
}

#[test]
fn a_run_that_has_not_said_which_run_it_is_shows_as_starting_and_cannot_be_cancelled_yet() -> Result<(), askama::Error>
{
    let html = render(
        JobLifecycle::Enabled,
        RunActivity {
            claim: ClaimState::Held(None),
            page: PageRunsOfJob::default(),
        },
    )?;

    assert!(html.contains("starting"), "{html}");
    assert!(!html.contains("Cancel run"), "{html}");
    assert!(html.contains("hx-trigger=\"every 3s\""), "{html}");
    Ok(())
}

#[test]
fn a_dry_run_in_progress_is_shown_and_followed_and_does_not_stop_a_live_run() -> Result<(), askama::Error> {
    let html = render(JobLifecycle::Registered, dry_running())?;

    assert!(html.contains("A dry run is in progress."), "{html}");
    assert!(html.contains("hx-trigger=\"every 3s\""), "{html}");
    assert!(
        html.contains(RUN_NOW) && !html.contains("active run in progress"),
        "{html}"
    );
    Ok(())
}

#[test]
fn the_last_run_started_from_a_page_is_shown_with_how_it_ended() -> Result<(), askama::Error> {
    let ended = |ok: bool, text: &str| RunActivity {
        claim: ClaimState::Idle,
        page: PageRunsOfJob {
            running: Vec::new(),
            last_ended: Some(PageRunEnded {
                kind: PageRunKind::Live,
                at: NOW,
                ok,
                text: text.to_owned(),
            }),
        },
    };

    let good = render(JobLifecycle::Registered, ended(true, "succeeded"))?;
    let bad = render(
        JobLifecycle::Registered,
        ended(false, "workflow/package-unavailable: the package is not installed"),
    )?;

    assert!(
        good.contains("Last run started here") && good.contains("succeeded"),
        "{good}"
    );
    assert!(
        bad.contains("workflow/package-unavailable") && bad.contains("role=\"alert\""),
        "{bad}"
    );
    assert!(!bad.contains("hx-trigger"), "an ended run is not followed: {bad}");
    Ok(())
}

#[test]
fn the_log_lists_each_run_and_marks_a_dry_run() -> Result<(), askama::Error> {
    let html = render_in(
        JobLifecycle::Registered,
        RunActivity::default(),
        Some(vec![run(4, "succeeded", true), run(3, "failed", false)]),
        false,
    )?;

    assert!(html.contains("R-4") && html.contains("R-3"), "{html}");
    assert_eq!(html.matches("dry run</span>").count(), 1, "{html}");
    Ok(())
}

#[test]
fn a_job_with_no_runs_says_so_and_a_log_that_could_not_be_read_says_that() -> Result<(), askama::Error> {
    let none = render(JobLifecycle::Registered, RunActivity::default())?;
    let unreadable = render_in(JobLifecycle::Registered, RunActivity::default(), None, false)?;

    assert!(none.contains("No runs recorded yet"), "{none}");
    assert!(
        unreadable.contains("Run history is unavailable") && unreadable.contains("role=\"alert\""),
        "{unreadable}"
    );
    Ok(())
}

#[test]
fn a_region_sent_beside_another_replaces_the_one_on_the_page_by_itself() -> Result<(), askama::Error> {
    let beside = render_in(JobLifecycle::Registered, RunActivity::default(), Some(Vec::new()), true)?;
    let alone = render(JobLifecycle::Registered, RunActivity::default())?;

    assert!(beside.contains("hx-swap-oob=\"outerHTML\""), "{beside}");
    assert!(!alone.contains("hx-swap-oob"), "{alone}");
    Ok(())
}

#[test]
fn a_chain_that_could_not_be_read_says_the_log_holds_only_the_jobs_own_runs() -> Result<(), askama::Error> {
    let history = RunHistory::of_the_job_alone(Vec::new());

    let page = JobRunsView::from(JobRunsInput {
        base_path: BASE,
        lifecycle: JobLifecycle::Enabled,
        activity: RunActivity::default(),
        history: Some(history),
        message: None,
        beside_another: false,
    })
    .render()?;

    assert!(
        page.contains("could not be read, so only this job's runs are shown"),
        "{page}"
    );
    assert!(!page.contains("Run history is unavailable"), "{page}");
    assert!(!render(JobLifecycle::Enabled, RunActivity::default())?.contains("so only this job's runs are shown"));
    Ok(())
}
