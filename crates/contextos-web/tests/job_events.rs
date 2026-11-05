//! Events the Web scheduler records for vault jobs: one scheduler pass over a real temporary vault with a real
//! `contextos` session records `job.failed` for each failed scheduled run, `job.suspended` for each system suspension,
//! and `job.recovered` for the first scheduled success after a failure, once each however many passes or restarts come
//! between, and never for runs started by hand, dry runs, or any other outcome.

mod support;

use contextos_workflows::JobsState;
use support::events::{EVENTS_SIDECAR_WITHOUT_KINDS, events_of_kind, job_event_notes, seed_events};
use support::jobs::{
    FAILING_SCRIPT, JobVault, Spec, enabled_state, frontmatter_field, lifecycle_state, suspended_state,
};
use time::macros::datetime;
use time::{Duration as Span, OffsetDateTime};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const NOW: OffsetDateTime = datetime!(2026-10-01 03:00:30 UTC);

fn minute(n: i64) -> OffsetDateTime {
    NOW + Span::minutes(n)
}

/// An enabled job whose script fails, with a task system and the events base, and a schedule that never falls due.
async fn vault_with_events() -> Result<JobVault, BoxError> {
    vault_with_events_logging(false).await
}

/// As [`vault_with_events`], with the server's log kept for the test to read.
async fn logged_vault_with_events() -> Result<JobVault, BoxError> {
    vault_with_events_logging(true).await
}

async fn vault_with_events_logging(logged: bool) -> Result<JobVault, BoxError> {
    let spec = Spec {
        script: FAILING_SCRIPT,
        ..Spec::default()
    };
    let state = enabled_state(&spec).replace("schedule = \"* * * * *\"", "schedule = \"0 0 1 1 *\"");
    let spec = Spec {
        state: Some(&state),
        ..spec
    };
    let vault = if logged {
        JobVault::open_logged(&spec).await?
    } else {
        JobVault::open(&spec).await?
    };
    seed_events(vault.root())?;
    Ok(vault)
}

/// Runs the job once by hand and rewrites that run's record into `trigger`, `status`, and `code`, so a test can say how
/// each run ended. Returns the record's vault path.
async fn run_as(
    vault: &JobVault,
    trigger: &str,
    status: &str,
    code: Option<&str>,
    dry_run: bool,
) -> Result<String, BoxError> {
    let before = vault.run_records().len();
    support::run_job(
        std::sync::Arc::clone(&vault.clients),
        &vault.host,
        support::jobs::VAULT_NAME,
        "ops",
        "sync",
        "nightly",
        contextos_web::cli_run::CliRunOptions {
            dry_run_scenario: None,
            record: true,
        },
    )
    .await?;
    let mut records = vault.run_records();
    if records.len() != before + 1 {
        return Err(format!("expected one new run record, found {}", records.len() - before).into());
    }
    records.sort_by_key(|(path, _)| {
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.strip_prefix("R-"))
            .and_then(|number| number.parse::<u64>().ok())
            .unwrap_or(0)
    });
    let (path, text) = records.pop().ok_or("no record")?;
    let code_line = code.map_or_else(|| "code: ~".to_owned(), |code| format!("code: {code}"));
    let rewritten: Vec<String> = text
        .lines()
        .filter(|line| !line.starts_with("code:"))
        .flat_map(|line| {
            if line.starts_with("trigger:") {
                vec![format!("trigger: {trigger}")]
            } else if line.starts_with("status:") {
                vec![format!("status: {status}")]
            } else if line.starts_with("dry_run:") {
                vec![format!("dry_run: {dry_run}")]
            } else if line.starts_with("error:") {
                vec![line.to_owned(), code_line.clone()]
            } else {
                vec![line.to_owned()]
            }
        })
        .collect();
    std::fs::write(&path, rewritten.join("\n") + "\n")?;
    let relative = path.strip_prefix(vault.root())?.to_string_lossy().replace('\\', "/");
    Ok(relative)
}

