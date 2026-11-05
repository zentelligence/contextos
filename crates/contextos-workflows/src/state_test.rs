use time::macros::datetime;

use super::*;

#[test]
fn a_tick_starts_a_run_when_nothing_has_fired_yet() {
    assert_eq!(
        evaluate_tick(None, datetime!(2026-09-08 03:00:00 UTC)),
        TickDecision::Start
    );
}

#[test]
fn a_tick_already_recorded_as_fired_is_not_started_again_across_a_restart() {
    let scheduled_for = datetime!(2026-09-08 03:00:00 UTC);
    assert_eq!(
        evaluate_tick(Some(scheduled_for), scheduled_for),
        TickDecision::AlreadyFired
    );
}

#[test]
fn a_tick_older_than_the_last_one_fired_is_never_started() {
    assert_eq!(
        evaluate_tick(
            Some(datetime!(2026-09-09 03:00:00 UTC)),
            datetime!(2026-09-08 03:00:00 UTC)
        ),
        TickDecision::AlreadyFired
    );
}

#[test]
fn a_later_tick_starts_once_the_previous_one_has_fired() {
    assert_eq!(
        evaluate_tick(
            Some(datetime!(2026-09-08 03:00:00 UTC)),
            datetime!(2026-09-09 03:00:00 UTC)
        ),
        TickDecision::Start
    );
}

#[test]
fn jobs_state_round_trips_through_toml() -> Result<(), String> {
    let mut state = PackageJobsState::default();
    state.job_entry("sync-crm", "nightly-sync").last_scheduled_fire = Some(datetime!(2026-09-08 03:00:00 UTC));

    let serialised = toml::to_string(&state).map_err(|error| error.to_string())?;
    let round_tripped: PackageJobsState = toml::from_str(&serialised).map_err(|error| error.to_string())?;

    assert_eq!(round_tripped, state);
    Ok(())
}

#[test]
fn a_job_state_with_no_favourite_field_at_all_defaults_to_not_favourited() -> Result<(), String> {
    let job_state: JobState = toml::from_str("").map_err(|error| error.to_string())?;
    assert!(!job_state.favourite);
    Ok(())
}

#[test]
fn a_favourited_job_state_round_trips_through_toml() -> Result<(), String> {
    let job_state = JobState {
        favourite: true,
        ..JobState::default()
    };
    let serialised = toml::to_string(&job_state).map_err(|error| error.to_string())?;
    let round_tripped: JobState = toml::from_str(&serialised).map_err(|error| error.to_string())?;
    assert_eq!(round_tripped, job_state);
    assert!(round_tripped.favourite);
    Ok(())
}

#[test]
fn an_empty_workflows_table_serialises_to_nothing_rather_than_an_empty_section() -> Result<(), String> {
    let serialised = toml::to_string(&PackageJobsState::default()).map_err(|error| error.to_string())?;
    assert_eq!(serialised.trim(), "");
    Ok(())
}

#[test]
fn a_state_file_that_still_has_the_old_run_keys_loads_and_drops_them() -> Result<(), String> {
    let text = "next_run_id = 4\n[current_run]\nworkflow = \"a\"\njob = \"b\"\nrun_id = 3\nstarted = \"2026-01-01T00:00:00Z\"\n[workflows.a.jobs.b]\nfavourite = true\n";

    let state: PackageJobsState = toml::from_str(text).map_err(|error| error.to_string())?;
    let written = toml::to_string(&state).map_err(|error| error.to_string())?;

    assert!(state.job("a", "b").is_some_and(|job| job.favourite));
    assert!(
        !written.contains("current_run") && !written.contains("next_run_id"),
        "{written}"
    );
    Ok(())
}

#[test]
fn a_run_leaves_nothing_of_its_own_in_the_state_file() -> Result<(), String> {
    let mut state = PackageJobsState::default();
    state.job_entry("sync-crm", "nightly-sync").favourite = true;

    let serialised = toml::to_string(&state).map_err(|error| error.to_string())?;

    assert!(!serialised.contains("current_run"), "{serialised}");
    assert!(!serialised.contains("next_run_id"), "{serialised}");
    Ok(())
}

