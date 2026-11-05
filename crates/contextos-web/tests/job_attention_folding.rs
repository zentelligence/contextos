//! Which failures of a vault job raise a task, decided from what is durably there: the run history and the tasks that
//! already exist, never from what one scheduler process happens to remember. A failure folded into an open task never
//! comes back as a task of its own, a restart or a dry run changes nothing about that, and a run whose script never
//! started is not a defect in the job. Real `contextos` sessions over real temporary vaults, with a fixed clock.

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use contextos_web::cli_run::CliRunOptions;
use support::jobs::{FAILING_SCRIPT, FixedClock, JobVault, Spec, VAULT_NAME, enabled_state, suspended_state};
use support::{BoxError, run_job};
use time::macros::datetime;
use time::{Duration as Span, OffsetDateTime};

const NOW: OffsetDateTime = datetime!(2026-10-01 03:00:30 UTC);

fn field(text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    text.lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .map(|value| value.trim().trim_matches(|c| c == '\'' || c == '"').to_owned())
}

/// Passes at the minute `n` after `NOW`, each inside its own tick.
fn minute(n: i64) -> OffsetDateTime {
    NOW + Span::minutes(n)
}

/// Polls `condition` until it holds. The deadline only ends a wait that can never finish.
async fn eventually(what: &str, mut condition: impl FnMut() -> bool) -> Result<(), BoxError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !condition() {
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("timed out waiting for {what}").into());
        }
        // A pause, not a bare yield: each check walks the filesystem, blocking the one thread the run needs as well.
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    Ok(())
}

/// Waits until `count` run records exist and none of them is still running.
async fn runs_finished(vault: &JobVault, count: usize) -> Result<(), BoxError> {
    eventually("the runs to finish", || {
        let records = vault.run_records();
        records.len() == count && records.iter().all(|(_, text)| !text.contains("status: running"))
    })
    .await?;
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    Ok(())
}

/// A vault whose enabled, scheduled job is due on every pass and whose script fails, past its trial.
async fn failing_scheduled() -> Result<JobVault, BoxError> {
    let spec = Spec {
        script: FAILING_SCRIPT,
        ..Spec::default()
    };
    let state = enabled_state(&spec);
    JobVault::open(&Spec {
        state: Some(&state),
        ..spec
    })
    .await
}

fn close_every_task(vault: &JobVault) -> Result<(), BoxError> {
    for (path, text) in vault.raised_tasks() {
        std::fs::write(path, text.replace("state: next", "state: done"))?;
    }
    Ok(())
}

fn run_failed_origins(vault: &JobVault) -> Vec<String> {
    let mut origins: Vec<String> = vault
        .raised_tasks_for("run-failed")
        .iter()
        .filter_map(|(_, text)| field(text, "origin"))
        .collect();
    origins.sort();
    origins
}

fn origin(number: u64) -> String {
    format!("contextos-web:run-failed:ops/sync/nightly:R-{number}")
}

/// R-1 fails and raises a task, R-2 fails while it is open and is folded into it, and the task is then closed. Returns
/// the vault at that point, with its records numbered R-1 and R-2.
async fn vault_with_a_folded_failure_and_a_closed_task() -> Result<JobVault, BoxError> {
    let vault = failing_scheduled().await?;
    vault.pass(NOW).await;
    runs_finished(&vault, 1).await?;
    vault.pass(NOW + Span::seconds(10)).await;
    assert_eq!(run_failed_origins(&vault), vec![origin(1)], "R-1 raises the task");

    vault.pass(minute(2)).await;
    runs_finished(&vault, 2).await?;
    vault.pass(minute(2) + Span::seconds(10)).await;
    assert_eq!(run_failed_origins(&vault), vec![origin(1)], "R-2 is folded into it");
    close_every_task(&vault)?;
    Ok(vault)
}

