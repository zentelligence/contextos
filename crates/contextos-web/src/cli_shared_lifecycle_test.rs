//! The lifecycle gate, probation, and dry runs of a saved shared job, and a one-off dry run: the second half of the
//! shared command-line tests, which share their fixture with `cli_shared_test.rs`.

use super::*;
use crate::shared_ledger::RunStart;

// ---------------------------------------------------------------------
// The lifecycle gate
// ---------------------------------------------------------------------

fn scheduled() -> SharedRunOptions {
    SharedRunOptions {
        trigger: RunTrigger::Scheduled,
        dry_run_scenario: None,
    }
}

fn dry(scenario: Option<&str>) -> SharedRunOptions {
    SharedRunOptions {
        trigger: RunTrigger::Manual,
        dry_run_scenario: Some(scenario.map(str::to_owned)),
    }
}

#[tokio::test]
async fn a_live_run_by_hand_needs_the_job_registered_enabled_or_suspended() -> Result<(), Box<dyn std::error::Error>> {
    for (stage, allowed) in [
        (None, false),
        (Some(Stage::Validated), false),
        (Some(Stage::Registered), true),
        (Some(Stage::Enabled), true),
        (Some(Stage::Suspended), true),
        (Some(Stage::Retired), false),
    ] {
        let mut host = Host::new(EFFECT, CALLING_SCRIPT)?;
        host.register_jobs = false;
        host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
        if let Some(stage) = stage {
            host.bring_to(stage)?;
        }

        let result = host.run_job().await;

        if allowed {
            assert!(matches!(result, Ok(RunOutcome::Succeeded)), "{result:?}");
        } else {
            assert!(matches!(result, Err(CliSharedError::NotRunnable { .. })), "{result:?}");
            assert_eq!(host.calls(), 0);
            assert!(host.record(1).is_err(), "a refused run leaves no record");
        }
    }
    Ok(())
}

#[tokio::test]
async fn the_refusal_names_the_state_and_carries_the_stable_code() -> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.register_jobs = false;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;

    let refused = host.run_job().await;

    let message = refused.as_ref().err().map(ToString::to_string).unwrap_or_default();
    assert!(message.starts_with("workflow/job-not-runnable"), "{message}");
    assert!(message.contains("draft"), "{message}");
    Ok(())
}

#[tokio::test]
async fn the_refusal_of_a_suspended_job_carries_its_suspension_record() -> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.register_jobs = false;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    host.bring_to(Stage::Suspended)?;

    let refused = host.run_with(scheduled()).await;

    let message = refused.as_ref().err().map(ToString::to_string).unwrap_or_default();
    assert!(message.starts_with("workflow/job-not-runnable"), "{message}");
    assert!(message.contains("suspended"), "{message}");
    assert!(
        message.contains("stopped by the operator"),
        "the refusal says why the job was suspended: {message}"
    );
    assert!(message.contains("operator"), "and who suspended it: {message}");
    Ok(())
}

#[tokio::test]
async fn the_state_refusal_comes_before_the_busy_check() -> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.register_jobs = false;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    let _held = SharedClaim::acquire(host.claims.path(), &Host::key())?;

    assert!(matches!(host.run_job().await, Err(CliSharedError::NotRunnable { .. })));
    Ok(())
}

#[tokio::test]
async fn only_an_enabled_job_runs_when_the_scheduler_ticks() -> Result<(), Box<dyn std::error::Error>> {
    for (stage, allowed) in [
        (Stage::Registered, false),
        (Stage::Suspended, false),
        (Stage::Retired, false),
        (Stage::Enabled, true),
    ] {
        let mut host = Host::new(EFFECT, CALLING_SCRIPT)?;
        host.register_jobs = false;
        host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
        host.bring_to(stage)?;

        let result = host.run_with(scheduled()).await;

        assert_eq!(result.is_ok(), allowed, "{result:?}");
    }
    Ok(())
}

#[tokio::test]
async fn a_scheduled_run_is_recorded_as_scheduled() -> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.register_jobs = false;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    host.bring_to(Stage::Enabled)?;

    host.run_with(scheduled()).await?;

    assert!(host.record(1)?.contains("trigger: scheduled\n"));
    Ok(())
}

