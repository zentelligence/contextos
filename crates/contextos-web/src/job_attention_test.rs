use contextos_core::{MemberKind, MemberSlug, OwnerStanding, TeamFile, TeamMemberRecord, TeamRoster};
use contextos_workflows::{
    Actor, AttentionPurpose, Episode, FinishedRun, JobFingerprint, JobLifecycle, RunStatus, RunTrigger, StoredFailure,
    StoredValidation, SuspensionRecord, ValidationRecord, ValidationReport,
};
use time::macros::datetime;

use super::*;
use crate::task_sink::RaiseOutcome;
use crate::task_sink_fake::FakeTaskSink;
use crate::tasks_config::Closes;

const AT: OffsetDateTime = datetime!(2026-10-21 21:30:00 +11:00);
const MESSAGE_MARKER: &str = "SECRET-FAILURE-MESSAGE-9f2";
const INPUT_MARKER: &str = "SECRET-INPUT-VALUE-7c1";
const HOST_PATH_MARKER: &str = "/home/operator/private-host-path";

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn key() -> JobKey {
    JobKey {
        package: "ops".to_owned(),
        workflow: "sync".to_owned(),
        job: "nightly".to_owned(),
    }
}

fn files() -> JobFilePaths {
    JobFilePaths {
        workflow_directory: "ops/sync-dir".to_owned(),
        entry: "sync.rhai".to_owned(),
        job_file: "nightly.toml".to_owned(),
    }
}

fn version() -> JobFingerprint {
    JobFingerprint::from(&contextos_workflows::ReleaseFingerprintInput {
        store: contextos_workflows::StoreKind::Vault,
        package: "ops",
        release: "1.0.0",
        digest: "sha256:00",
        job_toml: "",
        release_manifest: "",
        libraries: &[],
    })
}

fn leaky_facts(lifecycle: JobLifecycle) -> JobFacts {
    let mut validation = StoredValidation::from(ValidationRecord {
        report: &ValidationReport::default(),
        fingerprint: Some(version()),
        at: AT,
    });
    validation.passed = false;
    validation.failures = vec![StoredFailure {
        code: "validation/script-syntax".to_owned(),
        artefact: "sync.rhai".to_owned(),
        message: format!("{MESSAGE_MARKER} input {INPUT_MARKER} at {HOST_PATH_MARKER}"),
    }];
    JobFacts {
        key: key(),
        lifecycle,
        ready: true,
        version: Some(version()),
        validation: Some(validation),
        suspension: Some(SuspensionRecord {
            code: "workflow/probation-failed".to_owned(),
            by: Some(Actor::System),
            at: Some(AT),
            reason: format!("{MESSAGE_MARKER} {HOST_PATH_MARKER}"),
            run: Some(7),
            failure: Some("workflow/script-error".to_owned()),
        }),
        latest_run: Some(FinishedRun {
            number: 7,
            trigger: RunTrigger::Scheduled,
            status: RunStatus::Failed,
            dry_run: false,
            failure_code: Some("workflow/script-error".to_owned()),
        }),
    }
}

fn chief() -> Result<TeamRoster, Box<dyn std::error::Error>> {
    Ok(TeamRoster::try_from(TeamFile {
        member: vec![TeamMemberRecord {
            slug: "atlas".to_owned(),
            name: "Atlas".to_owned(),
            kind: MemberKind::Agent,
            chief_of_staff: true,
        }],
    })?)
}

fn task_for(
    purpose: AttentionPurpose,
    codes: &[&str],
    episode: Episode,
) -> Result<RaisedTask, Box<dyn std::error::Error>> {
    let roster = chief()?;
    let event = AttentionEvent {
        purpose,
        episode,
        codes: codes.iter().map(|code| (*code).to_owned()).collect(),
    };
    let assignee = Assignee::from((&event, &OwnerStanding::Undeclared, &roster));
    let facts = leaky_facts(JobLifecycle::Suspended);
    let paths = files();
    Ok(RaisedTask::from(TaskSource {
        event: &event,
        facts: &facts,
        files: &paths,
        run_record: Some("memory/jobs/ops/sync/nightly/R-7.md"),
        entity: "zenith",
        assignee: &assignee,
        at: AT,
    }))
}