async fn next_failure_raises_only_its_own_task(vault: &JobVault, already: usize) -> Result<(), BoxError> {
    vault.pass(minute(4)).await;
    runs_finished(vault, already + 1).await?;
    vault.pass(minute(4) + Span::seconds(10)).await;
    assert_eq!(
        run_failed_origins(vault),
        vec![origin(1), origin(u64::try_from(already + 1)?)],
        "the next failure raises one task, and it names that run"
    );
    Ok(())
}

#[tokio::test]
async fn a_failure_folded_into_a_task_does_not_come_back_as_a_task_once_that_task_is_closed_and_a_dry_run_moves_the_counter()
-> Result<(), BoxError> {
    let vault = vault_with_a_folded_failure_and_a_closed_task().await?;
    vault.write(
        "registry/workflows/ops/sync/tests/happy-path.toml",
        "[[call]]\nserver = \"contextos\"\ntool = \"fs_read_text_file\"\nkind = \"valid\"\nresponse = { content = \"mocked\" }\n",
    )?;
    run_job(
        Arc::clone(&vault.clients),
        &vault.host,
        VAULT_NAME,
        "ops",
        "sync",
        "nightly",
        CliRunOptions {
            dry_run_scenario: Some(Some("happy-path".to_owned())),
            record: true,
        },
    )
    .await?;
    runs_finished(&vault, 3).await?;

    vault.pass(minute(2) + Span::seconds(20)).await;
    vault.pass(minute(2) + Span::seconds(25)).await;

    assert_eq!(
        run_failed_origins(&vault),
        vec![origin(1)],
        "R-2 was folded, so closing the task for R-1 does not raise one for R-2"
    );
    next_failure_raises_only_its_own_task(&vault, 3).await
}

#[tokio::test]
async fn a_failure_folded_into_a_task_does_not_come_back_as_a_task_after_a_restart_with_that_task_closed()
-> Result<(), BoxError> {
    let vault = vault_with_a_folded_failure_and_a_closed_task().await?;

    let restarted = vault.restarted().await?;
    restarted.pass(minute(2) + Span::seconds(20)).await;
    restarted.pass(minute(2) + Span::seconds(25)).await;

    assert_eq!(
        run_failed_origins(&restarted),
        vec![origin(1)],
        "a scheduler that remembers nothing decides from the tasks and the records, so R-2 is still folded"
    );
    next_failure_raises_only_its_own_task(&restarted, 2).await
}

/// An enabled job with no stored version, so a run of it counts as reviewed even though its entry script cannot be
/// read, which is how a scheduled run ends before its script starts.
async fn due_job_whose_entry_script_cannot_be_read() -> Result<JobVault, BoxError> {
    let spec = Spec::default();
    let without_version: String = enabled_state(&spec)
        .lines()
        .filter(|line| !line.starts_with("fingerprint"))
        .flat_map(|line| [line, "\n"])
        .collect();
    let vault = JobVault::open(&Spec {
        state: Some(&without_version),
        ..spec
    })
    .await?;
    // The release's script is not text, so the run cannot read its code.
    std::fs::write(
        vault.root().join("registry/workflows/ops/sync/sync.rhai"),
        [0xff_u8, 0xfe, 0x00, 0x01],
    )?;
    vault.install_releases()?;
    Ok(vault)
}

