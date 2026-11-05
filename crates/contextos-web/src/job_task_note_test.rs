use contextos_core::{MemberKind, OwnerStanding, TeamFile, TeamMemberRecord, TeamRoster};
use contextos_workflows::{
    Actor, Assignee, AttentionEvent, JobFacts, JobFingerprint, JobKey, JobLifecycle, StoredFailure, StoredValidation,
    SuspensionRecord, ValidationRecord, ValidationReport,
};
use time::OffsetDateTime;
use time::macros::datetime;

use super::*;
use crate::job_attention::JobFilePaths;

/// When the scheduler reconciled, which is not when any episode happened.
const RECONCILED_AT: OffsetDateTime = datetime!(2026-10-22 09:00:00 UTC);
/// When the job was suspended.
const SUSPENDED_AT: OffsetDateTime = datetime!(2026-10-21 10:30:00 UTC);
/// When the validation that failed was made.
const VALIDATED_AT: OffsetDateTime = datetime!(2026-10-20 08:15:00 UTC);

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn key() -> JobKey {
    JobKey {
        package: "ops".to_owned(),
        workflow: "sync".to_owned(),
        job: "nightly".to_owned(),
    }
}

fn paths() -> JobFilePaths {
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

fn failure(code: &str, artefact: &str) -> StoredFailure {
    StoredFailure {
        code: code.to_owned(),
        artefact: artefact.to_owned(),
        message: "a message that is never cited".to_owned(),
    }
}

fn facts(lifecycle: JobLifecycle, failures: Vec<StoredFailure>) -> JobFacts {
    let mut validation = StoredValidation::from(ValidationRecord {
        report: &ValidationReport::default(),
        fingerprint: Some(version()),
        at: VALIDATED_AT,
    });
    validation.passed = false;
    validation.failures = failures;
    JobFacts {
        key: key(),
        lifecycle,
        ready: true,
        version: Some(version()),
        validation: Some(validation),
        suspension: Some(SuspensionRecord {
            code: "workflow/probation-failed".to_owned(),
            by: Some(Actor::System),
            at: Some(SUSPENDED_AT),
            reason: "a reason that is never cited".to_owned(),
            run: Some(7),
            failure: Some("workflow/script-error".to_owned()),
        }),
        latest_run: None,
    }
}

fn roster() -> Result<TeamRoster, Box<dyn std::error::Error>> {
    Ok(TeamRoster::try_from(TeamFile {
        member: vec![TeamMemberRecord {
            slug: "atlas".to_owned(),
            name: "Atlas".to_owned(),
            kind: MemberKind::Agent,
            chief_of_staff: true,
        }],
    })?)
}

fn event(purpose: AttentionPurpose, episode: Episode, codes: &[&str]) -> AttentionEvent {
    AttentionEvent {
        purpose,
        episode,
        codes: codes.iter().map(|code| (*code).to_owned()).collect(),
    }
}

/// The title and body of the task for `event` against `facts`, with `run_record` as the evidence of the run.
fn note(
    event: &AttentionEvent,
    facts: &JobFacts,
    run_record: Option<&str>,
) -> Result<(String, String), Box<dyn std::error::Error>> {
    let roster = roster()?;
    let assignee = Assignee::from((event, &OwnerStanding::Undeclared, &roster));
    let files = paths();
    let source = TaskSource {
        event,
        facts,
        files: &files,
        run_record,
        entity: "zenith",
        assignee: &assignee,
        at: RECONCILED_AT,
    };
    Ok((title_of(&source), body_of(&source)))
}

fn section<'a>(body: &'a str, heading: &str) -> &'a str {
    let after = body.split(heading).nth(1).unwrap_or_default();
    after.split("\n## ").next().unwrap_or_default()
}

