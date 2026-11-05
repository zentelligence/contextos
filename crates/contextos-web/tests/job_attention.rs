//! Tasks raised for vault job failures: one scheduler pass over a real temporary vault with a real `contextos` session
//! raises one task per episode for the owner or the chief of staff, citing evidence by path and stable code only.
//!
//! The first group covers a ready draft that fails validation and a new version that suspends an enabled job; the
//! second covers failed runs and suspensions of every kind. A test that expects no task also proves, in the same
//! harness, that a task is raised when it should be.

mod support;

use std::time::Duration;

use contextos_workflows::{Actor, JobsState, RULES_REVISION, StoredFailure, StoredValidation};
use support::jobs::{
    FAILING_SCRIPT, JobVault, READY_JOB, Spec, TEAM, TEAM_WITHOUT_VELA, WORKFLOW, enabled_state, lifecycle_state,
    suspended_state, version_of,
};
use time::macros::datetime;
use time::{Duration as Span, OffsetDateTime, UtcOffset};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const NOW: OffsetDateTime = datetime!(2026-10-01 03:00:30 UTC);
const NOW_STAMP: &str = "20261001T030030Z";
const BROKEN: &str = "let y = (2 +;";
const SCRIPT_PATH: &str = "registry/workflows/ops/sync/sync.rhai";
const WORKFLOW_PATH: &str = "registry/workflows/ops/sync/workflow.toml";
const MESSAGE_MARKER: &str = "MESSAGE-MARKER-7f3a";
const INPUT_MARKER: &str = "INPUT-MARKER-91c2";

// ---------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------

/// The value of a frontmatter line, without its quotes.
fn field(text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    text.lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .map(|value| value.trim().trim_matches(|c| c == '\'' || c == '"').to_owned())
}

/// The origin of the task for run `number` of `sync/nightly`.
fn run_origin(number: u64) -> String {
    format!("contextos-web:run-failed:ops/sync/nightly:R-{number}")
}

fn run_title(number: u64, code: &str) -> String {
    format!("Scheduled run R-{number} of ops/sync/nightly failed: {code}")
}

fn utc_stamp(at: OffsetDateTime) -> String {
    let at = at.to_offset(UtcOffset::UTC);
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute(),
        at.second()
    )
}

fn episode_of(version: &str) -> String {
    let digest = version.strip_prefix("sha256:").unwrap_or(version);
    format!("v-{}", &digest[..12])
}

/// The one task raised for `purpose`, or a description of what was found instead.
fn only_task(vault: &JobVault, purpose: &str) -> Result<(String, String), BoxError> {
    let found = vault.raised_tasks_for(purpose);
    if found.len() != 1 {
        return Err(format!("expected one {purpose} task, found {}", found.len()).into());
    }
    let (path, text) = found.into_iter().next().ok_or("no task")?;
    Ok((path.display().to_string(), text))
}

/// A vault job that is enabled and past its trial, with a schedule that never falls due in the tests.
fn quiet_enabled(spec: &Spec<'_>) -> String {
    enabled_state(spec).replace("schedule = \"* * * * *\"", "schedule = \"0 0 1 1 *\"")
}

fn on_trial(state: &str) -> String {
    format!("{state}first_run = true\n")
}

/// Polls `condition` until it holds, leaving the detached run the thread between checks. No outcome depends on the
/// length of the pause. The deadline only ends a wait that can never finish.
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

fn first_state_job(vault: &JobVault) -> Result<contextos_workflows::JobState, BoxError> {
    vault
        .state()?
        .job("ops", "sync", "nightly")
        .cloned()
        .ok_or_else(|| "no stored job".into())
}

