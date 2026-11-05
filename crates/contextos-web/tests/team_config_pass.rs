//! A vault whose `.contextos/team.toml` cannot be used for jobs, seen from the scheduler, the command line, and the
//! workflows page, against a real `contextos` session and a real temporary vault.
//!
//! A team that breaks a rule, or that cannot be parsed, is a configuration error: the vault's whole job pass waits and
//! nothing is validated, suspended, raised, started, or recorded against any job. A team that cannot be read because
//! the server is unreachable is an outage, which is neither a team error nor a defect in a job.

mod support;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use contextos_core::TeamRule;
use contextos_web::cli_run::{CliRunError, CliRunOptions};
use support::jobs::{
    JobVault, READY_JOB, SCRIPT, STATE_PATH, Spec, TEAM, TEAM_UNPARSABLE, TEAM_WITH_TWO_CHIEFS, TEAM_WITHOUT_CHIEF,
    VAULT_NAME, enabled_state, lifecycle_state,
};
use support::logs::capture;
use support::{BoxError, run_job};
use time::OffsetDateTime;
use time::macros::datetime;
use tower::ServiceExt as _;

/// 03:00:30, when a job scheduled every minute fell due at 03:00:00.
const NOW: OffsetDateTime = datetime!(2026-10-01 03:00:30 UTC);

/// A script that survives the failure of its one call, so a dry run against an error scenario completes.
const TOLERANT_SCRIPT: &str =
    "try { call_mcp(\"contextos\", \"vault_info\", #{}); } catch (e) { flag_degraded(\"the read failed\"); }";
const ERROR_SCENARIO: &str =
    "[[call]]\nserver = \"contextos\"\ntool = \"vault_info\"\nkind = \"error\"\nerror = \"offline\"\n";

fn state_bytes(host: &JobVault) -> Result<Option<Vec<u8>>, BoxError> {
    let path = host.root().join(STATE_PATH);
    Ok(if path.exists() {
        Some(std::fs::read(path)?)
    } else {
        None
    })
}

/// Stops the `contextos` server a vault's session talks to, by the process id its own session holds, so the next call
/// cannot reach it and no other process is touched.
#[cfg(unix)]
fn kill_server(host: &JobVault) -> Result<(), BoxError> {
    let pid = host
        .clients
        .primary()
        .and_then(|client| client.pid())
        .ok_or("the vault's stdio session reports its child process id")?;
    let status = std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err("the vault's server could not be signalled".into())
    }
}

#[tokio::test]
async fn a_pass_over_a_vault_whose_team_has_no_chief_of_staff_validates_suspends_raises_and_starts_nothing()
-> Result<(), BoxError> {
    let mut spec = Spec {
        team: Some(TEAM_WITHOUT_CHIEF),
        ..Spec::default()
    };
    let state = enabled_state(&spec);
    spec.state = Some(&state);
    let host = JobVault::open(&spec).await?;
    host.write("registry/workflows/ops/sync/jobs/second.toml", READY_JOB)?;
    let before = state_bytes(&host)?;

    host.pass(NOW).await;

    assert_eq!(state_bytes(&host)?, before, "the state file is byte for byte as it was");
    assert_eq!(host.lifecycle()?, "enabled", "the enabled job is not suspended");
    assert!(host.run_records().is_empty(), "the due job was not started");
    assert!(host.raised_tasks().is_empty(), "no task is raised for a team error");
    assert!(
        host.state()?.job("ops", "sync", "second").is_none(),
        "the ready draft was not validated, so nothing is recorded for it"
    );

    // The same vault with a usable team starts the due job and validates the draft, so the quiet pass above was the
    // team's doing and not a fixture that never had anything to do.
    host.write(".contextos/team.toml", TEAM)?;
    host.pass(NOW).await;
    assert_eq!(host.run_records().len(), 1, "once the team is valid the due job starts");
    assert!(
        host.state()?.job("ops", "sync", "second").is_some(),
        "and the ready draft is validated"
    );
    Ok(())
}

