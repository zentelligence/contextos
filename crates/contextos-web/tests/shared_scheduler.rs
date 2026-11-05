//! The Web scheduler's pass over shared jobs (`contextos_web::shared_scheduler`) against a real `contextos-mcp`
//! session, a real package store, and a real host state file, with the clock fixed so the due tick is known: only an
//! enabled job that is due is started, a tick fires once, a tick that finds the claim held is recorded skipped and not
//! run late, two due jobs never block each other, and nothing needs a vault.

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use contextos_web::McpClientSet;
use contextos_web::run_settings::HostContext;
use contextos_web::shared_claim::SharedClaim;
use contextos_web::shared_scheduler::{SharedSchedulerHost, poll_shared_once, start_due_shared_jobs};
use contextos_workflows::{Actor, Clock, SuspensionReason, Transition};
use support::BoxError;
use support::shared::{OTHER_DIGEST, SharedFixture as Fixture, key, write};
use time::OffsetDateTime;
use time::macros::datetime;

const READY: &str = "name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nready = true\n";
/// A job file as it was written before `timeout_secs` was required.
const READY_WITHOUT_TIMEOUT: &str = "name = \"Nightly\"\nversion = \"1.0.0\"\nready = true\n";
const ENABLED_AT: OffsetDateTime = datetime!(2026-10-05 02:00:00 UTC);

/// 03:00:30, half a minute after a job scheduled for 03:00 daily fell due.
struct FixedClock(OffsetDateTime);

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        self.0
    }
}

const DUE: FixedClock = FixedClock(datetime!(2026-10-05 03:00:30 UTC));
const NOT_YET: FixedClock = FixedClock(datetime!(2026-10-05 02:30:00 UTC));

fn host_for(fixture: &Fixture) -> (HostContext, SharedSchedulerHost) {
    (
        HostContext {
            attention: contextos_web::job_attention::JobAttentionMemory::default(),
            log_level: None,
            claims_root: Some(fixture.claims_root()),
            // As before: a run's own times come from the system clock, in this host's zone with no server to ask.
            clock: std::sync::Arc::new(contextos_web::vault_time::VaultTime::from(
                contextos_web::vault_time::VaultTimeInput {
                    clock: std::sync::Arc::new(contextos_workflows::SystemClock),
                    clients: std::sync::Arc::new(contextos_web::McpClientSet::default()),
                },
            )),
        },
        SharedSchedulerHost {
            store_root: fixture.store_root(),
            jobs_root: fixture.jobs_root(),
            claims_root: fixture.claims_root(),
            runs_root: fixture.runs_root(),
            state: fixture.state(),
            run_retention: time::Duration::days(30),
            owner: None,
            log_level: None,
        },
    )
}

async fn poll(fixture: &Fixture, clock: &FixedClock) -> contextos_web::shared_scheduler::SharedPollReport {
    let (host, shared) = host_for(fixture);
    poll_shared_once(&fixture.clients, &host, &shared, clock).await
}

fn records(root: &Path) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("R-"))
                && let Ok(text) = std::fs::read_to_string(&path)
            {
                found.push((path, text));
            }
        }
    }
    found
}

