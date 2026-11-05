//! A release removed, replaced, or changed while a snapshot of a job's code is being read is a change on the host caught
//! mid-read, not a defect in the job: nothing about the job changes, a due tick stays due, and the next pass reads again.
//! Shares its fixture with `cli_shared_test.rs`, and uses the snapshot's test seam to make the change at the exact point
//! a real install or removal would race the read.

use std::sync::atomic::{AtomicBool, Ordering};

use contextos_workflows::{Actor, JobLifecycle, StoredValidation};
use time::macros::datetime;

use super::*;
use crate::job_operation::JobStore;
use crate::run_settings::HostContext;
use crate::shared_jobs::{SharedJobFile, list_shared_job_files};
use crate::shared_lifecycle::{SharedPassHost, shared_validation_pass};
use crate::shared_scheduler::{SharedSchedulerHost, start_due_shared_jobs};
use crate::shared_validation::{SharedValidationHost, validate_shared_job};
use crate::snapshot_hooks::{AFTER_MODULES, set_hook};

const PLAIN_SCRIPT: &str = "log_info(\"a\");";
const READY_JOB: &str = "name = \"Nightly\"\ntimeout_secs = 60\nversion = \"1.0.0\"\nready = true\n";

/// Installs release 2.0.0 of the package beside the host's 1.0.0.
fn install_newer(host: &Host) -> Result<(), Box<dyn std::error::Error>> {
    let dir = host.store.path().join("acme").join("2.0.0");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("manifest.toml"),
        format!(
            "name = \"acme\"\nversion = \"2.0.0\"\ndigest = \"sha256:{}\"\n\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n",
            "b".repeat(64)
        ),
    )?;
    std::fs::write(dir.join("sync.rhai"), PLAIN_SCRIPT)?;
    crate::release_fixture::seal(&dir)?;
    Ok(())
}

/// Arranges for the release `version` to be replaced by one with another digest, once, as soon as the modules of the
/// next snapshot have been read: its script gains a comment and the release is made whole again. A read after that sees
/// the replacement as the release installed.
fn replace_release_during_next_read(host: &Host, version: &str) {
    let dir = host.store.path().join("acme").join(version);
    let armed = AtomicBool::new(true);
    set_hook(
        &AFTER_MODULES,
        host.store.path(),
        Box::new(move |_| {
            if armed.swap(false, Ordering::SeqCst) {
                let script = dir.join("sync/sync.rhai");
                let text = std::fs::read_to_string(&script).unwrap_or_default();
                let _ = std::fs::write(&script, format!("{text}\n// replaced"));
                let _ = crate::release_fixture::seal(&dir);
            }
        }),
    );
}

async fn pass(host: &Host) -> Result<(), Box<dyn std::error::Error>> {
    let clients = McpClientSet::connect(&[]).await?;
    let known = clients.names();
    shared_validation_pass(
        &SharedPassHost {
            jobs_root: host.jobs.path(),
            state: &host.state,
            validation: SharedValidationHost {
                store_root: host.store.path(),
                clients: &clients,
                primary_server: "",
                known_servers: &known,
            },
        },
        host.now,
    )
    .await;
    Ok(())
}

fn job_file(host: &Host) -> Result<SharedJobFile, Box<dyn std::error::Error>> {
    list_shared_job_files(host.jobs.path())?
        .into_iter()
        .next()
        .ok_or_else(|| "the host has no job".into())
}

/// Records a validation result made under the first rules, which the pass validates again.
fn record_result_under_old_rules(host: &Host) -> Result<(), Box<dyn std::error::Error>> {
    let version = host.version()?;
    let stored = StoredValidation {
        by: Actor::Validator,
        rules: 1,
        at: host.now,
        fingerprint: Some(version),
        passed: true,
        failures: Vec::new(),
        unverifiable: Vec::new(),
        made_from: None,
    };
    host.state
        .update(|jobs| jobs.record_validation("acme", "sync", "nightly", stored))?;
    Ok(())
}

fn stored_result(host: &Host) -> Result<Option<StoredValidation>, Box<dyn std::error::Error>> {
    Ok(host
        .state
        .read()?
        .job("acme", "sync", "nightly")
        .and_then(|job| job.validation().cloned()))
}

#[tokio::test]
async fn an_enabled_job_whose_new_release_changes_while_it_is_read_is_not_suspended_and_is_reviewed_by_the_next_pass()
-> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new("", PLAIN_SCRIPT)?;
    host.register_jobs = false;
    host.add_job(READY_JOB)?;
    host.bring_to(Stage::EnabledProven)?;
    let recorded = host.version()?;
    install_newer(&host)?;
    host.name_release("2.0.0")?;
    replace_release_during_next_read(&host, "2.0.0");

    pass(&host).await?;

    assert_eq!(
        host.lifecycle()?,
        JobLifecycle::Enabled,
        "a race is not a defect of the job"
    );
    let state = host.state.read()?;
    let job = state.job("acme", "sync", "nightly").ok_or("the job has no state")?;
    assert_eq!(job.fingerprint(), Some(&recorded), "the job keeps the version it had");
    assert!(
        job.validation().is_none(),
        "no result was stored for a read that did not complete"
    );
    assert!(
        !job.first_run_pending(),
        "no trial began for a release that was not reviewed"
    );

    pass(&host).await?;

    assert_eq!(host.lifecycle()?, JobLifecycle::Enabled);
    assert!(
        host.on_trial()?,
        "the next pass read the release again and put it on trial"
    );
    Ok(())
}