#[test]
fn a_title_names_the_job_and_leaves_out_a_code_the_event_does_not_have() -> TestResult {
    let state = facts(JobLifecycle::Suspended, Vec::new());
    let cases = [
        (
            event(AttentionPurpose::ValidationFailed, Episode::Version(version()), &[]),
            "Job ops/sync/nightly failed validation",
        ),
        (
            event(AttentionPurpose::Suspended, Episode::Suspension(SUSPENDED_AT), &[]),
            "Job ops/sync/nightly was suspended",
        ),
        (
            event(AttentionPurpose::RunFailed, Episode::Run(9), &[]),
            "Scheduled run R-9 of ops/sync/nightly failed",
        ),
        (
            event(AttentionPurpose::RunFailed, Episode::Run(9), &["workflow/timed-out"]),
            "Scheduled run R-9 of ops/sync/nightly failed: workflow/timed-out",
        ),
    ];

    for (event, expected) in cases {
        assert_eq!(note(&event, &state, None)?.0, expected);
    }
    Ok(())
}

#[test]
fn a_run_failed_title_names_the_episode_as_written_when_it_is_not_a_run_number() -> TestResult {
    let state = facts(JobLifecycle::Enabled, Vec::new());
    let odd = event(
        AttentionPurpose::RunFailed,
        Episode::Suspension(SUSPENDED_AT),
        &["workflow/timed-out"],
    );

    let (title, _) = note(&odd, &state, None)?;

    assert_eq!(
        title,
        "Scheduled run 20261021T103000Z of ops/sync/nightly failed: workflow/timed-out"
    );
    Ok(())
}

#[test]
fn the_body_has_context_then_evidence_then_what_to_do_in_that_order() -> TestResult {
    let state = facts(JobLifecycle::Enabled, Vec::new());
    let failed = event(AttentionPurpose::RunFailed, Episode::Run(9), &["workflow/script-error"]);

    let (_, body) = note(&failed, &state, None)?;

    let at = |heading: &str| body.find(heading);
    assert!(at("## Context") < at("## Evidence"), "{body}");
    assert!(at("## Evidence") < at("## What to do"), "{body}");
    assert!(body.ends_with('\n'), "a note ends with a newline: {body:?}");
    Ok(())
}

#[test]
fn the_evidence_cites_the_run_record_only_when_there_is_one() -> TestResult {
    let state = facts(JobLifecycle::Enabled, Vec::new());
    let failed = event(AttentionPurpose::RunFailed, Episode::Run(9), &["workflow/script-error"]);

    let (_, with) = note(&failed, &state, Some("memory/jobs/ops/sync/nightly/R-9.md"))?;
    let (_, without) = note(&failed, &state, None)?;

    assert!(
        section(&with, "## Evidence").contains("[[memory/jobs/ops/sync/nightly/R-9.md]]"),
        "{with}"
    );
    assert!(
        !section(&without, "## Evidence").contains("The run record"),
        "{without}"
    );
    assert!(
        section(&without, "## Evidence").contains("memory/jobs/ops/sync/nightly/"),
        "the run history is cited either way: {without}"
    );
    Ok(())
}

#[test]
fn a_validation_context_lists_each_failure_code_and_artefact_once_and_never_a_message() -> TestResult {
    let state = facts(
        JobLifecycle::Draft,
        vec![
            failure("validation/script-syntax", "sync.rhai"),
            failure("validation/script-syntax", "sync.rhai"),
            failure("validation/owner-unresolved", "nightly.toml"),
        ],
    );
    let failed = event(
        AttentionPurpose::ValidationFailed,
        Episode::Version(version()),
        &["validation/script-syntax", "validation/owner-unresolved"],
    );

    let (_, body) = note(&failed, &state, None)?;

    let context = section(&body, "## Context");
    assert_eq!(context.matches("`validation/script-syntax`").count(), 1, "{context}");
    assert!(context.contains("(concerning `nightly.toml`)"), "{context}");
    assert!(context.contains("state: draft"), "{context}");
    assert!(!body.contains("never cited"), "{body}");
    Ok(())
}

