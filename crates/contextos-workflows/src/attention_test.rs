use contextos_core::{MemberKind, TeamFile, TeamMemberRecord};
use time::macros::datetime;

use super::*;
use crate::fingerprint::{ReleaseFingerprintInput, StoreKind};
use crate::lifecycle::Actor;
use crate::state::{StoredFailure, ValidationRecord};
use crate::validation::ValidationReport;

const AT: OffsetDateTime = datetime!(2026-10-21 10:30:00 UTC);

fn version(salt: &str) -> JobFingerprint {
    JobFingerprint::from(&ReleaseFingerprintInput {
        store: StoreKind::Host,
        package: "acme",
        release: "1.0.0",
        digest: "sha256:0",
        job_toml: "name = \"Nightly\"",
        release_manifest: salt,
        libraries: &[],
    })
}

fn key() -> JobKey {
    JobKey {
        package: "ops".to_owned(),
        workflow: "sync".to_owned(),
        job: "nightly".to_owned(),
    }
}

fn validation(version: Option<JobFingerprint>, failures: usize, unverifiable: usize, by: Actor) -> StoredValidation {
    let mut stored = StoredValidation::from(ValidationRecord {
        report: &ValidationReport::default(),
        fingerprint: version,
        at: AT,
    })
    .made_by(by);
    stored.passed = failures == 0 && unverifiable == 0;
    stored.failures = (0..failures)
        .map(|index| StoredFailure {
            code: "validation/script-syntax".to_owned(),
            artefact: format!("script-{index}.rhai"),
            message: "a message".to_owned(),
        })
        .collect();
    stored.unverifiable = (0..unverifiable)
        .map(|index| format!("check {index}: unreachable"))
        .collect();
    stored
}

fn suspension(code: &str, by: Actor, at: OffsetDateTime) -> SuspensionRecord {
    SuspensionRecord {
        code: code.to_owned(),
        by: Some(by),
        at: Some(at),
        reason: "a reason".to_owned(),
        run: None,
        failure: None,
    }
}

fn facts(lifecycle: JobLifecycle) -> JobFacts {
    JobFacts {
        key: key(),
        lifecycle,
        ready: true,
        version: Some(version("a")),
        validation: None,
        suspension: None,
        latest_run: None,
    }
}

fn run(trigger: RunTrigger, status: RunStatus, dry_run: bool) -> FinishedRun {
    FinishedRun {
        number: 7,
        trigger,
        status,
        dry_run,
        failure_code: (status == RunStatus::Failed).then(|| "workflow/script-error".to_owned()),
    }
}

fn roster() -> Result<TeamRoster, Box<dyn std::error::Error>> {
    let record = |slug: &str, chief: bool| TeamMemberRecord {
        slug: slug.to_owned(),
        name: slug.to_owned(),
        kind: MemberKind::Agent,
        chief_of_staff: chief,
    };
    Ok(TeamRoster::try_from(TeamFile {
        member: vec![record("atlas", true), record("vela", false)],
    })?)
}

fn event(purpose: AttentionPurpose, codes: &[&str]) -> AttentionEvent {
    AttentionEvent {
        purpose,
        episode: Episode::Run(7),
        codes: codes.iter().map(|code| (*code).to_owned()).collect(),
    }
}

#[test]
fn an_origin_names_the_raiser_purpose_job_and_episode_in_that_order() {
    let origin = Origin::from((&AttentionPurpose::RunFailed, &key(), &Episode::Run(7)));

    assert_eq!(origin.to_string(), "contextos-web:run-failed:ops/sync/nightly:R-7");
    assert_eq!(
        Origin::from((&AttentionPurpose::Suspended, &key(), &Episode::Suspension(AT))).to_string(),
        "contextos-web:suspended:ops/sync/nightly:20261021T103000Z"
    );
    assert_eq!(
        Origin::from((&AttentionPurpose::ValidationFailed, &key(), &Episode::Unread(AT))).to_string(),
        "contextos-web:validation-failed:ops/sync/nightly:unread-20261021T103000Z"
    );
}

#[test]
fn a_validation_episode_is_the_first_twelve_hex_characters_of_the_version_or_unread_and_the_utc_time() {
    let fingerprint = version("a");
    let hex = fingerprint.as_str().trim_start_matches("sha256:").to_owned();

    assert_eq!(Episode::Version(fingerprint).to_string(), format!("v-{}", &hex[..12]));
    assert_eq!(Episode::Unread(AT).to_string(), "unread-20261021T103000Z");
}

#[test]
fn a_suspension_episode_is_its_time_in_utc_whatever_offset_it_was_stored_with() {
    for stored in [
        datetime!(2026-10-21 10:30:00 UTC),
        datetime!(2026-10-21 21:30:00 +11:00),
        datetime!(2026-10-21 03:00:00 -07:30),
    ] {
        assert_eq!(Episode::Suspension(stored).to_string(), "20261021T103000Z", "{stored}");
    }
}

#[test]
fn a_run_episode_is_its_run_id() {
    assert_eq!(Episode::Run(7).to_string(), "R-7");
    assert_eq!(Episode::Run(1204).to_string(), "R-1204");
}

