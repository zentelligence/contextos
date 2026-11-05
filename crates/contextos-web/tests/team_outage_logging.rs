//! What a scheduler pass says in its log about the vault's team: a problem is a `warn` when it starts and an `info`
//! when it clears, never a line per pass, and each line carries the stable code and the vault's name and nothing from
//! the team file. Real `contextos` sessions over real temporary vaults, with the log captured.

mod support;

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::Request;
use support::BoxError;
use support::jobs::{JobVault, READY_JOB, SCRIPT, Spec, TEAM, TEAM_WITH_TWO_CHIEFS, VAULT_NAME};
use support::logs::capture;
use time::OffsetDateTime;
use time::macros::datetime;
use tower::ServiceExt as _;

const NOW: OffsetDateTime = datetime!(2026-10-01 03:00:30 UTC);

const EMPTY_TEAM: &str = "# nobody has joined yet\n";

#[tokio::test]
async fn a_team_error_is_one_warning_with_its_code_and_vault_when_it_starts_and_one_info_with_its_code_when_it_clears()
-> Result<(), BoxError> {
    let host = JobVault::open(&Spec {
        team: Some(TEAM_WITH_TWO_CHIEFS),
        ..Spec::default()
    })
    .await?;
    let (log, _guard) = capture();

    for seconds in [0, 30, 60] {
        host.pass(NOW + time::Duration::seconds(seconds)).await;
    }
    let started = log.at("WARN", "workflow/team-invalid");
    assert_eq!(started.len(), 1, "one warning for three passes: {:?}", log.lines());
    assert!(started[0].contains(VAULT_NAME), "{}", started[0]);
    assert!(started[0].contains("several-chiefs-of-staff"), "{}", started[0]);

    host.write(".contextos/team.toml", TEAM)?;
    host.pass(NOW + time::Duration::seconds(90)).await;
    host.pass(NOW + time::Duration::seconds(120)).await;

    let cleared = log.at("INFO", "workflow/team-invalid");
    assert_eq!(cleared.len(), 1, "one info when it clears: {:?}", log.lines());
    assert!(cleared[0].contains(VAULT_NAME), "{}", cleared[0]);
    Ok(())
}

#[tokio::test]
async fn an_empty_team_file_is_reported_as_the_empty_team_rule() -> Result<(), BoxError> {
    let host = JobVault::open(&Spec {
        team: Some(EMPTY_TEAM),
        ..Spec::default()
    })
    .await?;
    let (log, _guard) = capture();

    host.pass(NOW).await;

    let started = log.at("WARN", "workflow/team-invalid");
    assert_eq!(started.len(), 1, "{:?}", log.lines());
    assert!(started[0].contains("team-empty"), "{}", started[0]);
    assert!(!started[0].contains("no-chief-of-staff"), "{}", started[0]);
    Ok(())
}

/// Makes `.contextos/team.toml` unreadable to this process, or reports that it cannot be (a process that ignores file
/// permissions, such as root, reads it anyway).
#[cfg(unix)]
fn make_team_unreadable(host: &JobVault) -> Result<bool, BoxError> {
    use std::os::unix::fs::PermissionsExt as _;
    let path = host.root().join(".contextos/team.toml");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000))?;
    Ok(std::fs::read(&path).is_err())
}

#[cfg(unix)]
fn make_team_readable(host: &JobVault) -> Result<(), BoxError> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(
        host.root().join(".contextos/team.toml"),
        std::fs::Permissions::from_mode(0o644),
    )?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_team_that_cannot_be_read_is_one_warning_with_the_server_unavailable_code_and_one_info_when_it_clears()
-> Result<(), BoxError> {
    let host = JobVault::open(&Spec::default()).await?;
    if !make_team_unreadable(&host)? {
        // The outage cannot be made by a process that ignores file permissions, so there is nothing to observe.
        return Ok(());
    }
    let (log, _guard) = capture();

    for seconds in [0, 30, 60] {
        host.pass(NOW + time::Duration::seconds(seconds)).await;
    }
    let started = log.at("WARN", "workflow/server-unavailable");
    assert_eq!(started.len(), 1, "one warning for three passes: {:?}", log.lines());
    assert!(started[0].contains(VAULT_NAME), "{}", started[0]);
    assert!(
        log.at("WARN", "workflow/team-invalid").is_empty(),
        "an outage is not a team error"
    );

    make_team_readable(&host)?;
    host.pass(NOW + time::Duration::seconds(90)).await;
    host.pass(NOW + time::Duration::seconds(120)).await;

    let cleared = log.at("INFO", "workflow/server-unavailable");
    assert_eq!(cleared.len(), 1, "one info when it clears: {:?}", log.lines());
    assert!(cleared[0].contains(VAULT_NAME), "{}", cleared[0]);
    Ok(())
}

#[tokio::test]
async fn a_vault_with_a_workflow_but_no_saved_job_and_no_team_file_is_not_paused_and_says_nothing()
-> Result<(), BoxError> {
    let host = JobVault::open(&Spec {
        team: None,
        ..Spec::default()
    })
    .await?;
    std::fs::remove_file(host.root().join("registry/workflows/ops/sync/jobs/nightly.toml"))?;
    let (log, _guard) = capture();

    host.pass(NOW).await;
    host.pass(NOW + time::Duration::seconds(30)).await;

    assert!(
        log.at("WARN", "workflow/team-invalid").is_empty(),
        "a team is needed once there is a job, not a workflow: {:?}",
        log.lines()
    );
    let router = contextos_web::build_router(
        Arc::clone(&host.clients),
        Some(host.config.path()),
        &host.config.path().join("web.toml"),
    );
    let response = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/{VAULT_NAME}/workflows"))
                .body(Body::empty())?,
        )
        .await?;
    let page = String::from_utf8_lossy(&to_bytes(response.into_body(), usize::MAX).await?).into_owned();
    assert!(
        !page.contains("workflow/team-invalid") && !page.contains("are paused"),
        "the workflows page shows no team notice: {page}"
    );

    // With a job the same vault needs its team, so the quiet above was the absence of a job and not of the check.
    host.write("registry/workflows/ops/sync/jobs/nightly.toml", READY_JOB)?;
    host.write("registry/workflows/ops/sync/sync.rhai", SCRIPT)?;
    host.pass(NOW + time::Duration::seconds(60)).await;
    assert_eq!(log.at("WARN", "workflow/team-invalid").len(), 1, "{:?}", log.lines());
    Ok(())
}