#[tokio::test]
async fn a_failed_scheduled_run_records_one_job_failed_event_however_many_passes_follow() -> Result<(), BoxError> {
    let vault = vault_with_events().await?;
    let record = run_as(&vault, "scheduled", "failed", Some("workflow/script-error"), false).await?;

    for n in 0..4 {
        vault.pass(minute(n)).await;
    }

    let failed = events_of_kind(vault.root(), "job.failed");
    assert_eq!(failed.len(), 1, "{failed:?}");
    let (_, text) = &failed[0];
    assert_eq!(frontmatter_field(text, "severity").as_deref(), Some("error"), "{text}");
    assert_eq!(frontmatter_field(text, "workflow").as_deref(), Some("sync"), "{text}");
    assert_eq!(frontmatter_field(text, "job").as_deref(), Some("nightly"), "{text}");
    assert_eq!(frontmatter_field(text, "run").as_deref(), Some("R-1"), "{text}");
    assert_eq!(
        frontmatter_field(text, "code").as_deref(),
        Some("workflow/script-error"),
        "{text}"
    );
    assert_eq!(frontmatter_field(text, "entity").as_deref(), Some("vault"), "{text}");
    let subject = frontmatter_field(text, "subject").ok_or("the event names its subject")?;
    assert!(
        subject.contains(record.trim_end_matches(".md")),
        "{subject} should link {record}"
    );
    Ok(())
}

#[tokio::test]
async fn a_restarted_scheduler_records_no_second_event_for_a_failure_it_already_recorded() -> Result<(), BoxError> {
    let vault = vault_with_events().await?;
    run_as(&vault, "scheduled", "failed", Some("workflow/script-error"), false).await?;
    vault.pass(NOW).await;
    assert_eq!(events_of_kind(vault.root(), "job.failed").len(), 1);

    let vault = vault.restarted().await?;
    vault.pass(minute(1)).await;

    assert_eq!(
        events_of_kind(vault.root(), "job.failed").len(),
        1,
        "the server refuses the repeat"
    );
    Ok(())
}

#[tokio::test]
async fn every_failed_scheduled_run_records_its_own_event_even_when_one_task_covers_them() -> Result<(), BoxError> {
    let vault = vault_with_events().await?;
    run_as(&vault, "scheduled", "failed", Some("workflow/script-error"), false).await?;
    run_as(&vault, "scheduled", "failed", Some("workflow/timed-out"), false).await?;

    vault.pass(NOW).await;

    let mut runs: Vec<String> = events_of_kind(vault.root(), "job.failed")
        .iter()
        .filter_map(|(_, text)| frontmatter_field(text, "run"))
        .collect();
    runs.sort();
    assert_eq!(runs, vec!["R-1", "R-2"]);
    Ok(())
}

#[tokio::test]
async fn runs_started_by_hand_dry_runs_and_other_outcomes_record_no_event() -> Result<(), BoxError> {
    let vault = vault_with_events().await?;
    run_as(&vault, "manual", "failed", Some("workflow/script-error"), false).await?;
    run_as(&vault, "scheduled", "failed", Some("workflow/script-error"), true).await?;
    run_as(&vault, "scheduled", "cancelled", None, false).await?;
    run_as(&vault, "scheduled", "interrupted", None, false).await?;
    run_as(&vault, "scheduled", "degraded", None, false).await?;

    vault.pass(NOW).await;
    vault.pass(minute(1)).await;

    assert_eq!(
        job_event_notes(vault.root()).len(),
        0,
        "{:?}",
        job_event_notes(vault.root())
    );
    Ok(())
}

#[tokio::test]
async fn the_first_scheduled_success_after_a_failure_records_one_job_recovered_event() -> Result<(), BoxError> {
    let vault = vault_with_events().await?;
    run_as(&vault, "scheduled", "failed", Some("workflow/script-error"), false).await?;
    let record = run_as(&vault, "scheduled", "succeeded", None, false).await?;
    run_as(&vault, "scheduled", "succeeded", None, false).await?;

    vault.pass(NOW).await;
    vault.pass(minute(1)).await;

    let recovered = events_of_kind(vault.root(), "job.recovered");
    assert_eq!(
        recovered.len(),
        1,
        "only the first success after the failure: {recovered:?}"
    );
    let (_, text) = &recovered[0];
    assert_eq!(frontmatter_field(text, "severity").as_deref(), Some("info"), "{text}");
    assert_eq!(frontmatter_field(text, "run").as_deref(), Some("R-2"), "{text}");
    let subject = frontmatter_field(text, "subject").ok_or("the event names its subject")?;
    assert!(subject.contains(record.trim_end_matches(".md")), "{subject}");
    Ok(())
}

#[tokio::test]
async fn a_success_that_follows_a_success_records_no_recovery() -> Result<(), BoxError> {
    let vault = vault_with_events().await?;
    run_as(&vault, "scheduled", "succeeded", None, false).await?;
    run_as(&vault, "scheduled", "succeeded", None, false).await?;

    vault.pass(NOW).await;

    assert_eq!(events_of_kind(vault.root(), "job.recovered").len(), 0);
    Ok(())
}