#[test]
fn a_ready_draft_whose_automatic_validation_failed_for_its_current_version_is_an_event() {
    let mut draft = facts(JobLifecycle::Draft);
    draft.validation = Some(validation(draft.version.clone(), 2, 0, Actor::Validator));

    let found = events(&draft);

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].purpose, AttentionPurpose::ValidationFailed);
    assert_eq!(found[0].episode, Episode::Version(version("a")));
    assert_eq!(found[0].codes, vec!["validation/script-syntax".to_owned()]);
}

#[test]
fn a_ready_draft_whose_files_could_not_be_read_is_an_unread_event_at_the_time_it_was_stored() {
    let mut draft = facts(JobLifecycle::Draft);
    draft.version = None;
    draft.validation = Some(validation(None, 1, 0, Actor::Validator));

    let found = events(&draft);

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].episode, Episode::Unread(AT));
}

#[test]
fn no_event_for_a_draft_not_ready_a_validation_by_the_operator_one_for_another_version_or_one_with_only_unverifiable_checks()
 {
    let current = version("a");
    let mut baseline = facts(JobLifecycle::Draft);
    baseline.validation = Some(validation(Some(current.clone()), 1, 0, Actor::Validator));
    assert_eq!(
        events(&baseline).len(),
        1,
        "the same draft, validated automatically, is an event"
    );
    let table: Vec<(&str, JobFacts)> = vec![
        ("not ready", {
            let mut found = facts(JobLifecycle::Draft);
            found.ready = false;
            found.validation = Some(validation(Some(current.clone()), 1, 0, Actor::Validator));
            found
        }),
        ("asked for by the operator", {
            let mut found = facts(JobLifecycle::Draft);
            found.validation = Some(validation(Some(current.clone()), 1, 0, Actor::Operator));
            found
        }),
        ("for another version", {
            let mut found = facts(JobLifecycle::Draft);
            found.validation = Some(validation(Some(version("b")), 1, 0, Actor::Validator));
            found
        }),
        ("only unverifiable checks", {
            let mut found = facts(JobLifecycle::Draft);
            found.validation = Some(validation(Some(current.clone()), 0, 2, Actor::Validator));
            found
        }),
        ("passed", {
            let mut found = facts(JobLifecycle::Draft);
            found.validation = Some(validation(Some(current.clone()), 0, 0, Actor::Validator));
            found
        }),
        ("no validation", facts(JobLifecycle::Draft)),
        ("a validated job", {
            let mut found = facts(JobLifecycle::Validated);
            found.validation = Some(validation(Some(current.clone()), 1, 0, Actor::Validator));
            found
        }),
        ("a registered job", {
            let mut found = facts(JobLifecycle::Registered);
            found.validation = Some(validation(Some(current.clone()), 1, 0, Actor::Validator));
            found
        }),
        ("an enabled job", {
            let mut found = facts(JobLifecycle::Enabled);
            found.validation = Some(validation(Some(current.clone()), 1, 0, Actor::Validator));
            found
        }),
    ];

    for (name, case) in table {
        assert!(events(&case).is_empty(), "{name}: {:?}", events(&case));
    }
}

#[test]
fn every_system_suspension_is_an_event_and_an_operator_suspension_is_not() {
    let codes = [
        "workflow/validation-failed",
        "workflow/probation-failed",
        "workflow/owner-unresolved",
        "workflow/package-unavailable",
        "workflow/suspended",
    ];
    for code in codes {
        let mut suspended = facts(JobLifecycle::Suspended);
        suspended.suspension = Some(suspension(code, Actor::System, AT));

        let found = events(&suspended);

        assert_eq!(found.len(), 1, "{code}");
        assert_eq!(found[0].purpose, AttentionPurpose::Suspended, "{code}");
        assert_eq!(found[0].episode, Episode::Suspension(AT), "{code}");
        assert_eq!(found[0].codes, vec![code.to_owned()], "{code}");
    }
    let mut by_operator = facts(JobLifecycle::Suspended);
    by_operator.suspension = Some(suspension("workflow/operator-suspended", Actor::Operator, AT));
    assert_eq!(events(&by_operator), Vec::new());
}

#[test]
fn a_failed_or_timed_out_scheduled_run_of_an_enabled_job_is_an_event() {
    for code in ["workflow/script-error", "workflow/timed-out"] {
        let mut enabled = facts(JobLifecycle::Enabled);
        let mut failed = run(RunTrigger::Scheduled, RunStatus::Failed, false);
        failed.failure_code = Some(code.to_owned());
        enabled.latest_run = Some(failed);

        let found = events(&enabled);

        assert_eq!(found.len(), 1, "{code}");
        assert_eq!(found[0].purpose, AttentionPurpose::RunFailed, "{code}");
        assert_eq!(found[0].episode, Episode::Run(7), "{code}");
        assert_eq!(found[0].codes, vec![code.to_owned()], "{code}");
    }
}

