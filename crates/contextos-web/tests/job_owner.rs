//! The owner gate over a real `contextos-mcp` session and a real temporary vault, with the clock fixed: a vault job
//! whose owner is missing or has left the team never runs live, the system suspends it when it is enabled, a dry run
//! stays allowed, and the owner coming back changes nothing until the operator resumes the job.

mod support;

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use contextos_core::{DeclaredOwner, OwnerStanding};
use contextos_web::McpClient;
use contextos_web::cli_run::{CliRunError, CliRunOptions};
use contextos_web::discover_jobs;
use contextos_web::job_operation::{OperationError, OperationOutcome, OperatorRequest, perform};
use contextos_web::job_operation_vault::VaultJobStore;
use contextos_web::job_validation::validate_now;
use contextos_web::scheduler::start_due_jobs;
use contextos_workflows::{Actor, JobState, RULES_REVISION, RunOutcome};
use support::BoxError;
use support::jobs::{
    FixedClock, JobVault, STATE_PATH, Spec, TEAM, TEAM_WITHOUT_VELA, VAULT_NAME, current_state, enabled_state,
    frontmatter_field, legacy_enabled_state, roster_of, suspended_state,
};
use time::OffsetDateTime;
use time::macros::datetime;
use tower::ServiceExt as _;

/// 03:00:30, half a minute after a job scheduled every minute fell due, inside the scheduler's look-back window.
const NOW: OffsetDateTime = datetime!(2026-10-01 03:00:30 UTC);
const READY_JOB: &str = support::jobs::READY_JOB;
/// A job file that declares no owner.
const JOB_WITHOUT_OWNER: &str = "ready = true\nname = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";
/// A job file whose owner differs from the member `vela` only in case.
const JOB_WITH_CAPITALISED_OWNER: &str =
    "ready = true\nname = \"Nightly\"\nowner = \"Vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";
const CODE_OWNER_UNRESOLVED: &str = "workflow/owner-unresolved";

