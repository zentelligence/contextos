use super::*;

fn finished_dry_run() -> RunReport {
    RunReport::from(RunOutcome::Succeeded)
}

#[test]
fn a_dry_run_record_that_could_not_be_finished_keeps_its_location_and_says_so() {
    let mut report = finished_dry_run();

    note_dry_record(
        "va",
        DryRecord::NotCompleted("memory/jobs/ops/sync/nightly/2026/10/R-1.md".to_owned()),
        &mut report,
    );

    assert_eq!(
        report.record,
        Some(RecordLocation::Vault {
            vault: "va".to_owned(),
            path: "memory/jobs/ops/sync/nightly/2026/10/R-1.md".to_owned(),
        }),
        "the record exists, so the summary still says where it is"
    );
    assert_eq!(report.host_warnings.len(), 1, "{:?}", report.host_warnings);
    assert_eq!(report.host_warnings[0].code, "workflow/record-unavailable");
    assert!(
        report.host_warnings[0].message.contains("could not be finished"),
        "{:?}",
        report.host_warnings
    );
}

#[test]
fn a_dry_run_record_that_could_not_be_created_has_no_location_and_says_it_could_not_be_written() {
    let mut report = finished_dry_run();

    note_dry_record("va", DryRecord::NotCreated, &mut report);

    assert_eq!(report.record, None);
    assert_eq!(report.host_warnings.len(), 1, "{:?}", report.host_warnings);
    assert!(
        report.host_warnings[0].message.contains("could not be written"),
        "{:?}",
        report.host_warnings
    );
}

#[test]
fn a_dry_run_record_that_was_completed_is_located_without_a_warning() {
    let mut report = finished_dry_run();

    note_dry_record(
        "va",
        DryRecord::Completed("memory/jobs/ops/sync/nightly/2026/10/R-1.md".to_owned()),
        &mut report,
    );

    assert_eq!(
        report.record,
        Some(RecordLocation::Vault {
            vault: "va".to_owned(),
            path: "memory/jobs/ops/sync/nightly/2026/10/R-1.md".to_owned(),
        })
    );
    assert!(report.host_warnings.is_empty(), "{:?}", report.host_warnings);
}

fn an_operator_suspension() -> contextos_workflows::SuspensionRecord {
    contextos_workflows::SuspensionRecord {
        code: "workflow/operator-suspended".to_owned(),
        by: Some(contextos_workflows::Actor::Operator),
        at: None,
        reason: "paused for the migration".to_owned(),
        run: None,
        failure: None,
    }
}

#[test]
fn a_claim_refused_for_a_suspended_vault_job_carries_the_suspension_into_the_refusal() {
    let claim = ClaimRunError::NotRunnable {
        state: JobLifecycle::Suspended,
        suspension: Some(an_operator_suspension()),
    };

    let error = claim_failure(&claim, RunKind::Live, ("work", "ops", "sync", "nightly"));

    let message = error.to_string();
    assert!(message.contains("workflow/operator-suspended"), "{message}");
    assert!(message.contains("operator"), "{message}");
    assert!(message.contains("paused for the migration"), "{message}");
    let refusal = crate::run_summary::Refusal::from(&error);
    assert_eq!(refusal.suspension.as_deref(), Some(&an_operator_suspension()));
}

#[test]
fn a_claim_refused_for_a_job_that_is_not_suspended_names_no_suspension() {
    let claim = ClaimRunError::NotRunnable {
        state: JobLifecycle::Draft,
        suspension: None,
    };

    let error = claim_failure(&claim, RunKind::Live, ("work", "ops", "sync", "nightly"));

    assert!(!error.to_string().contains("suspension"), "{error}");
    assert_eq!(crate::run_summary::Refusal::from(&error).suspension, None);
}

#[test]
fn a_run_the_scheduler_starts_requires_review_and_a_run_by_hand_does_not() {
    use crate::run_snapshot::ReviewGate;

    assert_eq!(ReviewGate::from(RunRequest::scheduled().trigger), ReviewGate::Required);
    assert_eq!(
        ReviewGate::from(RunRequest::by_hand(None).trigger),
        ReviewGate::NotRequired
    );
    let options = CliRunOptions {
        dry_run_scenario: None,
        record: true,
    };
    assert_eq!(
        ReviewGate::from(RunRequest::from(&options).trigger),
        ReviewGate::NotRequired
    );
}
