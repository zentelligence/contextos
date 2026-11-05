use super::*;
use crate::caller::WorkflowFailureKind;
use crate::fingerprint::{ReleaseFingerprintInput, StoreKind};
use crate::release_state::{JobIdentity, ReplacedBy};
use crate::run_record::MAX_ERROR_BYTES;
use crate::script::RunFailure;
use crate::validation::{ValidationCode, ValidationFailure, ValidationReport};

const STATES: [JobLifecycle; 6] = [
    JobLifecycle::Draft,
    JobLifecycle::Validated,
    JobLifecycle::Registered,
    JobLifecycle::Enabled,
    JobLifecycle::Suspended,
    JobLifecycle::Retired,
];

const ACTORS: [Actor; 4] = [Actor::Operator, Actor::Validator, Actor::DryRun, Actor::System];

fn schedule() -> Result<JobSchedule, CronScheduleError> {
    JobSchedule::try_from("0 3 * * *")
}

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

fn reason() -> Result<SuspensionReason, EmptySuspensionReason> {
    SuspensionReason::try_from("the first live run failed")
}

fn replaced_by() -> ReplacedBy {
    ReplacedBy {
        job: "nightly-v2-0-0".to_owned(),
        identity: JobIdentity::try_from("job-2".to_owned()).unwrap_or_else(|_| unreachable!("a fixed form")),
    }
}

fn every_transition() -> Result<Vec<Transition>, Box<dyn std::error::Error>> {
    Ok(vec![
        Transition::Validated(version()),
        Transition::DryRunCompleted(version()),
        Transition::Enable(schedule()?, version()),
        Transition::SetSchedule(schedule()?),
        Transition::ClearSchedule,
        Transition::Suspend(Suspension {
            cause: SuspensionCause::ValidationFailed,
            reason: reason()?,
        }),
        Transition::Suspend(Suspension {
            cause: SuspensionCause::Operator,
            reason: reason()?,
        }),
        Transition::Resume(version()),
        Transition::Retire,
        Transition::SuccessorEnabled(replaced_by()),
        Transition::Reactivate,
        Transition::Delete,
        Transition::VersionChanged,
        Transition::ValidationFailed,
        Transition::OnProbation(version()),
        Transition::ProbationPassed(version()),
        Transition::ProbationFailed(
            version(),
            Suspension {
                cause: SuspensionCause::ProbationFailed {
                    run: Some(1),
                    failure: None,
                },
                reason: reason()?,
            },
        ),
    ])
}

/// The name a transition has in the matrix. A suspension by the operator and one by the system are separate rows of the
/// table, so the operator's cause is named apart from the system's causes.
fn kind_of(transition: &Transition) -> &'static str {
    match transition {
        Transition::Suspend(suspension) if suspension.cause.is_operators() => "SuspendByOperator",
        other => other.name(),
    }
}

/// Every transition the table allows, as (from, actor, transition kind, result): the whole transition
/// table, and nothing else, so an unlisted combination is refused.
fn allowed() -> Vec<(JobLifecycle, Actor, &'static str, Applied)> {
    use Actor::{DryRun, Operator, System, Validator};
    use JobLifecycle::{Draft, Enabled, Registered, Retired, Suspended, Validated};
    vec![
        (Draft, Validator, "Validated", Applied::To(Validated)),
        (Validated, DryRun, "DryRunCompleted", Applied::To(Registered)),
        (Registered, Operator, "Enable", Applied::To(Enabled)),
        (Enabled, Operator, "SetSchedule", Applied::To(Enabled)),
        (Suspended, Operator, "SetSchedule", Applied::To(Suspended)),
        (Enabled, Operator, "ClearSchedule", Applied::To(Registered)),
        (Enabled, Operator, "SuspendByOperator", Applied::To(Suspended)),
        (Enabled, System, "Suspend", Applied::To(Suspended)),
        (Suspended, Operator, "Resume", Applied::To(Enabled)),
        (Draft, Operator, "Retire", Applied::To(Retired)),
        (Validated, Operator, "Retire", Applied::To(Retired)),
        (Registered, Operator, "Retire", Applied::To(Retired)),
        (Enabled, Operator, "Retire", Applied::To(Retired)),
        (Suspended, Operator, "Retire", Applied::To(Retired)),
        (Draft, System, "SuccessorEnabled", Applied::To(Retired)),
        (Validated, System, "SuccessorEnabled", Applied::To(Retired)),
        (Registered, System, "SuccessorEnabled", Applied::To(Retired)),
        (Enabled, System, "SuccessorEnabled", Applied::To(Retired)),
        (Suspended, System, "SuccessorEnabled", Applied::To(Retired)),
        (Retired, Operator, "Reactivate", Applied::To(Draft)),
        (Retired, Operator, "Delete", Applied::Removed),
        (Draft, System, "VersionChanged", Applied::To(Draft)),
        (Validated, System, "VersionChanged", Applied::To(Draft)),
        (Registered, System, "VersionChanged", Applied::To(Draft)),
        (Validated, System, "ValidationFailed", Applied::To(Draft)),
        (Registered, System, "ValidationFailed", Applied::To(Draft)),
        (Enabled, Validator, "OnProbation", Applied::To(Enabled)),
        (Enabled, System, "ProbationPassed", Applied::To(Enabled)),
        (Suspended, System, "ProbationPassed", Applied::To(Suspended)),
        (Enabled, System, "ProbationFailed", Applied::To(Suspended)),
    ]
}

