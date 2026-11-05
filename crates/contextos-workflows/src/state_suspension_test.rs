use time::macros::datetime;

use super::*;
use crate::fingerprint::{JobFingerprint, ReleaseFingerprintInput, StoreKind};
use crate::lifecycle::{
    Actor, Applied, JobLifecycle, JobSchedule, Suspension, SuspensionCause, SuspensionReason, Transition,
};
use crate::validation::{RULES_REVISION, ValidationReport};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const NOW: OffsetDateTime = datetime!(2026-10-01 03:00:00 UTC);
const LATER: OffsetDateTime = datetime!(2026-10-02 03:00:00 UTC);
const LATEST: OffsetDateTime = datetime!(2026-10-03 03:00:00 UTC);

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

fn suspension(cause: SuspensionCause, reason: &str) -> Result<Transition, Box<dyn std::error::Error>> {
    Ok(Transition::Suspend(Suspension {
        cause,
        reason: SuspensionReason::try_from(reason)?,
    }))
}

fn enabled() -> Result<PackageJobsState, Box<dyn std::error::Error>> {
    let mut state = PackageJobsState::default();
    for (actor, transition) in [
        (Actor::Validator, Transition::Validated(version("()"))),
        (Actor::DryRun, Transition::DryRunCompleted(version("()"))),
        (
            Actor::Operator,
            Transition::Enable(JobSchedule::try_from("0 3 * * *")?, version("()")),
        ),
    ] {
        state.transition("wf", "job", actor, &transition, NOW)?;
    }
    Ok(state)
}

fn job(state: &PackageJobsState) -> Result<&JobState, Box<dyn std::error::Error>> {
    Ok(state.job("wf", "job").ok_or("the job has an entry")?)
}

fn passed_validation(for_version: &JobFingerprint) -> StoredValidation {
    StoredValidation::from(ValidationRecord {
        report: &ValidationReport::default(),
        fingerprint: Some(for_version.clone()),
        at: NOW,
    })
}

#[test]
fn a_system_suspension_stores_its_code_actor_time_and_reason() -> TestResult {
    let mut state = enabled()?;

    state.transition(
        "wf",
        "job",
        Actor::System,
        &suspension(SuspensionCause::OwnerUnresolved, "the owner left the team")?,
        LATER,
    )?;

    let record = job(&state)?.suspension().ok_or("a suspension record is stored")?;
    assert_eq!(record.code, "workflow/owner-unresolved");
    assert_eq!(record.by, Some(Actor::System));
    assert_eq!(record.at, Some(LATER));
    assert_eq!(record.reason, "the owner left the team");
    assert_eq!((record.run, record.failure.as_deref()), (None, None));
    Ok(())
}

#[test]
fn a_failed_first_run_stores_probation_failed_with_its_run_number_and_failure_code() -> TestResult {
    let mut state = enabled()?;

    state.transition(
        "wf",
        "job",
        Actor::System,
        &Transition::ProbationFailed(
            version("()"),
            Suspension {
                cause: SuspensionCause::ProbationFailed {
                    run: Some(12),
                    failure: Some("workflow/timed-out".to_owned()),
                },
                reason: SuspensionReason::try_from("the first live run failed")?,
            },
        ),
        LATER,
    )?;

    let record = job(&state)?.suspension().ok_or("a suspension record is stored")?;
    assert_eq!(record.code, "workflow/probation-failed");
    assert_eq!(record.run, Some(12));
    assert_eq!(record.failure.as_deref(), Some("workflow/timed-out"));
    assert!(job(&state)?.first_run_pending(), "a failed trial stays pending");
    Ok(())
}

#[test]
fn an_operator_suspension_stores_operator_suspended_with_the_operators_reason() -> TestResult {
    let mut state = enabled()?;

    state.transition(
        "wf",
        "job",
        Actor::Operator,
        &suspension(SuspensionCause::Operator, "paused for the migration")?,
        LATER,
    )?;

    let record = job(&state)?.suspension().ok_or("a suspension record is stored")?;
    assert_eq!(record.code, "workflow/operator-suspended");
    assert_eq!(record.by, Some(Actor::Operator));
    assert_eq!(record.reason, "paused for the migration");
    Ok(())
}

