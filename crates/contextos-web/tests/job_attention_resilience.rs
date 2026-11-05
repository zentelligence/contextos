//! Raising a task for a vault job's failure stays safe when things go wrong around it: a restarted or a second
//! scheduler never raises a task twice, a vault with no usable task system raises nothing yet still starts its due jobs
//! and raises the task once the system exists, the task carries the right entity, and raising changes nothing about the
//! job, its schedule, or its run records. Real `contextos` sessions over real temporary vaults, with a fixed clock.

mod support;

use std::path::PathBuf;

use support::BoxError;
use support::jobs::{
    JobVault, READY_JOB, SecondSession, Spec, TASKS_SIDECAR_WITHOUT_ACTION_SERIES, TEAM_WITHOUT_VELA, WORKFLOW,
    enabled_state, seed_tasks, suspended_state, version_of,
};
use time::OffsetDateTime;
use time::macros::datetime;

/// 03:00:30, half a minute after a job scheduled every minute fell due.
const NOW: OffsetDateTime = datetime!(2026-10-01 03:00:30 UTC);
const LATER: OffsetDateTime = datetime!(2026-10-01 03:01:30 UTC);
const BROKEN_SCRIPT: &str = "let y = (2 +;";
const ENTITIES: &str = "[[entities]]\nslug = \"zenith\"\nname = \"Zenith\"\naccent = 1\n\n\
                        [[entities]]\nslug = \"personal\"\nname = \"Personal\"\naccent = 2\n";
const SIDECAR_WITH_ACTION_SERIES: &str = "---\nkey_prefix: A\nkey_format: '{key_prefix}-{key_id}'\nlast_id: 0\n\
    last_create: 2026-01-01T00:00:00Z\nlast_update: 2026-01-01T00:00:00Z\n---\n";

/// A ready draft whose script has a syntax error, so every automatic validation of it fails.
fn failing_draft() -> Spec<'static> {
    Spec {
        script: BROKEN_SCRIPT,
        ..Spec::default()
    }
}

/// An enabled job whose owner has left the team.
fn owner_left() -> Spec<'static> {
    let mut spec = Spec {
        team: Some(TEAM_WITHOUT_VELA),
        ..Spec::default()
    };
    spec.state = Some(Box::leak(enabled_state(&spec).into_boxed_str()));
    spec
}

/// Adds a second workflow, `broken`, whose one ready job fails validation, to a vault.
fn add_broken_workflow(vault: &JobVault) -> std::io::Result<()> {
    vault.write("registry/workflows/broken/workflows.toml", "workflows = [\"broken\"]\n")?;
    vault.write(
        "registry/workflows/broken/broken/workflow.toml",
        "name = \"Broken\"\nentry = \"broken.rhai\"\nmcp_servers.allow = [\"contextos\"]\n",
    )?;
    vault.write("registry/workflows/broken/broken/broken.rhai", BROKEN_SCRIPT)?;
    vault.write("registry/workflows/broken/broken/jobs/other.toml", READY_JOB)
}

/// An enabled job that is due at [`NOW`], beside a ready job in another workflow that fails validation.
fn due_job_beside_a_failing_draft(tasks: bool) -> Spec<'static> {
    let mut spec = Spec {
        tasks,
        ..Spec::default()
    };
    spec.state = Some(Box::leak(enabled_state(&spec).into_boxed_str()));
    spec
}

fn frontmatter_value<'a>(note: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key}: ");
    note.lines()
        .skip(1)
        .take_while(|line| *line != "---")
        .find_map(|line| line.strip_prefix(&prefix))
        .map(|value| value.trim().trim_matches('\'').trim_matches('"'))
}

fn paths(records: &[(PathBuf, String)]) -> Vec<PathBuf> {
    records.iter().map(|(path, _)| path.clone()).collect()
}

#[tokio::test]
async fn a_restarted_scheduler_raises_no_second_task_for_an_episode_already_raised() -> Result<(), BoxError> {
    let vault = JobVault::open(&owner_left()).await?;
    vault.pass(NOW).await;
    assert_eq!(
        vault.raised_tasks_for("suspended").len(),
        1,
        "the first pass raises the task"
    );

    let restarted = vault.restarted().await?;
    restarted.pass(LATER).await;

    assert_eq!(
        restarted.raised_tasks().len(),
        1,
        "a scheduler that remembers nothing finds the task already there and raises no other"
    );
    Ok(())
}