/// A vault whose team is `team`, whose job file is `job`, and whose stored state is what `state` makes of those files.
async fn open_with(team: &str, job: &str, state: impl Fn(&Spec<'_>) -> String) -> Result<JobVault, BoxError> {
    let mut spec = Spec {
        team: Some(team),
        job,
        ..Spec::default()
    };
    let stored = state(&spec);
    spec.state = Some(&stored);
    JobVault::open(&spec).await
}

fn primary(vault: &JobVault) -> Result<Arc<McpClient>, BoxError> {
    Ok(vault.clients.get("contextos").ok_or("no primary server")?)
}

async fn ask(
    vault: &JobVault,
    request: OperatorRequest,
    now: OffsetDateTime,
) -> Result<OperationOutcome, OperationError> {
    let client = vault
        .clients
        .get("contextos")
        .ok_or_else(|| OperationError::Unavailable("no primary server".to_owned()))?;
    let servers = vec!["contextos".to_owned()];
    let store = VaultJobStore {
        client: &client,
        vault_name: VAULT_NAME,
        known_mcp_servers: &servers,
        package: "ops",
        workflow_slug: "sync",
        job_slug: "nightly",
        claims_root: None,
    };
    perform(&store, request, now).await
}

async fn run_live(vault: &JobVault) -> Result<RunOutcome, CliRunError> {
    support::run_job(
        Arc::clone(&vault.clients),
        &vault.host,
        VAULT_NAME,
        "ops",
        "sync",
        "nightly",
        CliRunOptions {
            dry_run_scenario: None,
            record: true,
        },
    )
    .await
}

fn suspension_of(vault: &JobVault) -> Result<contextos_workflows::SuspensionRecord, BoxError> {
    let state = vault.state()?;
    Ok(state
        .job("ops", "sync", "nightly")
        .and_then(JobState::suspension)
        .cloned()
        .ok_or("the job has no suspension record")?)
}

#[tokio::test]
async fn an_enabled_job_whose_owner_left_the_team_is_suspended_by_the_system_with_owner_unresolved()
-> Result<(), BoxError> {
    let vault = open_with(TEAM_WITHOUT_VELA, READY_JOB, enabled_state).await?;

    vault.pass(NOW).await;

    assert_eq!(vault.lifecycle()?, "suspended");
    let record = suspension_of(&vault)?;
    assert_eq!(record.code, CODE_OWNER_UNRESOLVED);
    assert_eq!(record.by, Some(Actor::System));
    assert_eq!(record.at, Some(NOW));
    assert!(vault.run_records().is_empty(), "no run was started");

    vault.pass(NOW + time::Duration::minutes(1)).await;
    assert_eq!(
        suspension_of(&vault)?,
        record,
        "a later pass leaves the record, its time included, as it was"
    );
    Ok(())
}

#[tokio::test]
async fn an_enabled_job_with_no_owner_is_suspended_on_the_first_pass_of_this_release() -> Result<(), BoxError> {
    let vault = open_with(TEAM, JOB_WITHOUT_OWNER, legacy_enabled_state).await?;

    vault.pass(NOW).await;

    assert_eq!(vault.lifecycle()?, "suspended");
    let record = suspension_of(&vault)?;
    assert_eq!(record.code, CODE_OWNER_UNRESOLVED);
    assert_eq!(record.by, Some(Actor::System));
    assert!(vault.run_records().is_empty(), "no run was started");
    let tasks = vault.raised_tasks_for("suspended");
    let [(_, task)] = tasks.as_slice() else {
        return Err(format!("expected exactly one suspended task, found {}", tasks.len()).into());
    };
    assert_eq!(
        frontmatter_field(task, "assignee").as_deref(),
        Some("atlas"),
        "a job with no owner goes to the chief of staff: {task}"
    );
    Ok(())
}

#[tokio::test]
async fn a_live_run_by_hand_of_a_job_whose_owner_left_is_refused_before_the_claim_with_no_run_number_or_record()
-> Result<(), BoxError> {
    let vault = open_with(TEAM_WITHOUT_VELA, READY_JOB, |spec| current_state(spec, "registered")).await?;

    {
        // The claim is held, so a refusal that came after taking it would be "busy" and not about the owner.
        let slot = contextos_web::vault_runs::claim_slot(&vault.host, VAULT_NAME, "ops", "sync", "nightly")?;
        let _held = contextos_web::run_claim::RunClaim::acquire(&slot)?;
        match run_live(&vault).await {
            Err(CliRunError::OwnerUnresolved { owner }) => {
                assert_eq!(owner.as_ref().map(DeclaredOwner::as_str), Some("vela"));
            }
            other => return Err(format!("expected the owner to be unresolved, got {other:?}").into()),
        }
    }
    assert!(vault.run_records().is_empty(), "a refused run leaves no record");

    // The owner comes back: the refused call took no run number, so this is the job's first run.
    vault.write(".contextos/team.toml", TEAM)?;
    assert_eq!(run_live(&vault).await?, RunOutcome::Succeeded);
    let records = vault.run_records();
    assert_eq!(records.len(), 1);
    assert!(
        records[0].0.file_name().and_then(|name| name.to_str()) == Some("R-1.md"),
        "the first run to be started is R-1: {:?}",
        records[0].0
    );
    Ok(())
}

#[tokio::test]
async fn a_refused_manual_run_of_an_enabled_job_still_on_trial_leaves_the_trial_pending_and_the_job_enabled()
-> Result<(), BoxError> {
    let vault = open_with(TEAM_WITHOUT_VELA, READY_JOB, |spec| {
        format!("{}first_run = true\n", enabled_state(spec))
    })
    .await?;
    let before = vault.read(STATE_PATH)?;
    assert!(
        vault
            .state()?
            .job("ops", "sync", "nightly")
            .is_some_and(JobState::first_run_pending),
        "the fixture job is on trial"
    );

    match run_live(&vault).await {
        Err(CliRunError::OwnerUnresolved { .. }) => {}
        other => return Err(format!("expected the owner to be unresolved, got {other:?}").into()),
    }

    assert_eq!(vault.lifecycle()?, "enabled", "a refusal does not suspend the job");
    assert!(
        vault
            .state()?
            .job("ops", "sync", "nightly")
            .is_some_and(JobState::first_run_pending),
        "a run that never started cannot settle the trial"
    );
    assert_eq!(vault.read(STATE_PATH)?, before, "the state is as it was");
    Ok(())
}

#[tokio::test]
async fn a_live_run_by_hand_of_a_job_with_no_owner_is_refused_naming_no_owner() -> Result<(), BoxError> {
    let vault = open_with(TEAM, JOB_WITHOUT_OWNER, |spec| current_state(spec, "registered")).await?;

    match run_live(&vault).await {
        Err(CliRunError::OwnerUnresolved { owner: None }) => {}
        other => return Err(format!("expected an undeclared owner to be refused, got {other:?}").into()),
    }
    assert!(vault.run_records().is_empty(), "a refused run leaves no record");
    Ok(())
}

#[tokio::test]
async fn a_due_tick_of_a_job_whose_owner_left_is_not_started_and_not_recorded_as_fired() -> Result<(), BoxError> {
    let vault = open_with(TEAM_WITHOUT_VELA, READY_JOB, enabled_state).await?;
    let clock = FixedClock(NOW);
    let servers = vec!["contextos".to_owned()];

    start_due_jobs(&vault.clients, &vault.host, VAULT_NAME, &clock).await;

    let client = primary(&vault)?;
    let jobs = discover_jobs(&client, VAULT_NAME, &servers).await?;
    let declared: Vec<Option<&str>> = jobs
        .iter()
        .map(|job| job.declared_owner.as_ref().map(DeclaredOwner::as_str))
        .collect();
    assert_eq!(
        declared,
        vec![Some("vela")],
        "the job is still known, with the owner its file declares, so it is the gate that held it back"
    );
    assert!(vault.run_records().is_empty(), "the due job was not started");
    let slot = contextos_web::vault_runs::claim_slot(&vault.host, VAULT_NAME, "ops", "sync", "nightly")?;
    assert_eq!(slot.last_fire()?, None, "the tick was not recorded as fired");

    // The owner returns: the same tick, never recorded as fired, is still due at the same instant.
    vault.write(".contextos/team.toml", TEAM)?;
    start_due_jobs(&vault.clients, &vault.host, VAULT_NAME, &clock).await;
    let records = vault.run_records();
    assert_eq!(records.len(), 1, "the tick starts once the owner is a member");
    assert!(records[0].1.contains("trigger: scheduled"), "{}", records[0].1);
    Ok(())
}

#[tokio::test]
async fn a_dry_run_of_a_job_whose_owner_left_is_allowed() -> Result<(), BoxError> {
    let script = "call_mcp(\"contextos\", \"fs_read_text_file\", #{ path: \"does-not-exist.md\" });";
    let mut spec = Spec {
        team: Some(TEAM_WITHOUT_VELA),
        script,
        ..Spec::default()
    };
    let stored = current_state(&spec, "registered");
    spec.state = Some(&stored);
    let vault = JobVault::open(&spec).await?;
    vault.write(
        "registry/workflows/ops/sync/tests/happy-path.toml",
        "[[call]]\nserver = \"contextos\"\ntool = \"fs_read_text_file\"\nkind = \"valid\"\nresponse = { content = \"mocked\" }\n",
    )?;

    let outcome = support::run_job(
        Arc::clone(&vault.clients),
        &vault.host,
        VAULT_NAME,
        "ops",
        "sync",
        "nightly",
        CliRunOptions {
            dry_run_scenario: Some(Some("happy-path".to_owned())),
            record: false,
        },
    )
    .await?;

    assert_eq!(outcome, RunOutcome::Succeeded);
    Ok(())
}

#[tokio::test]
async fn resuming_a_job_whose_owner_is_still_unresolved_is_refused_with_validation_failed() -> Result<(), BoxError> {
    let vault = open_with(TEAM_WITHOUT_VELA, READY_JOB, suspended_state).await?;

    let refused = ask(&vault, OperatorRequest::Resume, NOW).await;

    let Err(error) = refused else {
        return Err("resuming a job with no owner among the team was allowed".into());
    };
    assert_eq!(error.code(), "workflow/validation-failed");
    assert_eq!(vault.lifecycle()?, "suspended");
    let state = vault.state()?;
    let stored = state
        .job("ops", "sync", "nightly")
        .and_then(JobState::validation)
        .ok_or("the refused resume stored no validation")?;
    assert!(!stored.passed);
    assert!(
        stored
            .failures
            .iter()
            .any(|failure| failure.code == "validation/owner-unresolved" && failure.artefact == "nightly.toml"),
        "{:?}",
        stored.failures
    );
    assert_eq!(
        stored.by,
        Actor::Operator,
        "a resume asked for by hand is the operator's validation"
    );
    assert_eq!(stored.rules, RULES_REVISION);
    Ok(())
}

#[tokio::test]
async fn a_job_whose_owner_returns_stays_suspended_until_resumed_and_runs_no_missed_tick() -> Result<(), BoxError> {
    let vault = open_with(TEAM_WITHOUT_VELA, READY_JOB, enabled_state).await?;
    vault.pass(NOW).await;
    assert_eq!(
        vault.lifecycle()?,
        "suspended",
        "the owner left, so the system suspended the job"
    );

    vault.write(".contextos/team.toml", TEAM)?;
    for minutes in 1..=9 {
        vault.pass(NOW + time::Duration::minutes(minutes)).await;
        assert_eq!(
            vault.lifecycle()?,
            "suspended",
            "the owner returning does not resume the job"
        );
        assert!(vault.run_records().is_empty(), "a suspended job runs nothing");
    }

    let resumed_at = NOW + time::Duration::minutes(10);
    ask(&vault, OperatorRequest::Resume, resumed_at).await?;
    assert_eq!(vault.lifecycle()?, "enabled");
    vault.pass(resumed_at + time::Duration::seconds(20)).await;
    assert!(
        vault.run_records().is_empty(),
        "no tick that fell due while the job was suspended is run: the schedule counts from the resume"
    );
    vault.pass(resumed_at + time::Duration::seconds(40)).await;
    assert_eq!(vault.run_records().len(), 1, "the first tick after the resume runs");
    Ok(())
}

#[tokio::test]
async fn a_registered_job_whose_owner_left_keeps_its_state_and_raises_no_task() -> Result<(), BoxError> {
    let vault = open_with(TEAM_WITHOUT_VELA, READY_JOB, |spec| current_state(spec, "registered")).await?;
    let before = vault.read(STATE_PATH)?;

    vault.pass(NOW).await;

    assert_eq!(vault.read(STATE_PATH)?, before, "the state is byte for byte as it was");
    assert_eq!(vault.lifecycle()?, "registered");
    assert!(
        vault.raised_tasks().is_empty(),
        "a job that never runs unattended raises nothing"
    );
    match run_live(&vault).await {
        Err(CliRunError::OwnerUnresolved { owner }) => {
            assert_eq!(owner.as_ref().map(DeclaredOwner::as_str), Some("vela"));
        }
        other => return Err(format!("expected the owner to be unresolved, got {other:?}").into()),
    }
    Ok(())
}

async fn send(router: &Router, method: &str, path: &str) -> Result<(StatusCode, String), BoxError> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("hx-request", "true")
                .body(Body::empty())?,
        )
        .await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
}