#[tokio::test]
async fn a_scheduled_run_whose_script_never_started_raises_no_task_while_one_that_started_and_failed_does()
-> Result<(), BoxError> {
    let vault = due_job_whose_entry_script_cannot_be_read().await?;

    contextos_web::scheduler::start_due_jobs(&vault.clients, &vault.host, VAULT_NAME, &FixedClock(NOW)).await;
    runs_finished(&vault, 1).await?;
    let records = vault.run_records();
    let text = &records.first().ok_or("the due run left a record")?.1;
    assert_eq!(field(text, "status").as_deref(), Some("failed"), "{text}");
    assert_eq!(field(text, "trigger").as_deref(), Some("scheduled"), "{text}");
    assert_eq!(
        field(text, "code").as_deref(),
        Some("workflow/package-unavailable"),
        "{text}"
    );
    vault.pass(NOW + Span::seconds(10)).await;
    vault.pass(NOW + Span::seconds(20)).await;

    assert!(
        vault.raised_tasks_for("run-failed").is_empty(),
        "the script never started, so the job has no defect to raise: {:?}",
        vault.raised_tasks()
    );

    let control = failing_scheduled().await?;
    control.pass(NOW).await;
    runs_finished(&control, 1).await?;
    control.pass(NOW + Span::seconds(10)).await;
    assert_eq!(
        run_failed_origins(&control),
        vec![origin(1)],
        "a script that started and failed does raise one"
    );
    Ok(())
}

#[tokio::test]
async fn a_run_failed_task_says_when_the_run_started_and_not_when_the_scheduler_noticed() -> Result<(), BoxError> {
    let vault = failing_scheduled().await?;
    vault.pass(NOW).await;
    runs_finished(&vault, 1).await?;
    let noticed = NOW + Span::seconds(20);

    vault.pass(noticed).await;

    let records = vault.run_records();
    let started =
        field(&records.first().ok_or("the run left a record")?.1, "started").ok_or("the record has a start")?;
    let tasks = vault.raised_tasks_for("run-failed");
    let (_, task) = tasks.first().ok_or("the failure raised a task")?;
    let context = task.split("## Evidence").next().unwrap_or_default();
    assert!(
        context.contains(&started),
        "the context names the run's own start {started}: {context}"
    );
    assert!(
        !context.contains("2026-10-01T03:00:50"),
        "and not the time the pass noticed it: {context}"
    );
    Ok(())
}

/// A suspension an earlier release stored as free text, with nothing recorded of who made it.
fn legacy_suspension(spec: &Spec<'_>, actor: Option<&str>) -> String {
    let state = suspended_state(spec);
    let state = match actor {
        Some(actor) => state.replace(
            "lifecycle_actor = \"operator\"",
            &format!("lifecycle_actor = \"{actor}\""),
        ),
        None => state.replace("lifecycle_actor = \"operator\"\n", ""),
    };
    format!("{state}suspended_reason = \"the new version failed\"\n")
}

#[tokio::test]
async fn a_legacy_suspension_with_no_recorded_actor_raises_no_task_and_is_not_rewritten() -> Result<(), BoxError> {
    let mut spec = Spec::default();
    let unknown = legacy_suspension(&spec, None);
    spec.state = Some(&unknown);
    let vault = JobVault::open(&spec).await?;
    let before = vault.read(support::jobs::STATE_PATH)?;

    vault.pass(NOW).await;
    vault.pass(NOW + Span::seconds(30)).await;

    assert!(
        vault.raised_tasks().is_empty(),
        "it may have been the operator's, so it is not assumed to be the system's: {:?}",
        vault.raised_tasks()
    );
    assert_eq!(
        vault.read(support::jobs::STATE_PATH)?,
        before,
        "nothing is invented and written back for the actor or the time"
    );

    let mut spec = Spec::default();
    let known = legacy_suspension(&spec, Some("system"));
    spec.state = Some(&known);
    let control = JobVault::open(&spec).await?;
    control.pass(NOW).await;
    assert_eq!(
        control.raised_tasks_for("suspended").len(),
        1,
        "a legacy suspension that names the system does raise one"
    );
    Ok(())
}

// What the host remembers beside the job's claim decides the fold, never the modification time of any file.

/// The folder the host keeps the job's claim and bookkeeping in.
fn claim_folder(vault: &JobVault) -> PathBuf {
    vault
        .config
        .path()
        .join("claims")
        .join("vaults")
        .join(VAULT_NAME.to_ascii_lowercase())
        .join("ops")
        .join("sync")
}

fn mark_path(vault: &JobVault) -> PathBuf {
    claim_folder(vault).join("nightly.run-failed-examined")
}