#[test]
fn replacing_the_schedule_of_a_suspended_job_keeps_its_suspension_and_its_last_change() -> TestResult {
    let mut state = enabled()?;
    state.transition(
        "wf",
        "job",
        Actor::System,
        &suspension(SuspensionCause::ValidationFailed, "the new version failed")?,
        LATER,
    )?;
    let before = job(&state)?.clone();

    state.transition(
        "wf",
        "job",
        Actor::Operator,
        &Transition::SetSchedule(JobSchedule::try_from("0 4 * * *")?),
        LATEST,
    )?;

    let after = job(&state)?;
    assert_eq!(after.schedule(), Some("0 4 * * *"));
    assert_eq!(after.lifecycle(), JobLifecycle::Suspended);
    assert_eq!(
        after.lifecycle_changed(),
        Some(LATER),
        "a schedule is not a change of state"
    );
    assert_eq!(after.lifecycle_actor(), Some(Actor::System));
    assert!(before.suspension().is_some(), "the job was suspended with a record");
    assert_eq!(after.suspension(), before.suspension());
    Ok(())
}

#[test]
fn leaving_suspended_removes_the_suspension_record() -> TestResult {
    let mut state = enabled()?;
    state.transition(
        "wf",
        "job",
        Actor::System,
        &suspension(SuspensionCause::ValidationFailed, "the new version failed")?,
        LATER,
    )?;
    assert!(job(&state)?.suspension().is_some(), "suspended with a record");
    state.record_validation("wf", "job", passed_validation(&version("()")));

    let resumed = state.transition("wf", "job", Actor::Operator, &Transition::Resume(version("()")), LATEST)?;

    assert_eq!(resumed, Applied::To(JobLifecycle::Enabled));
    assert_eq!(job(&state)?.suspension(), None);
    Ok(())
}

#[test]
fn a_legacy_suspended_reason_reads_as_a_suspended_record_from_the_lifecycle_actor_and_time() -> TestResult {
    let state: PackageJobsState = toml::from_str(
        "[workflows.wf.jobs.job]\nlifecycle = \"suspended\"\nlifecycle_changed = \"2026-10-02T03:00:00Z\"\n\
         lifecycle_actor = \"system\"\nsuspended_reason = \"validation of the new version failed\"\n",
    )?;

    let record = job(&state)?.suspension().ok_or("the legacy reason becomes a record")?;

    assert_eq!(record.code, "workflow/suspended");
    assert_eq!(record.by, Some(Actor::System));
    assert_eq!(record.at, Some(LATER));
    assert_eq!(record.reason, "validation of the new version failed");
    Ok(())
}

#[test]
fn a_legacy_entry_is_written_back_in_the_new_form_and_never_with_suspended_reason() -> TestResult {
    let state: PackageJobsState = toml::from_str(
        "[workflows.wf.jobs.job]\nlifecycle = \"suspended\"\nlifecycle_changed = \"2026-10-02T03:00:00Z\"\n\
         lifecycle_actor = \"operator\"\nsuspended_reason = \"paused by hand\"\n",
    )?;

    let written = toml::to_string(&state)?;

    assert!(!written.contains("suspended_reason"), "{written}");
    assert!(written.contains("workflow/suspended"), "{written}");
    assert!(written.contains("paused by hand"), "{written}");
    Ok(())
}

#[test]
fn a_suspension_record_round_trips_through_toml_unchanged() -> TestResult {
    let mut state = enabled()?;
    state.transition(
        "wf",
        "job",
        Actor::System,
        &Transition::ProbationFailed(
            version("()"),
            Suspension {
                cause: SuspensionCause::ProbationFailed {
                    run: Some(3),
                    failure: Some("workflow/script-error".to_owned()),
                },
                reason: SuspensionReason::try_from("the first live run failed")?,
            },
        ),
        LATER,
    )?;
    assert!(job(&state)?.suspension().is_some(), "there is a record to round trip");

    let read: PackageJobsState = toml::from_str(&toml::to_string(&state)?)?;

    assert_eq!(read, state);
    Ok(())
}