#[test]
fn no_event_for_degraded_skipped_interrupted_cancelled_manual_or_dry_runs() {
    let mut baseline = facts(JobLifecycle::Enabled);
    baseline.latest_run = Some(run(RunTrigger::Scheduled, RunStatus::Failed, false));
    assert_eq!(events(&baseline).len(), 1, "a failed scheduled run is an event");
    let table = [
        ("degraded", run(RunTrigger::Scheduled, RunStatus::Degraded, false)),
        ("skipped", run(RunTrigger::Scheduled, RunStatus::Skipped, false)),
        ("interrupted", run(RunTrigger::Scheduled, RunStatus::Interrupted, false)),
        ("cancelled", run(RunTrigger::Scheduled, RunStatus::Cancelled, false)),
        ("succeeded", run(RunTrigger::Scheduled, RunStatus::Succeeded, false)),
        ("manual failure", run(RunTrigger::Manual, RunStatus::Failed, false)),
        ("dry run failure", run(RunTrigger::Scheduled, RunStatus::Failed, true)),
    ];
    for (name, finished) in table {
        let mut enabled = facts(JobLifecycle::Enabled);
        enabled.latest_run = Some(finished);
        assert!(events(&enabled).is_empty(), "{name}: {:?}", events(&enabled));
    }
}

#[test]
fn a_failed_scheduled_run_of_a_suspended_job_is_not_a_run_failed_event() {
    let mut suspended = facts(JobLifecycle::Suspended);
    suspended.suspension = Some(suspension("workflow/operator-suspended", Actor::Operator, AT));
    suspended.latest_run = Some(run(RunTrigger::Scheduled, RunStatus::Failed, false));

    assert!(
        events(&suspended)
            .iter()
            .all(|found| found.purpose != AttentionPurpose::RunFailed)
    );
    // The same failed run is an event while the job is enabled, so it is the state that silences it.
    let mut enabled = suspended.clone();
    enabled.lifecycle = JobLifecycle::Enabled;
    enabled.suspension = None;
    assert_eq!(events(&enabled).len(), 1);
}

#[test]
fn the_assignee_is_the_owner_when_a_member_and_otherwise_the_chief_of_staff() -> Result<(), Box<dyn std::error::Error>>
{
    let roster = roster()?;
    let vela = roster
        .member(&MemberSlug::try_from("vela")?)
        .ok_or("vela is a member")?;
    let failed = event(AttentionPurpose::RunFailed, &["workflow/script-error"]);

    let for_member = Assignee::from((&failed, &OwnerStanding::Member(vela), &roster));
    let for_stranger = Assignee::from((
        &failed,
        &OwnerStanding::NotAMember(MemberSlug::try_from("ghost")?),
        &roster,
    ));
    let for_nobody = Assignee::from((&failed, &OwnerStanding::Undeclared, &roster));

    assert_eq!(for_member.slug().as_str(), "vela");
    assert_eq!(for_stranger.slug().as_str(), "atlas");
    assert_eq!(for_nobody.slug().as_str(), "atlas");
    Ok(())
}

#[test]
fn an_owner_unresolved_suspension_is_always_assigned_to_the_chief_of_staff() -> Result<(), Box<dyn std::error::Error>> {
    let roster = roster()?;
    let vela = roster
        .member(&MemberSlug::try_from("vela")?)
        .ok_or("vela is a member")?;
    let unresolved = event(AttentionPurpose::Suspended, &["workflow/owner-unresolved"]);

    let assignee = Assignee::from((&unresolved, &OwnerStanding::Member(vela), &roster));

    assert_eq!(assignee.slug().as_str(), "atlas");
    Ok(())
}

/// The facts of a suspended job whose stored entry is `entry`, a TOML table body read as a release before structured
/// suspensions wrote it.
fn suspended_facts_from(entry: &str) -> Result<JobFacts, Box<dyn std::error::Error>> {
    let state: crate::state::PackageJobsState = toml::from_str(&format!("[workflows.sync.jobs.nightly]\n{entry}"))?;
    let stored = state.job("sync", "nightly").ok_or("the job has an entry")?;
    Ok(JobFacts {
        suspension: stored.suspension().cloned(),
        ..facts(JobLifecycle::Suspended)
    })
}

#[test]
fn a_legacy_suspension_recorded_by_the_system_raises_a_suspended_task() -> Result<(), Box<dyn std::error::Error>> {
    let found = events(&suspended_facts_from(
        "lifecycle = \"suspended\"\nlifecycle_changed = \"2026-10-21T10:30:00Z\"\nlifecycle_actor = \"system\"\n\
         suspended_reason = \"validation failed\"\n",
    )?);

    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].purpose, AttentionPurpose::Suspended);
    Ok(())
}

#[test]
fn a_legacy_suspension_whose_actor_was_never_recorded_raises_no_task_because_it_may_be_the_operators()
-> Result<(), Box<dyn std::error::Error>> {
    let found = events(&suspended_facts_from(
        "lifecycle = \"suspended\"\nlifecycle_changed = \"2026-10-21T10:30:00Z\"\nsuspended_reason = \"paused\"\n",
    )?);

    assert!(
        found.is_empty(),
        "an unknown actor is not assumed to be the system: {found:?}"
    );
    Ok(())
}