fn mark(vault: &JobVault) -> Option<String> {
    std::fs::read_to_string(mark_path(vault))
        .ok()
        .map(|text| text.trim().to_owned())
}

fn set_modified(path: &std::path::Path, time: SystemTime) -> Result<(), BoxError> {
    std::fs::OpenOptions::new().write(true).open(path)?.set_modified(time)?;
    Ok(())
}

/// Sets the modification time of every task file the vault raised.
fn stamp_tasks(vault: &JobVault, time: SystemTime) -> Result<(), BoxError> {
    for (path, _) in vault.raised_tasks() {
        set_modified(&path, time)?;
    }
    Ok(())
}

/// Sets the modification time of every run record the vault holds.
fn stamp_records(vault: &JobVault, time: SystemTime) -> Result<(), BoxError> {
    for (path, _) in vault.run_records() {
        set_modified(&path, time)?;
    }
    Ok(())
}

const LONG_AGO: Duration = Duration::from_hours(8760);

#[tokio::test]
async fn a_folded_failure_stays_folded_when_file_times_say_its_run_ended_after_the_task_was_closed()
-> Result<(), BoxError> {
    let vault = vault_with_a_folded_failure_and_a_closed_task().await?;
    let now = SystemTime::now();
    stamp_tasks(&vault, now - LONG_AGO)?;
    stamp_records(&vault, now)?;

    let restarted = vault.restarted().await?;
    restarted.pass(minute(2) + Span::seconds(20)).await;
    restarted.pass(minute(2) + Span::seconds(25)).await;

    assert_eq!(
        run_failed_origins(&restarted),
        vec![origin(1)],
        "the mark says R-2 was examined, whatever the files' times say"
    );
    assert_eq!(
        mark(&restarted).as_deref(),
        Some("2"),
        "R-1 was raised and R-2 folded, so both were examined"
    );
    next_failure_raises_only_its_own_task(&restarted, 2).await
}

#[tokio::test]
async fn a_folded_failure_stays_folded_when_every_file_time_is_reset_to_one_instant() -> Result<(), BoxError> {
    let vault = vault_with_a_folded_failure_and_a_closed_task().await?;
    let instant = SystemTime::now() - LONG_AGO;
    stamp_tasks(&vault, instant)?;
    stamp_records(&vault, instant)?;

    let restarted = vault.restarted().await?;
    restarted.pass(minute(2) + Span::seconds(20)).await;

    assert_eq!(run_failed_origins(&restarted), vec![origin(1)]);
    assert_eq!(mark(&restarted).as_deref(), Some("2"));
    next_failure_raises_only_its_own_task(&restarted, 2).await
}

#[tokio::test]
async fn a_new_failure_raises_even_when_file_times_say_it_ended_before_the_task_was_closed() -> Result<(), BoxError> {
    let vault = vault_with_a_folded_failure_and_a_closed_task().await?;
    let now = SystemTime::now();
    stamp_tasks(&vault, now + LONG_AGO)?;
    stamp_records(&vault, now - LONG_AGO)?;

    vault.pass(minute(4)).await;
    runs_finished(&vault, 3).await?;
    stamp_records(&vault, now - LONG_AGO)?;
    vault.pass(minute(4) + Span::seconds(10)).await;

    assert_eq!(
        run_failed_origins(&vault),
        vec![origin(1), origin(3)],
        "R-3 is above the mark and above every run a task names, so it raises"
    );
    assert_eq!(mark(&vault).as_deref(), Some("3"));
    Ok(())
}

#[tokio::test]
async fn a_host_with_no_mark_raises_a_folded_failure_once_and_then_remembers_it() -> Result<(), BoxError> {
    let vault = vault_with_a_folded_failure_and_a_closed_task().await?;
    std::fs::remove_file(mark_path(&vault))?;

    let restarted = vault.restarted().await?;
    restarted.pass(minute(2) + Span::seconds(20)).await;
    restarted.pass(minute(2) + Span::seconds(25)).await;

    assert_eq!(
        run_failed_origins(&restarted),
        vec![origin(1), origin(2)],
        "a second host with its own empty mark cannot know R-2 was folded, so it surfaces once"
    );
    assert_eq!(mark(&restarted).as_deref(), Some("2"));
    Ok(())
}