#[test]
fn a_stored_validation_without_by_or_rules_reads_as_the_validators_at_revision_zero() -> TestResult {
    let state: PackageJobsState = toml::from_str(
        "[workflows.wf.jobs.job.validation]\nat = \"2026-10-01T03:00:00Z\"\npassed = true\nfailures = []\nunverifiable = []\n",
    )?;

    let stored = job(&state)?.validation().ok_or("a validation is stored")?;

    assert_eq!(stored.by, Actor::Validator);
    assert_eq!(stored.rules, 0);
    let fresh = passed_validation(&version("()"));
    assert_eq!(
        fresh.rules, RULES_REVISION,
        "a result made now carries the current revision"
    );
    Ok(())
}

#[test]
fn an_unchanged_version_is_due_validation_again_when_its_result_predates_the_rules_revision() {
    let current = version("()");
    let mut state = PackageJobsState::default();
    let mut failed = passed_validation(&current);
    failed.passed = false;
    failed.rules = 0;
    state.record_validation("wf", "job", failed);

    assert!(
        state.validation_due("wf", "job", true, Some(&current)),
        "a failure made under older rules is made again"
    );
    let mut fresh = passed_validation(&current);
    fresh.passed = false;
    fresh.rules = RULES_REVISION;
    state.record_validation("wf", "job", fresh);
    assert!(
        !state.validation_due("wf", "job", true, Some(&current)),
        "a failure made under the current rules stands until the version changes"
    );
}

#[test]
fn an_enabled_job_under_older_rules_is_reported_as_needing_review_without_a_version_change() -> TestResult {
    let mut state = enabled()?;
    let mut old = passed_validation(&version("()"));
    old.rules = 0;
    state.record_validation("wf", "job", old);

    assert!(state.rules_outdated("wf", "job"));
    assert!(
        !state.new_version_under_enabled_job("wf", "job", Some(&version("()"))),
        "the version did not change"
    );
    state.record_validation("wf", "job", passed_validation(&version("()")));
    assert!(!state.rules_outdated("wf", "job"));
    assert!(
        !state.rules_outdated("wf", "other"),
        "a job with no stored validation is not outdated"
    );
    Ok(())
}

#[test]
fn a_legacy_suspension_without_a_change_time_has_no_time_and_none_is_written_back() -> TestResult {
    let state: PackageJobsState = toml::from_str(
        "[workflows.wf.jobs.job]\nlifecycle = \"suspended\"\nlifecycle_actor = \"system\"\n\
         suspended_reason = \"validation of the new version failed\"\n",
    )?;

    let written = toml::to_string(&state)?;

    assert!(
        !written.contains("1970"),
        "the time the file never recorded is not invented as the epoch: {written}"
    );
    assert!(
        !written.contains("\nat = "),
        "no `at` is stored for a suspension whose time is unknown: {written}"
    );
    assert!(
        written.contains("by = \"system\""),
        "what the file did record is kept: {written}"
    );
    Ok(())
}

#[test]
fn a_legacy_suspension_without_an_actor_has_no_actor_and_none_is_written_back() -> TestResult {
    let state: PackageJobsState = toml::from_str(
        "[workflows.wf.jobs.job]\nlifecycle = \"suspended\"\nlifecycle_changed = \"2026-10-02T03:00:00Z\"\n\
         suspended_reason = \"paused by somebody\"\n",
    )?;

    let written = toml::to_string(&state)?;

    assert!(
        !written.contains("by = "),
        "an actor the file never recorded is not invented as the system: {written}"
    );
    assert!(
        written.contains("at = 2026-10-02T03:00:00Z") || written.contains("at = \"2026-10-02T03:00:00Z\""),
        "what the file did record is kept: {written}"
    );
    Ok(())
}

#[test]
fn a_suspension_reads_as_its_code_who_when_run_failure_and_reason_leaving_out_what_is_unknown() {
    let record = SuspensionRecord {
        code: "workflow/probation-failed".to_owned(),
        by: Some(Actor::System),
        at: Some(LATER),
        reason: "the first live run failed".to_owned(),
        run: Some(4),
        failure: Some("workflow/script-error".to_owned()),
    };
    let unknown = SuspensionRecord {
        by: None,
        at: None,
        run: None,
        failure: None,
        ..record.clone()
    };

    assert_eq!(
        record.to_string(),
        "workflow/probation-failed by system at 2026-10-02T03:00:00Z, run R-4, failure workflow/script-error: \
         the first live run failed"
    );
    assert_eq!(
        unknown.to_string(),
        "workflow/probation-failed: the first live run failed",
        "nothing is shown for what the record never held"
    );
}