#[tokio::test]
async fn a_pass_over_a_vault_whose_team_does_not_parse_writes_nothing_to_the_state_or_any_record()
-> Result<(), BoxError> {
    let spec = Spec {
        team: Some(TEAM_UNPARSABLE),
        ..Spec::default()
    };
    let host = JobVault::open(&spec).await?;
    let (log, _guard) = capture();

    host.pass(NOW).await;

    assert!(
        !host.root().join(STATE_PATH).exists(),
        "no job state is created, so nothing is recorded against the draft, in particular no owner failure"
    );
    assert!(host.run_records().is_empty(), "no run record is written");
    assert!(host.raised_tasks().is_empty(), "no task is raised");
    let reported = log.at("WARN", "workflow/team-invalid");
    assert_eq!(
        reported.len(),
        1,
        "the unusable team is reported as the vault's configuration error: {:?}",
        log.lines()
    );
    assert!(
        reported[0].contains("file-unparsable"),
        "and the report says it does not parse: {}",
        reported[0]
    );

    host.write(".contextos/team.toml", TEAM)?;
    host.pass(NOW).await;
    assert_eq!(
        host.lifecycle()?,
        "validated",
        "a pass over a valid team validates the ready draft"
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_pass_whose_server_cannot_read_the_team_is_an_outage_and_not_a_team_error() -> Result<(), BoxError> {
    let mut spec = Spec {
        team: Some(TEAM_WITHOUT_CHIEF),
        ..Spec::default()
    };
    let state = enabled_state(&spec);
    spec.state = Some(&state);
    let host = JobVault::open(&spec).await?;
    let (log, _guard) = capture();
    host.pass(NOW).await;
    let started = log.at("WARN", "workflow/team-invalid");
    assert_eq!(
        started.len(),
        1,
        "a pass that reads a team without a chief reports it: {:?}",
        log.lines()
    );
    assert!(started[0].contains("no-chief-of-staff"), "{}", started[0]);

    host.write(".contextos/team.toml", TEAM)?;
    kill_server(&host)?;
    let before = state_bytes(&host)?;
    host.pass(NOW).await;

    assert_eq!(
        state_bytes(&host)?,
        before,
        "nothing is written while the server is down"
    );
    assert_eq!(host.lifecycle()?, "enabled", "nothing is suspended");
    assert_eq!(host.run_records(), Vec::new());
    assert_eq!(host.raised_tasks(), Vec::new());
    assert_eq!(
        log.at("WARN", "workflow/team-invalid").len(),
        1,
        "an unreachable server records no new team problem: {:?}",
        log.lines()
    );
    assert!(
        log.at("INFO", "workflow/team-invalid").is_empty(),
        "and does not say the earlier one cleared: {:?}",
        log.lines()
    );
    Ok(())
}

#[tokio::test]
async fn of_the_ticks_missed_while_the_team_was_invalid_only_the_latest_runs_once_after_it_is_corrected()
-> Result<(), BoxError> {
    let mut spec = Spec {
        team: Some(TEAM_WITH_TWO_CHIEFS),
        ..Spec::default()
    };
    // Hourly, last fired at midnight: the 01:00, 02:00, and 03:00 ticks are all behind it at half past three.
    let state = format!(
        "{}last_scheduled_fire = \"2026-10-01T00:00:00Z\"\n",
        enabled_state(&spec).replace("* * * * *", "0 * * * *")
    );
    spec.state = Some(&state);
    let host = JobVault::open(&spec).await?;
    let half_past_three = datetime!(2026-10-01 03:30:30 UTC);
    let before = state_bytes(&host)?;

    host.pass(half_past_three).await;

    assert!(host.run_records().is_empty(), "nothing runs while the team is invalid");
    assert_eq!(state_bytes(&host)?, before);

    host.write(".contextos/team.toml", TEAM)?;
    host.pass(half_past_three).await;
    host.pass(datetime!(2026-10-01 03:31:30 UTC)).await;
    host.pass(datetime!(2026-10-01 03:45:00 UTC)).await;

    assert_eq!(
        host.run_records().len(),
        1,
        "the missed ticks run once, and not one more on each later pass"
    );
    let slot = contextos_web::vault_runs::claim_slot(&host.host, VAULT_NAME, "ops", "sync", "nightly")?;
    assert_eq!(
        slot.last_fire()?,
        Some(datetime!(2026-10-01 03:00:00 UTC)),
        "the tick that ran is the latest one that was missed"
    );
    Ok(())
}

#[tokio::test]
async fn a_team_error_is_logged_when_it_starts_and_when_it_clears_and_not_on_every_pass() -> Result<(), BoxError> {
    let spec = Spec {
        team: Some(TEAM_WITH_TWO_CHIEFS),
        ..Spec::default()
    };
    let host = JobVault::open(&spec).await?;
    let (log, _guard) = capture();

    for seconds in [0, 30, 60] {
        host.pass(NOW + time::Duration::seconds(seconds)).await;
    }
    let started = log.at("WARN", "workflow/team-invalid");
    assert_eq!(
        started.len(),
        1,
        "three passes over the same problem log it once: {:?}",
        log.lines()
    );
    assert!(started[0].contains("several-chiefs-of-staff"), "{}", started[0]);
    assert!(
        log.at("INFO", "workflow/team-invalid").is_empty(),
        "nothing has cleared yet"
    );

    host.write(".contextos/team.toml", TEAM)?;
    host.pass(NOW + time::Duration::seconds(90)).await;
    host.pass(NOW + time::Duration::seconds(120)).await;

    assert_eq!(
        log.at("INFO", "workflow/team-invalid").len(),
        1,
        "the problem is logged again only when it clears: {:?}",
        log.lines()
    );
    assert_eq!(
        log.at("WARN", "workflow/team-invalid").len(),
        1,
        "and not started again"
    );
    Ok(())
}

#[tokio::test]
async fn a_live_run_by_hand_while_the_team_is_invalid_is_refused_with_team_invalid_before_the_claim_and_a_dry_run_goes_ahead()
-> Result<(), BoxError> {
    let mut spec = Spec {
        team: Some(TEAM_WITHOUT_CHIEF),
        script: TOLERANT_SCRIPT,
        ..Spec::default()
    };
    let state = lifecycle_state(&spec, "registered");
    spec.state = Some(&state);
    let host = JobVault::open(&spec).await?;
    host.write("registry/workflows/ops/sync/tests/offline.toml", ERROR_SCENARIO)?;
    let before = state_bytes(&host)?;
    let live = || CliRunOptions {
        dry_run_scenario: None,
        record: true,
    };

    {
        // The claim is held, so a refusal that came after taking it would be "busy" and not about the team.
        let slot = contextos_web::vault_runs::claim_slot(&host.host, VAULT_NAME, "ops", "sync", "nightly")?;
        let _held = contextos_web::run_claim::RunClaim::acquire(&slot)?;
        let refused = run_job(
            Arc::clone(&host.clients),
            &host.host,
            VAULT_NAME,
            "ops",
            "sync",
            "nightly",
            live(),
        )
        .await;
        match refused {
            Err(CliRunError::TeamInvalid { rule }) => assert_eq!(rule, TeamRule::NoChiefOfStaff),
            other => return Err(format!("expected the team-invalid refusal, got {other:?}").into()),
        }
    }
    assert!(host.run_records().is_empty(), "the refusal wrote no record");
    assert_eq!(state_bytes(&host)?, before, "and changed nothing in the state");

    let dry = CliRunOptions {
        dry_run_scenario: Some(None),
        record: false,
    };
    run_job(
        Arc::clone(&host.clients),
        &host.host,
        VAULT_NAME,
        "ops",
        "sync",
        "nightly",
        dry,
    )
    .await
    .map_err(|error| format!("a dry run does not depend on the team, but was refused: {error}"))?;

    host.write(".contextos/team.toml", TEAM)?;
    run_job(
        Arc::clone(&host.clients),
        &host.host,
        VAULT_NAME,
        "ops",
        "sync",
        "nightly",
        live(),
    )
    .await
    .map_err(|error| format!("a live run with a valid team is allowed: {error}"))?;
    let records = host.run_records();
    assert_eq!(records.len(), 1, "{records:?}");
    assert!(
        records[0].0.ends_with(format!("R-{}.md", 2)),
        "the dry run took the first run number and the refused attempt took none, so this is the second: {:?}",
        records[0].0
    );
    Ok(())
}

async fn workflows_page(router: &Router, path: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(Request::builder().method("GET").uri(path).body(Body::empty())?)
        .await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
}

#[tokio::test]
async fn a_live_run_started_from_the_job_page_while_the_team_is_invalid_is_refused_with_team_invalid_and_takes_no_run_number()
-> Result<(), BoxError> {
    let mut spec = Spec {
        team: Some(TEAM_WITHOUT_CHIEF),
        ..Spec::default()
    };
    let state = lifecycle_state(&spec, "registered");
    spec.state = Some(&state);
    let host = JobVault::open(&spec).await?;
    let before = state_bytes(&host)?;
    let router = contextos_web::build_router(
        Arc::clone(&host.clients),
        Some(host.config.path()),
        &host.config.path().join("web.toml"),
    );

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/{VAULT_NAME}/workflows/ops/sync/jobs/nightly/runs"))
                .body(Body::empty())?,
        )
        .await?;
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    let body = String::from_utf8_lossy(&bytes);

    assert!(
        body.contains("workflow/team-invalid"),
        "the refusal names its stable code:\n{body}"
    );
    assert!(host.run_records().is_empty(), "a refused run leaves no record");
    assert_eq!(state_bytes(&host)?, before, "and changes no state");
    assert!(
        !host.root().join("memory/jobs/ops/sync/nightly").exists(),
        "no run number was taken"
    );
    Ok(())
}

#[tokio::test]
async fn a_vault_with_no_workflows_and_no_team_is_not_paused_and_logs_nothing() -> Result<(), BoxError> {
    let host = JobVault::open(&Spec {
        team: None,
        ..Spec::default()
    })
    .await?;
    std::fs::remove_dir_all(host.root().join("registry/workflows"))?;
    let (log, _guard) = capture();

    host.pass(NOW).await;

    assert!(
        log.at("WARN", "workflow/team-invalid").is_empty(),
        "a vault with no jobs needs no team, so an absent one is no problem: {:?}",
        log.lines()
    );
    assert!(
        log.at("INFO", "workflow/team-invalid").is_empty(),
        "and nothing is logged: {:?}",
        log.lines()
    );

    // The same vault gaining a job needs its team, so the quiet pass above was the absence of jobs and not of the
    // check.
    host.write("registry/workflows/ops/sync/workflow.toml", support::jobs::WORKFLOW)?;
    host.write("registry/workflows/ops/sync/sync.rhai", SCRIPT)?;
    host.write("registry/workflows/ops/sync/jobs/nightly.toml", READY_JOB)?;
    host.pass(NOW).await;
    let paused = log.at("WARN", "workflow/team-invalid");
    assert_eq!(
        paused.len(),
        1,
        "a vault with a job and no team is paused for it: {:?}",
        log.lines()
    );
    assert!(paused[0].contains("file-absent"), "{}", paused[0]);
    Ok(())
}

#[tokio::test]
async fn the_workflows_page_shows_the_team_error_and_says_scheduled_jobs_are_paused() -> Result<(), BoxError> {
    let spec = Spec {
        script: SCRIPT,
        ..Spec::default()
    };
    let host = JobVault::open(&spec).await?;
    let router = contextos_web::build_router(
        Arc::clone(&host.clients),
        Some(host.config.path()),
        &host.config.path().join("web.toml"),
    );
    let list = format!("/{VAULT_NAME}/workflows");
    let job = format!("/{VAULT_NAME}/workflows/ops/sync/jobs/nightly");

    for path in [&list, &job] {
        let (status, page) = workflows_page(&router, path).await?;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(
            !page.contains("workflow/team-invalid") && !page.contains("scheduled jobs in this vault are paused"),
            "a valid team shows no notice on {path}"
        );
    }

    host.write(".contextos/team.toml", TEAM_WITHOUT_CHIEF)?;
    for path in [&list, &job] {
        let (status, page) = workflows_page(&router, path).await?;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(page.contains("workflow/team-invalid"), "{path} names the code: {page}");
        assert!(
            page.contains("no-chief-of-staff"),
            "{path} names the rule that is broken"
        );
        assert!(
            page.contains("scheduled jobs in this vault are paused"),
            "{path} says what the error means for the schedule"
        );
    }
    Ok(())
}