#[tokio::test]
async fn a_system_suspension_records_one_job_suspended_event() -> Result<(), BoxError> {
    let spec = Spec {
        script: FAILING_SCRIPT,
        ..Spec::default()
    };
    let state = format!(
        "{}reason = \"x\"\n\n[ops.sync.nightly.suspension]\ncode = \"workflow/validation-failed\"\n\
         by = \"system\"\nat = \"2026-09-30T10:00:00Z\"\nreason = \"The new version failed validation.\"\n",
        suspended_state(&spec).replace("lifecycle_actor = \"operator\"\n", "lifecycle_actor = \"system\"\n")
    );
    let state = state.replace("reason = \"x\"\n\n", "\n");
    let vault = JobVault::open(&Spec {
        state: Some(&state),
        ..spec
    })
    .await?;
    seed_events(vault.root())?;
    let _: JobsState = vault.state()?;

    vault.pass(NOW).await;
    vault.pass(minute(1)).await;

    let suspended = events_of_kind(vault.root(), "job.suspended");
    assert_eq!(suspended.len(), 1, "{suspended:?}");
    let (_, text) = &suspended[0];
    assert_eq!(
        frontmatter_field(text, "severity").as_deref(),
        Some("warning"),
        "{text}"
    );
    assert_eq!(
        frontmatter_field(text, "code").as_deref(),
        Some("workflow/validation-failed"),
        "{text}"
    );
    assert_eq!(
        frontmatter_field(text, "suspended_at").as_deref(),
        Some("2026-09-30T10:00:00Z"),
        "{text}"
    );
    let subject = frontmatter_field(text, "subject").ok_or("the event names its subject")?;
    assert!(
        subject.contains("registry/workflows/ops/sync/jobs/nightly.toml"),
        "{subject}"
    );
    Ok(())
}

#[tokio::test]
async fn a_job_retired_by_its_successors_enabling_records_one_info_job_retired_event_naming_the_successor()
-> Result<(), BoxError> {
    let spec = Spec::default();
    let state = String::from(
        "[ops.sync.nightly]\nlifecycle = \"retired\"\nlifecycle_changed = \"2026-09-30T10:00:00Z\"\n\
         lifecycle_actor = \"system\"\n\n[ops.sync.nightly.retirement]\nby = \"system\"\n\
         cause = \"successor-enabled\"\nat = \"2026-09-30T10:00:00Z\"\n\
         replaced_by = { job = \"nightly-v2\", identity = \"job-2\" }\n",
    );
    let vault = JobVault::open(&Spec {
        state: Some(&state),
        ..spec
    })
    .await?;
    seed_events(vault.root())?;

    vault.pass(NOW).await;
    vault.pass(minute(1)).await;

    let retired = events_of_kind(vault.root(), "job.retired");
    assert_eq!(retired.len(), 1, "{retired:?}");
    let (_, text) = &retired[0];
    assert_eq!(frontmatter_field(text, "severity").as_deref(), Some("info"), "{text}");
    assert_eq!(frontmatter_field(text, "package").as_deref(), Some("ops"), "{text}");
    assert_eq!(
        frontmatter_field(text, "replaced_by").as_deref(),
        Some("nightly-v2"),
        "{text}"
    );
    assert_eq!(
        frontmatter_field(text, "retired_at").as_deref(),
        Some("2026-09-30T10:00:00Z"),
        "{text}"
    );
    assert!(
        vault.raised_tasks().is_empty(),
        "a retirement follows the operator's act, so it raises no task"
    );
    Ok(())
}

/// A job the system suspended on 30 September, whose suspension the first pass records.
async fn vault_suspended_by_the_system() -> Result<JobVault, BoxError> {
    let spec = Spec {
        script: FAILING_SCRIPT,
        ..Spec::default()
    };
    let state = format!(
        "{}\n[ops.sync.nightly.suspension]\ncode = \"workflow/validation-failed\"\n\
         by = \"system\"\nat = \"2026-09-30T10:00:00Z\"\nreason = \"The new version failed validation.\"\n",
        suspended_state(&spec).replace("lifecycle_actor = \"operator\"\n", "lifecycle_actor = \"system\"\n")
    );
    let vault = JobVault::open(&Spec {
        state: Some(&state),
        ..spec
    })
    .await?;
    seed_events(vault.root())?;
    Ok(vault)
}