#[test]
fn the_task_body_cites_evidence_by_vault_path_and_code_and_never_a_message_input_value_or_host_path() -> TestResult {
    let task = task_for(
        AttentionPurpose::Suspended,
        &["workflow/probation-failed", "workflow/script-error"],
        Episode::Suspension(AT),
    )?;

    for private in [MESSAGE_MARKER, INPUT_MARKER, HOST_PATH_MARKER] {
        assert!(!task.body.contains(private), "the body leaks {private}: {}", task.body);
        assert!(
            !task.title.contains(private),
            "the title leaks {private}: {}",
            task.title
        );
    }
    for expected in [
        "## Context",
        "## Evidence",
        "## What to do",
        "workflow/probation-failed",
        "workflow/script-error",
        "registry/workflows/ops/sync-dir/workflow.toml",
        "registry/workflows/ops/sync-dir/sync.rhai",
        "registry/workflows/ops/sync-dir/jobs/nightly.toml",
        ".contextos/jobs.toml",
        "memory/jobs/ops/sync/nightly/R-7.md",
        "memory/jobs/ops/sync/nightly/",
    ] {
        assert!(task.body.contains(expected), "the body lacks {expected}: {}", task.body);
    }
    Ok(())
}

#[test]
fn each_event_has_its_title_and_what_to_do_text() -> TestResult {
    let table = [
        (
            AttentionPurpose::ValidationFailed,
            vec!["validation/script-syntax"],
            Episode::Version(version()),
            "Job ops/sync/nightly failed validation",
            vec!["contextos-web jobs validate", "Validate"],
        ),
        (
            AttentionPurpose::Suspended,
            vec!["workflow/validation-failed"],
            Episode::Suspension(AT),
            "Job ops/sync/nightly was suspended: workflow/validation-failed",
            vec!["jobs resume"],
        ),
        (
            AttentionPurpose::Suspended,
            vec!["workflow/probation-failed"],
            Episode::Suspension(AT),
            "Job ops/sync/nightly was suspended: workflow/probation-failed",
            vec!["resume", "leaves the job suspended"],
        ),
        (
            AttentionPurpose::Suspended,
            vec!["workflow/owner-unresolved"],
            Episode::Suspension(AT),
            "Job ops/sync/nightly was suspended: workflow/owner-unresolved",
            vec![".contextos/team.toml", "owner", "resume"],
        ),
        (
            AttentionPurpose::RunFailed,
            vec!["workflow/script-error"],
            Episode::Run(7),
            "Scheduled run R-7 of ops/sync/nightly failed: workflow/script-error",
            vec!["not retried"],
        ),
    ];

    for (purpose, codes, episode, title, phrases) in table {
        let task = task_for(purpose, &codes, episode)?;
        assert_eq!(task.title, title);
        let todo = task.body.split("## What to do").nth(1).unwrap_or_default();
        for phrase in phrases {
            assert!(todo.contains(phrase), "{title}: what to do lacks {phrase:?}: {todo}");
        }
    }
    Ok(())
}

fn raised(episode: &Episode) -> Result<RaisedTask, Box<dyn std::error::Error>> {
    Ok(RaisedTask {
        origin: Origin::from((&AttentionPurpose::Suspended, &key(), episode)),
        title: "t".to_owned(),
        entity: "vault".to_owned(),
        assignee: MemberSlug::try_from("atlas")?,
        body: "b".to_owned(),
    })
}