#[test]
fn two_jobs_with_the_identical_slug_under_different_workflows_are_tracked_independently() {
    let mut state = PackageJobsState::default();
    state.job_entry("workflow-a", "nightly").favourite = true;
    state.job_entry("workflow-b", "nightly").favourite = false;
    assert!(state.job("workflow-a", "nightly").is_some_and(|job| job.favourite));
    assert!(!state.job("workflow-b", "nightly").is_some_and(|job| job.favourite));
}

mod lifecycle {
    use time::macros::datetime;

    use super::*;
    use crate::fingerprint::{JobFingerprint, ReleaseFingerprintInput, StoreKind};
    use crate::lifecycle::{
        Actor, Applied, JobLifecycle, JobSchedule, Suspension, SuspensionCause, SuspensionReason, Transition,
    };

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

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const NOW: OffsetDateTime = datetime!(2026-10-01 03:00:00 UTC);

    fn schedule(expression: &str) -> Result<JobSchedule, crate::schedule::CronScheduleError> {
        JobSchedule::try_from(expression)
    }

    fn reason(text: &str) -> Result<SuspensionReason, crate::lifecycle::EmptySuspensionReason> {
        SuspensionReason::try_from(text)
    }

    fn make(state: &mut PackageJobsState, actor: Actor, transition: &Transition) -> Result<Applied, TransitionError> {
        state.transition("wf", "job", actor, transition, NOW)
    }

    /// A state in which `wf`/`job` has been taken to `registered` through the real transitions.
    fn registered() -> Result<PackageJobsState, Box<dyn std::error::Error>> {
        let mut state = PackageJobsState::default();
        make(&mut state, Actor::Validator, &Transition::Validated(version("()")))?;
        make(&mut state, Actor::DryRun, &Transition::DryRunCompleted(version("()")))?;
        Ok(state)
    }

    /// A passed validation of the script `script`, as the validator stores it.
    fn passed_for(script: &str) -> StoredValidation {
        StoredValidation::from(ValidationRecord {
            report: &crate::validation::ValidationReport::default(),
            fingerprint: Some(version(script)),
            at: NOW,
        })
    }

    fn enabled() -> Result<PackageJobsState, Box<dyn std::error::Error>> {
        let mut state = registered()?;
        make(
            &mut state,
            Actor::Operator,
            &Transition::Enable(schedule("0 3 * * *")?, version("()")),
        )?;
        Ok(state)
    }

    #[test]
    fn a_job_nothing_is_recorded_for_is_a_draft_with_no_schedule() {
        let state = PackageJobsState::default();

        assert_eq!(state.lifecycle_of("wf", "job"), JobLifecycle::Draft);
        assert_eq!(state.scheduled_expression("wf", "job"), None);
    }

    #[test]
    fn enabling_a_registered_job_stores_its_schedule_and_only_then_is_it_scheduled() -> TestResult {
        let mut state = registered()?;
        assert_eq!(
            state.scheduled_expression("wf", "job"),
            None,
            "registered runs only by hand"
        );

        let applied = make(
            &mut state,
            Actor::Operator,
            &Transition::Enable(schedule("0 3 * * *")?, version("()")),
        )?;

        assert_eq!(applied, Applied::To(JobLifecycle::Enabled));
        assert_eq!(state.scheduled_expression("wf", "job"), Some("0 3 * * *"));
        Ok(())
    }

    #[test]
    fn a_suspended_job_keeps_its_schedule_but_is_not_scheduled() -> TestResult {
        let mut state = enabled()?;

        make(
            &mut state,
            Actor::System,
            &Transition::Suspend(Suspension {
                cause: SuspensionCause::ValidationFailed,
                reason: reason("first run failed")?,
            }),
        )?;

        let job = state.job("wf", "job");
        assert_eq!(job.and_then(JobState::schedule), Some("0 3 * * *"));
        assert_eq!(job.and_then(JobState::suspended_reason), Some("first run failed"));
        assert_eq!(state.scheduled_expression("wf", "job"), None);
        Ok(())
    }