#[tokio::test]
async fn a_failing_run_numbered_below_the_mark_is_already_examined_and_raises_nothing() -> Result<(), BoxError> {
    let vault = failing_scheduled().await?;
    std::fs::create_dir_all(claim_folder(&vault))?;
    std::fs::write(mark_path(&vault), "10")?;

    vault.pass(NOW).await;
    runs_finished(&vault, 1).await?;
    vault.pass(NOW + Span::seconds(10)).await;
    vault.pass(NOW + Span::seconds(20)).await;

    assert!(
        vault.raised_tasks_for("run-failed").is_empty(),
        "R-1 is below a mark of 10: {:?}",
        vault.raised_tasks()
    );
    assert_eq!(mark(&vault).as_deref(), Some("10"), "the mark never moves back");
    Ok(())
}

#[tokio::test]
async fn a_mark_ahead_of_the_records_does_not_suppress_the_failure_of_the_next_run_above_it() -> Result<(), BoxError> {
    let vault = failing_scheduled().await?;
    std::fs::create_dir_all(claim_folder(&vault))?;
    std::fs::write(mark_path(&vault), "10")?;
    std::fs::write(claim_folder(&vault).join("nightly.next-run-id"), "11")?;

    vault.pass(NOW).await;
    runs_finished(&vault, 1).await?;
    vault.pass(NOW + Span::seconds(10)).await;

    assert_eq!(run_failed_origins(&vault), vec![origin(11)]);
    Ok(())
}

#[tokio::test]
async fn a_mark_that_is_not_a_number_is_one_warning_that_names_no_host_folder_and_raises_nothing()
-> Result<(), BoxError> {
    let vault = failing_scheduled().await?;
    vault.pass(NOW).await;
    runs_finished(&vault, 1).await?;
    let corrupt = format!("R-1 {}", "z".repeat(80));
    std::fs::write(mark_path(&vault), &corrupt)?;
    let (writer, _guard) = support::logs::capture();

    for seconds in [10, 20, 30] {
        vault.pass(NOW + Span::seconds(seconds)).await;
    }

    assert!(
        vault.raised_tasks().is_empty(),
        "nothing can be decided: {:?}",
        vault.raised_tasks()
    );
    assert_eq!(
        std::fs::read_to_string(mark_path(&vault))?,
        corrupt,
        "the mark is left unchanged"
    );
    let warnings = writer.at("WARN", "run-failed-examined");
    assert_eq!(
        warnings.len(),
        1,
        "reported once for three passes: {:?}",
        writer.lines()
    );
    let folder = vault.config.path().display().to_string();
    assert!(!warnings[0].contains(&folder), "{}", warnings[0]);
    assert!(!warnings[0].contains(&"z".repeat(33)), "{}", warnings[0]);
    Ok(())
}

#[tokio::test]
async fn a_run_that_did_not_fail_moves_the_mark_past_itself_and_raises_nothing() -> Result<(), BoxError> {
    let spec = Spec::default();
    let state = enabled_state(&spec);
    let vault = JobVault::open(&Spec {
        state: Some(&state),
        ..spec
    })
    .await?;

    vault.pass(NOW).await;
    runs_finished(&vault, 1).await?;
    vault.pass(NOW + Span::seconds(10)).await;

    assert!(
        vault.raised_tasks_for("run-failed").is_empty(),
        "{:?}",
        vault.raised_tasks()
    );
    assert_eq!(
        mark(&vault).as_deref(),
        Some("1"),
        "R-1 succeeded, so it was examined and is not looked at for a task again"
    );
    Ok(())
}