#[tokio::test]
async fn a_duplicate_record_refusal_is_treated_as_already_raised_and_remembered() -> TestResult {
    let sink = FakeTaskSink::answering(RaiseOutcome::AlreadyRaised);
    let memory = JobAttentionMemory::default();
    let task = raised(&Episode::Suspension(AT))?;

    let report = raise_tasks(&sink, &memory, "work", std::slice::from_ref(&task)).await;

    assert_eq!(report.already_raised, 1);
    assert_eq!(report.created, 0);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert!(memory.knows("work", &task.origin));
    assert!(
        !memory.knows("other", &task.origin),
        "what is raised is remembered per vault"
    );
    Ok(())
}

#[tokio::test]
async fn an_origin_remembered_makes_no_further_call() -> TestResult {
    let sink = FakeTaskSink::answering(RaiseOutcome::Created("A-1".to_owned()));
    let memory = JobAttentionMemory::default();
    let task = raised(&Episode::Suspension(AT))?;

    let first = raise_tasks(&sink, &memory, "work", std::slice::from_ref(&task)).await;
    let second = raise_tasks(&sink, &memory, "work", std::slice::from_ref(&task)).await;

    assert_eq!(first.created, 1);
    assert_eq!(second.created, 0);
    assert_eq!(sink.calls(), 1, "the second pass made no call");
    Ok(())
}

#[tokio::test]
async fn a_missing_tasks_base_or_action_series_is_no_task_system_and_warns_once_per_vault() -> TestResult {
    let sink = FakeTaskSink::answering(RaiseOutcome::NoTaskSystem);
    let memory = JobAttentionMemory::default();
    let task = raised(&Episode::Suspension(AT))?;

    let first = raise_tasks(&sink, &memory, "work", std::slice::from_ref(&task)).await;
    let second = raise_tasks(&sink, &memory, "work", std::slice::from_ref(&task)).await;
    let other_vault = raise_tasks(&sink, &memory, "home", std::slice::from_ref(&task)).await;

    assert_eq!(
        first.warnings,
        vec![AttentionWarning::NoTaskSystem {
            vault: "work".to_owned()
        }]
    );
    assert!(second.warnings.is_empty(), "{:?}", second.warnings);
    assert_eq!(
        other_vault.warnings,
        vec![AttentionWarning::NoTaskSystem {
            vault: "home".to_owned()
        }]
    );
    assert!(
        !memory.knows("work", &task.origin),
        "nothing was raised, so nothing is remembered"
    );

    sink.answer(RaiseOutcome::Created("A-1".to_owned()));
    let later = raise_tasks(&sink, &memory, "work", std::slice::from_ref(&task)).await;
    assert_eq!(later.created, 1, "once the task system exists the task is raised");
    Ok(())
}

#[tokio::test]
async fn any_other_refusal_is_retried_on_the_next_pass_and_warned_once_per_origin_and_code() -> TestResult {
    let sink = FakeTaskSink::answering(RaiseOutcome::Failed("io/write-failed".to_owned()));
    let memory = JobAttentionMemory::default();
    let task = raised(&Episode::Suspension(AT))?;
    let warning = |code: &str| AttentionWarning::RaiseFailed {
        origin: task.origin.to_string(),
        code: code.to_owned(),
    };

    let first = raise_tasks(&sink, &memory, "work", std::slice::from_ref(&task)).await;
    let second = raise_tasks(&sink, &memory, "work", std::slice::from_ref(&task)).await;
    sink.answer(RaiseOutcome::Failed("unreachable".to_owned()));
    let third = raise_tasks(&sink, &memory, "work", std::slice::from_ref(&task)).await;

    assert_eq!(first.warnings, vec![warning("io/write-failed")]);
    assert!(second.warnings.is_empty(), "{:?}", second.warnings);
    assert_eq!(
        third.warnings,
        vec![warning("unreachable")],
        "a different code warns again"
    );
    assert_eq!(sink.calls(), 3, "each pass tried again");
    assert!(!memory.knows("work", &task.origin));
    Ok(())
}