    #[test]
    fn resuming_clears_the_reason_and_schedules_the_job_again() -> TestResult {
        let mut state = enabled()?;
        make(
            &mut state,
            Actor::Operator,
            &Transition::Suspend(Suspension::operator(reason("maintenance")?)),
        )?;

        state.record_validation("wf", "job", passed_for("()"));
        make(&mut state, Actor::Operator, &Transition::Resume(version("()")))?;

        assert_eq!(state.job("wf", "job").and_then(JobState::suspended_reason), None);
        assert_eq!(state.scheduled_expression("wf", "job"), Some("0 3 * * *"));
        Ok(())
    }

    #[test]
    fn a_new_schedule_replaces_the_old_one_without_changing_the_state() -> TestResult {
        for suspend in [false, true] {
            let mut state = enabled()?;
            if suspend {
                make(
                    &mut state,
                    Actor::Operator,
                    &Transition::Suspend(Suspension::operator(reason("held")?)),
                )?;
            }
            let before = state.lifecycle_of("wf", "job");

            make(
                &mut state,
                Actor::Operator,
                &Transition::SetSchedule(schedule("30 4 * * *")?),
            )?;

            assert_eq!(state.lifecycle_of("wf", "job"), before);
            assert_eq!(state.job("wf", "job").and_then(JobState::schedule), Some("30 4 * * *"));
        }
        Ok(())
    }

    #[test]
    fn a_suspended_job_cannot_be_enabled_afresh_it_is_resumed() -> TestResult {
        let mut state = enabled()?;
        make(
            &mut state,
            Actor::Operator,
            &Transition::Suspend(Suspension::operator(reason("held")?)),
        )?;

        let refused = make(
            &mut state,
            Actor::Operator,
            &Transition::Enable(schedule("0 5 * * *")?, version("()")),
        );

        assert!(refused.is_err());
        Ok(())
    }

    #[test]
    fn every_transition_records_when_and_by_whom() -> TestResult {
        let mut state = PackageJobsState::default();

        make(&mut state, Actor::Validator, &Transition::Validated(version("()")))?;

        let job = state.job("wf", "job");
        assert_eq!(job.and_then(JobState::lifecycle_changed), Some(NOW));
        assert_eq!(job.and_then(JobState::lifecycle_actor), Some(Actor::Validator));
        Ok(())
    }

    #[test]
    fn a_refused_transition_changes_nothing() -> TestResult {
        let mut state = registered()?;
        let before = state.clone();

        let refused = make(
            &mut state,
            Actor::System,
            &Transition::Enable(schedule("0 3 * * *")?, version("()")),
        );

        assert!(refused.is_err());
        assert_eq!(state, before);
        Ok(())
    }

    #[test]
    fn a_refused_transition_of_a_job_nothing_is_stored_for_leaves_no_entry_behind() -> TestResult {
        let mut state = PackageJobsState::default();

        let refused = make(
            &mut state,
            Actor::Operator,
            &Transition::Enable(schedule("0 3 * * *")?, version("()")),
        );

        assert!(refused.is_err(), "a draft cannot be enabled");
        assert_eq!(state, PackageJobsState::default());
        Ok(())
    }

    #[test]
    fn retiring_a_job_stops_its_schedule_and_re_activating_it_makes_it_a_draft() -> TestResult {
        let mut state = enabled()?;

        make(&mut state, Actor::Operator, &Transition::Retire)?;
        assert_eq!(state.job("wf", "job").and_then(JobState::schedule), None);
        assert_eq!(state.scheduled_expression("wf", "job"), None);

        make(&mut state, Actor::Operator, &Transition::Reactivate)?;
        assert_eq!(state.lifecycle_of("wf", "job"), JobLifecycle::Draft);
        Ok(())
    }

