//! What a shared command-line run reports back for its summary: identity, record, release, timings, owner, overrides,
//! and registration. Shares its fixture with `cli_shared_test.rs`.

use super::*;
use crate::run_report::RunReport;
use crate::run_summary::{NotRegistered, RecordLocation, Registration, ReleaseRef};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const TRY_SCRIPT: &str = r#"try { call_mcp("contextos", "vault_info", #{}); } catch (e) { flag_degraded("offline"); }"#;
const OFFLINE: &str =
    "[[call]]\nserver = \"contextos\"\ntool = \"vault_info\"\nkind = \"error\"\nerror = \"offline\"\n";
const TARGET_INPUT: &str = "\n[[workflow.inputs]]\nname = \"target\"\nkind = \"string\"\n";
const JOB_WITH_TARGET: &str =
    "name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n[inputs]\ntarget = \"from-the-job\"\n";

fn dry_run() -> SharedRunOptions {
    SharedRunOptions {
        trigger: RunTrigger::Manual,
        dry_run_scenario: Some(None),
    }
}

async fn reported(host: &Host, options: SharedRunOptions, overrides: &[&str]) -> Result<RunReport, CliSharedError> {
    let clients = Arc::new(
        McpClientSet::connect(&[])
            .await
            .map_err(|_| CliSharedError::NoClients)?,
    );
    run_shared_job_reported(
        host.host(clients),
        Host::key(),
        options,
        overrides.iter().map(|argument| (*argument).to_owned()).collect(),
    )
    .await
}

#[tokio::test]
async fn a_shared_run_reports_its_run_id_record_location_release_and_timings() -> TestResult {
    let host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;

    let report = reported(&host, SharedRunOptions::default(), &[]).await?;

    assert_eq!(report.outcome, RunOutcome::Succeeded);
    assert!(report.started);
    assert_eq!(report.run_id, Some(1));
    assert_eq!(
        report.record,
        Some(RecordLocation::Host {
            relative: "runs/acme/sync/nightly/2026/10/R-1.md".to_owned()
        })
    );
    assert_eq!(
        report.release,
        Some(ReleaseRef {
            version: "1.0.0".to_owned(),
            digest: host.release_digest()?,
        })
    );
    assert_eq!(report.started_at, Some(host.now), "timings come from the host's clock");
    assert_eq!(report.ended_at, Some(host.now));
    assert_eq!(report.registration, None, "a live run registers nothing");
    Ok(())
}

#[tokio::test]
async fn a_failed_shared_job_reports_its_owner_as_a_field_and_not_in_the_message() -> TestResult {
    let mut failing = Host::new(EFFECT, CALLING_SCRIPT)?;
    failing.owner = Some("Piper");
    failing.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    failing.uninstall()?;
    let mut succeeding = Host::new(EFFECT, CALLING_SCRIPT)?;
    succeeding.owner = Some("Piper");
    succeeding.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;

    let failed = reported(&failing, SharedRunOptions::default(), &[]).await?;
    let succeeded = reported(&succeeding, SharedRunOptions::default(), &[]).await?;

    assert!(
        matches!(&failed.outcome, RunOutcome::Failed(failure) if !failure.message.contains("owner")),
        "the message does not carry the owner: {:?}",
        failed.outcome
    );
    assert_eq!(failed.owner.as_deref(), Some("Piper"));
    assert!(!failed.started, "the package was gone, so the script never started");
    assert_eq!(succeeded.owner, None, "an owner is named only for a failure");
    Ok(())
}

#[tokio::test]
async fn a_shared_run_with_overrides_records_their_names_and_leaves_the_first_run_trial_unsettled() -> TestResult {
    let mut host = Host::new(&format!("{EFFECT}{TARGET_INPUT}"), CALLING_SCRIPT)?;
    host.register_jobs = false;
    host.add_job(JOB_WITH_TARGET)?;
    host.bring_to(Stage::Enabled)?;
    assert!(host.on_trial()?, "the first live run of this version is on trial");

    let report = reported(&host, SharedRunOptions::default(), &["target=override-value-x"]).await?;

    assert_eq!(report.outcome, RunOutcome::Succeeded);
    assert_eq!(report.overridden_inputs, vec!["target".to_owned()]);
    assert!(
        host.on_trial()?,
        "a run with other values says nothing about the version on trial"
    );
    let record = host.record(1)?;
    assert!(record.contains("overrides: [\"target\"]\n"), "{record}");
    assert!(
        !record.contains("override-value-x"),
        "a value is never recorded: {record}"
    );
    Ok(())
}

