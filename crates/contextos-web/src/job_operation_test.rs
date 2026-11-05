//! The operator operation over an in-memory store, so each rule is proved without a server or a filesystem. The adapters
//! for a vault's state and the host's state are tested against the real thing in `tests/`.

use contextos_workflows::JobState;

use super::*;
use crate::job_operation_fake::{FakeJob, Finding, NOW, reason, schedule, version};

type TestResult = Result<(), Box<dyn std::error::Error>>;

async fn ask(job: &FakeJob, request: OperatorRequest) -> Result<OperationOutcome, OperationError> {
    perform(job, request, NOW).await
}

// ---------------------------------------------------------------------
// Validate
// ---------------------------------------------------------------------

#[tokio::test]
async fn validating_a_draft_that_passes_moves_it_to_validated_and_reports_the_result() -> TestResult {
    let job = FakeJob::new("()");

    let outcome = ask(&job, OperatorRequest::Validate).await?;

    let OperationOutcome::Validated { result, lifecycle } = outcome else {
        return Err("expected a validation outcome".into());
    };
    assert!(result.passed);
    assert_eq!(lifecycle, JobLifecycle::Validated);
    assert_eq!(job.lifecycle_now(), JobLifecycle::Validated);
    Ok(())
}

#[tokio::test]
async fn validating_a_draft_that_fails_reports_every_failure_and_leaves_it_a_draft() -> TestResult {
    let job = FakeJob::new("()");
    job.finds(Finding::Fails);

    let outcome = ask(&job, OperatorRequest::Validate).await?;

    let OperationOutcome::Validated { result, lifecycle } = outcome else {
        return Err("expected a validation outcome".into());
    };
    assert!(!result.passed);
    assert_eq!(result.failures.len(), 1);
    assert_eq!(result.failures[0].code, "validation/script-syntax");
    assert_eq!(lifecycle, JobLifecycle::Draft);
    Ok(())
}

#[tokio::test]
async fn validating_a_suspended_job_records_the_result_without_changing_its_state() -> TestResult {
    let job = FakeJob::new("()");
    job.suspend_after_enabling()?;

    ask(&job, OperatorRequest::Validate).await?;

    assert_eq!(job.lifecycle_now(), JobLifecycle::Suspended);
    assert!(
        job.state()
            .job("pkg", "wf", "job")
            .and_then(JobState::validation)
            .is_some_and(|stored| stored.passed)
    );
    Ok(())
}

// ---------------------------------------------------------------------
// Schedule
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_schedule_on_a_registered_job_enables_it_for_the_version_it_has_now() -> TestResult {
    let job = FakeJob::new("()");
    job.register()?;

    let outcome = ask(&job, OperatorRequest::Schedule(schedule("0 3 * * *")?)).await?;

    assert!(matches!(
        outcome,
        OperationOutcome::Changed(Applied::To(JobLifecycle::Enabled))
    ));
    assert_eq!(job.state().scheduled_expression("pkg", "wf", "job"), Some("0 3 * * *"));
    Ok(())
}

#[tokio::test]
async fn a_schedule_on_a_registered_job_whose_files_changed_since_validation_is_refused() -> TestResult {
    let job = FakeJob::new("()");
    job.register()?;
    job.edit_files("edited()");
    let before = job.state();

    let refused = ask(&job, OperatorRequest::Schedule(schedule("0 3 * * *")?)).await;

    assert!(
        matches!(
            refused,
            Err(OperationError::Refused(TransitionError::WrongVersion { .. }))
        ),
        "{refused:?}"
    );
    assert_eq!(job.state(), before, "a refused enabling writes nothing");
    Ok(())
}

#[tokio::test]
async fn a_new_schedule_replaces_the_stored_one_on_an_enabled_or_suspended_job() -> TestResult {
    for suspend in [false, true] {
        let job = FakeJob::new("()");
        if suspend {
            job.suspend_after_enabling()?;
        } else {
            job.register()?;
            job.step(
                Actor::Operator,
                &Transition::Enable(schedule("0 3 * * *")?, job.current()),
            )?;
        }
        let before = job.lifecycle_now();

        let outcome = ask(&job, OperatorRequest::Schedule(schedule("30 4 * * *")?)).await?;

        assert!(matches!(outcome, OperationOutcome::Changed(Applied::To(state)) if state == before));
        assert_eq!(
            job.state().job("pkg", "wf", "job").and_then(JobState::schedule),
            Some("30 4 * * *")
        );
    }
    Ok(())
}