#[tokio::test]
async fn a_scheduled_tick_that_finds_the_claim_held_is_recorded_skipped_and_runs_nothing()
-> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.register_jobs = false;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    host.bring_to(Stage::Enabled)?;
    let _held = SharedClaim::acquire(host.claims.path(), &Host::key())?;

    let outcome = host.run_with(scheduled()).await?;

    assert!(failed_with(&outcome, WorkflowFailureKind::JobBusy), "{outcome:?}");
    assert_eq!(host.calls(), 0);
    let record = host.record(1)?;
    assert!(record.contains("status: skipped\n"), "{record}");
    assert!(record.contains("trigger: scheduled\n"), "{record}");
    assert!(host.on_trial()?, "a tick that did not run does not settle the trial");
    Ok(())
}

#[tokio::test]
async fn a_run_by_hand_that_finds_the_claim_held_leaves_no_record() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    let _held = SharedClaim::acquire(host.claims.path(), &Host::key())?;

    host.run_job().await?;

    assert!(host.record(1).is_err());
    Ok(())
}

// ---------------------------------------------------------------------
// Probation
// ---------------------------------------------------------------------

const FAILING_SCRIPT: &str = "throw \"the crm is down\";";

#[tokio::test]
async fn a_first_live_run_that_succeeds_ends_the_trial() -> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.register_jobs = false;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    host.bring_to(Stage::Enabled)?;
    assert!(host.on_trial()?);

    host.run_job().await?;

    assert_eq!(host.lifecycle()?, JobLifecycle::Enabled);
    assert!(!host.on_trial()?);
    Ok(())
}

#[tokio::test]
async fn a_first_live_run_that_fails_suspends_the_job_with_the_failure() -> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(EFFECT, FAILING_SCRIPT)?;
    host.register_jobs = false;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    host.bring_to(Stage::Enabled)?;

    let outcome = host.run_with(scheduled()).await?;

    assert!(matches!(outcome, RunOutcome::Failed(_)), "{outcome:?}");
    assert_eq!(host.lifecycle()?, JobLifecycle::Suspended);
    let state = host.state.read()?;
    let reason = state
        .job("acme", "sync", "nightly")
        .and_then(contextos_workflows::JobState::suspended_reason);
    assert!(
        reason.is_some_and(|reason| reason.contains("the crm is down")),
        "{reason:?}"
    );
    assert!(host.on_trial()?, "a fixed job is still on trial when it is resumed");
    Ok(())
}

#[tokio::test]
async fn a_first_live_run_refused_because_a_vault_is_gone_leaves_the_job_enabled_and_on_trial()
-> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(
        &format!("{EFFECT}\n[[workflow.inputs]]\nname = \"target\"\nkind = \"vault\"\n"),
        CALLING_SCRIPT,
    )?;
    host.register_jobs = false;
    // The vault was configured when the job was accepted, and the primary server no longer reports it.
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n[inputs]\ntarget = \"client-z\"\n")?;
    host.bring_to(Stage::Enabled)?;

    let outcome = host.run_job().await?;

    assert!(
        failed_with(&outcome, WorkflowFailureKind::VaultUnavailable),
        "{outcome:?}"
    );
    assert_eq!(
        host.lifecycle()?,
        JobLifecycle::Enabled,
        "a run whose script never started says nothing about the job, so it is not suspended"
    );
    assert!(host.on_trial()?, "the first live run of the version is still to come");
    Ok(())
}

#[tokio::test]
async fn a_first_live_run_refused_because_the_primary_server_is_not_connected_leaves_the_job_enabled_and_on_trial()
-> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(
        &format!("{EFFECT}\n[[workflow.inputs]]\nname = \"target\"\nkind = \"vault\"\n"),
        CALLING_SCRIPT,
    )?;
    host.register_jobs = false;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n[inputs]\ntarget = \"client-z\"\n")?;
    host.bring_to(Stage::Enabled)?;
    // The primary server is configured, but nothing answers at its endpoint.
    let clients = Arc::new(
        McpClientSet::connect(&[crate::config::McpServerConfig::Http {
            name: "contextos".to_owned(),
            endpoint: crate::test_support::unreachable_loopback_url()?,
            token_env: None,
            auth: None,
            headers: std::collections::BTreeMap::new(),
        }])
        .await?,
    );

    let report =
        run_shared_job_reported(host.host(clients), Host::key(), SharedRunOptions::default(), Vec::new()).await?;

    assert!(
        failed_with(&report.outcome, WorkflowFailureKind::ServerUnavailable),
        "{:?}",
        report.outcome
    );
    assert!(!report.started);
    assert_eq!(
        host.lifecycle()?,
        JobLifecycle::Enabled,
        "an outage is never judged as a defect of the job"
    );
    assert!(host.on_trial()?, "the first live run of the version is still to come");
    Ok(())
}