#[test]
fn a_refused_transition_of_a_suspended_job_says_why_it_is_suspended() -> TestResult {
    let mut state = enabled()?;
    state.transition(
        "wf",
        "job",
        Actor::System,
        &suspension(SuspensionCause::OwnerUnresolved, "the job's owner is not on the team")?,
        LATER,
    )?;

    let refused = state.transition("wf", "job", Actor::Operator, &Transition::Reactivate, LATER);

    let text = refused
        .err()
        .ok_or("reactivating a suspended job is refused")?
        .to_string();
    assert!(text.contains("suspended"), "{text}");
    assert!(text.contains("workflow/owner-unresolved by system"), "{text}");
    assert!(text.contains("the job's owner is not on the team"), "{text}");
    Ok(())
}

// ---------------------------------------------------------------------
// Text read from a hand-edited state file is bounded when it is read
// ---------------------------------------------------------------------

/// Text far longer than a recorded error may be, in a form that shows where it was cut.
fn very_long() -> String {
    "x".repeat(64 * 1024)
}

fn assert_cut_within_the_recorded_error_limit(text: &str, what: &str) {
    assert!(
        text.len() <= crate::run_record::MAX_ERROR_BYTES,
        "{what} is {} bytes, past the {} a recorded error may have",
        text.len(),
        crate::run_record::MAX_ERROR_BYTES
    );
    assert!(
        text.ends_with(crate::logging::CUT_MARKER),
        "{what} says it was cut: {text:?}"
    );
}

#[test]
fn a_suspension_reason_read_from_the_state_is_cut_to_the_length_a_recorded_error_may_have() -> TestResult {
    let state: PackageJobsState = toml::from_str(&format!(
        "[workflows.wf.jobs.job]\nlifecycle = \"suspended\"\n\n[workflows.wf.jobs.job.suspension]\n\
         code = \"workflow/probation-failed\"\nby = \"system\"\nreason = \"{}\"\n",
        very_long()
    ))?;

    let record = job(&state)?.suspension().ok_or("a suspension is stored")?;

    assert_cut_within_the_recorded_error_limit(&record.reason, "the reason");
    Ok(())
}

#[test]
fn a_suspension_failure_and_code_read_from_the_state_are_cut_the_same_way() -> TestResult {
    let state: PackageJobsState = toml::from_str(&format!(
        "[workflows.wf.jobs.job]\nlifecycle = \"suspended\"\n\n[workflows.wf.jobs.job.suspension]\n\
         code = \"{long}\"\nby = \"system\"\nreason = \"short\"\nrun = 4\nfailure = \"{long}\"\n",
        long = very_long()
    ))?;

    let record = job(&state)?.suspension().ok_or("a suspension is stored")?;

    assert_cut_within_the_recorded_error_limit(&record.code, "the code");
    assert_cut_within_the_recorded_error_limit(record.failure.as_deref().ok_or("a failure is stored")?, "the failure");
    assert_eq!(record.reason, "short", "text that fits is read as it is");
    assert_eq!(record.run, Some(4));
    Ok(())
}

#[test]
fn a_legacy_suspended_reason_read_from_the_state_is_cut_to_the_length_a_recorded_error_may_have() -> TestResult {
    let state: PackageJobsState = toml::from_str(&format!(
        "[workflows.wf.jobs.job]\nlifecycle = \"suspended\"\nsuspended_reason = \"{}\"\n",
        very_long()
    ))?;

    let record = job(&state)?.suspension().ok_or("the legacy reason becomes a record")?;

    assert_cut_within_the_recorded_error_limit(&record.reason, "the legacy reason");
    Ok(())
}

#[test]
fn a_cut_suspension_text_never_splits_a_character() -> TestResult {
    let state: PackageJobsState = toml::from_str(&format!(
        "[workflows.wf.jobs.job]\nlifecycle = \"suspended\"\nsuspended_reason = \"{}\"\n",
        "é".repeat(4096)
    ))?;

    let record = job(&state)?.suspension().ok_or("the legacy reason becomes a record")?;

    assert_cut_within_the_recorded_error_limit(&record.reason, "the reason");
    Ok(())
}
