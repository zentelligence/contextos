use super::*;
use crate::caller::WorkflowFailureKind;
use crate::fingerprint::{ReleaseFingerprintInput, StoreKind};
use crate::lifecycle::EmptySuspensionReason;
use crate::logging::DegradedReasons;
use crate::run_record::MAX_ERROR_BYTES;
use crate::script::RunFailure;
use crate::validation::{Unverifiable, ValidationCode, ValidationFailure, ValidationReport};

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

/// The reason a failed run's verdict carries, or an empty string for any other verdict.
fn failure_reason(outcome: &RunOutcome) -> String {
    match FirstRunVerdict::try_from(outcome) {
        Ok(FirstRunVerdict::Failed { reason, .. }) => reason.as_str().to_owned(),
        Ok(FirstRunVerdict::Passed) | Err(NoVerdict) => String::new(),
    }
}

#[test]
fn a_run_that_succeeded_passes_its_probation() {
    assert_eq!(
        FirstRunVerdict::try_from(&RunOutcome::Succeeded).ok(),
        Some(FirstRunVerdict::Passed)
    );
}

#[test]
fn a_degraded_run_passes_its_probation() {
    let outcome = RunOutcome::Degraded(DegradedReasons {
        kept: vec!["a record was skipped".to_owned()],
        omitted: 0,
    });

    assert_eq!(FirstRunVerdict::try_from(&outcome).ok(), Some(FirstRunVerdict::Passed));
}

#[test]
fn a_failed_run_fails_its_probation_and_says_why() {
    let outcome = RunOutcome::Failed(RunFailure::from("the script threw".to_owned()));

    let reason = failure_reason(&outcome);

    assert!(reason.contains("the script threw"), "{reason}");
}

#[test]
fn a_timed_out_run_fails_its_probation() {
    let outcome = RunOutcome::Failed(RunFailure {
        kind: Some(WorkflowFailureKind::TimedOut),
        message: "workflow/timed-out: the run exceeded its limit".to_owned(),
    });

    assert!(matches!(
        FirstRunVerdict::try_from(&outcome),
        Ok(FirstRunVerdict::Failed { .. })
    ));
}

#[test]
fn a_cancelled_run_gives_no_verdict() {
    assert_eq!(FirstRunVerdict::try_from(&RunOutcome::Cancelled), Err(NoVerdict));
}

#[test]
fn a_failure_text_longer_than_the_error_limit_is_cut_in_the_reason() {
    let outcome = RunOutcome::Failed(RunFailure::from("x".repeat(MAX_ERROR_BYTES * 4)));

    let reason = failure_reason(&outcome);

    assert!(reason.len() < MAX_ERROR_BYTES * 2, "{}", reason.len());
}

#[test]
fn a_passed_verdict_for_a_version_is_the_probation_passed_transition() {
    assert_eq!(
        Transition::from((FirstRunVerdict::Passed, version(), Some(3))),
        Transition::ProbationPassed(version())
    );
}

#[test]
fn a_failed_verdict_for_a_version_is_the_probation_failed_transition() -> Result<(), EmptySuspensionReason> {
    let reason = SuspensionReason::try_from("it failed")?;

    assert_eq!(
        Transition::from((
            FirstRunVerdict::Failed {
                reason: reason.clone(),
                code: Some("workflow/timed-out".to_owned()),
            },
            version(),
            Some(3),
        )),
        Transition::ProbationFailed(
            version(),
            crate::lifecycle::Suspension {
                cause: crate::lifecycle::SuspensionCause::ProbationFailed {
                    run: Some(3),
                    failure: Some("workflow/timed-out".to_owned()),
                },
                reason,
            }
        )
    );
    Ok(())
}

fn failing() -> ValidationReport {
    ValidationReport {
        failures: vec![ValidationFailure {
            code: ValidationCode::ScriptSyntax,
            artefact: "sync.rhai".to_owned(),
            message: "line 1: unexpected token".to_owned(),
        }],
        unverifiable: Vec::new(),
    }
}

fn unverifiable_only() -> ValidationReport {
    ValidationReport {
        failures: Vec::new(),
        unverifiable: vec![Unverifiable {
            check: "tools".to_owned(),
            reason: "the server is not connected".to_owned(),
        }],
    }
}

#[test]
fn a_new_version_that_passes_validation_goes_on_probation() {
    assert_eq!(
        NewVersionVerdict::from((&ValidationReport::default(), Some(version()))),
        NewVersionVerdict::OnProbation(version())
    );
}

#[test]
fn a_new_version_that_fails_validation_suspends_the_job_with_the_failures() {
    let verdict = NewVersionVerdict::from((&failing(), Some(version())));

    assert!(
        matches!(&verdict, NewVersionVerdict::Suspend(reason) if reason.as_str().contains("validation/script-syntax")),
        "{verdict:?}"
    );
}

#[test]
fn a_new_version_whose_files_cannot_be_read_suspends_the_job_even_with_nothing_listed() {
    let verdict = NewVersionVerdict::from((&ValidationReport::default(), None));

    assert!(matches!(verdict, NewVersionVerdict::Suspend(_)), "{verdict:?}");
}

#[test]
fn a_new_version_that_could_not_be_fully_checked_waits_rather_than_suspending_or_passing() {
    assert_eq!(
        NewVersionVerdict::from((&unverifiable_only(), Some(version()))),
        NewVersionVerdict::Wait
    );
}

#[test]
fn a_failure_outweighs_a_check_that_could_not_be_made() {
    let mut report = failing();
    report.unverifiable = unverifiable_only().unverifiable;

    assert!(matches!(
        NewVersionVerdict::from((&report, Some(version()))),
        NewVersionVerdict::Suspend(_)
    ));
}