#[tokio::test]
async fn a_live_run_started_from_the_job_page_is_refused_with_owner_unresolved_and_takes_no_run_number()
-> Result<(), BoxError> {
    let vault = open_with(TEAM_WITHOUT_VELA, READY_JOB, |spec| current_state(spec, "registered")).await?;
    let before = vault.read(STATE_PATH)?;
    let router = contextos_web::build_router(
        Arc::clone(&vault.clients),
        Some(vault.config.path()),
        &vault.config.path().join("web.toml"),
    );

    let (_, body) = send(
        &router,
        "POST",
        &format!("/{VAULT_NAME}/workflows/ops/sync/jobs/nightly/runs"),
    )
    .await?;

    assert!(
        body.contains(CODE_OWNER_UNRESOLVED),
        "the refusal names its stable code:\n{body}"
    );
    assert!(vault.run_records().is_empty(), "a refused run leaves no record");
    assert_eq!(vault.read(STATE_PATH)?, before, "and changes no state");
    assert!(
        !vault.root().join("memory/jobs/ops/sync/nightly").exists(),
        "no run number was taken"
    );
    Ok(())
}

#[tokio::test]
async fn a_job_whose_owner_left_is_listed_flagged_and_can_be_validated_from_its_page() -> Result<(), BoxError> {
    let vault = open_with(TEAM_WITHOUT_VELA, READY_JOB, |spec| current_state(spec, "registered")).await?;
    let client = primary(&vault)?;
    let servers = vec!["contextos".to_owned()];

    let jobs = discover_jobs(&client, VAULT_NAME, &servers).await?;

    let [job] = jobs.as_slice() else {
        return Err(format!("expected the one job to be discovered, found {}", jobs.len()).into());
    };
    assert_eq!(job.declared_owner.as_ref().map(DeclaredOwner::as_str), Some("vela"));

    let router = contextos_web::build_router(
        Arc::clone(&vault.clients),
        Some(vault.config.path()),
        &vault.config.path().join("web.toml"),
    );
    let page = format!("/{VAULT_NAME}/workflows/ops/sync/jobs/nightly");
    let (status, body) = send(&router, "GET", &page).await?;
    assert_eq!(status, StatusCode::OK);
    let lowered = body.to_lowercase();
    assert!(
        lowered.contains("owner") && (lowered.contains("not a member") || body.contains("validation/owner-unresolved")),
        "the page flags the owner problem:\n{body}"
    );

    let (status, _) = send(&router, "PUT", &format!("{page}/execute/validate")).await?;
    assert_eq!(status, StatusCode::OK);
    let state = vault.state()?;
    let stored = state
        .job("ops", "sync", "nightly")
        .and_then(JobState::validation)
        .ok_or("validating from the page stored nothing")?;
    assert!(
        stored
            .failures
            .iter()
            .any(|failure| failure.code == "validation/owner-unresolved"),
        "{:?}",
        stored.failures
    );
    Ok(())
}