#[test]
fn run_history_is_read_only_when_the_jobs_run_counter_moved() {
    let memory = JobAttentionMemory::default();
    let other = JobKey {
        package: "ops".to_owned(),
        workflow: "sync".to_owned(),
        job: "weekly".to_owned(),
    };

    assert!(
        memory.run_counter_moved("work", &key(), 4),
        "the first look is a change"
    );
    assert!(!memory.run_counter_moved("work", &key(), 4), "the same counter is not");
    assert!(memory.run_counter_moved("work", &key(), 5), "a new run is");
    assert!(
        memory.run_counter_moved("work", &other, 5),
        "another job is examined on its own"
    );
    assert!(memory.run_counter_moved("home", &key(), 5), "and another vault");
}

fn existing(origin: &str, state: &str) -> ExistingTask {
    ExistingTask {
        origin: origin.to_owned(),
        task_type: "action".to_owned(),
        state: state.to_owned(),
    }
}

#[test]
fn an_open_run_failed_task_folds_a_later_failure_and_a_closed_one_does_not() {
    let config = TasksConfig::default();
    let origin = "contextos-web:run-failed:ops/sync/nightly:R-3";

    assert!(run_failed_task_is_open(&[existing(origin, "next")], &config));
    assert!(run_failed_task_is_open(&[existing(origin, "doing")], &config));
    assert!(!run_failed_task_is_open(&[existing(origin, "done")], &config));
    assert!(!run_failed_task_is_open(&[existing(origin, "dropped")], &config));
    assert!(!run_failed_task_is_open(&[], &config));
    assert!(
        run_failed_task_is_open(
            &[
                existing(origin, "done"),
                existing("contextos-web:run-failed:ops/sync/nightly:R-9", "next")
            ],
            &config
        ),
        "any open one folds"
    );
    assert!(
        run_failed_task_is_open(&[existing(origin, "an-undeclared-state")], &config),
        "a state that is not one of the configured closed states counts as open, so no duplicate task is raised"
    );

    let mut custom = TasksConfig::default();
    for stage in &mut custom.stage {
        if stage.state == "review" {
            stage.closes = Some(Closes::Completed);
        }
    }
    assert!(run_failed_task_is_open(&[existing(origin, "review")], &config));
    assert!(
        !run_failed_task_is_open(&[existing(origin, "review")], &custom),
        "what is closed comes from the vault's tasks configuration"
    );
}

fn failed(number: u64) -> FailedRun {
    FailedRun { number }
}

#[test]
fn a_failure_is_new_only_when_it_is_numbered_after_every_run_the_job_has_a_task_for() {
    let config = TasksConfig::default();
    let tasks = [
        existing("contextos-web:run-failed:ops/sync/nightly:R-4", "done"),
        existing("contextos-web:run-failed:ops/sync/nightly:R-9", "done"),
    ];

    assert!(
        !is_unfolded_run_failure(&tasks, &config, None, &failed(9)),
        "R-9 has its task"
    );
    assert!(
        !is_unfolded_run_failure(&tasks, &config, None, &failed(6)),
        "R-6 is older than R-9"
    );
    assert!(is_unfolded_run_failure(&tasks, &config, None, &failed(10)));
    assert!(
        is_unfolded_run_failure(&[], &config, None, &failed(1)),
        "a job with no run-failed task at all raises the first"
    );
}

#[test]
fn a_failure_is_new_only_when_it_is_numbered_after_the_hosts_examined_mark() {
    let config = TasksConfig::default();

    assert!(!is_unfolded_run_failure(&[], &config, Some(5), &failed(5)));
    assert!(!is_unfolded_run_failure(&[], &config, Some(5), &failed(2)));
    assert!(is_unfolded_run_failure(&[], &config, Some(5), &failed(6)));
}

#[test]
fn a_failure_above_the_mark_and_every_named_run_is_still_folded_while_a_task_is_open() {
    let open = [existing("contextos-web:run-failed:ops/sync/nightly:R-1", "next")];

    assert!(!is_unfolded_run_failure(
        &open,
        &TasksConfig::default(),
        Some(1),
        &failed(2)
    ));
}