    #[test]
    fn deleting_a_retired_job_removes_its_entry() -> TestResult {
        let mut state = enabled()?;
        make(&mut state, Actor::Operator, &Transition::Retire)?;

        let applied = make(&mut state, Actor::Operator, &Transition::Delete)?;

        assert_eq!(applied, Applied::Removed);
        assert!(state.job("wf", "job").is_none());
        Ok(())
    }

    #[test]
    fn enabling_or_resuming_counts_the_schedule_from_now_so_no_old_tick_is_run_late() -> TestResult {
        let long_ago = datetime!(2026-01-01 03:00:00 UTC);
        let later = datetime!(2026-10-01 12:00:00 UTC);

        let mut state = registered()?;
        state.job_entry("wf", "job").last_scheduled_fire = Some(long_ago);
        state.transition(
            "wf",
            "job",
            Actor::Operator,
            &Transition::Enable(schedule("0 3 * * *")?, version("()")),
            later,
        )?;
        assert_eq!(
            state.job("wf", "job").and_then(|job| job.last_scheduled_fire),
            Some(later)
        );

        state.transition(
            "wf",
            "job",
            Actor::Operator,
            &Transition::Suspend(Suspension::operator(reason("held")?)),
            later,
        )?;
        state.job_entry("wf", "job").last_scheduled_fire = Some(long_ago);
        state.record_validation("wf", "job", passed_for("()"));
        state.transition("wf", "job", Actor::Operator, &Transition::Resume(version("()")), later)?;
        assert_eq!(
            state.job("wf", "job").and_then(|job| job.last_scheduled_fire),
            Some(later)
        );

        state.job_entry("wf", "job").last_scheduled_fire = Some(long_ago);
        state.transition(
            "wf",
            "job",
            Actor::Operator,
            &Transition::SetSchedule(schedule("30 4 * * *")?),
            later,
        )?;
        assert_eq!(
            state.job("wf", "job").and_then(|job| job.last_scheduled_fire),
            Some(later)
        );
        Ok(())
    }

    #[test]
    fn the_lifecycle_survives_a_round_trip_through_the_state_file() -> TestResult {
        let mut state = enabled()?;
        make(
            &mut state,
            Actor::System,
            &Transition::Suspend(Suspension {
                cause: SuspensionCause::ValidationFailed,
                reason: reason("first run failed")?,
            }),
        )?;

        let text = toml::to_string(&state)?;
        let read: PackageJobsState = toml::from_str(&text)?;

        assert_eq!(read, state);
        assert!(text.contains("lifecycle = \"suspended\""), "{text}");
        Ok(())
    }
}

mod version_facts {
    use time::macros::datetime;