#[tokio::test]
async fn a_first_live_run_refused_because_its_release_is_uninstalled_leaves_the_job_enabled_and_on_trial()
-> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.register_jobs = false;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    host.bring_to(Stage::Enabled)?;
    host.uninstall()?;

    let outcome = host.run_with(scheduled()).await?;

    assert!(
        failed_with(&outcome, WorkflowFailureKind::PackageUnavailable),
        "{outcome:?}"
    );
    assert_eq!(host.lifecycle()?, JobLifecycle::Enabled);
    assert!(host.on_trial()?, "a missing release is not a defect of the job");
    Ok(())
}

#[tokio::test]
async fn a_run_whose_version_cannot_be_read_decides_nothing_about_the_trial() -> Result<(), Box<dyn std::error::Error>>
{
    let mut host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.register_jobs = false;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    host.bring_to(Stage::Enabled)?;
    // The package the job runs is gone, so the run ends before any claim or call and has no version to be settled against.
    host.uninstall()?;

    let outcome = host.run_job().await?;

    assert!(
        failed_with(&outcome, WorkflowFailureKind::PackageUnavailable),
        "{outcome:?}"
    );
    assert_eq!(
        host.lifecycle()?,
        JobLifecycle::Enabled,
        "the validation pass is what suspends a job whose package is gone"
    );
    assert!(host.on_trial()?);
    Ok(())
}

#[tokio::test]
async fn a_run_that_found_the_claim_held_does_not_settle_the_trial() -> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(EFFECT, FAILING_SCRIPT)?;
    host.register_jobs = false;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    host.bring_to(Stage::Enabled)?;
    let _held = SharedClaim::acquire(host.claims.path(), &Host::key())?;

    host.run_job().await?;

    assert_eq!(host.lifecycle()?, JobLifecycle::Enabled);
    Ok(())
}

#[tokio::test]
async fn a_manual_run_of_a_registered_job_changes_nothing_whether_it_succeeds_or_fails()
-> Result<(), Box<dyn std::error::Error>> {
    for script in [CALLING_SCRIPT, FAILING_SCRIPT] {
        let host = Host::new(EFFECT, script)?;
        host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
        let before = std::fs::read_to_string(host.state.state_path())?;

        host.run_job().await?;

        assert_eq!(host.lifecycle()?, JobLifecycle::Registered, "{script}");
        assert_eq!(std::fs::read_to_string(host.state.state_path())?, before, "{script}");
    }
    Ok(())
}

#[tokio::test]
async fn a_job_whose_trial_is_over_is_not_suspended_when_a_later_run_fails() -> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(EFFECT, FAILING_SCRIPT)?;
    host.register_jobs = false;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    host.bring_to(Stage::EnabledProven)?;

    host.run_job().await?;

    assert_eq!(host.lifecycle()?, JobLifecycle::Enabled);
    Ok(())
}

// ---------------------------------------------------------------------
// Dry runs
// ---------------------------------------------------------------------

/// A script that makes the one call its scenario declares and recovers from the scenario's error, so the run ends with a
/// warning and completes, with no server needed to supply a schema.
const TRY_SCRIPT: &str =
    r#"try { call_mcp("contextos", "vault_info", #{}); } catch (e) { flag_degraded("the read failed"); }"#;
const ERROR_SCENARIO: &str =
    "[[call]]\nserver = \"contextos\"\ntool = \"vault_info\"\nkind = \"error\"\nerror = \"offline\"\n";

