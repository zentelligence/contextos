use time::macros::datetime;

use super::*;
use crate::fingerprint::{JobFingerprint, ReleaseFingerprintInput, StoreKind};
use crate::lifecycle::{
    Actor, Applied, JobLifecycle, JobSchedule, Suspension, SuspensionCause, SuspensionReason, Transition,
    TransitionError,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const NOW: OffsetDateTime = datetime!(2026-10-01 03:00:00 UTC);
const LATER: OffsetDateTime = datetime!(2026-10-02 03:00:00 UTC);

fn version(script: &str) -> JobFingerprint {
    JobFingerprint::from(&ReleaseFingerprintInput {
        store: StoreKind::Host,
        package: "acme",
        release: "1.0.0",
        digest: "sha256:0",
        job_toml: script,
        release_manifest: "",
        libraries: &[],
    })
}

fn make(state: &mut PackageJobsState, actor: Actor, transition: &Transition) -> Result<Applied, TransitionError> {
    state.transition("wf", "job", actor, transition, LATER)
}

fn failed(version: JobFingerprint) -> Result<Transition, crate::lifecycle::EmptySuspensionReason> {
    Ok(Transition::ProbationFailed(
        version,
        Suspension {
            cause: SuspensionCause::ProbationFailed {
                run: Some(1),
                failure: None,
            },
            reason: SuspensionReason::try_from("the first live run failed")?,
        },
    ))
}

/// A job taken to `enabled` through the real transitions, validated and dry-run for `()`.
fn enabled() -> Result<PackageJobsState, Box<dyn std::error::Error>> {
    let mut state = PackageJobsState::default();
    state.transition(
        "wf",
        "job",
        Actor::Validator,
        &Transition::Validated(version("()")),
        NOW,
    )?;
    state.transition(
        "wf",
        "job",
        Actor::DryRun,
        &Transition::DryRunCompleted(version("()")),
        NOW,
    )?;
    state.transition(
        "wf",
        "job",
        Actor::Operator,
        &Transition::Enable(JobSchedule::try_from("0 3 * * *")?, version("()")),
        NOW,
    )?;
    Ok(state)
}

fn pending(state: &PackageJobsState) -> bool {
    state.job("wf", "job").is_some_and(JobState::first_run_pending)
}

#[test]
fn a_job_that_has_never_been_enabled_has_no_first_run_pending() -> TestResult {
    assert!(!pending(&PackageJobsState::default()));
    let mut state = PackageJobsState::default();
    state.transition(
        "wf",
        "job",
        Actor::Validator,
        &Transition::Validated(version("()")),
        NOW,
    )?;
    assert!(!pending(&state));
    Ok(())
}

#[test]
fn enabling_a_job_puts_its_first_live_run_on_trial() -> TestResult {
    assert!(pending(&enabled()?));
    Ok(())
}

#[test]
fn a_new_version_that_passes_validation_keeps_the_job_enabled_on_trial_without_repeating_the_dry_run() -> TestResult {
    let mut state = enabled()?;
    make(&mut state, Actor::System, &Transition::ProbationPassed(version("()")))?;
    assert!(!pending(&state));

    let applied = make(&mut state, Actor::Validator, &Transition::OnProbation(version("();")))?;

    assert_eq!(applied, Applied::To(JobLifecycle::Enabled));
    let job = state.job("wf", "job");
    assert_eq!(job.map(JobState::lifecycle), Some(JobLifecycle::Enabled));
    assert_eq!(job.and_then(JobState::fingerprint), Some(&version("();")));
    assert_eq!(
        job.and_then(JobState::dry_run_at),
        Some(NOW),
        "the dry run is not repeated"
    );
    assert!(pending(&state));
    assert_eq!(state.scheduled_expression("wf", "job"), Some("0 3 * * *"));
    Ok(())
}

#[test]
fn a_first_live_run_that_passed_ends_the_trial_and_leaves_the_state_and_its_history_alone() -> TestResult {
    let mut state = enabled()?;

    let applied = make(&mut state, Actor::System, &Transition::ProbationPassed(version("()")))?;

    assert_eq!(applied, Applied::To(JobLifecycle::Enabled));
    assert!(!pending(&state));
    let job = state.job("wf", "job");
    assert_eq!(
        job.and_then(JobState::lifecycle_changed),
        Some(NOW),
        "no state change happened"
    );
    assert_eq!(job.and_then(JobState::lifecycle_actor), Some(Actor::Operator));
    Ok(())
}

#[test]
fn a_first_live_run_that_failed_suspends_the_job_with_the_reason_and_keeps_the_trial_pending() -> TestResult {
    let mut state = enabled()?;

    let applied = make(&mut state, Actor::System, &failed(version("()"))?)?;

    assert_eq!(applied, Applied::To(JobLifecycle::Suspended));
    let job = state.job("wf", "job");
    assert_eq!(
        job.and_then(JobState::suspended_reason),
        Some("the first live run failed")
    );
    assert_eq!(job.and_then(JobState::lifecycle_actor), Some(Actor::System));
    assert!(pending(&state), "a fixed job is still on trial when it is resumed");
    assert_eq!(state.scheduled_expression("wf", "job"), None);
    Ok(())
}

#[test]
fn an_outcome_for_a_version_other_than_the_one_on_trial_is_refused_and_changes_nothing() -> TestResult {
    let mut state = enabled()?;
    make(&mut state, Actor::Validator, &Transition::OnProbation(version("();")))?;
    let before = state.clone();

    for transition in [Transition::ProbationPassed(version("()")), failed(version("()"))?] {
        let refused = make(&mut state, Actor::System, &transition);
        assert!(
            matches!(refused, Err(TransitionError::WrongVersion { .. })),
            "{refused:?}"
        );
    }

    assert_eq!(state, before);
    Ok(())
}

#[test]
fn an_outcome_for_a_job_not_on_trial_is_refused_and_changes_nothing() -> TestResult {
    let mut state = enabled()?;
    make(&mut state, Actor::System, &Transition::ProbationPassed(version("()")))?;
    let before = state.clone();

    for transition in [Transition::ProbationPassed(version("()")), failed(version("()"))?] {
        let refused = make(&mut state, Actor::System, &transition);
        assert!(
            matches!(refused, Err(TransitionError::NotOnProbation { .. })),
            "{refused:?}"
        );
    }

    assert_eq!(state, before);
    Ok(())
}

#[test]
fn a_suspended_job_that_passes_a_manual_run_ends_its_trial_but_stays_suspended() -> TestResult {
    let mut state = enabled()?;
    make(&mut state, Actor::System, &failed(version("()"))?)?;

    let applied = make(&mut state, Actor::System, &Transition::ProbationPassed(version("()")))?;

    assert_eq!(applied, Applied::To(JobLifecycle::Suspended));
    assert!(!pending(&state));
    assert_eq!(
        state.job("wf", "job").and_then(JobState::suspended_reason),
        Some("the first live run failed"),
        "resuming is the operator's call"
    );
    Ok(())
}

#[test]
fn a_suspended_job_that_fails_a_manual_run_is_not_suspended_again() -> TestResult {
    let mut state = enabled()?;
    make(&mut state, Actor::System, &failed(version("()"))?)?;
    let before = state.clone();

    let refused = make(&mut state, Actor::System, &failed(version("()"))?);

    assert!(
        matches!(refused, Err(TransitionError::WrongState { .. })),
        "{refused:?}"
    );
    assert_eq!(state, before);
    Ok(())
}

#[test]
fn resuming_a_suspended_job_keeps_its_trial_pending() -> TestResult {
    let mut state = enabled()?;
    make(&mut state, Actor::System, &failed(version("()"))?)?;

    state.record_validation(
        "wf",
        "job",
        StoredValidation::from(ValidationRecord {
            report: &crate::validation::ValidationReport::default(),
            fingerprint: Some(version("()")),
            at: NOW,
        }),
    );
    make(&mut state, Actor::Operator, &Transition::Resume(version("()")))?;

    assert!(pending(&state));
    Ok(())
}

#[test]
fn a_job_that_leaves_enabled_or_suspended_has_no_trial_pending() -> TestResult {
    for leave in [Transition::ClearSchedule, Transition::Retire] {
        let mut state = enabled()?;
        make(&mut state, Actor::Operator, &leave)?;
        assert!(!pending(&state), "{leave:?}");
    }
    let mut state = enabled()?;
    make(&mut state, Actor::Operator, &Transition::Retire)?;
    make(&mut state, Actor::Operator, &Transition::Reactivate)?;
    assert!(!pending(&state));
    Ok(())
}

#[test]
fn a_job_enabled_again_after_its_schedule_was_cleared_is_on_trial_again() -> TestResult {
    let mut state = enabled()?;
    make(&mut state, Actor::System, &Transition::ProbationPassed(version("()")))?;
    make(&mut state, Actor::Operator, &Transition::ClearSchedule)?;

    make(
        &mut state,
        Actor::Operator,
        &Transition::Enable(JobSchedule::try_from("0 4 * * *")?, version("()")),
    )?;

    assert!(pending(&state));
    Ok(())
}

#[test]
fn the_trial_survives_a_round_trip_through_the_state_file() -> TestResult {
    let state = enabled()?;

    let text = toml::to_string(&state)?;
    let read: PackageJobsState = toml::from_str(&text)?;

    assert_eq!(read, state);
    assert!(pending(&read));
    assert!(text.contains("first_run"), "{text}");
    Ok(())
}

#[test]
fn a_job_not_on_trial_stores_no_trace_of_one() -> TestResult {
    let mut state = enabled()?;
    make(&mut state, Actor::System, &Transition::ProbationPassed(version("()")))?;

    assert!(!toml::to_string(&state)?.contains("first_run"));
    Ok(())
}

#[test]
fn only_an_enabled_job_whose_recorded_version_differs_from_its_current_one_is_due_a_review() -> TestResult {
    let mut state = enabled()?;
    assert!(!state.new_version_under_enabled_job("wf", "job", Some(&version("()"))));
    assert!(state.new_version_under_enabled_job("wf", "job", Some(&version("();"))));
    assert!(
        state.new_version_under_enabled_job("wf", "job", None),
        "files that cannot be read are a change of version"
    );

    make(&mut state, Actor::System, &failed(version("()"))?)?;
    assert!(
        !state.new_version_under_enabled_job("wf", "job", Some(&version("();"))),
        "a suspended job waits for the operator"
    );
    assert!(!PackageJobsState::default().new_version_under_enabled_job("wf", "job", Some(&version("()"))));
    Ok(())
}