/// Runs the job once by hand, then rewrites that record into the kind of record under test: its trigger, its status,
/// its error text, and its stable failure `code`, which is the only place a task reads the failure's kind from.
async fn vault_with_record(
    state_for: impl Fn(&Spec<'_>) -> String,
    trigger: &str,
    status: &str,
    error: &str,
    code: Option<&str>,
) -> Result<JobVault, BoxError> {
    let spec = Spec {
        script: FAILING_SCRIPT,
        ..Spec::default()
    };
    let state = state_for(&spec);
    let vault = JobVault::open(&Spec {
        state: Some(&state),
        ..spec
    })
    .await?;
    manual_run(&vault).await?;
    let (path, text) = vault
        .run_records()
        .into_iter()
        .next()
        .ok_or("the manual run left no record")?;
    let code_line = code.map_or_else(|| "code: ~".to_owned(), |code| format!("code: {code}"));
    let rewritten: Vec<String> = text
        .lines()
        .filter(|line| !line.starts_with("code:"))
        .flat_map(|line| {
            if line.starts_with("trigger:") {
                vec![format!("trigger: {trigger}")]
            } else if line.starts_with("status:") {
                vec![format!("status: {status}")]
            } else if line.starts_with("error:") {
                vec![format!("error: '{error}'"), code_line.clone()]
            } else {
                vec![line.to_owned()]
            }
        })
        .collect();
    std::fs::write(path, rewritten.join("\n") + "\n")?;
    Ok(vault)
}

async fn manual_run(vault: &JobVault) -> Result<(), BoxError> {
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
    Ok(())
}

fn close_every_task(vault: &JobVault) -> Result<(), BoxError> {
    for (path, text) in vault.raised_tasks() {
        std::fs::write(path, text.replace("state: next", "state: done"))?;
    }
    Ok(())
}

/// A vault whose enabled, scheduled job is due on every pass and whose script fails, past its trial.
async fn failing_scheduled(script: &str) -> Result<JobVault, BoxError> {
    let spec = Spec {
        script,
        ..Spec::default()
    };
    let state = enabled_state(&spec);
    JobVault::open(&Spec {
        state: Some(&state),
        ..spec
    })
    .await
}

/// Passes at the minute `n` after `NOW`, each inside its own tick.
fn minute(n: i64) -> OffsetDateTime {
    NOW + Span::minutes(n)
}

// ---------------------------------------------------------------------
// A ready draft that fails validation
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_ready_draft_that_fails_validation_raises_one_task_for_its_owner() -> Result<(), BoxError> {
    let spec = Spec {
        script: BROKEN,
        ..Spec::default()
    };
    let vault = JobVault::open(&spec).await?;

    vault.pass(NOW).await;

    let (_, text) = only_task(&vault, "validation-failed")?;
    let origin = format!(
        "contextos-web:validation-failed:ops/sync/nightly:{}",
        episode_of(&String::from(version_of(&spec)))
    );
    assert_eq!(field(&text, "origin").as_deref(), Some(origin.as_str()), "{text}");
    assert_eq!(field(&text, "assignee").as_deref(), Some("vela"), "{text}");
    assert_eq!(field(&text, "entity").as_deref(), Some("vault"), "{text}");
    assert_eq!(
        field(&text, "title").as_deref(),
        Some("Job ops/sync/nightly failed validation"),
        "{text}"
    );
    Ok(())
}

#[tokio::test]
async fn the_same_failing_version_over_many_passes_raises_one_task() -> Result<(), BoxError> {
    let vault = JobVault::open(&Spec {
        script: BROKEN,
        ..Spec::default()
    })
    .await?;

    for n in 0..5 {
        vault.pass(minute(n)).await;
    }

    only_task(&vault, "validation-failed")?;
    assert_eq!(vault.raised_tasks().len(), 1, "no other kind of task either");
    Ok(())
}

#[tokio::test]
async fn a_ready_draft_naming_a_release_that_is_not_installed_raises_one_unread_task() -> Result<(), BoxError> {
    let vault = JobVault::open(&Spec::default()).await?;
    // The release the job names is not in the package store, so the job has no version to validate.
    vault.name_version("2.0.0")?;

    vault.pass(NOW).await;
    vault.pass(minute(1)).await;

    let (_, text) = only_task(&vault, "validation-failed")?;
    let origin = format!("contextos-web:validation-failed:ops/sync/nightly:unread-{NOW_STAMP}");
    assert_eq!(field(&text, "origin").as_deref(), Some(origin.as_str()), "{text}");
    Ok(())
}

// ---------------------------------------------------------------------
// Suspensions
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_new_version_that_fails_validation_suspends_the_job_and_raises_one_suspended_task() -> Result<(), BoxError> {
    let spec = Spec::default();
    let state = quiet_enabled(&spec);
    let vault = JobVault::open(&Spec {
        state: Some(&state),
        ..spec
    })
    .await?;
    vault.write(SCRIPT_PATH, BROKEN)?;
    vault.publish_source_as("2.0.0")?;

    vault.pass(NOW).await;
    vault.pass(minute(1)).await;

    assert_eq!(vault.lifecycle()?, "suspended");
    let job = first_state_job(&vault)?;
    let suspension = job.suspension().ok_or("a system suspension must be recorded")?;
    assert_eq!(suspension.code, "workflow/validation-failed");
    let (_, text) = only_task(&vault, "suspended")?;
    let origin = format!(
        "contextos-web:suspended:ops/sync/nightly:{}",
        utc_stamp(suspension.at.ok_or("the suspension records when")?)
    );
    assert_eq!(field(&text, "origin").as_deref(), Some(origin.as_str()), "{text}");
    assert_eq!(vault.raised_tasks().len(), 1, "the suspension raises the one task");
    Ok(())
}

#[tokio::test]
async fn a_failed_scheduled_first_run_raises_one_suspended_task_citing_its_run_record() -> Result<(), BoxError> {
    let spec = Spec {
        script: FAILING_SCRIPT,
        ..Spec::default()
    };
    let state = on_trial(&enabled_state(&spec));
    let vault = JobVault::open(&Spec {
        state: Some(&state),
        ..spec
    })
    .await?;

    vault.pass(NOW).await;
    eventually("the failed first run to suspend the job", || {
        vault.lifecycle().is_ok_and(|lifecycle| lifecycle == "suspended")
    })
    .await?;
    vault.pass(minute(2)).await;

    let job = first_state_job(&vault)?;
    let suspension = job.suspension().ok_or("the suspension must be recorded")?;
    assert_eq!(suspension.code, "workflow/probation-failed");
    assert_eq!(suspension.run, Some(1));
    let (_, text) = only_task(&vault, "suspended")?;
    let origin = format!(
        "contextos-web:suspended:ops/sync/nightly:{}",
        utc_stamp(suspension.at.ok_or("the suspension records when")?)
    );
    assert_eq!(field(&text, "origin").as_deref(), Some(origin.as_str()), "{text}");
    let records = vault.run_records();
    let (record, _) = records.first().ok_or("the run left no record")?;
    // A vault path is written with forward slashes on every platform.
    let relative = record
        .strip_prefix(vault.root())?
        .display()
        .to_string()
        .replace('\\', "/");
    let cited = relative.trim_end_matches(".md");
    assert!(text.contains(cited), "the task must link {cited}: {text}");
    assert!(text.contains("workflow/probation-failed"), "{text}");
    Ok(())
}

#[tokio::test]
async fn a_failed_manual_first_run_that_suspends_the_job_raises_a_task() -> Result<(), BoxError> {
    let spec = Spec {
        script: FAILING_SCRIPT,
        ..Spec::default()
    };
    let state = on_trial(&quiet_enabled(&spec));
    let vault = JobVault::open(&Spec {
        state: Some(&state),
        ..spec
    })
    .await?;

    manual_run(&vault).await?;
    assert_eq!(vault.lifecycle()?, "suspended", "the manual first run failed the trial");
    vault.pass(NOW).await;

    let (_, text) = only_task(&vault, "suspended")?;
    assert_eq!(field(&text, "assignee").as_deref(), Some("vela"), "{text}");
    assert!(text.contains("workflow/probation-failed"), "{text}");
    Ok(())
}

// ---------------------------------------------------------------------
// Failed scheduled runs after the trial
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_failed_scheduled_run_after_the_trial_raises_one_run_failed_task_and_another_failure_while_it_is_open_raises_none()
-> Result<(), BoxError> {
    let vault = failing_scheduled(FAILING_SCRIPT).await?;

    vault.pass(NOW).await;
    runs_finished(&vault, 1).await?;
    vault.pass(minute(2)).await;
    runs_finished(&vault, 2).await?;
    vault.pass(minute(4)).await;
    runs_finished(&vault, 3).await?;
    vault.pass(minute(6)).await;

    let (_, text) = only_task(&vault, "run-failed")?;
    assert_eq!(
        field(&text, "origin").as_deref(),
        Some(run_origin(1).as_str()),
        "{text}"
    );
    assert_eq!(
        field(&text, "title").as_deref(),
        Some(run_title(1, "workflow/script-error").as_str()),
        "{text}"
    );
    assert_eq!(field(&text, "assignee").as_deref(), Some("vela"), "{text}");
    assert_eq!(
        vault.lifecycle()?,
        "enabled",
        "a failed run after the trial suspends nothing"
    );
    assert_eq!(vault.raised_tasks().len(), 1, "later failures fold into the open task");
    Ok(())
}

#[tokio::test]
async fn a_failed_scheduled_run_after_the_run_failed_task_is_closed_raises_a_new_task() -> Result<(), BoxError> {
    let vault = failing_scheduled(FAILING_SCRIPT).await?;
    // Each scheduled run is followed by a pass inside the same tick, which starts nothing and only reconciles, so which
    // run is the latest finished one at each step does not depend on a run still under way.
    let seconds = |n: i64, extra: i64| minute(n) + Span::seconds(extra);

    vault.pass(NOW).await;
    runs_finished(&vault, 1).await?;
    vault.pass(seconds(0, 10)).await;
    let (first_path, first_text) = only_task(&vault, "run-failed")?;
    assert_eq!(field(&first_text, "origin").as_deref(), Some(run_origin(1).as_str()));

    // R-2 fails while the task for R-1 is open: nothing is raised for it.
    vault.pass(minute(2)).await;
    runs_finished(&vault, 2).await?;
    vault.pass(seconds(2, 10)).await;
    assert_eq!(
        vault.raised_tasks_for("run-failed").len(),
        1,
        "a failure while a run-failed task is open raises nothing"
    );

    // R-3 fails after the task for R-1 was closed: it raises a new task. The task is closed first, because a failure
    // that ended while it was open is folded into it and stays folded.
    std::fs::write(&first_path, first_text.replace("state: next", "state: done"))?;
    vault.pass(minute(4)).await;
    runs_finished(&vault, 3).await?;
    vault.pass(seconds(4, 10)).await;
    vault.pass(seconds(4, 20)).await;

    let tasks = vault.raised_tasks_for("run-failed");
    let mut origins: Vec<String> = tasks.iter().filter_map(|(_, text)| field(text, "origin")).collect();
    origins.sort();
    assert_eq!(
        origins,
        vec![run_origin(1), run_origin(3)],
        "exactly two run-failed tasks: the first, and the one for the failure after it was closed"
    );
    Ok(())
}

#[tokio::test]
async fn a_timed_out_scheduled_run_raises_a_run_failed_task() -> Result<(), BoxError> {
    let vault = vault_with_record(
        quiet_enabled,
        "scheduled",
        "failed",
        "the run exceeded its limit of 1 seconds",
        Some("workflow/timed-out"),
    )
    .await?;

    vault.pass(NOW).await;

    let (_, text) = only_task(&vault, "run-failed")?;
    assert_eq!(
        field(&text, "title").as_deref(),
        Some(run_title(1, "workflow/timed-out").as_str()),
        "{text}"
    );
    assert_eq!(
        field(&text, "origin").as_deref(),
        Some(run_origin(1).as_str()),
        "{text}"
    );
    Ok(())
}

#[tokio::test]
async fn degraded_skipped_cancelled_interrupted_and_manual_failed_runs_raise_nothing() -> Result<(), BoxError> {
    let interrupted = contextos_workflows::INTERRUPTED_ERROR;
    let cases = [
        ("scheduled", "degraded", "", None),
        ("scheduled", "skipped", "", None),
        ("scheduled", "cancelled", "", None),
        ("scheduled", "interrupted", interrupted, None),
        ("manual", "failed", "the script failed", Some("workflow/script-error")),
    ];
    for (trigger, status, error, code) in cases {
        let vault = vault_with_record(quiet_enabled, trigger, status, error, code).await?;
        vault.pass(NOW).await;
        vault.pass(minute(1)).await;
        assert!(
            vault.raised_tasks().is_empty(),
            "a {trigger} run that ended {status} raises nothing"
        );
    }

    let control = vault_with_record(
        quiet_enabled,
        "scheduled",
        "failed",
        "the script failed",
        Some("workflow/script-error"),
    )
    .await?;
    control.pass(NOW).await;
    only_task(&control, "run-failed")?;
    Ok(())
}

// ---------------------------------------------------------------------
// What a task may contain
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_raised_task_never_contains_the_failure_message_an_input_value_or_a_host_path() -> Result<(), BoxError> {
    let workflow = format!("{WORKFLOW}\n[[input]]\nname = \"label\"\nkind = \"string\"\n");
    let job = format!("{READY_JOB}\n[inputs]\nlabel = \"{INPUT_MARKER}\"\n");
    let script = format!("throw \"{MESSAGE_MARKER}\";");
    let template = Spec {
        workflow: &workflow,
        script: &script,
        job: &job,
        ..Spec::default()
    };

    // A failed first run suspends the job, and a failed run after the trial raises its own task.
    for (purpose, state) in [
        ("suspended", on_trial(&enabled_state(&template))),
        ("run-failed", enabled_state(&template)),
    ] {
        let vault = JobVault::open(&Spec {
            state: Some(&state),
            ..template
        })
        .await?;
        vault.pass(NOW).await;
        runs_finished(&vault, 1).await?;
        vault.pass(minute(2)).await;

        let (_, text) = only_task(&vault, purpose)?;
        assert!(
            !text.contains(MESSAGE_MARKER),
            "{purpose}: the failure message leaked: {text}"
        );
        assert!(!text.contains(INPUT_MARKER), "{purpose}: an input value leaked: {text}");
        for directory in [vault.root(), vault.config.path()] {
            let host_path = directory.display().to_string();
            assert!(!text.contains(&host_path), "{purpose}: a host path leaked: {text}");
        }
        assert!(
            text.contains(WORKFLOW_PATH),
            "{purpose}: the task cites its evidence by path: {text}"
        );
        assert!(
            text.contains("memory/jobs/"),
            "{purpose}: the task cites the run history: {text}"
        );
        let code = if purpose == "suspended" {
            "workflow/probation-failed"
        } else {
            "workflow/script-error"
        };
        assert!(text.contains(code), "{purpose}: the task names the stable code: {text}");
    }
    Ok(())
}

// ---------------------------------------------------------------------
// What does not raise
// ---------------------------------------------------------------------

fn operator_suspension(spec: &Spec<'_>) -> String {
    format!(
        "{}suspension = {{ code = \"workflow/operator-suspended\", by = \"operator\", at = \"2026-09-30T00:00:00Z\", \
         reason = \"paused for a review\" }}\n",
        suspended_state(spec)
    )
}

fn system_suspension(spec: &Spec<'_>) -> String {
    format!(
        "{}suspension = {{ code = \"workflow/validation-failed\", by = \"system\", at = \"2026-09-30T00:00:00Z\", \
         reason = \"a new version failed validation\" }}\n",
        suspended_state(spec)
    )
}

/// A stored failed validation of the version `spec` has, made by `by`.
fn failed_validation(spec: &Spec<'_>, by: Actor) -> Result<String, BoxError> {
    let mut state: JobsState = toml::from_str(&lifecycle_state(spec, "draft"))?;
    state.record_validation(
        "ops",
        "sync",
        "nightly",
        StoredValidation {
            by,
            rules: RULES_REVISION,
            at: datetime!(2026-09-30 00:00:00 UTC),
            fingerprint: Some(version_of(spec)),
            passed: false,
            failures: vec![StoredFailure {
                code: "validation/script-syntax".to_owned(),
                artefact: SCRIPT_PATH.to_owned(),
                message: "the script does not parse".to_owned(),
            }],
            unverifiable: Vec::new(),
            made_from: None,
        },
    );
    Ok(toml::to_string(&state)?)
}

#[tokio::test]
async fn an_operator_suspension_and_a_validation_asked_for_by_hand_raise_nothing() -> Result<(), BoxError> {
    // The same vault and the same pass raise a task for the system's version of each.
    let spec = Spec {
        script: BROKEN,
        ..Spec::default()
    };

    let by_operator = operator_suspension(&spec);
    let suspended = JobVault::open(&Spec {
        state: Some(&by_operator),
        ..spec
    })
    .await?;
    suspended.pass(NOW).await;
    assert!(
        suspended.raised_tasks().is_empty(),
        "an operator's suspension raises nothing"
    );

    let by_system = system_suspension(&spec);
    let control = JobVault::open(&Spec {
        state: Some(&by_system),
        ..spec
    })
    .await?;
    control.pass(NOW).await;
    only_task(&control, "suspended")?;

    let asked_by_hand = failed_validation(&spec, Actor::Operator)?;
    let by_hand = JobVault::open(&Spec {
        state: Some(&asked_by_hand),
        ..spec
    })
    .await?;
    by_hand.pass(NOW).await;
    assert!(
        by_hand.raised_tasks().is_empty(),
        "a validation the operator asked for raises nothing"
    );

    let automatic = failed_validation(&spec, Actor::Validator)?;
    let validator = JobVault::open(&Spec {
        state: Some(&automatic),
        ..spec
    })
    .await?;
    validator.pass(NOW).await;
    only_task(&validator, "validation-failed")?;
    Ok(())
}

// ---------------------------------------------------------------------
// Assignees
// ---------------------------------------------------------------------

#[tokio::test]
async fn an_owner_who_left_raises_one_task_for_the_chief_of_staff() -> Result<(), BoxError> {
    let spec = Spec {
        team: Some(TEAM_WITHOUT_VELA),
        ..Spec::default()
    };
    let state = quiet_enabled(&spec);
    let vault = JobVault::open(&Spec {
        state: Some(&state),
        ..spec
    })
    .await?;

    vault.pass(NOW).await;
    vault.pass(minute(1)).await;

    let job = first_state_job(&vault)?;
    let suspension = job.suspension().ok_or("the owner check must suspend the job")?;
    assert_eq!(suspension.code, "workflow/owner-unresolved");
    let (_, text) = only_task(&vault, "suspended")?;
    assert_eq!(field(&text, "assignee").as_deref(), Some("atlas"), "{text}");
    assert!(text.contains("workflow/owner-unresolved"), "{text}");
    Ok(())
}

#[tokio::test]
async fn a_task_for_a_job_whose_owner_is_not_a_member_goes_to_the_chief_of_staff() -> Result<(), BoxError> {
    let job = "ready = true\nname = \"Nightly\"\nowner = \"ghost\"\ntimeout_secs = 30\nversion = \"1.0.0\"\n";
    let vault = JobVault::open(&Spec {
        script: BROKEN,
        job,
        team: Some(TEAM),
        ..Spec::default()
    })
    .await?;

    vault.pass(NOW).await;

    let (_, text) = only_task(&vault, "validation-failed")?;
    assert_eq!(field(&text, "assignee").as_deref(), Some("atlas"), "{text}");
    Ok(())
}

// ---------------------------------------------------------------------
// A closed task
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_closed_task_is_not_raised_again_for_the_same_episode() -> Result<(), BoxError> {
    let vault = JobVault::open(&Spec {
        script: BROKEN,
        ..Spec::default()
    })
    .await?;
    vault.pass(NOW).await;
    only_task(&vault, "validation-failed")?;

    close_every_task(&vault)?;
    vault.pass(minute(1)).await;
    vault.pass(minute(2)).await;
    assert_eq!(vault.raised_tasks().len(), 1, "the scheduler remembers the episode");

    // A restart remembers nothing, so only the server's refusal of a repeated origin keeps the count at one.
    let restarted = vault.restarted().await?;
    restarted.pass(minute(3)).await;
    restarted.pass(minute(4)).await;
    let tasks = restarted.raised_tasks();
    assert_eq!(tasks.len(), 1, "a closed task's origin is refused, not raised again");
    assert!(
        tasks.iter().all(|(_, text)| text.contains("state: done")),
        "the closed task was left as it was closed"
    );
    Ok(())
}
