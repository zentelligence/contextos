//! The two checks that make enabling and resuming safe: a job is enabled only for the version that was validated and dry
//! run, and resumed only once validation has passed for the version it has now.

use time::macros::datetime;

use super::*;
use crate::fingerprint::{JobFingerprint, ReleaseFingerprintInput, StoreKind};
use crate::lifecycle::{
    Actor, Applied, EmptySuspensionReason, JobLifecycle, JobSchedule, Suspension, SuspensionReason, Transition,
    TransitionError,
};
use crate::validation::ValidationReport;

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

fn schedule() -> Result<JobSchedule, Box<dyn std::error::Error>> {
    Ok(JobSchedule::try_from("0 3 * * *")?)
}

fn make(state: &mut PackageJobsState, actor: Actor, transition: &Transition) -> Result<Applied, TransitionError> {
    state.transition("wf", "job", actor, transition, LATER)
}

fn reason() -> Result<SuspensionReason, EmptySuspensionReason> {
    SuspensionReason::try_from("stopped by the operator")
}

/// A job taken through the real transitions to `registered` for the script `()`.
fn registered() -> Result<PackageJobsState, Box<dyn std::error::Error>> {
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
    Ok(state)
}

/// A job suspended by the operator after being enabled for the script `()`.
fn suspended() -> Result<PackageJobsState, Box<dyn std::error::Error>> {
    let mut state = registered()?;
    make(
        &mut state,
        Actor::Operator,
        &Transition::Enable(schedule()?, version("()")),
    )?;
    make(&mut state, Actor::System, &Transition::ProbationPassed(version("()")))?;
    make(
        &mut state,
        Actor::Operator,
        &Transition::Suspend(Suspension::operator(reason()?)),
    )?;
    Ok(state)
}

fn validation(passed: bool, for_version: Option<JobFingerprint>) -> StoredValidation {
    let mut report = ValidationReport::default();
    if !passed {
        report.failures.push(crate::validation::ValidationFailure {
            code: crate::validation::ValidationCode::ScriptSyntax,
            artefact: "sync.rhai".to_owned(),
            message: "unexpected token".to_owned(),
        });
    }
    StoredValidation::from(ValidationRecord {
        report: &report,
        fingerprint: for_version,
        at: LATER,
    })
}

fn pending(state: &PackageJobsState) -> bool {
    state.job("wf", "job").is_some_and(JobState::first_run_pending)
}

fn recorded_version(state: &PackageJobsState) -> Option<JobFingerprint> {
    state.job("wf", "job").and_then(|job| job.fingerprint().cloned())
}

#[test]
fn a_registered_job_is_enabled_for_the_version_that_was_validated() -> TestResult {
    let mut state = registered()?;

    let applied = make(
        &mut state,
        Actor::Operator,
        &Transition::Enable(schedule()?, version("()")),
    )?;

    assert_eq!(applied, Applied::To(JobLifecycle::Enabled));
    assert!(pending(&state), "enabling puts the version's first live run on trial");
    Ok(())
}

#[test]
fn a_registered_job_whose_files_changed_since_validation_is_not_enabled() -> TestResult {
    let mut state = registered()?;
    let before = state.clone();

    let refused = make(
        &mut state,
        Actor::Operator,
        &Transition::Enable(schedule()?, version("changed()")),
    );

    assert_eq!(refused, Err(TransitionError::WrongVersion { transition: "Enable" }));
    assert_eq!(state, before, "a refused enabling writes nothing");
    Ok(())
}

#[test]
fn a_suspended_job_is_resumed_once_validation_passed_for_its_current_version() -> TestResult {
    let mut state = suspended()?;
    state.record_validation("wf", "job", validation(true, Some(version("()"))));

    let applied = make(&mut state, Actor::Operator, &Transition::Resume(version("()")))?;

    assert_eq!(applied, Applied::To(JobLifecycle::Enabled));
    assert_eq!(recorded_version(&state), Some(version("()")));
    assert!(
        !pending(&state),
        "the version already proved itself, so it is not on trial again"
    );
    Ok(())
}

#[test]
fn resuming_a_job_for_a_version_it_was_not_enabled_with_puts_that_version_on_trial() -> TestResult {
    let mut state = suspended()?;
    state.record_validation("wf", "job", validation(true, Some(version("fixed()"))));

    make(&mut state, Actor::Operator, &Transition::Resume(version("fixed()")))?;

    assert_eq!(recorded_version(&state), Some(version("fixed()")));
    assert!(pending(&state), "a new version's first live run is on trial");
    Ok(())
}

#[test]
fn a_suspended_job_with_no_validation_on_record_is_not_resumed() -> TestResult {
    let mut state = suspended()?;
    let before = state.clone();

    let refused = make(&mut state, Actor::Operator, &Transition::Resume(version("()")));

    assert_eq!(refused, Err(TransitionError::NotValidated { transition: "Resume" }));
    assert_eq!(state, before);
    Ok(())
}

#[test]
fn a_suspended_job_whose_last_validation_failed_is_not_resumed() -> TestResult {
    let mut state = suspended()?;
    state.record_validation("wf", "job", validation(false, Some(version("()"))));
    let before = state.clone();

    let refused = make(&mut state, Actor::Operator, &Transition::Resume(version("()")));

    assert!(
        matches!(refused, Err(TransitionError::NotValidated { .. })),
        "{refused:?}"
    );
    assert_eq!(state, before);
    Ok(())
}

#[test]
fn a_suspended_job_validated_for_another_version_is_not_resumed() -> TestResult {
    let mut state = suspended()?;
    state.record_validation("wf", "job", validation(true, Some(version("older()"))));
    let before = state.clone();

    let refused = make(&mut state, Actor::Operator, &Transition::Resume(version("()")));

    assert!(
        matches!(refused, Err(TransitionError::NotValidated { .. })),
        "{refused:?}"
    );
    assert_eq!(state, before);
    Ok(())
}

#[test]
fn a_validation_made_without_a_version_never_allows_a_resume() -> TestResult {
    let mut state = suspended()?;
    state.record_validation("wf", "job", validation(true, None));

    let refused = make(&mut state, Actor::Operator, &Transition::Resume(version("()")));

    assert!(
        matches!(refused, Err(TransitionError::NotValidated { .. })),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn only_the_operator_resumes_or_enables_whatever_the_version() -> TestResult {
    for actor in [Actor::System, Actor::Validator, Actor::DryRun] {
        let mut registered = registered()?;
        let enabling = make(&mut registered, actor, &Transition::Enable(schedule()?, version("()")));
        assert!(matches!(enabling, Err(TransitionError::WrongActor { .. })), "{actor:?}");

        let mut suspended = suspended()?;
        suspended.record_validation("wf", "job", validation(true, Some(version("()"))));
        let resuming = make(&mut suspended, actor, &Transition::Resume(version("()")));
        assert!(matches!(resuming, Err(TransitionError::WrongActor { .. })), "{actor:?}");
    }
    Ok(())
}

#[test]
fn a_resume_from_any_state_but_suspended_is_refused_before_the_validation_is_looked_at() -> TestResult {
    let mut state = registered()?;
    state.record_validation("wf", "job", validation(true, Some(version("()"))));

    let refused = make(&mut state, Actor::Operator, &Transition::Resume(version("()")));

    assert!(
        matches!(refused, Err(TransitionError::WrongState { .. })),
        "{refused:?}"
    );
    Ok(())
}