/// Resumes the job on 30 September, after its suspension, with a schedule that never falls due.
fn resume(vault: &JobVault) -> Result<(), BoxError> {
    let spec = Spec {
        script: FAILING_SCRIPT,
        ..Spec::default()
    };
    let resumed = enabled_state(&spec)
        .replace("schedule = \"* * * * *\"", "schedule = \"0 0 1 1 *\"")
        .replace("2026-09-01T00:00:00Z", "2026-09-30T12:00:00Z");
    std::fs::write(vault.root().join(support::jobs::STATE_PATH), resumed)?;
    Ok(())
}

#[tokio::test]
async fn the_first_scheduled_success_after_a_suspension_was_resumed_records_one_job_recovered_event()
-> Result<(), BoxError> {
    let vault = vault_suspended_by_the_system().await?;
    vault.pass(NOW).await;
    assert_eq!(events_of_kind(vault.root(), "job.suspended").len(), 1);
    resume(&vault)?;
    let record = run_as(&vault, "scheduled", "succeeded", None, false).await?;

    vault.pass(minute(1)).await;
    vault.pass(minute(2)).await;

    let recovered = events_of_kind(vault.root(), "job.recovered");
    assert_eq!(recovered.len(), 1, "{recovered:?}");
    let (_, text) = &recovered[0];
    assert_eq!(frontmatter_field(text, "run").as_deref(), Some("R-1"), "{text}");
    assert_eq!(frontmatter_field(text, "workflow").as_deref(), Some("sync"), "{text}");
    assert!(
        frontmatter_field(text, "subject").is_some_and(|subject| subject.contains(record.trim_end_matches(".md"))),
        "{text}"
    );
    Ok(())
}

#[tokio::test]
async fn a_first_success_of_a_job_that_was_never_suspended_records_no_recovery() -> Result<(), BoxError> {
    let vault = vault_with_events().await?;
    run_as(&vault, "scheduled", "succeeded", None, false).await?;

    vault.pass(NOW).await;

    assert_eq!(events_of_kind(vault.root(), "job.recovered").len(), 0);
    Ok(())
}

#[tokio::test]
async fn a_vault_without_an_events_base_records_nothing_and_still_raises_its_tasks() -> Result<(), BoxError> {
    let vault = vault_with_events().await?;
    std::fs::remove_file(vault.root().join("memory/events.base"))?;
    std::fs::remove_file(vault.root().join("memory/events.md"))?;
    run_as(&vault, "scheduled", "failed", Some("workflow/script-error"), false).await?;

    vault.pass(NOW).await;
    vault.pass(minute(1)).await;

    assert_eq!(job_event_notes(vault.root()).len(), 0);
    assert_eq!(vault.raised_tasks_for("run-failed").len(), 1, "tasks are unaffected");
    Ok(())
}

#[tokio::test]
async fn a_vault_whose_sidecar_declares_no_kind_registry_records_nothing() -> Result<(), BoxError> {
    let vault = vault_with_events().await?;
    vault.write("memory/events.md", EVENTS_SIDECAR_WITHOUT_KINDS)?;
    run_as(&vault, "scheduled", "failed", Some("workflow/script-error"), false).await?;

    vault.pass(NOW).await;

    assert_eq!(job_event_notes(vault.root()).len(), 0);
    Ok(())
}