#[tokio::test]
async fn a_dry_run_that_completes_registers_a_validated_job_without_calling_a_real_server()
-> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(EFFECT, TRY_SCRIPT)?;
    host.register_jobs = false;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    host.ship_scenario("offline", ERROR_SCENARIO)?;
    host.bring_to(Stage::Validated)?;

    let outcome = host.run_with(dry(None)).await?;

    assert!(matches!(outcome, RunOutcome::Degraded(_)), "{outcome:?}");
    assert_eq!(host.calls(), 0, "a dry run never reaches the real caller");
    assert_eq!(host.lifecycle()?, JobLifecycle::Registered);
    Ok(())
}

#[tokio::test]
async fn a_dry_run_that_leaves_declared_calls_unused_registers_nothing() -> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(EFFECT, "()")?;
    host.register_jobs = false;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    host.ship_scenario("offline", ERROR_SCENARIO)?;
    host.bring_to(Stage::Validated)?;

    host.run_with(dry(None)).await?;

    assert_eq!(host.lifecycle()?, JobLifecycle::Validated);
    Ok(())
}

#[tokio::test]
async fn a_dry_run_of_a_job_whose_script_changed_since_it_was_validated_registers_nothing()
-> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(EFFECT, TRY_SCRIPT)?;
    host.register_jobs = false;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    host.ship_scenario("offline", ERROR_SCENARIO)?;
    host.bring_to(Stage::Validated)?;
    // The job file is changed to name a newer release, so the job runs a version that was never validated.
    let newer = host.store.path().join("acme/1.1.0");
    std::fs::create_dir_all(newer.join("sync/tests"))?;
    std::fs::write(
        newer.join("manifest.toml"),
        format!(
            "name = \"acme\"\nversion = \"1.1.0\"\ndigest = \"sha256:{}\"\n\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n{EFFECT}",
            "b".repeat(64)
        ),
    )?;
    std::fs::write(newer.join("sync.rhai"), TRY_SCRIPT)?;
    std::fs::write(newer.join("sync/tests/offline.toml"), ERROR_SCENARIO)?;
    crate::release_fixture::seal(&newer)?;
    host.name_release("1.1.0")?;

    host.run_with(dry(None)).await?;

    assert_eq!(host.lifecycle()?, JobLifecycle::Validated);
    Ok(())
}

#[tokio::test]
async fn a_dry_run_never_settles_a_trial_whatever_it_finds() -> Result<(), Box<dyn std::error::Error>> {
    for script in [TRY_SCRIPT, FAILING_SCRIPT] {
        let mut host = Host::new(EFFECT, script)?;
        host.register_jobs = false;
        host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
        host.ship_scenario("offline", ERROR_SCENARIO)?;
        host.bring_to(Stage::Enabled)?;

        host.run_with(dry(None)).await?;

        assert_eq!(host.lifecycle()?, JobLifecycle::Enabled, "{script}");
        assert!(host.on_trial()?, "{script}");
    }
    Ok(())
}

#[tokio::test]
async fn a_dry_run_is_allowed_for_a_draft_and_refused_for_a_retired_job() -> Result<(), Box<dyn std::error::Error>> {
    let mut draft = Host::new(EFFECT, TRY_SCRIPT)?;
    draft.register_jobs = false;
    draft.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    draft.ship_scenario("offline", ERROR_SCENARIO)?;
    let mut retired = Host::new(EFFECT, TRY_SCRIPT)?;
    retired.register_jobs = false;
    retired.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    retired.ship_scenario("offline", ERROR_SCENARIO)?;
    retired.bring_to(Stage::Retired)?;

    assert!(draft.run_with(dry(None)).await.is_ok());
    assert_eq!(
        draft.lifecycle()?,
        JobLifecycle::Draft,
        "a draft was not validated, so nothing registers it"
    );
    assert!(matches!(
        retired.run_with(dry(None)).await,
        Err(CliSharedError::NotRunnable { dry_run: true, .. })
    ));
    Ok(())
}

#[tokio::test]
async fn a_dry_run_of_a_workflow_with_no_scenario_is_refused_naming_it() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new(EFFECT, TRY_SCRIPT)?;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;

    let refused = host.run_with(dry(None)).await;

    let message = refused.as_ref().err().map(ToString::to_string).unwrap_or_default();
    assert!(matches!(refused, Err(CliSharedError::Scenario(_))), "{refused:?}");
    assert!(message.contains("sync"), "{message}");
    Ok(())
}