#[tokio::test]
async fn a_shared_dry_run_with_overrides_completes_and_does_not_register_the_job() -> TestResult {
    let mut host = Host::new(&format!("{EFFECT}{TARGET_INPUT}"), TRY_SCRIPT)?;
    host.register_jobs = false;
    host.add_job(JOB_WITH_TARGET)?;
    host.ship_scenario("offline", OFFLINE)?;
    host.bring_to(Stage::Validated)?;

    let report = reported(&host, dry_run(), &["target=one-off"]).await?;

    assert!(
        matches!(report.outcome, RunOutcome::Degraded(_)),
        "the dry run itself completes: {:?}",
        report.outcome
    );
    assert_eq!(report.overridden_inputs, vec!["target".to_owned()]);
    assert_eq!(
        report.registration,
        Some(Registration::NotRegistered(NotRegistered::InputsOverridden))
    );
    assert_eq!(host.lifecycle()?, JobLifecycle::Validated, "the job stays validated");
    Ok(())
}

#[tokio::test]
async fn an_invalid_shared_override_is_refused_before_the_claim_and_leaves_no_record() -> TestResult {
    let mut host = Host::new(&format!("{EFFECT}{TARGET_INPUT}"), CALLING_SCRIPT)?;
    host.register_jobs = false;
    host.add_job(JOB_WITH_TARGET)?;
    host.bring_to(Stage::Enabled)?;

    let report = reported(&host, SharedRunOptions::default(), &["not-declared=1"]).await?;

    assert!(
        failed_with(&report.outcome, WorkflowFailureKind::InputInvalid),
        "{:?}",
        report.outcome
    );
    assert!(!report.started, "no script step ran");
    assert_eq!(report.run_id, None);
    assert_eq!(report.record, None);
    assert_eq!(host.calls(), 0);
    assert!(host.record(1).is_err(), "no record was written");
    assert!(
        !host.claims.path().join("shared/acme/sync/nightly.lock").exists()
            && !host.claims.path().join("acme/sync/nightly.lock").exists(),
        "no claim was taken"
    );
    assert!(host.on_trial()?, "a refused override decides nothing about the job");
    assert_eq!(
        host.lifecycle()?,
        JobLifecycle::Enabled,
        "a run that never started cannot suspend the job"
    );
    Ok(())
}

#[tokio::test]
async fn a_shared_dry_run_reports_registered_or_the_reason_it_was_not() -> TestResult {
    // A validated job whose dry run completes is registered.
    let mut completes = Host::new(EFFECT, TRY_SCRIPT)?;
    completes.register_jobs = false;
    completes.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    completes.ship_scenario("offline", OFFLINE)?;
    completes.bring_to(Stage::Validated)?;

    // A draft job is not registered by a dry run.
    let mut draft = Host::new(EFFECT, TRY_SCRIPT)?;
    draft.register_jobs = false;
    draft.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    draft.ship_scenario("offline", OFFLINE)?;

    // A scenario call the script never made.
    let mut unused = Host::new(EFFECT, "log_info(\"no calls\");")?;
    unused.register_jobs = false;
    unused.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    unused.ship_scenario("offline", OFFLINE)?;
    unused.bring_to(Stage::Validated)?;

    // A script that fails.
    let mut fails = Host::new(EFFECT, "throw \"boom\";")?;
    fails.register_jobs = false;
    fails.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    fails.ship_scenario("offline", OFFLINE)?;
    fails.bring_to(Stage::Validated)?;

    // A job file changed after it was validated.
    let mut changed = Host::new(EFFECT, TRY_SCRIPT)?;
    changed.register_jobs = false;
    changed.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    changed.ship_scenario("offline", OFFLINE)?;
    changed.bring_to(Stage::Validated)?;
    std::fs::write(
        changed.jobs.path().join("acme/sync/jobs/nightly.toml"),
        "name = \"Nightly, edited\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n",
    )?;

    let expected = [
        (&completes, Registration::Registered),
        (&draft, Registration::NotRegistered(NotRegistered::NotValidated)),
        (&unused, Registration::NotRegistered(NotRegistered::ScenarioCallsUnused)),
        (&fails, Registration::NotRegistered(NotRegistered::DryRunNotCompleted)),
        (&changed, Registration::NotRegistered(NotRegistered::VersionChanged)),
    ];
    for (host, registration) in expected {
        let report = reported(host, dry_run(), &[]).await?;

        assert_eq!(report.registration, Some(registration), "{:?}", report.outcome);
    }
    Ok(())
}