#[tokio::test]
async fn scheduling_a_job_that_is_not_registered_names_the_state_and_what_would_allow_it() -> TestResult {
    for prepare in [Prepare::Draft, Prepare::Validated, Prepare::Retired] {
        let job = FakeJob::new("()");
        prepare.apply(&job)?;
        let before = job.state();

        let refused = ask(&job, OperatorRequest::Schedule(schedule("0 3 * * *")?)).await;

        let Err(error @ OperationError::Refused(TransitionError::WrongState { .. })) = refused else {
            return Err(format!("{prepare:?}: {refused:?}").into());
        };
        let message = error.to_string();
        assert!(message.contains("registered"), "{message}");
        assert_eq!(job.state(), before);
    }
    Ok(())
}

#[tokio::test]
async fn a_state_that_cannot_take_a_request_is_refused_by_the_table_even_when_the_files_are_gone() -> TestResult {
    for prepare in [Prepare::Draft, Prepare::Validated, Prepare::Retired] {
        let job = FakeJob::new("()");
        prepare.apply(&job)?;
        job.finds(Finding::FilesUnreadable);

        let refused = ask(&job, OperatorRequest::Schedule(schedule("0 3 * * *")?)).await;

        assert!(
            matches!(
                refused,
                Err(OperationError::Refused(TransitionError::WrongState { .. }))
            ),
            "{prepare:?}: {refused:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn enabling_a_registered_job_whose_files_cannot_be_read_says_so_and_not_that_a_resume_failed() -> TestResult {
    let job = FakeJob::new("()");
    job.register()?;
    job.finds(Finding::FilesUnreadable);

    let refused = ask(&job, OperatorRequest::Schedule(schedule("0 3 * * *")?)).await;

    let Err(error @ OperationError::Unreadable(_)) = refused else {
        return Err(format!("{refused:?}").into());
    };
    assert_eq!(error.code(), "workflow/job-unreadable");
    let message = error.to_string();
    assert!(
        message.contains("could not all be read") && message.contains("enable"),
        "{message}"
    );
    assert!(!message.contains("resumed"), "{message}");
    Ok(())
}

#[derive(Debug)]
enum Prepare {
    Draft,
    Validated,
    Retired,
}

impl Prepare {
    fn apply(&self, job: &FakeJob) -> TestResult {
        match self {
            Self::Draft => {}
            Self::Validated => job.step(Actor::Validator, &Transition::Validated(job.current()))?,
            Self::Retired => {
                job.suspend_after_enabling()?;
                job.step(Actor::Operator, &Transition::Retire)?;
            }
        }
        Ok(())
    }
}

#[tokio::test]
async fn clearing_the_schedule_returns_an_enabled_job_to_registered() -> TestResult {
    let job = FakeJob::new("()");
    job.register()?;
    job.step(
        Actor::Operator,
        &Transition::Enable(schedule("0 3 * * *")?, job.current()),
    )?;

    let outcome = ask(&job, OperatorRequest::ClearSchedule).await?;

    assert!(matches!(
        outcome,
        OperationOutcome::Changed(Applied::To(JobLifecycle::Registered))
    ));
    assert_eq!(job.state().scheduled_expression("pkg", "wf", "job"), None);
    Ok(())
}

// ---------------------------------------------------------------------
// Suspend, retire, reactivate, delete
// ---------------------------------------------------------------------

#[tokio::test]
async fn suspending_an_enabled_job_stores_the_operators_reason() -> TestResult {
    let job = FakeJob::new("()");
    job.register()?;
    job.step(
        Actor::Operator,
        &Transition::Enable(schedule("0 3 * * *")?, job.current()),
    )?;

    ask(&job, OperatorRequest::Suspend(reason("the upstream API is down")?)).await?;

    assert_eq!(job.lifecycle_now(), JobLifecycle::Suspended);
    assert_eq!(
        job.state().job("pkg", "wf", "job").and_then(JobState::suspended_reason),
        Some("the upstream API is down")
    );
    Ok(())
}

#[tokio::test]
async fn retiring_reactivating_and_deleting_follow_the_table() -> TestResult {
    let job = FakeJob::new("()");
    job.suspend_after_enabling()?;

    ask(&job, OperatorRequest::Retire).await?;
    assert_eq!(job.lifecycle_now(), JobLifecycle::Retired);

    ask(&job, OperatorRequest::Reactivate).await?;
    assert_eq!(job.lifecycle_now(), JobLifecycle::Draft);

    job.register()?;
    job.step(
        Actor::Operator,
        &Transition::Enable(schedule("0 3 * * *")?, job.current()),
    )?;
    ask(&job, OperatorRequest::Retire).await?;
    let outcome = ask(&job, OperatorRequest::Delete).await?;
    assert!(matches!(outcome, OperationOutcome::Changed(Applied::Removed)));
    assert!(job.state().job("pkg", "wf", "job").is_none());
    Ok(())
}

#[tokio::test]
async fn deleting_a_job_that_is_not_retired_is_refused_naming_its_state() -> TestResult {
    let job = FakeJob::new("()");
    job.register()?;

    let refused = ask(&job, OperatorRequest::Delete).await;

    let Err(error @ OperationError::Refused(_)) = refused else {
        return Err(format!("{refused:?}").into());
    };
    let message = error.to_string();
    assert!(
        message.contains("registered") && message.contains("retired"),
        "{message}"
    );
    Ok(())
}

// ---------------------------------------------------------------------
// Resume
// ---------------------------------------------------------------------

#[tokio::test]
async fn resuming_validates_the_current_version_first_and_resumes_when_it_passes() -> TestResult {
    let job = FakeJob::new("()");
    job.suspend_after_enabling()?;

    let outcome = ask(&job, OperatorRequest::Resume).await?;

    assert!(matches!(
        outcome,
        OperationOutcome::Changed(Applied::To(JobLifecycle::Enabled))
    ));
    assert_eq!(
        job.validations(),
        1,
        "resuming validates, even when the version is unchanged"
    );
    Ok(())
}

#[tokio::test]
async fn resuming_after_the_files_were_fixed_puts_the_fixed_version_on_trial() -> TestResult {
    let job = FakeJob::new("()");
    job.suspend_after_enabling()?;
    job.edit_files("fixed()");

    ask(&job, OperatorRequest::Resume).await?;

    let state = job.state();
    let stored = state.job("pkg", "wf", "job");
    assert_eq!(
        stored.and_then(|job| job.fingerprint().cloned()),
        Some(version("fixed()"))
    );
    assert!(stored.is_some_and(JobState::first_run_pending));
    Ok(())
}

#[tokio::test]
async fn resuming_when_validation_fails_is_refused_with_every_failure_and_the_job_stays_suspended() -> TestResult {
    let job = FakeJob::new("()");
    job.suspend_after_enabling()?;
    job.finds(Finding::Fails);

    let refused = ask(&job, OperatorRequest::Resume).await;

    let Err(error @ OperationError::NotValid { .. }) = refused else {
        return Err(format!("{refused:?}").into());
    };
    assert_eq!(error.code(), "workflow/validation-failed");
    let message = error.to_string();
    assert!(
        message.contains("validation/script-syntax") && message.contains("sync.rhai"),
        "{message}"
    );
    assert_eq!(job.lifecycle_now(), JobLifecycle::Suspended);
    Ok(())
}

#[tokio::test]
async fn resuming_when_a_check_could_not_be_made_is_refused_and_says_so() -> TestResult {
    let job = FakeJob::new("()");
    job.suspend_after_enabling()?;
    job.finds(Finding::CannotCheck);

    let refused = ask(&job, OperatorRequest::Resume).await;

    let Err(error @ OperationError::NotValid { .. }) = refused else {
        return Err(format!("{refused:?}").into());
    };
    let message = error.to_string();
    assert!(
        message.contains("could not be checked") && message.contains("the server could not be reached"),
        "{message}"
    );
    assert_eq!(job.lifecycle_now(), JobLifecycle::Suspended);
    Ok(())
}

#[tokio::test]
async fn resuming_when_the_files_cannot_all_be_read_is_refused() -> TestResult {
    let job = FakeJob::new("()");
    job.suspend_after_enabling()?;
    job.finds(Finding::FilesUnreadable);

    let refused = ask(&job, OperatorRequest::Resume).await;

    let Err(error @ OperationError::NotValid { .. }) = refused else {
        return Err(format!("{refused:?}").into());
    };
    assert!(error.to_string().contains("could not all be read"), "{error}");
    assert_eq!(job.lifecycle_now(), JobLifecycle::Suspended);
    Ok(())
}

#[tokio::test]
async fn resuming_a_job_that_is_not_suspended_is_refused_without_validating_it() -> TestResult {
    let job = FakeJob::new("()");
    job.register()?;

    let refused = ask(&job, OperatorRequest::Resume).await;

    assert!(
        matches!(
            refused,
            Err(OperationError::Refused(TransitionError::WrongState { .. }))
        ),
        "{refused:?}"
    );
    assert_eq!(
        job.validations(),
        0,
        "nothing is validated for a resume that cannot apply"
    );
    Ok(())
}

// ---------------------------------------------------------------------
// Error codes
// ---------------------------------------------------------------------

#[test]
fn each_error_carries_its_stable_code() {
    assert_eq!(OperationError::Conflict.code(), "workflow/state-changed");
    assert_eq!(
        OperationError::NotFound("x".to_owned()).code(),
        "workflow/job-not-found"
    );
    assert_eq!(
        OperationError::Unavailable("gone".to_owned()).code(),
        "workflow/state-unavailable"
    );
    assert_eq!(
        OperationError::Refused(TransitionError::NotValidated { transition: "Resume" }).code(),
        "workflow/transition-refused"
    );
    assert_eq!(
        OperationError::NotValid {
            failures: vec![],
            unverifiable: vec![],
            unreadable: true
        }
        .code(),
        "workflow/validation-failed"
    );
}

// ---------------------------------------------------------------------
// What a state offers
// ---------------------------------------------------------------------

#[test]
fn each_state_offers_exactly_what_the_table_allows_the_operator() {
    use Action::{ClearSchedule, Delete, DryRun, Reactivate, Resume, Retire, Run, Schedule, Suspend, Validate};
    let cases: [(JobLifecycle, &[Action]); 6] = [
        (JobLifecycle::Draft, &[Validate, Retire, DryRun]),
        (JobLifecycle::Validated, &[Validate, Retire, DryRun]),
        (JobLifecycle::Registered, &[Validate, Schedule, Retire, Run, DryRun]),
        (
            JobLifecycle::Enabled,
            &[Validate, Schedule, ClearSchedule, Suspend, Retire, Run, DryRun],
        ),
        (
            JobLifecycle::Suspended,
            &[Validate, Schedule, Resume, Retire, Run, DryRun],
        ),
        (JobLifecycle::Retired, &[Reactivate, Delete]),
    ];
    for (lifecycle, expected) in cases {
        assert_eq!(offered(lifecycle), Offered::from(expected), "{lifecycle}");
    }
}

#[test]
fn nothing_is_offered_that_performing_it_would_refuse() -> TestResult {
    for lifecycle in [
        JobLifecycle::Draft,
        JobLifecycle::Validated,
        JobLifecycle::Registered,
        JobLifecycle::Enabled,
        JobLifecycle::Suspended,
        JobLifecycle::Retired,
    ] {
        let offered = offered(lifecycle);
        for (is_offered, transition) in [
            (
                offered.suspend(),
                Transition::Suspend(contextos_workflows::Suspension::operator(reason("held")?)),
            ),
            (offered.clear_schedule(), Transition::ClearSchedule),
            (offered.retire(), Transition::Retire),
            (offered.reactivate(), Transition::Reactivate),
            (offered.delete(), Transition::Delete),
        ] {
            assert_eq!(
                apply(lifecycle, Actor::Operator, &transition).is_ok(),
                is_offered,
                "{lifecycle}: {transition:?}"
            );
        }
    }
    Ok(())
}

#[test]
fn a_run_by_hand_is_offered_exactly_when_the_job_may_be_run_by_hand() {
    use crate::scheduler::{RunKind, run_allowed};
    for lifecycle in [
        JobLifecycle::Draft,
        JobLifecycle::Validated,
        JobLifecycle::Registered,
        JobLifecycle::Enabled,
        JobLifecycle::Suspended,
        JobLifecycle::Retired,
    ] {
        let offered = offered(lifecycle);
        assert_eq!(offered.run(), run_allowed(lifecycle, RunKind::Live), "{lifecycle}");
        assert_eq!(offered.dry_run(), run_allowed(lifecycle, RunKind::Dry), "{lifecycle}");
    }
}

#[test]
fn a_job_needs_attention_when_it_is_a_draft_or_suspended() {
    for (lifecycle, attention) in [
        (JobLifecycle::Draft, true),
        (JobLifecycle::Validated, false),
        (JobLifecycle::Registered, false),
        (JobLifecycle::Enabled, false),
        (JobLifecycle::Suspended, true),
        (JobLifecycle::Retired, false),
    ] {
        assert_eq!(needs_attention(lifecycle), attention, "{lifecycle}");
    }
}

#[tokio::test]
async fn validating_a_retired_job_is_refused_naming_its_state_and_validates_nothing() -> TestResult {
    let job = FakeJob::new("()");
    job.suspend_after_enabling()?;
    job.step(Actor::Operator, &Transition::Retire)?;
    let before = job.state();

    let refused = ask(&job, OperatorRequest::Validate).await;

    let Err(error @ OperationError::Refused(TransitionError::WrongState { .. })) = refused else {
        return Err(format!("{refused:?}").into());
    };
    let message = error.to_string();
    assert!(message.contains("retired") && message.contains("draft"), "{message}");
    assert_eq!(job.validations(), 0);
    assert_eq!(job.state(), before);
    Ok(())
}

#[tokio::test]
async fn every_state_but_retired_may_be_validated() -> TestResult {
    for prepare in [Prepare::Draft, Prepare::Validated] {
        let job = FakeJob::new("()");
        prepare.apply(&job)?;
        assert!(ask(&job, OperatorRequest::Validate).await.is_ok(), "{prepare:?}");
    }
    let registered = FakeJob::new("()");
    registered.register()?;
    assert!(ask(&registered, OperatorRequest::Validate).await.is_ok());
    let suspended = FakeJob::new("()");
    suspended.suspend_after_enabling()?;
    assert!(ask(&suspended, OperatorRequest::Validate).await.is_ok());
    Ok(())
}

// Retention after what changes the releases that are kept.

async fn ask_retaining(job: &FakeJob, request: OperatorRequest) -> Result<Performed, OperationError> {
    perform_retaining(job, request, NOW).await
}

#[tokio::test]
async fn retention_runs_after_a_validation_that_passes_and_not_after_one_that_fails() -> TestResult {
    let passing = FakeJob::new("()");
    let failing = FakeJob::new("()");
    failing.finds(Finding::Fails);

    ask_retaining(&passing, OperatorRequest::Validate).await?;
    ask_retaining(&failing, OperatorRequest::Validate).await?;

    assert_eq!(passing.retentions(), 1);
    assert_eq!(failing.retentions(), 0);
    Ok(())
}

#[tokio::test]
async fn retention_runs_after_a_job_is_enabled_retired_and_deleted() -> TestResult {
    let job = FakeJob::new("()");
    job.register()?;

    ask_retaining(&job, OperatorRequest::Schedule(schedule("0 3 * * *")?)).await?;
    assert_eq!(job.retentions(), 1);
    ask_retaining(&job, OperatorRequest::Retire).await?;
    assert_eq!(job.retentions(), 2);
    ask_retaining(&job, OperatorRequest::Delete).await?;
    assert_eq!(job.retentions(), 3);
    Ok(())
}

#[tokio::test]
async fn retention_does_not_run_after_a_resume_which_keeps_the_same_releases() -> TestResult {
    let job = FakeJob::new("()");
    job.suspend_after_enabling()?;

    ask_retaining(&job, OperatorRequest::Resume).await?;

    assert_eq!(job.lifecycle_now(), JobLifecycle::Enabled);
    assert_eq!(job.retentions(), 0);
    Ok(())
}

#[tokio::test]
async fn a_request_the_table_refuses_runs_no_retention() -> TestResult {
    let job = FakeJob::new("()");

    let refused = ask_retaining(&job, OperatorRequest::Reactivate).await;

    assert!(refused.is_err());
    assert_eq!(job.retentions(), 0);
    Ok(())
}

#[tokio::test]
async fn a_retention_that_fails_is_a_warning_on_an_operation_that_still_succeeded() -> TestResult {
    let job = FakeJob::new("()");
    job.suspend_after_enabling()?;
    job.fails_retention();

    let performed = ask_retaining(&job, OperatorRequest::Retire).await?;

    assert!(matches!(performed.outcome, OperationOutcome::Changed(_)));
    assert_eq!(job.lifecycle_now(), JobLifecycle::Retired);
    assert_eq!(performed.warnings.len(), 1);
    assert!(
        performed.warnings[0]
            .to_string()
            .starts_with("packages/retention-failed: ")
    );
    Ok(())
}