#[tokio::test]
async fn a_vault_that_does_not_declare_the_kind_records_nothing_until_it_does() -> Result<(), BoxError> {
    let vault = vault_with_events().await?;
    std::fs::remove_file(vault.root().join("registry/events/job.failed.md"))?;
    run_as(&vault, "scheduled", "failed", Some("workflow/script-error"), false).await?;

    vault.pass(NOW).await;
    assert_eq!(job_event_notes(vault.root()).len(), 0);

    for (path, text) in support::events::job_kinds() {
        vault.write(path, &text)?;
    }
    // The unsupported vault is looked at again after a pause, never on every pass.
    vault.pass(minute(10)).await;

    assert_eq!(events_of_kind(vault.root(), "job.failed").len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_record_that_cannot_be_made_is_tried_again_on_the_next_pass() -> Result<(), BoxError> {
    let vault = logged_vault_with_events().await?;
    run_as(&vault, "scheduled", "failed", Some("workflow/script-error"), false).await?;
    // A file where the month folder must go makes the write fail, a refusal that is not about the vault's shape.
    vault.write("memory/events", "not a folder")?;

    vault.pass(NOW).await;
    assert_eq!(events_of_kind(vault.root(), "job.failed").len(), 0);
    // The server says so, by kind and code, for each note it could not write, rather than leave the refusal unremarked.
    let log = std::fs::read_to_string(vault.server_log())?;
    for kind in ["task.created", "vault.degraded"] {
        assert!(
            log.lines().any(|line| line.contains("WARN")
                && line.contains("an event note could not be written")
                && line.contains(&format!("kind={kind} "))
                && line.contains("code=path/invalid")),
            "the server warns that the {kind} note could not be written: {log}"
        );
    }

    std::fs::remove_file(vault.root().join("memory/events"))?;
    vault.pass(minute(1)).await;

    assert_eq!(events_of_kind(vault.root(), "job.failed").len(), 1);
    Ok(())
}

/// Rewrites when the run at `record` (a vault path) started.
fn set_started(vault: &JobVault, record: &str, started: &str) -> Result<(), BoxError> {
    let text = vault.read(record)?;
    let rewritten: Vec<String> = text
        .lines()
        .map(|line| {
            if line.starts_with("started:") {
                format!("started: {started}")
            } else {
                line.to_owned()
            }
        })
        .collect();
    vault.write(record, &(rewritten.join("\n") + "\n"))?;
    Ok(())
}

#[tokio::test]
async fn a_failed_scheduled_run_of_a_job_retired_since_is_still_recorded() -> Result<(), BoxError> {
    let vault = vault_with_events().await?;
    run_as(&vault, "scheduled", "failed", Some("workflow/script-error"), false).await?;
    let spec = Spec {
        script: FAILING_SCRIPT,
        ..Spec::default()
    };
    vault.write(support::jobs::STATE_PATH, &lifecycle_state(&spec, "retired"))?;

    vault.pass(NOW).await;

    assert_eq!(events_of_kind(vault.root(), "job.failed").len(), 1);
    Ok(())
}

#[tokio::test]
async fn only_the_first_reconcile_looks_back_seven_days_and_a_later_one_records_everything_since()
-> Result<(), BoxError> {
    let vault = vault_with_events().await?;
    let old = run_as(&vault, "scheduled", "failed", Some("workflow/script-error"), false).await?;
    set_started(&vault, &old, "2026-09-01T00:00:00Z")?;
    let recent = run_as(&vault, "scheduled", "failed", Some("workflow/script-error"), false).await?;
    set_started(&vault, &recent, "2026-09-30T00:00:00Z")?;

    vault.pass(NOW).await;
    let first = events_of_kind(vault.root(), "job.failed");
    assert_eq!(
        first.len(),
        1,
        "the failure from before the lookback is not recorded as new: {first:?}"
    );
    assert_eq!(frontmatter_field(&first[0].1, "run").as_deref(), Some("R-2"));

    // A restarted scheduler remembers nothing but what the host persisted, and a failure that happened after the first
    // reconcile is recorded however old it has become by the time it is read.
    let vault = vault.restarted().await?;
    let later = run_as(&vault, "scheduled", "failed", Some("workflow/script-error"), false).await?;
    set_started(&vault, &later, "2026-10-02T00:00:00Z")?;
    vault.pass(NOW + Span::days(30)).await;

    let all = events_of_kind(vault.root(), "job.failed");
    let mut runs: Vec<String> = all
        .iter()
        .filter_map(|(_, text)| frontmatter_field(text, "run"))
        .collect();
    runs.sort();
    assert_eq!(
        runs,
        ["R-2", "R-3"],
        "the old run is still never recorded, the new one is"
    );
    Ok(())
}

#[tokio::test]
async fn a_reconcile_note_that_is_not_a_time_keeps_the_lookback_and_is_left_as_it_is() -> Result<(), BoxError> {
    let vault = vault_with_events().await?;
    let old = run_as(&vault, "scheduled", "failed", Some("workflow/script-error"), false).await?;
    set_started(&vault, &old, "2026-09-01T00:00:00Z")?;
    let recent = run_as(&vault, "scheduled", "failed", Some("workflow/script-error"), false).await?;
    set_started(&vault, &recent, "2026-09-30T00:00:00Z")?;
    let note = contextos_web::job_events_marker::marker_path(&vault.host, "contextos").ok_or("no note path")?;
    std::fs::create_dir_all(note.parent().ok_or("no parent")?)?;
    std::fs::write(&note, "garbled\n")?;

    vault.pass(NOW).await;

    let recorded = events_of_kind(vault.root(), "job.failed");
    assert_eq!(
        recorded.len(),
        1,
        "only the run inside the lookback is recorded: {recorded:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&note)?,
        "garbled\n",
        "the note is not overwritten with a later time, which would move the floor forward for good"
    );
    Ok(())
}
