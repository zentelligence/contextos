//! A refusal of a suspended shared job carries the suspension as a structured object beside its message, in the JSON
//! summary, and in its text.

use contextos_workflows::{Actor, JobLifecycle, SuspensionRecord};

use super::*;

fn probation_failed() -> SuspensionRecord {
    SuspensionRecord {
        code: "workflow/probation-failed".to_owned(),
        by: Some(Actor::System),
        at: Some(datetime!(2026-10-01 03:00:00 UTC)),
        reason: "the first live run failed".to_owned(),
        run: Some(4),
        failure: Some("workflow/script-error".to_owned()),
    }
}

fn refusal_of_a_suspended_job(suspension: SuspensionRecord) -> RunSummary {
    let error = CliSharedError::NotRunnable {
        state: JobLifecycle::Suspended,
        dry_run: false,
        scheduled: true,
        suspension: Some(suspension),
    };
    RunSummary::refused(invocation(), Refusal::from(&error))
}

#[test]
fn a_refusal_of_a_suspended_shared_job_carries_the_suspension_as_a_structured_object_beside_its_message()
-> Result<(), BoxError> {
    let refused = refusal_of_a_suspended_job(probation_failed());

    let summary = document(&refused)?;

    assert_eq!(summary["code"], json!("workflow/job-not-runnable"));
    assert!(summary["message"].is_string(), "{summary:?}");
    assert_eq!(
        summary["suspension"],
        json!({
            "code": "workflow/probation-failed",
            "by": "system",
            "at": "2026-10-01T03:00:00Z",
            "run": 4,
            "reason": "the first live run failed",
            "failure": "workflow/script-error",
        })
    );
    Ok(())
}

#[test]
fn a_suspension_object_leaves_out_what_the_record_never_had() -> Result<(), BoxError> {
    let refused = refusal_of_a_suspended_job(SuspensionRecord {
        by: None,
        at: None,
        run: None,
        failure: None,
        ..probation_failed()
    });

    let summary = document(&refused)?;

    assert_eq!(
        summary["suspension"],
        json!({ "code": "workflow/probation-failed", "reason": "the first live run failed" }),
        "absent fields are omitted, not null"
    );
    Ok(())
}

#[test]
fn a_refusal_that_names_no_suspension_has_no_suspension_object() -> Result<(), BoxError> {
    let error = CliSharedError::NotRunnable {
        state: JobLifecycle::Draft,
        dry_run: false,
        scheduled: false,
        suspension: None,
    };

    let summary = document(&RunSummary::refused(invocation(), Refusal::from(&error)))?;

    assert!(!summary.contains_key("suspension"), "{summary:?}");
    Ok(())
}

#[test]
fn the_text_of_a_refusal_of_a_suspended_shared_job_shows_the_suspension() {
    let refused = refusal_of_a_suspended_job(probation_failed());

    let text = refused.to_string();

    for shown in [
        "workflow/probation-failed",
        "system",
        "2026-10-01",
        "the first live run failed",
    ] {
        assert!(text.contains(shown), "the text lacks {shown:?}: {text}");
    }
    assert!(
        text.contains("R-4") || text.contains("run 4"),
        "the run that failed is named: {text}"
    );
}