#[tokio::test]
async fn a_registered_job_whose_release_changes_while_it_is_revalidated_stays_registered_with_its_result()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new("", PLAIN_SCRIPT)?;
    host.add_job(READY_JOB)?;
    record_result_under_old_rules(&host)?;
    let before = stored_result(&host)?;
    replace_release_during_next_read(&host, "1.0.0");

    pass(&host).await?;

    assert_eq!(
        host.lifecycle()?,
        JobLifecycle::Registered,
        "a race does not return a job to draft"
    );
    assert_eq!(
        stored_result(&host)?,
        before,
        "the stored result is exactly what it was"
    );
    Ok(())
}

#[tokio::test]
async fn validating_on_demand_while_the_release_changes_reports_it_unverifiable_and_stores_nothing()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new("", PLAIN_SCRIPT)?;
    host.add_job(READY_JOB)?;
    let before = stored_result(&host)?;
    replace_release_during_next_read(&host, "1.0.0");
    let clients = McpClientSet::connect(&[]).await?;
    let known = clients.names();
    let validation_host = SharedValidationHost {
        store_root: host.store.path(),
        clients: &clients,
        primary_server: "",
        known_servers: &known,
    };

    let validation = validate_shared_job(&validation_host, &job_file(&host)?).await;

    assert!(
        validation.report.failures.is_empty(),
        "{:?}",
        validation.report.failures
    );
    let reasons: Vec<&str> = validation
        .report
        .unverifiable
        .iter()
        .map(|check| check.reason.as_str())
        .collect();
    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains("changed while it was read") && reason.contains("try again")),
        "{reasons:?}"
    );
    assert!(!validation.report.passed(), "an unverifiable job does not pass");

    replace_release_during_next_read(&host, "1.0.0");
    let store = crate::job_operation_shared::SharedJobStore {
        state: &host.state,
        jobs_root: host.jobs.path(),
        validation: &validation_host,
        key: &Host::key(),
        claims_root: host.claims.path(),
    };
    let asked = store.validate(host.now).await?;

    assert!(asked.failures.is_empty() && !asked.unverifiable.is_empty());
    assert_eq!(
        stored_result(&host)?,
        before,
        "a read that did not complete leaves no result"
    );
    assert_eq!(host.lifecycle()?, JobLifecycle::Registered);
    Ok(())
}

#[tokio::test]
async fn a_manual_run_whose_release_changes_while_it_is_read_fails_before_its_script_starts()
-> Result<(), Box<dyn std::error::Error>> {
    let host = Host::new("", PLAIN_SCRIPT)?;
    host.add_job(READY_JOB)?;
    replace_release_during_next_read(&host, "1.0.0");

    let clients = Arc::new(McpClientSet::connect(&[]).await?);
    let report =
        run_shared_job_reported(host.host(clients), Host::key(), SharedRunOptions::default(), Vec::new()).await?;

    assert!(!report.started, "the script never started");
    assert!(
        matches!(&report.outcome, RunOutcome::Failed(failure) if failure.message.contains("changed while it was read")),
        "{:?}",
        report.outcome
    );
    assert_eq!(
        host.lifecycle()?,
        JobLifecycle::Registered,
        "a refused run decides nothing about the job"
    );
    Ok(())
}

#[tokio::test]
async fn a_due_tick_whose_release_changes_while_it_is_read_stays_due_and_leaves_no_record()
-> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new("", PLAIN_SCRIPT)?;
    host.register_jobs = false;
    host.add_job(READY_JOB)?;
    host.bring_to(Stage::EnabledProven)?;
    replace_release_during_next_read(&host, "1.0.0");
    let fired_before = host.state.read()?.last_scheduled_fire("acme", "sync", "nightly");
    let context = HostContext {
        attention: crate::job_attention::JobAttentionMemory::default(),
        log_level: None,
        claims_root: Some(host.claims.path().to_path_buf()),
        clock: Arc::new(crate::vault_time::VaultTime::from(crate::vault_time::VaultTimeInput {
            clock: Arc::new(contextos_workflows::SystemClock),
            clients: Arc::new(McpClientSet::default()),
        })),
    };
    let shared = SharedSchedulerHost {
        store_root: host.store.path().to_path_buf(),
        jobs_root: host.jobs.path().to_path_buf(),
        claims_root: host.claims.path().to_path_buf(),
        runs_root: host.runs.path().to_path_buf(),
        state: host.state.clone(),
        run_retention: time::Duration::days(30),
        owner: None,
        log_level: None,
    };
    let clients = Arc::new(McpClientSet::default());
    let due = FixedClock(datetime!(2026-10-02 03:00:30 UTC));

    let report = start_due_shared_jobs(&clients, &context, &shared, &due).await;

    assert_eq!((report.spawned, report.held_back), (0, 0), "{report:?}");
    assert_eq!(
        host.state.read()?.last_scheduled_fire("acme", "sync", "nightly"),
        fired_before,
        "the tick was not consumed"
    );
    assert_eq!(host.lifecycle()?, JobLifecycle::Enabled);
    assert!(
        std::fs::read_dir(host.runs.path())?.next().is_none(),
        "no run record, not even a skipped one"
    );
    Ok(())
}