#[tokio::test]
async fn a_dry_run_leaves_a_record_flagged_as_a_dry_run() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new(EFFECT, TRY_SCRIPT)?;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    host.ship_scenario("offline", ERROR_SCENARIO)?;

    host.run_with(dry(None)).await?;

    let record = host.record(1)?;
    assert!(record.contains("dry_run: true\n"), "{record}");
    assert!(record.contains("status: degraded\n"), "{record}");
    assert!(
        record.contains("| vault_info |"),
        "the scenario's calls are listed:\n{record}"
    );
    Ok(())
}

#[tokio::test]
async fn a_live_run_record_is_not_flagged_as_a_dry_run() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;

    host.run_job().await?;

    let record = host.record(1)?;
    assert!(record.contains("dry_run: false\n"), "{record}");
    Ok(())
}

#[tokio::test]
async fn a_dry_run_runs_beside_a_held_claim_and_a_live_run_after_it_is_not_blocked()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new(EFFECT, TRY_SCRIPT)?;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    host.ship_scenario("offline", ERROR_SCENARIO)?;

    {
        let _held_by_a_live_run = SharedClaim::acquire(host.claims.path(), &Host::key())?;
        let outcome = host.run_with(dry(None)).await?;
        assert!(matches!(outcome, RunOutcome::Degraded(_)), "{outcome:?}");
    }

    assert!(claim_is_free(&host), "a dry run holds no claim");
    let live = host.run_job().await?;
    assert!(
        matches!(live, RunOutcome::Degraded(_) | RunOutcome::Succeeded),
        "{live:?}"
    );
    assert!(
        host.record(2)?.contains("dry_run: false\n"),
        "the live run is the job's second record"
    );
    Ok(())
}

#[tokio::test]
async fn a_dry_run_leaves_a_live_runs_running_record_alone() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new(EFFECT, TRY_SCRIPT)?;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    host.ship_scenario("offline", ERROR_SCENARIO)?;
    let ledger = SharedLedger::new(
        host.runs.path().to_path_buf(),
        Arc::new(FixedClock(host.now)),
        time::Duration::days(30),
    );
    let claim = SharedClaim::acquire(host.claims.path(), &Host::key())?;
    let identity = contextos_workflows::PackageIdentity {
        name: contextos_workflows::PackageName::try_from("acme")?,
        version: semver::Version::parse("1.0.0")?,
        digest: contextos_workflows::ContentDigest::try_new(format!("sha256:{}", "a".repeat(64)))?,
    };
    let live = ledger.begin(
        &claim,
        RunStart {
            identity: &identity,
            inputs: Vec::new(),
            libraries: &[],
            trigger: RunTrigger::Manual,
        },
    )?;

    host.run_with(dry(None)).await?;

    let still_running = std::fs::read_to_string(live.path())?;
    assert!(still_running.contains("status: running\n"), "{still_running}");
    Ok(())
}

#[tokio::test]
async fn a_one_off_dry_run_of_a_shared_workflow_uses_the_scenario_the_package_ships()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new(EFFECT, TRY_SCRIPT)?;
    host.ship_scenario("offline", ERROR_SCENARIO)?;

    let outcome = host.run_once_dry(None).await?;

    assert!(matches!(outcome, RunOutcome::Degraded(_)), "{outcome:?}");
    assert_eq!(host.calls(), 0, "a dry run never reaches the real caller");
    assert!(!host.state.state_path().exists(), "a one-off run has no job to change");
    assert!(host.record(1).is_err(), "and leaves no record");
    Ok(())
}

#[tokio::test]
async fn a_one_off_dry_run_of_a_shared_workflow_with_no_scenario_is_refused() -> Result<(), Box<dyn std::error::Error>>
{
    let host = Host::new(EFFECT, TRY_SCRIPT)?;

    let refused = host.run_once_dry(None).await;

    assert!(matches!(refused, Err(CliSharedError::Scenario(_))), "{refused:?}");
    Ok(())
}

