use contextos_workflows::{StoredFailure, StoredValidation};
use time::macros::datetime;

use super::*;

fn validation(passed: bool) -> StoredValidation {
    StoredValidation {
        by: contextos_workflows::Actor::Validator,
        rules: 0,
        at: datetime!(2026-10-01 03:00:00 UTC),
        fingerprint: None,
        passed,
        failures: if passed {
            Vec::new()
        } else {
            vec![
                StoredFailure {
                    code: "validation/script-syntax".to_owned(),
                    artefact: "sync.rhai".to_owned(),
                    message: "unexpected token on line 3".to_owned(),
                },
                StoredFailure {
                    code: "validation/owner-unresolved".to_owned(),
                    artefact: "nightly.toml".to_owned(),
                    message: "no team member named nobody".to_owned(),
                },
            ]
        },
        unverifiable: if passed {
            Vec::new()
        } else {
            vec!["tools: the server could not be reached".to_owned()]
        },
        made_from: None,
    }
}

#[test]
fn a_validation_that_passed_says_what_state_the_job_is_in_and_what_comes_next() -> Result<(), CliJobsError> {
    let text = describe(
        OperationOutcome::Validated {
            result: Box::new(validation(true)),
            lifecycle: JobLifecycle::Validated,
        },
        "job ops/sync/nightly of vault harbour",
    )?;

    assert!(
        text.contains("Validation passed for job ops/sync/nightly of vault harbour"),
        "{text}"
    );
    assert!(text.contains("validated"), "{text}");
    assert!(text.contains("jobs dry-run"), "the next step is named:\n{text}");
    Ok(())
}

#[test]
fn a_validation_that_passed_for_a_job_past_validated_names_only_its_state() -> Result<(), CliJobsError> {
    let text = describe(
        OperationOutcome::Validated {
            result: Box::new(validation(true)),
            lifecycle: JobLifecycle::Suspended,
        },
        "job x",
    )?;

    assert!(text.contains("suspended"), "{text}");
    assert!(!text.contains("jobs dry-run"), "{text}");
    Ok(())
}

#[test]
fn a_validation_that_failed_is_an_error_listing_every_failure_with_its_code_and_file() -> Result<(), String> {
    let outcome = describe(
        OperationOutcome::Validated {
            result: Box::new(validation(false)),
            lifecycle: JobLifecycle::Draft,
        },
        "job ops/sync/nightly",
    );

    let message = outcome
        .err()
        .map(|error| error.to_string())
        .ok_or("a failed validation must not read as success")?;
    assert!(message.starts_with("workflow/validation-failed: "), "{message}");
    assert!(
        message.contains("validation/script-syntax: sync.rhai: unexpected token on line 3"),
        "{message}"
    );
    assert!(
        message.contains("validation/owner-unresolved: nightly.toml"),
        "{message}"
    );
    assert!(
        message.contains("tools: the server could not be reached could not be checked"),
        "{message}"
    );
    Ok(())
}

#[test]
fn a_change_of_state_names_the_job_and_its_new_state() -> Result<(), CliJobsError> {
    let text = describe(
        OperationOutcome::Changed(Applied::To(JobLifecycle::Enabled)),
        "shared job acme sync/nightly",
    )?;

    assert_eq!(text, "shared job acme sync/nightly is now enabled.");
    Ok(())
}

#[test]
fn a_deletion_says_the_files_were_left_alone() -> Result<(), CliJobsError> {
    let text = describe(OperationOutcome::Changed(Applied::Removed), "job x")?;

    assert!(
        text.contains("deleted") && text.contains("files") && text.contains("untouched"),
        "{text}"
    );
    Ok(())
}

#[test]
fn an_operation_error_keeps_its_stable_code_in_the_message() {
    let error = CliJobsError::from(OperationError::Conflict);

    assert!(error.to_string().starts_with("workflow/state-changed"), "{error}");
}

