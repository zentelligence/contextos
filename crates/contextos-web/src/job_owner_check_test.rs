use contextos_core::{MemberKind, TeamFile, TeamMemberRecord};
use contextos_workflows::{Applied, JobLifecycle, JobSchedule, MAX_ERROR_BYTES, Transition};
use time::macros::datetime;

use super::*;

const NOW: OffsetDateTime = datetime!(2026-10-22 09:00:00 UTC);
const ENABLED_AT: OffsetDateTime = datetime!(2026-10-01 03:00:00 UTC);

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn roster() -> Result<TeamRoster, Box<dyn std::error::Error>> {
    let member = |slug: &str, chief: bool| TeamMemberRecord {
        slug: slug.to_owned(),
        name: slug.to_owned(),
        kind: MemberKind::Agent,
        chief_of_staff: chief,
    };
    Ok(TeamRoster::try_from(TeamFile {
        member: vec![member("atlas", true), member("vela", false)],
    })?)
}

fn version() -> contextos_workflows::JobFingerprint {
    contextos_workflows::JobFingerprint::from(&contextos_workflows::ReleaseFingerprintInput {
        store: contextos_workflows::StoreKind::Vault,
        package: "ops",
        release: "1.0.0",
        digest: "sha256:00",
        job_toml: "",
        release_manifest: "",
        libraries: &[],
    })
}

/// A state holding one enabled job, `sync/nightly`.
fn enabled() -> Result<JobsState, Box<dyn std::error::Error>> {
    let mut state = JobsState::default();
    for (actor, transition) in [
        (Actor::Validator, Transition::Validated(version())),
        (Actor::DryRun, Transition::DryRunCompleted(version())),
        (
            Actor::Operator,
            Transition::Enable(JobSchedule::try_from("0 3 * * *")?, version()),
        ),
    ] {
        state.transition("ops", "sync", "nightly", actor, &transition, ENABLED_AT)?;
    }
    Ok(state)
}

fn suspended_by(state: &JobsState) -> Option<(String, Actor, String)> {
    state
        .job("ops", "sync", "nightly")
        .and_then(|job| job.suspension())
        .and_then(|record| Some((record.code.clone(), record.by?, record.reason.clone())))
}

fn suspend(state: &mut JobsState, declared: Option<&DeclaredOwner>) -> Result<bool, Box<dyn std::error::Error>> {
    Ok(suspend_for_unresolved_owner(
        state,
        JobScope {
            package: "ops",
            workflow: "sync",
            job: "nightly",
        },
        declared,
        &roster()?,
        NOW,
    ))
}

#[test]
fn an_enabled_job_whose_owner_is_a_member_is_left_alone() -> TestResult {
    let mut state = enabled()?;
    let vela = DeclaredOwner::from("vela");

    let suspended = suspend(&mut state, Some(&vela))?;

    assert!(!suspended);
    assert_eq!(state.lifecycle_of("ops", "sync", "nightly"), JobLifecycle::Enabled);
    assert_eq!(suspended_by(&state), None);
    Ok(())
}

#[test]
fn an_owner_who_is_not_a_member_suspends_the_job_by_the_system_naming_what_the_file_declares() -> TestResult {
    let mut state = enabled()?;
    let ghost = DeclaredOwner::from("ghost");

    let suspended = suspend(&mut state, Some(&ghost))?;

    assert!(suspended);
    assert_eq!(state.lifecycle_of("ops", "sync", "nightly"), JobLifecycle::Suspended);
    let (code, by, reason) = suspended_by(&state).ok_or("a suspension is stored")?;
    assert_eq!(code, "workflow/owner-unresolved");
    assert_eq!(by, Actor::System);
    assert!(reason.contains("\"ghost\""), "{reason}");
    assert!(reason.contains(".contextos/team.toml"), "{reason}");
    Ok(())
}

#[test]
fn an_owner_that_is_not_a_slug_is_named_as_written_and_never_lowercased_to_match_a_member() -> TestResult {
    let mut state = enabled()?;
    let shouted = DeclaredOwner::from("Vela");

    let suspended = suspend(&mut state, Some(&shouted))?;

    assert!(suspended, "Vela is not vela");
    let (_, _, reason) = suspended_by(&state).ok_or("a suspension is stored")?;
    assert!(reason.contains("\"Vela\""), "{reason}");
    Ok(())
}

#[test]
fn a_job_that_declares_no_owner_is_suspended_saying_so() -> TestResult {
    let mut state = enabled()?;

    let suspended = suspend(&mut state, None)?;

    assert!(suspended);
    let (code, _, reason) = suspended_by(&state).ok_or("a suspension is stored")?;
    assert_eq!(code, "workflow/owner-unresolved");
    assert!(reason.contains("declares no owner"), "{reason}");
    Ok(())
}

#[test]
fn a_job_that_is_not_enabled_is_not_suspended_because_the_state_refuses_the_transition() -> TestResult {
    let mut state = JobsState::default();
    state.transition(
        "ops",
        "sync",
        "nightly",
        Actor::Validator,
        &Transition::Validated(version()),
        ENABLED_AT,
    )?;

    let suspended = suspend(&mut state, None)?;

    assert!(!suspended);
    assert_eq!(
        state.transition(
            "ops",
            "sync",
            "nightly",
            Actor::System,
            &Transition::VersionChanged,
            NOW,
        ),
        Ok(Applied::To(JobLifecycle::Draft)),
        "the refused suspension left the job as it was"
    );
    Ok(())
}

#[test]
fn a_multi_kilobyte_owner_text_is_cut_to_the_length_a_recorded_reason_may_have() -> TestResult {
    let mut state = enabled()?;
    let huge = DeclaredOwner::from("Owner ".repeat(2_000).as_str());

    let suspended = suspend(&mut state, Some(&huge))?;

    assert!(suspended, "a long owner is still a suspension");
    let (_, _, reason) = suspended_by(&state).ok_or("a suspension is stored")?;
    assert!(
        reason.len() <= MAX_ERROR_BYTES,
        "the reason is {} bytes, over the {MAX_ERROR_BYTES} a recorded reason may have",
        reason.len()
    );
    Ok(())
}

#[test]
fn a_long_slug_that_names_no_member_is_cut_the_same_way() -> TestResult {
    let mut state = enabled()?;
    let long = DeclaredOwner::from("x".repeat(5_000).as_str());

    assert!(suspend(&mut state, Some(&long))?);

    let (_, _, reason) = suspended_by(&state).ok_or("a suspension is stored")?;
    assert!(reason.len() <= MAX_ERROR_BYTES, "{} bytes", reason.len());
    Ok(())
}
