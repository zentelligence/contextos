//! A first live run whose record could not be started has no run number, and a suspension made for it says so rather
//! than naming a run that never existed.

use contextos_workflows::{
    Actor, FirstRunVerdict, JobFingerprint, JobSchedule, JobsState, ReleaseFingerprintInput, StoreKind, Suspension,
    SuspensionCause, SuspensionReason, Transition,
};
use time::macros::datetime;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn version() -> JobFingerprint {
    JobFingerprint::from(&ReleaseFingerprintInput {
        store: StoreKind::Host,
        package: "acme",
        release: "1.0.0",
        digest: "sha256:0",
        job_toml: "()",
        release_manifest: "",
        libraries: &[],
    })
}

fn failed_verdict() -> Result<FirstRunVerdict, Box<dyn std::error::Error>> {
    Ok(FirstRunVerdict::Failed {
        reason: SuspensionReason::try_from("the first live run failed")?,
        code: Some("workflow/script-error".to_owned()),
    })
}

#[test]
fn a_failed_trial_whose_run_has_no_number_settles_as_a_suspension_with_no_run() -> TestResult {
    let transition = Transition::from((failed_verdict()?, version(), None::<u64>));

    assert!(
        matches!(
            transition,
            Transition::ProbationFailed(
                _,
                Suspension {
                    cause: SuspensionCause::ProbationFailed { run: None, .. },
                    ..
                }
            )
        ),
        "{transition:?}"
    );
    Ok(())
}

#[test]
fn a_failed_trial_with_a_run_number_still_names_the_run() -> TestResult {
    let transition = Transition::from((failed_verdict()?, version(), Some(12_u64)));

    assert!(
        matches!(
            transition,
            Transition::ProbationFailed(
                _,
                Suspension {
                    cause: SuspensionCause::ProbationFailed { run: Some(12), .. },
                    ..
                }
            )
        ),
        "{transition:?}"
    );
    Ok(())
}

#[test]
fn a_suspension_for_a_trial_with_no_run_number_stores_no_run_and_writes_none() -> TestResult {
    let at = datetime!(2026-10-01 03:00:00 UTC);
    let mut state = JobsState::default();
    for (actor, transition) in [
        (Actor::Validator, Transition::Validated(version())),
        (Actor::DryRun, Transition::DryRunCompleted(version())),
        (
            Actor::Operator,
            Transition::Enable(JobSchedule::try_from("0 3 * * *")?, version()),
        ),
        (
            Actor::System,
            Transition::from((failed_verdict()?, version(), None::<u64>)),
        ),
    ] {
        state.transition("pkg", "wf", "job", actor, &transition, at)?;
    }

    let record = state
        .job("pkg", "wf", "job")
        .and_then(|job| job.suspension())
        .ok_or("a suspension record is stored")?;

    assert_eq!(record.run, None, "no run is named, and 0 is not a run");
    let written = toml::to_string(&state)?;
    assert!(!written.contains("\nrun = "), "{written}");
    Ok(())
}