#[tokio::test]
async fn two_schedulers_reconciling_one_vault_at_once_raise_one_task() -> Result<(), BoxError> {
    let first = JobVault::open(&failing_draft()).await?;
    let second = SecondSession::open_over(&first).await?;

    tokio::join!(first.pass(NOW), second.pass(NOW));

    assert_eq!(
        first.raised_tasks_for("validation-failed").len(),
        1,
        "one task for the one failing version, whichever scheduler raised it"
    );
    assert_eq!(first.raised_tasks().len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_vault_with_no_tasks_base_raises_nothing_and_still_starts_due_jobs() -> Result<(), BoxError> {
    let vault = JobVault::open(&due_job_beside_a_failing_draft(false)).await?;
    add_broken_workflow(&vault)?;

    vault.pass(NOW).await;

    assert!(!vault.run_records().is_empty(), "the due job was started");
    assert!(vault.raised_tasks().is_empty(), "there is no task system to raise in");
    assert!(
        !vault.root().join("memory/tasks.base").exists(),
        "no task system is created"
    );
    // The failing draft does raise once a task system exists, so nothing above was only because nothing failed.
    seed_tasks(vault.root(), SIDECAR_WITH_ACTION_SERIES)?;
    vault.pass(LATER).await;
    assert_eq!(vault.raised_tasks_for("validation-failed").len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_vault_whose_tasks_sidecar_has_no_action_series_raises_nothing() -> Result<(), BoxError> {
    let vault = JobVault::open(&due_job_beside_a_failing_draft(false)).await?;
    add_broken_workflow(&vault)?;
    seed_tasks(vault.root(), TASKS_SIDECAR_WITHOUT_ACTION_SERIES)?;

    vault.pass(NOW).await;

    assert!(!vault.run_records().is_empty(), "the due job was still started");
    assert!(
        vault.raised_tasks().is_empty(),
        "the server refuses the action series, so nothing is raised"
    );
    // Once the series is declared the same failing draft is raised, so the refusal above was the only reason.
    vault.write("memory/tasks.md", SIDECAR_WITH_ACTION_SERIES)?;
    vault.pass(LATER).await;
    assert_eq!(vault.raised_tasks_for("validation-failed").len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_task_not_raised_while_the_task_system_was_missing_is_raised_once_it_exists() -> Result<(), BoxError> {
    let vault = JobVault::open(&Spec {
        tasks: false,
        ..failing_draft()
    })
    .await?;
    vault.pass(NOW).await;
    assert!(vault.raised_tasks().is_empty(), "no task system, no task");

    seed_tasks(vault.root(), SIDECAR_WITH_ACTION_SERIES)?;
    vault.pass(LATER).await;
    assert_eq!(
        vault.raised_tasks().len(),
        1,
        "the failure still stands, so it is raised now"
    );

    vault.pass(LATER).await;
    assert_eq!(vault.raised_tasks().len(), 1, "and only once");
    Ok(())
}

async fn entity_of_the_raised_task(workflow: &str, job: &str) -> Result<String, BoxError> {
    let vault = JobVault::open(&Spec {
        workflow,
        job,
        ..failing_draft()
    })
    .await?;
    vault.write(".contextos/entities.toml", ENTITIES)?;

    vault.pass(NOW).await;

    let tasks = vault.raised_tasks();
    assert_eq!(tasks.len(), 1, "one task is raised: {tasks:?}");
    Ok(frontmatter_value(&tasks[0].1, "entity").unwrap_or_default().to_owned())
}

#[tokio::test]
async fn a_raised_task_carries_the_jobs_entity_else_its_workflows_else_vault() -> Result<(), BoxError> {
    let job_entity = "ready = true\nname = \"Nightly\"\nowner = \"vela\"\ntimeout_secs = 30\nversion = \"1.0.0\"\nentity = \"personal\"\n";
    let workflow_entity = format!("{WORKFLOW}entity = \"personal\"\n");
    let with_job_entity = entity_of_the_raised_task(WORKFLOW, job_entity).await?;
    let with_workflow_entity = entity_of_the_raised_task(&workflow_entity, READY_JOB).await?;
    let with_neither = entity_of_the_raised_task(WORKFLOW, READY_JOB).await?;
    let workflow_says_another = format!("{WORKFLOW}entity = \"zenith\"\n");
    let with_both = entity_of_the_raised_task(&workflow_says_another, job_entity).await?;

    assert_eq!(with_job_entity, "personal", "the job's own entity");
    assert_eq!(
        with_workflow_entity, "personal",
        "the workflow's, when the job declares none"
    );
    assert_eq!(with_neither, "vault", "the vault's, when neither declares one");
    assert_eq!(
        with_both, "personal",
        "the job's wins when the job and its workflow name different entities"
    );
    Ok(())
}

#[tokio::test]
async fn raising_a_task_changes_no_job_state_schedule_or_run_record() -> Result<(), BoxError> {
    let mut spec = Spec::default();
    let suspended = format!(
        "{}[ops.sync.nightly.suspension]\ncode = \"workflow/validation-failed\"\nby = \"system\"\n\
         at = \"2026-09-01T00:00:00Z\"\nreason = \"the new version failed validation\"\n",
        suspended_state(&spec).replace("lifecycle_actor = \"operator\"", "lifecycle_actor = \"system\"")
    );
    spec.state = Some(&suspended);
    let vault = JobVault::open(&spec).await?;
    let state_before = vault.read(support::jobs::STATE_PATH)?;
    let records_before = paths(&vault.run_records());

    vault.pass(NOW).await;

    assert_eq!(
        vault.raised_tasks_for("suspended").len(),
        1,
        "the system suspension is raised"
    );
    assert_eq!(
        vault.read(support::jobs::STATE_PATH)?,
        state_before,
        "the state file is byte for byte as it was"
    );
    assert_eq!(
        paths(&vault.run_records()),
        records_before,
        "no run record was made or changed"
    );
    assert_eq!(vault.lifecycle()?, "suspended");
    Ok(())
}

/// A job suspended by the system in the form this release replaces: free text only, with no structured record.
fn legacy_suspension(actor: &str) -> String {
    let spec = Spec::default();
    format!(
        "[ops.sync.nightly]\nlifecycle = \"suspended\"\nfingerprint = \"{}\"\n\
         lifecycle_changed = \"2026-09-01T00:00:00Z\"\nlifecycle_actor = \"{actor}\"\n\
         suspended_reason = \"validation of the new version failed\"\n",
        String::from(version_of(&spec))
    )
}

#[tokio::test]
async fn a_legacy_system_suspension_raises_one_task_on_the_first_pass_of_this_release() -> Result<(), BoxError> {
    let legacy = legacy_suspension("system");
    let vault = JobVault::open(&Spec {
        state: Some(&legacy),
        ..Spec::default()
    })
    .await?;

    vault.pass(NOW).await;
    vault.pass(LATER).await;

    let tasks = vault.raised_tasks_for("suspended");
    assert_eq!(
        tasks.len(),
        1,
        "one task however many passes: {:?}",
        vault.raised_tasks()
    );
    assert!(
        tasks[0].1.contains("workflow/suspended"),
        "the task names the legacy code: {}",
        tasks[0].1
    );
    Ok(())
}

#[tokio::test]
async fn a_legacy_operator_suspension_raises_nothing() -> Result<(), BoxError> {
    let legacy = legacy_suspension("operator");
    let vault = JobVault::open(&Spec {
        state: Some(&legacy),
        ..Spec::default()
    })
    .await?;

    vault.pass(NOW).await;

    assert!(
        vault.raised_tasks().is_empty(),
        "the operator stopped it, so nothing needs attention"
    );

    // The same job suspended by the system does raise, so the silence above was the actor and nothing else.
    vault.write(support::jobs::STATE_PATH, &legacy_suspension("system"))?;
    vault.pass(LATER).await;
    assert_eq!(vault.raised_tasks_for("suspended").len(), 1);
    Ok(())
}