#[test]
fn a_suspension_context_lists_its_code_and_the_failure_code_of_the_run_without_repeating_either() -> TestResult {
    let state = facts(JobLifecycle::Suspended, Vec::new());
    let suspended = event(
        AttentionPurpose::Suspended,
        Episode::Suspension(SUSPENDED_AT),
        &["workflow/probation-failed", "workflow/script-error"],
    );

    let (_, body) = note(&suspended, &state, None)?;

    let context = section(&body, "## Context");
    assert_eq!(context.matches("`workflow/script-error`").count(), 1, "{context}");
    assert!(context.contains("`workflow/probation-failed`"), "{context}");
    assert!(
        !body.contains("never cited"),
        "the suspension's words are not copied: {body}"
    );
    Ok(())
}

#[test]
fn what_to_do_for_a_suspension_follows_its_code_and_an_unknown_code_points_at_the_suspension() -> TestResult {
    let state = facts(JobLifecycle::Suspended, Vec::new());
    let cases = [
        ("workflow/validation-failed", "so the new version passes validation"),
        ("workflow/probation-failed", "Read the run record for the failure"),
        ("workflow/owner-unresolved", "member of `.contextos/team.toml`"),
        ("workflow/package-unavailable", "Read the suspension on the job's page"),
    ];

    for (code, expected) in cases {
        let suspended = event(AttentionPurpose::Suspended, Episode::Suspension(SUSPENDED_AT), &[code]);
        let (_, body) = note(&suspended, &state, None)?;
        let todo = section(&body, "## What to do");
        assert!(todo.contains(expected), "{code}: {todo}");
        assert!(todo.contains("jobs resume <vault> ops sync nightly"), "{code}: {todo}");
    }
    Ok(())
}

#[test]
fn what_to_do_for_a_validation_failure_and_a_failed_run_names_the_next_step() -> TestResult {
    let state = facts(
        JobLifecycle::Draft,
        vec![failure("validation/script-syntax", "sync.rhai")],
    );
    let validation = event(
        AttentionPurpose::ValidationFailed,
        Episode::Version(version()),
        &["validation/script-syntax"],
    );
    let run = event(AttentionPurpose::RunFailed, Episode::Run(9), &["workflow/script-error"]);

    let (_, validating) = note(&validation, &state, None)?;
    let (_, running) = note(&run, &state, None)?;

    assert!(
        section(&validating, "## What to do").contains("contextos-web jobs validate <vault> ops sync nightly"),
        "{validating}"
    );
    assert!(
        section(&running, "## What to do").contains("Runs are not retried"),
        "{running}"
    );
    Ok(())
}

#[test]
fn a_suspension_context_says_when_it_was_suspended_and_not_when_the_scheduler_noticed() -> TestResult {
    let state = facts(JobLifecycle::Suspended, Vec::new());
    let suspended = event(
        AttentionPurpose::Suspended,
        Episode::Suspension(SUSPENDED_AT),
        &["workflow/probation-failed"],
    );

    let (_, body) = note(&suspended, &state, None)?;

    let context = section(&body, "## Context");
    assert!(
        context.contains("2026-10-21T10:30:00Z"),
        "the suspension's own time: {context}"
    );
    assert!(
        !context.contains("2026-10-22"),
        "not the time of the reconcile: {context}"
    );
    Ok(())
}

#[test]
fn a_validation_context_says_when_the_validation_was_made_and_not_when_the_scheduler_noticed() -> TestResult {
    let state = facts(
        JobLifecycle::Draft,
        vec![failure("validation/script-syntax", "sync.rhai")],
    );
    let failed = event(
        AttentionPurpose::ValidationFailed,
        Episode::Version(version()),
        &["validation/script-syntax"],
    );

    let (_, body) = note(&failed, &state, None)?;

    let context = section(&body, "## Context");
    assert!(
        context.contains("2026-10-20T08:15:00Z"),
        "the validation's own time: {context}"
    );
    assert!(
        !context.contains("2026-10-22"),
        "not the time of the reconcile: {context}"
    );
    Ok(())
}