#[tokio::test]
async fn an_owner_matching_only_without_regard_to_case_is_not_a_member() -> Result<(), BoxError> {
    let vault = open_with(TEAM, JOB_WITH_CAPITALISED_OWNER, enabled_state).await?;
    let client = primary(&vault)?;
    let servers = vec!["contextos".to_owned()];

    let jobs = discover_jobs(&client, VAULT_NAME, &servers).await?;
    let [job] = jobs.as_slice() else {
        return Err(format!("expected the one job to be discovered, found {}", jobs.len()).into());
    };
    assert_eq!(
        job.declared_owner,
        Some(DeclaredOwner::Invalid("Vela".to_owned())),
        "the job loads with the text it declared, not lowercased into `vela`"
    );
    let roster = roster_of(TEAM)?;
    assert_eq!(
        job.owner_standing(&roster),
        OwnerStanding::InvalidSlug("Vela".to_owned()),
        "a text that is not slug-shaped can match no member, and says what it declared"
    );

    vault.pass(NOW).await;
    assert_eq!(
        vault.lifecycle()?,
        "suspended",
        "an enabled job whose owner is no member is suspended"
    );
    assert_eq!(suspension_of(&vault)?.code, CODE_OWNER_UNRESOLVED);

    validate_now(
        &client,
        VAULT_NAME,
        &servers,
        contextos_web::jobs::WorkflowFolder {
            package: "ops",
            directory: "sync",
        },
        "nightly.toml",
        NOW,
    )
    .await?;
    let state = vault.state()?;
    let stored = state
        .job("ops", "sync", "nightly")
        .and_then(JobState::validation)
        .ok_or("validation stored nothing")?;
    assert!(
        stored
            .failures
            .iter()
            .any(|failure| failure.code == "validation/owner-unresolved" && failure.message.contains("Vela")),
        "the failure names the declared text: {:?}",
        stored.failures
    );
    Ok(())
}