#[test]
fn the_highest_run_named_by_the_jobs_tasks_is_found_and_unparseable_origins_are_ignored() {
    assert_eq!(highest_named_run(&[]), None);
    assert_eq!(
        highest_named_run(&[
            existing("contextos-web:run-failed:ops/sync/nightly:R-4", "done"),
            existing("contextos-web:run-failed:ops/sync/nightly:R-12", "done"),
            existing("contextos-web:run-failed:ops/sync/nightly:R-x", "done"),
            existing("something-else", "done"),
        ]),
        Some(12)
    );
}

#[test]
fn a_team_that_cannot_be_read_is_noted_once_when_it_starts_and_once_when_it_clears() {
    let memory = JobAttentionMemory::default();

    assert_eq!(
        memory.note_team_outage("work", false),
        None,
        "readable and was readable"
    );
    assert_eq!(memory.note_team_outage("work", true), Some(OutageLogEntry::Started));
    assert_eq!(memory.note_team_outage("work", true), None, "the same outage again");
    assert_eq!(
        memory.note_team_outage("home", true),
        Some(OutageLogEntry::Started),
        "per vault"
    );
    assert_eq!(memory.note_team_outage("work", false), Some(OutageLogEntry::Cleared));
    assert_eq!(memory.note_team_outage("work", false), None);
}

#[test]
fn a_job_condition_is_reported_when_it_starts_and_again_only_after_it_has_cleared() {
    let memory = JobAttentionMemory::default();
    let key = JobKey {
        package: "ops".to_owned(),
        workflow: "sync".to_owned(),
        job: "nightly".to_owned(),
    };
    let other = JobKey {
        package: "ops".to_owned(),
        workflow: "sync".to_owned(),
        job: "weekly".to_owned(),
    };

    assert!(
        memory.note_condition(JobCondition::MarkUnusable, "work", &key, true),
        "it starts"
    );
    assert!(
        !memory.note_condition(JobCondition::MarkUnusable, "work", &key, true),
        "it is still there"
    );
    assert!(
        memory.note_condition(JobCondition::MarkUnusable, "work", &other, true),
        "per job"
    );
    assert!(
        memory.note_condition(JobCondition::RecordUnreadable, "work", &key, true),
        "per kind of condition"
    );
    assert!(
        !memory.note_condition(JobCondition::MarkUnusable, "work", &key, false),
        "it clears"
    );
    assert!(
        memory.note_condition(JobCondition::MarkUnusable, "work", &key, true),
        "and starting again is reported again"
    );
}

#[test]
fn what_is_remembered_of_job_events_is_forgotten_after_the_lookback() {
    let memory = JobAttentionMemory::default();
    let start = time::macros::datetime!(2026-10-01 00:00:00 UTC);
    memory.remember_event("work", "job.failed:sync/nightly:R-1", start);
    assert!(memory.first_event_failure_of("work", "job.failed:sync/nightly", start));
    assert!(!memory.first_event_failure_of("work", "job.failed:sync/nightly", start));
    assert!(
        memory.first_event_failure_of("work", "job.recovered:sync/nightly", start),
        "another kind of the same job is logged on its own"
    );
    assert!(
        memory.first_event_failure_of("other", "job.failed:sync/nightly", start),
        "another vault is logged on its own"
    );

    let later = start + time::Duration::days(8);
    memory.remember_event("work", "job.failed:sync/nightly:R-20", later);

    assert!(
        !memory.knows_event("work", "job.failed:sync/nightly:R-1"),
        "pruned beyond the lookback"
    );
    assert!(memory.knows_event("work", "job.failed:sync/nightly:R-20"));
    assert!(
        memory.first_event_failure_of("work", "job.failed:sync/nightly", later),
        "a failure logged more than the lookback ago is logged again"
    );
}