const VAULT_INPUT: &str = "\n[[workflow.inputs]]\nname = \"source\"\nkind = \"vault\"\n";
const JOB_WITH_VAULT: &str =
    "name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n[inputs]\nsource = \"no-such-vault\"\n";

#[tokio::test]
async fn a_shared_dry_run_with_an_invalid_override_never_settles_the_first_run_trial()
-> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(&format!("{EFFECT}{TARGET_INPUT}"), CALLING_SCRIPT)?;
    host.register_jobs = false;
    host.add_job(JOB_WITH_TARGET)?;
    host.ship_scenario("offline", OFFLINE)?;
    host.bring_to(Stage::Enabled)?;

    let report = reported(&host, dry_run(), &["not-declared=1"]).await?;

    assert!(
        failed_with(&report.outcome, WorkflowFailureKind::InputInvalid),
        "{:?}",
        report.outcome
    );
    assert_eq!(
        host.lifecycle()?,
        JobLifecycle::Enabled,
        "a dry run is not the job's first live run, whatever becomes of it"
    );
    assert!(host.on_trial()?, "the first live run is still to come");
    Ok(())
}

#[tokio::test]
async fn a_shared_dry_run_whose_preparation_fails_never_suspends_the_job() -> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(&format!("{EFFECT}{VAULT_INPUT}"), CALLING_SCRIPT)?;
    host.register_jobs = false;
    host.add_job(JOB_WITH_VAULT)?;
    host.ship_scenario("offline", OFFLINE)?;
    host.bring_to(Stage::Enabled)?;

    let report = reported(&host, dry_run(), &[]).await?;

    assert!(
        matches!(report.outcome, RunOutcome::Failed(_)),
        "the vault is not configured, so the run cannot be prepared: {:?}",
        report.outcome
    );
    assert!(!report.started);
    assert_eq!(
        host.lifecycle()?,
        JobLifecycle::Enabled,
        "a dry run that could not be prepared decides nothing about the job"
    );
    assert!(host.on_trial()?);
    Ok(())
}

#[tokio::test]
async fn a_live_shared_run_with_an_override_that_cannot_be_prepared_never_suspends_the_job()
-> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(&format!("{EFFECT}{VAULT_INPUT}{TARGET_INPUT}"), CALLING_SCRIPT)?;
    host.register_jobs = false;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n[inputs]\nsource = \"no-such-vault\"\ntarget = \"a\"\n")?;
    host.bring_to(Stage::Enabled)?;

    let report = reported(&host, SharedRunOptions::default(), &["target=b"]).await?;

    assert!(
        matches!(report.outcome, RunOutcome::Failed(_)),
        "the vault is not configured: {:?}",
        report.outcome
    );
    assert_eq!(
        host.lifecycle()?,
        JobLifecycle::Enabled,
        "a run given other values says nothing about the version on trial, even when it cannot be prepared"
    );
    Ok(())
}

/// Whether a folder made read-only can still be written to, which is the case for a process running as root. A test
/// that relies on an unwritable folder returns early when it is, because the failure it needs cannot be made.
#[cfg(unix)]
fn directory_permissions_are_ignored() -> std::io::Result<bool> {
    use std::os::unix::fs::PermissionsExt as _;
    let folder = tempfile::tempdir()?;
    std::fs::set_permissions(folder.path(), std::fs::Permissions::from_mode(0o555))?;
    let writable = std::fs::File::create(folder.path().join("probe")).is_ok();
    std::fs::set_permissions(folder.path(), std::fs::Permissions::from_mode(0o755))?;
    Ok(writable)
}

/// A folder made unwritable for the length of a test, and made writable again when the test ends, however it ends.
#[cfg(unix)]
struct Unwritable(std::path::PathBuf);

#[cfg(unix)]
impl Unwritable {
    fn new(folder: &std::path::Path) -> std::io::Result<Self> {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o555))?;
        Ok(Self(folder.to_path_buf()))
    }
}