/// Waits for a run, which is detached, to leave a record that says `needle`, against a real deadline, never a fixed pause.
async fn wait_for_record(fixture: &Fixture, needle: &str) -> Result<String, BoxError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if let Some((_, text)) = records(&fixture.runs_root())
            .into_iter()
            .find(|(_, text)| text.contains(needle))
        {
            return Ok(text);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("no run record said {needle:?}: {:?}", records(&fixture.runs_root())).into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn an_enabled_job_that_is_due_is_run_once_and_recorded_as_scheduled() -> Result<(), BoxError> {
    let fixture = Fixture::new(READY).await?;
    fixture.enable_on("0 3 * * *", ENABLED_AT).await?;

    let report = poll(&fixture, &DUE).await;

    assert_eq!(report.spawned, 1);
    let record = wait_for_record(&fixture, "status: succeeded").await?;
    assert!(record.contains("trigger: scheduled"), "{record}");
    Ok(())
}

#[tokio::test]
async fn a_tick_that_has_fired_does_not_fire_again() -> Result<(), BoxError> {
    let fixture = Fixture::new(READY).await?;
    fixture.enable_on("0 3 * * *", ENABLED_AT).await?;
    assert_eq!(poll(&fixture, &DUE).await.spawned, 1);
    wait_for_record(&fixture, "status: succeeded").await?;

    let again = poll(&fixture, &DUE).await;

    assert_eq!(again.spawned, 0);
    assert_eq!(records(&fixture.runs_root()).len(), 1);
    Ok(())
}

#[tokio::test]
async fn the_last_fire_is_kept_in_the_host_state_so_a_restart_does_not_repeat_the_tick() -> Result<(), BoxError> {
    let fixture = Fixture::new(READY).await?;
    fixture.enable_on("0 3 * * *", ENABLED_AT).await?;
    poll(&fixture, &DUE).await;

    let state = fixture.read()?;

    assert_eq!(
        state.last_scheduled_fire("acme", "sync", "nightly"),
        Some(datetime!(2026-10-05 03:00:00 UTC))
    );
    Ok(())
}

#[tokio::test]
async fn a_job_that_is_not_due_is_not_started() -> Result<(), BoxError> {
    let fixture = Fixture::new(READY).await?;
    fixture.enable_on("0 3 * * *", ENABLED_AT).await?;

    assert_eq!(poll(&fixture, &NOT_YET).await.spawned, 0);
    let runs = records(&fixture.runs_root());
    assert!(runs.is_empty(), "no run should be recorded, got {runs:?}");
    Ok(())
}

#[tokio::test]
async fn a_job_that_is_not_enabled_is_not_started_whatever_schedule_is_stored_for_it() -> Result<(), BoxError> {
    let registered = Fixture::new(READY).await?;
    registered.register().await?;
    let suspended = Fixture::new(READY).await?;
    suspended.enable_on("0 3 * * *", ENABLED_AT).await?;
    suspended
        .change(
            Actor::Operator,
            Transition::Suspend(contextos_workflows::Suspension::operator(SuspensionReason::try_from(
                "stopped by the operator",
            )?)),
            ENABLED_AT,
        )
        .await?;
    let draft = Fixture::new("name = \"Nightly\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n").await?;

    for fixture in [&registered, &suspended, &draft] {
        assert_eq!(poll(fixture, &DUE).await.spawned, 0);
        let runs = records(&fixture.runs_root());
        assert!(runs.is_empty(), "no run should be recorded, got {runs:?}");
    }
    Ok(())
}

#[tokio::test]
async fn a_tick_that_finds_the_claim_held_is_recorded_skipped_and_is_not_run_late() -> Result<(), BoxError> {
    let fixture = Fixture::new(READY).await?;
    fixture.enable_on("0 3 * * *", ENABLED_AT).await?;
    let held = SharedClaim::acquire(&fixture.claims_root(), &key())?;

    let report = poll(&fixture, &DUE).await;

    assert_eq!(report.spawned, 1);
    let record = wait_for_record(&fixture, "status: skipped").await?;
    assert!(record.contains("trigger: scheduled"), "{record}");
    assert!(fixture.on_trial()?, "a tick that did not run does not settle the trial");
    drop(held);
    assert_eq!(
        poll(&fixture, &DUE).await.spawned,
        0,
        "the skipped tick is not run once the claim is free"
    );
    Ok(())
}

#[tokio::test]
async fn two_jobs_that_are_due_together_both_run() -> Result<(), BoxError> {
    let fixture = Fixture::new(READY).await?;
    fixture.enable_on("0 3 * * *", ENABLED_AT).await?;
    // A second job of the same workflow, enabled the same way.
    write(&fixture.jobs_root().join("acme/sync/jobs/weekly.toml"), READY)?;
    fixture.pass(ENABLED_AT).await?;
    let second = contextos_web::shared_claim::SharedJobKey {
        package: "acme".to_owned(),
        workflow: "sync".to_owned(),
        job: "weekly".to_owned(),
    };
    let version = fixture
        .read()?
        .job("acme", "sync", "weekly")
        .and_then(|job| job.fingerprint().cloned())
        .ok_or("the second job was not validated")?;
    for (actor, transition) in [
        (Actor::DryRun, Transition::DryRunCompleted(version.clone())),
        (
            Actor::Operator,
            Transition::Enable(contextos_workflows::JobSchedule::try_from("0 3 * * *")?, version),
        ),
    ] {
        fixture.change_job(&second, actor, transition, ENABLED_AT).await?;
    }

    let report = poll(&fixture, &DUE).await;

    assert_eq!(report.spawned, 2);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while records(&fixture.runs_root())
        .iter()
        .filter(|(_, text)| text.contains("status: succeeded"))
        .count()
        < 2
    {
        if tokio::time::Instant::now() >= deadline {
            return Err("both due jobs should have run".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(())
}

#[tokio::test]
async fn shared_jobs_run_with_no_vault_configured() -> Result<(), BoxError> {
    let fixture = Fixture::zero_vaults(READY).await?;
    fixture.enable_on("0 3 * * *", ENABLED_AT).await?;

    let report = poll(&fixture, &DUE).await;

    assert_eq!(report.spawned, 1);
    wait_for_record(&fixture, "status: succeeded").await?;
    Ok(())
}

#[tokio::test]
async fn a_failed_first_scheduled_run_suspends_the_job() -> Result<(), BoxError> {
    let fixture = Fixture::new(READY).await?;
    fixture.install("1.0.0", support::shared::DIGEST, "throw \"the crm is down\";")?;
    fixture.enable_on("0 3 * * *", ENABLED_AT).await?;

    poll(&fixture, &DUE).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while fixture.lifecycle()? != "suspended" {
        if tokio::time::Instant::now() >= deadline {
            return Err("the failed first run did not suspend the job".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let state = fixture.read()?;
    let suspension = state
        .job("acme", "sync", "nightly")
        .and_then(contextos_workflows::JobState::suspension);
    assert_eq!(
        suspension.map(|suspension| suspension.code.as_str()),
        Some("workflow/probation-failed")
    );
    assert_eq!(
        suspension.and_then(|suspension| suspension.by),
        Some(contextos_workflows::Actor::System)
    );
    assert!(
        suspension.is_some_and(|suspension| suspension.reason.contains("the crm is down")),
        "{suspension:?}"
    );
    let numbers: Vec<u64> = records(&fixture.runs_root())
        .iter()
        .filter_map(|(path, _)| path.file_stem()?.to_str()?.strip_prefix("R-")?.parse().ok())
        .collect();
    let [number] = numbers.as_slice() else {
        return Err(format!("expected the one run that failed, found runs {numbers:?}").into());
    };
    assert_eq!(
        suspension.and_then(|suspension| suspension.run),
        Some(*number),
        "the suspension names the run that failed the trial, which the scheduler's own run supplies"
    );
    Ok(())
}

#[tokio::test]
async fn the_pass_validates_a_ready_job_before_anything_is_scheduled() -> Result<(), BoxError> {
    let fixture = Fixture::new(READY).await?;

    poll(&fixture, &DUE).await;

    assert_eq!(fixture.lifecycle()?, "validated");
    Ok(())
}

#[tokio::test]
async fn a_new_version_is_reviewed_and_put_on_trial_in_the_same_poll_that_runs_it() -> Result<(), BoxError> {
    let fixture = Fixture::new(READY).await?;
    fixture.enable().await?;
    fixture
        .change(
            Actor::Operator,
            Transition::SetSchedule(contextos_workflows::JobSchedule::try_from("0 3 * * *")?),
            ENABLED_AT,
        )
        .await?;
    let before = fixture.recorded_version()?;

    // A changed timeout is a new version of the same release, which the job's file may change in place.
    write(
        &fixture.job_path(),
        &READY.replace("timeout_secs = 30", "timeout_secs = 60"),
    )?;
    let report = poll(&fixture, &DUE).await;

    assert_eq!(report.spawned, 1, "reviewed, then run");
    wait_for_record(&fixture, "status: succeeded").await?;
    assert_ne!(
        fixture.recorded_version()?,
        before,
        "the new version was recorded by the review"
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while fixture.on_trial()? {
        if tokio::time::Instant::now() >= deadline {
            return Err("the first run of the new version did not end its trial".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(())
}

#[tokio::test]
async fn a_due_job_whose_version_has_not_been_reviewed_is_held_back_so_a_failure_cannot_go_unsettled()
-> Result<(), BoxError> {
    let fixture = Fixture::new(READY).await?;
    fixture.enable_on("0 3 * * *", ENABLED_AT).await?;
    // The job file came to name a newer release and the review that would put it on trial has not happened, as it would
    // not have if the pass had been dropped.
    fixture.install("1.1.0", OTHER_DIGEST, "throw \"broken\";")?;
    fixture.name_release("1.1.0")?;
    let (host, shared) = host_for(&fixture);

    let report = start_due_shared_jobs(&fixture.clients, &host, &shared, &DUE).await;

    assert_eq!(report.spawned, 0);
    assert_eq!(report.held_back, 1);
    let runs = records(&fixture.runs_root());
    assert!(runs.is_empty(), "no run should be recorded, got {runs:?}");
    assert_eq!(
        fixture.read()?.last_scheduled_fire("acme", "sync", "nightly"),
        Some(ENABLED_AT),
        "the tick stays due"
    );
    Ok(())
}

#[tokio::test]
async fn a_workflow_that_makes_no_call_needs_no_server_to_run() -> Result<(), BoxError> {
    let fixture = Fixture::new(READY).await?;
    fixture.enable_on("0 3 * * *", ENABLED_AT).await?;
    let (host, shared) = host_for(&fixture);
    let empty = Arc::new(McpClientSet::connect(&[]).await?);

    let report = poll_shared_once(&empty, &host, &shared, &DUE).await;

    assert_eq!(report.spawned, 1);
    wait_for_record(&fixture, "status: succeeded").await?;
    Ok(())
}

const READY_WITH_TIMEOUT: &str = "name = \"Nightly\"\nready = true\ntimeout_secs = 30\nversion = \"1.0.0\"\n";

#[tokio::test]
async fn an_enabled_shared_job_without_timeout_secs_is_suspended_on_the_first_pass_under_the_new_rules()
-> Result<(), BoxError> {
    // The job was enabled before `timeout_secs` was required: its validation passed under the older rules and is stored
    // for the version it still has, so only the tightened rules can fault it.
    let fixture = Fixture::new(READY_WITHOUT_TIMEOUT).await?;
    let version =
        contextos_web::shared_version::shared_job_version(&fixture.store_root(), "acme", READY_WITHOUT_TIMEOUT)
            .ok_or("the installed job has a version")?;
    let version = String::from(version);
    write(
        &fixture.state().state_path(),
        &format!(
            "[acme.sync.nightly]\nlast_scheduled_fire = \"2026-10-05T02:00:00Z\"\n\
             favourite = false\nlifecycle = \"enabled\"\nschedule = \"0 3 * * *\"\n\
             lifecycle_changed = \"2026-10-05T02:00:00Z\"\nlifecycle_actor = \"operator\"\nfingerprint = \"{version}\"\n\n\
             [acme.sync.nightly.validation]\nat = \"2026-10-04T03:00:00Z\"\n\
             fingerprint = \"{version}\"\npassed = true\nfailures = []\nunverifiable = []\n"
        ),
    )?;
    assert_eq!(fixture.lifecycle()?, "enabled");

    fixture.pass(ENABLED_AT).await?;

    assert_eq!(
        fixture.lifecycle()?,
        "suspended",
        "the job file no longer meets the rules"
    );
    let state = fixture.read()?;
    let job = state.job("acme", "sync", "nightly").ok_or("the job has no state")?;
    let record = job.suspension().ok_or("the suspension was not stored as a record")?;
    assert_eq!(record.code, "workflow/validation-failed");
    assert_eq!(record.by, Some(Actor::System));
    assert!(
        record.reason.contains("timeout_secs"),
        "the reason names the missing key: {}",
        record.reason
    );
    assert_eq!(state.scheduled_expression("acme", "sync", "nightly"), None);
    Ok(())
}

fn files_under(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

#[tokio::test]
async fn no_task_is_raised_for_a_shared_job() -> Result<(), BoxError> {
    let fixture = Fixture::new(READY_WITH_TIMEOUT).await?;
    // The server's vault has a working task system, which a job that belonged to a vault would be raised in.
    support::jobs::seed_tasks(
        fixture.vault_root(),
        "---\nkey_prefix: A\nkey_format: '{key_prefix}-{key_id}'\nlast_id: 0\n\
        last_create: 2026-01-01T00:00:00Z\nlast_update: 2026-01-01T00:00:00Z\n---\n",
    )?;
    fixture.install("1.0.0", support::shared::DIGEST, "throw \"the crm is down\";")?;
    fixture.enable_on("0 3 * * *", ENABLED_AT).await?;
    let vault_before = files_under(fixture.vault_root());
    let tasks_base_before = std::fs::read_to_string(fixture.vault_root().join("memory/tasks.md"))?;

    poll(&fixture, &DUE).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while fixture.lifecycle()? != "suspended" {
        if tokio::time::Instant::now() >= deadline {
            return Err("the failed first run did not suspend the job".into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Further passes would raise it too if anything did.
    poll(&fixture, &NOT_YET).await;

    assert_eq!(
        files_under(fixture.vault_root()),
        vault_before,
        "the vault was not written to"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.vault_root().join("memory/tasks.md"))?,
        tasks_base_before,
        "no task record was created, so the key counter did not move"
    );

    // Positive control: the same vault, server, and pass raise a task for a vault job that fails. Without this the
    // silence above could be a harness that never raises anything.
    let root = fixture.vault_root();
    write(&root.join(".contextos/team.toml"), support::jobs::TEAM)?;
    write(
        &root.join("registry/workflows/ops/workflows.toml"),
        "workflows = [\"sync\"]\n",
    )?;
    write(
        &root.join("registry/workflows/ops/sync/workflow.toml"),
        support::jobs::WORKFLOW,
    )?;
    write(&root.join("registry/workflows/ops/sync/sync.rhai"), "let y = (2 +;")?;
    write(
        &root.join("registry/workflows/ops/sync/jobs/nightly.toml"),
        support::jobs::READY_JOB,
    )?;
    let (host, _) = host_for(&fixture);
    contextos_web::scheduler::poll_once(&fixture.clients, &host, &NOT_YET).await;
    assert_ne!(
        std::fs::read_to_string(root.join("memory/tasks.md"))?,
        tasks_base_before,
        "a vault job that fails validation raises a task in the same harness"
    );
    Ok(())
}

/// An enabled job on a package that depends on `acme-text`, whose library package has then been removed from the host.
async fn enabled_job_whose_library_package_is_gone() -> Result<Fixture, BoxError> {
    let fixture = Fixture::new(READY).await?;
    fixture.install_library("acme-text", "1.0.0", "strings", "fn shout() { 1 }")?;
    fixture.install_depending_on(
        "1.0.0",
        "import \"acme-text:strings\" as s;\ns::shout();",
        &[("acme-text", "^1")],
    )?;
    fixture.enable_on("0 3 * * *", ENABLED_AT).await?;
    std::fs::remove_dir_all(fixture.store_root().join("acme-text"))?;
    Ok(fixture)
}

#[tokio::test]
async fn a_due_job_whose_library_package_is_gone_fails_before_the_claim_and_leaves_no_run_record()
-> Result<(), BoxError> {
    let fixture = enabled_job_whose_library_package_is_gone().await?;
    let (host, shared) = host_for(&fixture);

    let report = start_due_shared_jobs(&fixture.clients, &host, &shared, &DUE).await;

    assert_eq!(report.spawned, 0);
    assert_eq!(report.held_back, 1);
    let found = records(&fixture.runs_root());
    assert!(
        found.is_empty(),
        "a failure before the claim leaves no record: {found:?}"
    );
    assert_eq!(
        fixture.read()?.last_scheduled_fire("acme", "sync", "nightly"),
        Some(ENABLED_AT),
        "the tick was not claimed, so it stays due"
    );
    Ok(())
}

#[tokio::test]
async fn an_enabled_job_whose_library_package_is_gone_is_suspended_as_a_job_whose_own_package_is_gone_is()
-> Result<(), BoxError> {
    let fixture = enabled_job_whose_library_package_is_gone().await?;

    let report = poll(&fixture, &DUE).await;

    assert_eq!(report.spawned, 0);
    assert_eq!(records(&fixture.runs_root()), Vec::new());
    assert_eq!(fixture.lifecycle()?, "suspended");
    let state = fixture.read()?;
    let suspension = state
        .job("acme", "sync", "nightly")
        .and_then(contextos_workflows::JobState::suspension);
    assert_eq!(
        suspension.map(|suspension| suspension.code.as_str()),
        Some("workflow/package-unavailable")
    );
    assert!(
        suspension.is_some_and(|suspension| suspension.reason.contains("acme-text")),
        "{suspension:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_due_job_runs_with_its_locked_libraries_when_a_newer_library_release_is_installed() -> Result<(), BoxError> {
    let fixture = Fixture::new(READY).await?;
    fixture.install_library("acme-text", "1.0.0", "strings", "fn shout() { 1 }")?;
    fixture.install_depending_on(
        "1.0.0",
        "import \"acme-text:strings\" as s;\ns::shout();",
        &[("acme-text", "^1")],
    )?;
    fixture.enable_on("0 3 * * *", ENABLED_AT).await?;
    // A newer release that satisfies the job's requirement does not change what the job runs, so it is not a new version.
    fixture.install_library("acme-text", "1.1.0", "strings", "fn shout() { 2 }")?;
    let (host, shared) = host_for(&fixture);

    let report = start_due_shared_jobs(&fixture.clients, &host, &shared, &DUE).await;

    assert_eq!(report.held_back, 0);
    assert_eq!(report.spawned, 1);
    wait_for_record(&fixture, "status: succeeded").await?;
    Ok(())
}