#[test]
fn only_the_transitions_in_the_table_are_allowed_and_each_goes_where_the_table_says()
-> Result<(), Box<dyn std::error::Error>> {
    let allowed = allowed();
    for from in STATES {
        for actor in ACTORS {
            for transition in every_transition()? {
                let expected = allowed
                    .iter()
                    .find(|(state, who, kind, _)| *state == from && *who == actor && *kind == kind_of(&transition))
                    .map(|(_, _, _, applied)| *applied);
                let result = apply(from, actor, &transition);
                assert_eq!(
                    result.ok(),
                    expected,
                    "{from:?} by {actor:?} doing {}",
                    kind_of(&transition)
                );
            }
        }
    }
    Ok(())
}

#[test]
fn a_transition_by_the_wrong_actor_names_who_may_make_it() {
    let refused = apply(JobLifecycle::Enabled, Actor::System, &Transition::Retire);

    assert!(
        matches!(refused, Err(TransitionError::WrongActor { .. })),
        "{refused:?}"
    );
    let message = refused.err().map(|error| error.to_string()).unwrap_or_default();
    assert!(message.contains("operator"), "{message}");
    assert!(message.contains("system"), "{message}");
}

#[test]
fn a_transition_from_the_wrong_state_names_the_state() {
    let refused = apply(JobLifecycle::Draft, Actor::Operator, &Transition::Resume(version()));

    let message = refused.err().map(|error| error.to_string()).unwrap_or_default();
    assert!(message.contains("draft"), "{message}");
}

#[test]
fn nothing_but_the_operator_can_enable_a_job() -> Result<(), CronScheduleError> {
    for actor in [Actor::Validator, Actor::DryRun, Actor::System] {
        for from in STATES {
            assert!(
                apply(from, actor, &Transition::Enable(schedule()?, version())).is_err(),
                "{actor:?} must not enable a job from {from:?}"
            );
        }
    }
    Ok(())
}

#[test]
fn only_an_enabled_job_runs_on_its_schedule() {
    for state in STATES {
        assert_eq!(state.runs_on_schedule(), state == JobLifecycle::Enabled, "{state:?}");
    }
}

#[test]
fn a_job_may_be_run_by_hand_when_registered_enabled_or_suspended() {
    for state in STATES {
        let expected = matches!(
            state,
            JobLifecycle::Registered | JobLifecycle::Enabled | JobLifecycle::Suspended
        );
        assert_eq!(state.runs_by_hand(), expected, "{state:?}");
    }
}

#[test]
fn a_state_reads_as_what_it_is_written_as() {
    for state in STATES {
        assert_eq!(JobLifecycle::try_from(state.as_str()).ok(), Some(state));
    }
    assert!(JobLifecycle::try_from("paused").is_err());
}

#[test]
fn a_schedule_that_is_not_a_cron_expression_is_refused() {
    assert!(JobSchedule::try_from("whenever").is_err());
    assert_eq!(
        JobSchedule::try_from("0 3 * * *")
            .map(|s| s.as_str().to_owned())
            .ok()
            .as_deref(),
        Some("0 3 * * *")
    );
}

#[test]
fn a_suspension_must_say_why() {
    assert!(SuspensionReason::try_from("").is_err());
    assert!(SuspensionReason::try_from("   ").is_err());
    assert_eq!(
        SuspensionReason::try_from("  the first live run failed ")
            .map(|reason| reason.as_str().to_owned())
            .ok()
            .as_deref(),
        Some("the first live run failed")
    );
}

#[test]
fn replacing_a_schedule_does_not_change_the_state() -> Result<(), CronScheduleError> {
    for state in [JobLifecycle::Enabled, JobLifecycle::Suspended] {
        assert_eq!(
            apply(state, Actor::Operator, &Transition::SetSchedule(schedule()?)).ok(),
            Some(Applied::To(state))
        );
    }
    Ok(())
}