#[tokio::test]
async fn a_one_off_dry_run_can_name_one_of_several_scenarios() -> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new(EFFECT, TRY_SCRIPT)?;
    host.ship_scenario("first", ERROR_SCENARIO)?;
    host.ship_scenario("second", ERROR_SCENARIO)?;

    assert!(matches!(
        host.run_once_dry(None).await,
        Err(CliSharedError::Scenario(SharedScenarioError::Ambiguous { .. }))
    ));
    assert!(host.run_once_dry(Some("second")).await.is_ok());
    Ok(())
}

// ---------------------------------------------------------------------
// A version nobody has reviewed
// ---------------------------------------------------------------------

/// Installs release 1.1.0 of the package with `script`, as an upload would, without any validation pass having looked at it.
fn install_newer(host: &Host, script: &str) -> Result<(), Box<dyn std::error::Error>> {
    let dir = host.store.path().join("acme/1.1.0");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("manifest.toml"),
        format!(
            "name = \"acme\"\nversion = \"1.1.0\"\ndigest = \"sha256:{}\"\n\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n{EFFECT}",
            "b".repeat(64)
        ),
    )?;
    std::fs::write(dir.join("sync.rhai"), script)?;
    crate::release_fixture::seal(&dir)?;
    Ok(())
}

#[tokio::test]
async fn a_scheduled_run_of_a_release_nobody_has_reviewed_is_refused_before_it_claims_anything()
-> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.register_jobs = false;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    host.bring_to(Stage::EnabledProven)?;
    // A newer release arrived after the scheduler decided the job was due, and no pass has reviewed it.
    install_newer(&host, FAILING_SCRIPT)?;
    host.name_release("1.1.0")?;

    let refused = host.run_with(scheduled()).await;

    let message = refused.as_ref().err().map(ToString::to_string).unwrap_or_default();
    assert!(matches!(refused, Err(CliSharedError::NotReviewed)), "{refused:?}");
    assert!(message.starts_with("workflow/job-not-runnable"), "{message}");
    assert_eq!(host.calls(), 0);
    assert!(host.record(1).is_err(), "nothing was recorded");
    assert!(claim_is_free(&host));
    Ok(())
}

#[tokio::test]
async fn a_scheduled_run_of_the_release_that_was_reviewed_is_not_held_back() -> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.register_jobs = false;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    host.bring_to(Stage::Enabled)?;

    assert!(matches!(host.run_with(scheduled()).await, Ok(RunOutcome::Succeeded)));
    Ok(())
}

#[tokio::test]
async fn a_run_by_hand_of_an_unreviewed_release_runs_and_does_not_settle_the_trial_of_the_one_that_was()
-> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.register_jobs = false;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    host.bring_to(Stage::Enabled)?;
    install_newer(&host, FAILING_SCRIPT)?;
    host.name_release("1.1.0")?;

    let outcome = host.run_job().await?;

    assert!(
        matches!(outcome, RunOutcome::Failed(_)),
        "the operator's own run is not held back: {outcome:?}"
    );
    assert_eq!(
        host.lifecycle()?,
        JobLifecycle::Enabled,
        "its failure is about a release that was not on trial"
    );
    assert!(host.on_trial()?);
    Ok(())
}

#[tokio::test]
async fn a_scheduled_run_holding_its_claim_for_a_job_no_longer_enabled_is_refused()
-> Result<(), Box<dyn std::error::Error>> {
    for stage in [Stage::Registered, Stage::Suspended, Stage::Retired] {
        let mut host = Host::new(EFFECT, CALLING_SCRIPT)?;
        host.register_jobs = false;
        host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
        host.bring_to(stage)?;

        let refused = require_still_enabled(&host.state, &Host::key()).await;

        assert!(
            matches!(
                refused,
                Err(CliSharedError::NotRunnable {
                    scheduled: true,
                    dry_run: false,
                    ..
                })
            ),
            "{stage:?}: {refused:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_scheduled_run_holding_its_claim_for_an_enabled_job_goes_ahead() -> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.register_jobs = false;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    host.bring_to(Stage::Enabled)?;

    require_still_enabled(&host.state, &Host::key()).await?;
    Ok(())
}