#[test]
fn only_a_validation_and_a_resume_need_a_server_to_be_reached() -> Result<(), Box<dyn std::error::Error>> {
    use contextos_workflows::{JobSchedule, SuspensionReason};
    assert!(needs_servers(&OperatorRequest::Validate));
    assert!(needs_servers(&OperatorRequest::Resume));
    for request in [
        OperatorRequest::Schedule(JobSchedule::try_from("0 3 * * *")?),
        OperatorRequest::ClearSchedule,
        OperatorRequest::Suspend(SuspensionReason::try_from("held")?),
        OperatorRequest::Retire,
        OperatorRequest::Reactivate,
        OperatorRequest::Delete,
    ] {
        assert!(!needs_servers(&request), "{request:?}");
    }
    Ok(())
}

fn report_of_a_successor_enabling() -> crate::enabling_report::EnablingReport {
    use crate::enabling_report::{EnablingReport, VersionMove};
    EnablingReport {
        schedule: "0 5 * * *".to_owned(),
        retired: Some("nightly".to_owned()),
        current: Some(VersionMove {
            from: Some("1.0.0".to_owned()),
            to: Some("2.0.0".to_owned()),
        }),
        previous: Some(VersionMove {
            from: None,
            to: Some("1.0.0".to_owned()),
        }),
    }
}

#[test]
fn an_enabling_names_the_predecessor_it_retired_and_how_current_and_previous_moved() -> Result<(), CliJobsError> {
    let text = describe(
        OperationOutcome::Enabled(Box::new(report_of_a_successor_enabling())),
        "job ops/sync/nightly-v2 of vault v",
    )?;

    assert_eq!(
        text,
        "job ops/sync/nightly-v2 of vault v is now enabled. Its schedule is 0 5 * * *. Its predecessor nightly was retired. Current moved from 1.0.0 to 2.0.0; previous moved from none to 1.0.0."
    );
    Ok(())
}

#[test]
fn a_json_result_carries_the_message_and_the_enabling_report() -> Result<(), Box<dyn std::error::Error>> {
    let result = Operated {
        warnings: Vec::new(),
        text: "job x is now enabled.".to_owned(),
        enabling: Some(report_of_a_successor_enabling()),
    };

    assert_eq!(
        serde_json::to_value(&result)?,
        serde_json::json!({
            "message": "job x is now enabled.",
            "enabling": {
                "schedule": "0 5 * * *",
                "retired": "nightly",
                "current": {"from": "1.0.0", "to": "2.0.0"},
                "previous": {"from": null, "to": "1.0.0"},
            },
        })
    );
    Ok(())
}

#[test]
fn a_json_result_of_a_change_that_enabled_nothing_has_no_enabling_report() -> Result<(), Box<dyn std::error::Error>> {
    let result = Operated {
        warnings: Vec::new(),
        text: "job x is now retired.".to_owned(),
        enabling: None,
    };

    assert_eq!(
        serde_json::to_value(&result)?,
        serde_json::json!({"message": "job x is now retired.", "enabling": null})
    );
    Ok(())
}

#[test]
fn a_retention_warning_is_a_line_of_the_text_and_a_field_of_the_document() -> Result<(), Box<dyn std::error::Error>> {
    let performed = Performed {
        outcome: OperationOutcome::Changed(Applied::To(JobLifecycle::Retired)),
        warnings: vec![crate::store_retention::RetentionWarning::raised(
            contextos_workflows::StoreKind::Host,
            &crate::package_retention::RetentionError::from(std::io::Error::other(
                "the jobs folder could not be listed",
            )),
        )],
    };

    let operated = operated(performed, "job x")?;

    assert_eq!(
        operated.text,
        "job x is now retired.\nwarning packages/retention-failed: the shared jobs could not be listed: the jobs folder could not be listed"
    );
    let document = serde_json::to_value(&operated)?;
    assert_eq!(document["warnings"][0]["code"], "packages/retention-failed");
    Ok(())
}

#[test]
fn a_command_with_nothing_to_warn_of_has_no_warnings_field() -> Result<(), Box<dyn std::error::Error>> {
    let performed = Performed {
        outcome: OperationOutcome::Changed(Applied::To(JobLifecycle::Retired)),
        warnings: Vec::new(),
    };

    let document = serde_json::to_value(operated(performed, "job x")?)?;

    assert!(document.get("warnings").is_none());
    Ok(())
}