#[test]
fn a_suspension_for_a_failed_validation_lists_every_failure_with_its_code_and_file() {
    let report = ValidationReport {
        failures: vec![
            ValidationFailure {
                code: ValidationCode::ScriptSyntax,
                artefact: "sync.rhai".to_owned(),
                message: "line 2: unexpected token".to_owned(),
            },
            ValidationFailure {
                code: ValidationCode::ServerNotConfigured,
                artefact: "workflow.toml".to_owned(),
                message: "crm is not configured".to_owned(),
            },
        ],
        unverifiable: Vec::new(),
    };

    let reason = SuspensionReason::from(&report);

    let text = reason.as_str();
    assert!(text.starts_with("validation of the new version failed"), "{text}");
    for expected in [
        "validation/script-syntax",
        "sync.rhai",
        "line 2: unexpected token",
        "validation/server-not-configured",
        "workflow.toml",
        "crm is not configured",
    ] {
        assert!(text.contains(expected), "{expected} missing from {text}");
    }
}

#[test]
fn a_suspension_for_a_failed_validation_is_never_empty_even_with_no_failure_listed() {
    let reason = SuspensionReason::from(&ValidationReport::default());

    assert!(
        !reason.as_str().trim().is_empty(),
        "`reason.as_str().trim()` should not be empty"
    );
}

#[test]
fn a_suspension_for_a_failed_validation_is_kept_within_the_recorded_error_limit() {
    let report = ValidationReport {
        failures: (0..200)
            .map(|index| ValidationFailure {
                code: ValidationCode::ArtefactInvalid,
                artefact: format!("file-{index}.toml"),
                message: "does not parse".repeat(10),
            })
            .collect(),
        unverifiable: Vec::new(),
    };

    let reason = SuspensionReason::from(&report);

    assert!(
        reason.as_str().len() <= MAX_ERROR_BYTES + 64,
        "{}",
        reason.as_str().len()
    );
}

#[test]
fn a_suspension_for_a_failed_first_run_carries_the_failure_text() {
    let failure = RunFailure {
        kind: Some(WorkflowFailureKind::TimedOut),
        message: "workflow/timed-out: the run exceeded 600 seconds".to_owned(),
    };

    let reason = SuspensionReason::from(&failure);

    assert!(reason.as_str().contains("first live run"), "{}", reason.as_str());
    assert!(reason.as_str().contains("workflow/timed-out"), "{}", reason.as_str());
}

fn every_cause() -> [SuspensionCause; 5] {
    [
        SuspensionCause::Operator,
        SuspensionCause::ValidationFailed,
        SuspensionCause::ProbationFailed {
            run: Some(4),
            failure: Some("workflow/timed-out".to_owned()),
        },
        SuspensionCause::OwnerUnresolved,
        SuspensionCause::PackageUnavailable,
    ]
}

fn suspend_for(cause: SuspensionCause) -> Result<Transition, EmptySuspensionReason> {
    Ok(Transition::Suspend(Suspension {
        cause,
        reason: SuspensionReason::try_from("a reason")?,
    }))
}

#[test]
fn every_suspension_cause_has_a_distinct_stable_code() {
    let codes = every_cause().map(|cause| cause.code());

    assert_eq!(
        codes,
        [
            "workflow/operator-suspended",
            "workflow/validation-failed",
            "workflow/probation-failed",
            "workflow/owner-unresolved",
            "workflow/package-unavailable",
        ]
    );
    let mut unique = codes.to_vec();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), codes.len());
}

#[test]
fn only_the_operator_may_suspend_with_the_operator_cause_and_only_the_system_with_the_others()
-> Result<(), Box<dyn std::error::Error>> {
    for cause in every_cause() {
        let operators = cause == SuspensionCause::Operator;
        let transition = suspend_for(cause.clone())?;

        let by_operator = apply(JobLifecycle::Enabled, Actor::Operator, &transition);
        let by_system = apply(JobLifecycle::Enabled, Actor::System, &transition);

        assert_eq!(
            by_operator.is_ok(),
            operators,
            "{cause:?} by the operator: {by_operator:?}"
        );
        assert_eq!(by_system.is_ok(), !operators, "{cause:?} by the system: {by_system:?}");
        if !operators {
            assert!(
                matches!(by_operator, Err(TransitionError::WrongActor { .. })),
                "{cause:?}: {by_operator:?}"
            );
        }
    }
    Ok(())
}