#[cfg(unix)]
impl Drop for Unwritable {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

#[cfg(unix)]
fn warning_codes(report: &RunReport) -> Vec<&str> {
    report
        .host_warnings
        .iter()
        .map(|warning| warning.code.as_str())
        .collect()
}

#[cfg(unix)]
#[tokio::test]
async fn a_shared_dry_run_that_cannot_save_its_registration_warns_that_the_state_is_unavailable() -> TestResult {
    if directory_permissions_are_ignored()? {
        return Ok(());
    }
    let mut host = Host::new(EFFECT, TRY_SCRIPT)?;
    host.register_jobs = false;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    host.ship_scenario("offline", OFFLINE)?;
    host.bring_to(Stage::Validated)?;
    let state_folder = host.state.lock_path().parent().map(std::path::Path::to_path_buf);
    let _locked = Unwritable::new(&state_folder.ok_or("the state store has no folder")?)?;

    let report = reported(&host, dry_run(), &[]).await?;

    assert_eq!(
        report.registration,
        Some(Registration::NotRegistered(NotRegistered::StateUnavailable))
    );
    assert!(
        warning_codes(&report).contains(&"workflow/state-unavailable"),
        "the summary says the host could not save the state: {:?}",
        report.host_warnings
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_shared_first_run_that_cannot_settle_its_trial_warns_that_the_state_is_unavailable() -> TestResult {
    if directory_permissions_are_ignored()? {
        return Ok(());
    }
    let mut host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.register_jobs = false;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    host.bring_to(Stage::Enabled)?;
    let state_folder = host.state.lock_path().parent().map(std::path::Path::to_path_buf);
    let _locked = Unwritable::new(&state_folder.ok_or("the state store has no folder")?)?;

    let report = reported(&host, SharedRunOptions::default(), &[]).await?;

    assert_eq!(report.outcome, RunOutcome::Succeeded, "the run itself is unaffected");
    assert!(
        warning_codes(&report).contains(&"workflow/state-unavailable"),
        "the summary says the trial could not be settled: {:?}",
        report.host_warnings
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn a_shared_run_whose_record_cannot_be_written_still_runs_and_warns_that_the_record_is_unavailable() -> TestResult
{
    if directory_permissions_are_ignored()? {
        return Ok(());
    }
    let host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    let _locked = Unwritable::new(host.runs.path())?;

    let report = reported(&host, SharedRunOptions::default(), &[]).await?;

    assert_eq!(report.outcome, RunOutcome::Succeeded);
    assert_eq!(report.run_id, None, "no record was begun, so no run number is reported");
    assert_eq!(
        warning_codes(&report)
            .iter()
            .filter(|code| **code == "workflow/record-unavailable")
            .count(),
        1,
        "one warning for a record that could not be started: {:?}",
        report.host_warnings
    );
    Ok(())
}

#[tokio::test]
async fn a_shared_dry_run_whose_record_cannot_be_started_warns_once_that_the_record_is_unavailable() -> TestResult {
    let mut host = Host::new(EFFECT, TRY_SCRIPT)?;
    host.register_jobs = false;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    host.ship_scenario("offline", OFFLINE)?;
    host.bring_to(Stage::Validated)?;
    // The runs root is a file, so no record folder can be made under it, whoever runs the test.
    let unusable = host.runs.path().join("not-a-folder");
    std::fs::write(&unusable, "")?;
    let clients = Arc::new(
        McpClientSet::connect(&[])
            .await
            .map_err(|_| CliSharedError::NoClients)?,
    );
    let mut shared = host.host(clients);
    shared.runs_root = &unusable;

    let report = run_shared_job_reported(shared, Host::key(), dry_run(), Vec::new()).await?;

    assert!(
        matches!(report.outcome, RunOutcome::Degraded(_)),
        "{:?}",
        report.outcome
    );
    let warnings: Vec<_> = report
        .host_warnings
        .iter()
        .filter(|warning| warning.code == "workflow/record-unavailable")
        .collect();
    assert_eq!(
        warnings.len(),
        1,
        "one warning for a record that could not be started: {warnings:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_live_run_of_a_job_that_is_not_on_trial_warns_of_nothing_when_its_release_is_gone() -> TestResult {
    let mut host = Host::new(EFFECT, CALLING_SCRIPT)?;
    host.register_jobs = false;
    host.add_job("name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\n")?;
    host.bring_to(Stage::EnabledProven)?;
    host.uninstall()?;

    let report = reported(&host, SharedRunOptions::default(), &[]).await?;

    assert!(
        failed_with(&report.outcome, WorkflowFailureKind::PackageUnavailable),
        "{:?}",
        report.outcome
    );
    assert!(
        report
            .host_warnings
            .iter()
            .all(|warning| warning.code != "workflow/state-unavailable"),
        "a job that is not on trial has nothing to settle, so no state was unavailable: {:?}",
        report.host_warnings
    );
    assert_eq!(host.lifecycle()?, JobLifecycle::Enabled);
    Ok(())
}