    use super::*;
    use crate::fingerprint::{JobFingerprint, ReleaseFingerprintInput, StoreKind};
    use crate::lifecycle::{Actor, Applied, JobLifecycle, Suspension, Transition, TransitionError};
    use crate::validation::{Unverifiable, ValidationCode, ValidationFailure, ValidationReport};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const NOW: OffsetDateTime = datetime!(2026-10-01 03:00:00 UTC);

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
        state.transition("wf", "job", actor, transition, NOW)
    }

    fn validated(script: &str) -> Result<PackageJobsState, Box<dyn std::error::Error>> {
        let mut state = PackageJobsState::default();
        make(&mut state, Actor::Validator, &Transition::Validated(version(script)))?;
        Ok(state)
    }

    #[test]
    fn validating_a_version_records_which_version_it_was() -> TestResult {
        let state = validated("()")?;

        assert_eq!(
            state.job("wf", "job").and_then(JobState::fingerprint),
            Some(&version("()"))
        );
        Ok(())
    }

    #[test]
    fn a_dry_run_registers_the_version_that_was_validated_and_notes_when() -> TestResult {
        let mut state = validated("()")?;

        let applied = make(&mut state, Actor::DryRun, &Transition::DryRunCompleted(version("()")))?;

        assert_eq!(applied, Applied::To(JobLifecycle::Registered));
        assert_eq!(state.job("wf", "job").and_then(JobState::dry_run_at), Some(NOW));
        Ok(())
    }

    #[test]
    fn a_dry_run_of_a_different_version_than_was_validated_is_refused_and_changes_nothing() -> TestResult {
        let mut state = validated("()")?;
        let before = state.clone();

        let refused = make(&mut state, Actor::DryRun, &Transition::DryRunCompleted(version("();")));

        assert!(
            matches!(refused, Err(TransitionError::WrongVersion { .. })),
            "{refused:?}"
        );
        assert_eq!(state, before);
        Ok(())
    }

    #[test]
    fn a_changed_version_is_drift_for_a_validated_or_registered_job_and_nothing_else() -> TestResult {
        let mut state = validated("()")?;
        assert!(
            !state.version_drift("wf", "job", Some(&version("()"))),
            "the same version is not drift"
        );
        assert!(state.version_drift("wf", "job", Some(&version("();"))), "validated");
        assert!(
            state.version_drift("wf", "job", None),
            "files that can no longer be read are a changed version"
        );

        make(&mut state, Actor::DryRun, &Transition::DryRunCompleted(version("()")))?;
        assert!(state.version_drift("wf", "job", Some(&version("();"))), "registered");

        assert!(
            !PackageJobsState::default().version_drift("wf", "job", Some(&version("();"))),
            "a job nothing is stored for has nothing to drift from"
        );

        // An enabled or suspended job's new version is for probation to handle, and is not drift here.
        make(
            &mut state,
            Actor::Operator,
            &Transition::Enable(crate::lifecycle::JobSchedule::try_from("0 3 * * *")?, version("()")),
        )?;
        assert!(!state.version_drift("wf", "job", Some(&version("();"))), "enabled");
        make(
            &mut state,
            Actor::Operator,
            &Transition::Suspend(Suspension::operator(crate::lifecycle::SuspensionReason::try_from(
                "held",
            )?)),
        )?;
        assert!(!state.version_drift("wf", "job", Some(&version("();"))), "suspended");
        Ok(())
    }

    #[test]
    fn a_version_change_voids_the_recorded_version_and_dry_run() -> TestResult {
        let mut state = validated("()")?;
        make(&mut state, Actor::DryRun, &Transition::DryRunCompleted(version("()")))?;

        make(&mut state, Actor::System, &Transition::VersionChanged)?;

        let job = state.job("wf", "job");
        assert_eq!(job.map(JobState::lifecycle), Some(JobLifecycle::Draft));
        assert_eq!(job.and_then(JobState::fingerprint), None);
        assert_eq!(job.and_then(JobState::dry_run_at), None);
        Ok(())
    }

    #[test]
    fn re_activating_a_job_voids_what_was_recorded_for_it() -> TestResult {
        let mut state = validated("()")?;
        make(&mut state, Actor::DryRun, &Transition::DryRunCompleted(version("()")))?;
        make(
            &mut state,
            Actor::Operator,
            &Transition::Enable(crate::lifecycle::JobSchedule::try_from("0 3 * * *")?, version("()")),
        )?;
        make(&mut state, Actor::Operator, &Transition::Retire)?;

        make(&mut state, Actor::Operator, &Transition::Reactivate)?;

        assert_eq!(state.job("wf", "job").and_then(JobState::fingerprint), None);
        assert_eq!(state.job("wf", "job").and_then(JobState::dry_run_at), None);
        Ok(())
    }

    #[test]
    fn a_validation_result_is_kept_whether_it_passed_or_not_without_changing_the_state() {
        let mut state = PackageJobsState::default();
        let report = ValidationReport {
            failures: vec![ValidationFailure {
                code: ValidationCode::ScriptSyntax,
                artefact: "sync.rhai".to_owned(),
                message: "unexpected token at line 2".to_owned(),
            }],
            unverifiable: vec![Unverifiable {
                check: "servers".to_owned(),
                reason: "contextos is not connected".to_owned(),
            }],
        };

        state.record_validation(
            "wf",
            "job",
            StoredValidation::from(ValidationRecord {
                report: &report,
                fingerprint: Some(version("()")),
                at: NOW,
            }),
        );

        let job = state.job("wf", "job");
        assert_eq!(job.map(JobState::lifecycle), Some(JobLifecycle::Draft));
        let stored = job.and_then(JobState::validation);
        assert_eq!(stored.map(|v| v.passed), Some(false));
        assert_eq!(stored.map(|v| v.failures.len()), Some(1));
        assert_eq!(
            stored.map(|v| v.failures[0].code.as_str()),
            Some("validation/script-syntax")
        );
        assert_eq!(stored.map(|v| v.unverifiable.len()), Some(1));
        assert_eq!(
            job.and_then(JobState::fingerprint),
            None,
            "only a pass records the version"
        );
    }

    #[test]
    fn the_recorded_facts_survive_a_round_trip_through_the_state_file() -> TestResult {
        let mut state = validated("()")?;
        make(&mut state, Actor::DryRun, &Transition::DryRunCompleted(version("()")))?;
        state.record_validation(
            "wf",
            "job",
            StoredValidation::from(ValidationRecord {
                report: &ValidationReport::default(),
                fingerprint: Some(version("()")),
                at: NOW,
            }),
        );

        let text = toml::to_string(&state)?;
        let read: PackageJobsState = toml::from_str(&text)?;

        assert_eq!(read, state);
        assert!(text.contains("sha256:"), "{text}");
        Ok(())
    }

    #[test]
    fn a_state_file_naming_something_that_is_not_a_fingerprint_is_refused() {
        let text = "[workflows.wf.jobs.job]\nlifecycle = \"validated\"\nfingerprint = \"sha256:short\"\n";

        assert!(toml::from_str::<PackageJobsState>(text).is_err());
    }

    #[test]
    fn a_ready_draft_is_due_validation_unless_its_current_version_already_failed() {
        let mut state = PackageJobsState::default();
        let current = version("()");
        assert!(
            state.validation_due("wf", "job", true, Some(&current)),
            "a job nothing is stored for is a draft"
        );
        assert!(
            !state.validation_due("wf", "job", false, Some(&current)),
            "it must say it is ready"
        );

        let failed = ValidationReport {
            failures: vec![ValidationFailure {
                code: ValidationCode::ScriptSyntax,
                artefact: "sync.rhai".to_owned(),
                message: "bad".to_owned(),
            }],
            unverifiable: Vec::new(),
        };
        state.record_validation(
            "wf",
            "job",
            StoredValidation::from(ValidationRecord {
                report: &failed,
                fingerprint: Some(current.clone()),
                at: NOW,
            }),
        );
        assert!(
            !state.validation_due("wf", "job", true, Some(&current)),
            "a failed result stands until the version changes"
        );
        assert!(
            state.validation_due("wf", "job", true, Some(&version("();"))),
            "a new version is validated"
        );
    }

    #[test]
    fn a_failure_recorded_for_unreadable_files_stands_until_they_can_be_read() {
        let mut state = PackageJobsState::default();
        state.record_validation(
            "wf",
            "job",
            StoredValidation::from(ValidationRecord {
                report: &ValidationReport {
                    failures: vec![ValidationFailure {
                        code: ValidationCode::ArtefactMissing,
                        artefact: "sync.rhai".to_owned(),
                        message: "gone".to_owned(),
                    }],
                    unverifiable: Vec::new(),
                },
                fingerprint: None,
                at: NOW,
            }),
        );

        assert!(!state.validation_due("wf", "job", true, None));
        assert!(state.validation_due("wf", "job", true, Some(&version("()"))));
    }

    #[test]
    fn only_a_draft_is_due_validation() -> TestResult {
        let state = validated("()")?;

        assert!(!state.validation_due("wf", "job", true, Some(&version("();"))));
        Ok(())
    }
}
