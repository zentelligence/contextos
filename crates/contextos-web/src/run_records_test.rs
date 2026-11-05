use super::*;

#[test]
fn trigger_renders_its_documented_frontmatter_value() {
    assert_eq!(Trigger::Scheduled.as_str(), "scheduled");
    assert_eq!(Trigger::Manual.as_str(), "manual");
}

#[test]
fn a_cancelled_run_is_recorded_with_its_own_status() {
    assert_eq!(status_of(&RunOutcome::Cancelled), "cancelled");
}

#[test]
fn every_outcome_has_its_own_status() {
    assert_eq!(status_of(&RunOutcome::Succeeded), "succeeded");
    assert_eq!(status_of(&RunOutcome::Failed("boom".to_owned().into())), "failed");
}

fn job_with_entity(entity: &str) -> RegisteredJob {
    RegisteredJob {
        package: "ops".to_owned(),
        workflow_slug: "sync".to_owned(),
        slug: "nightly".to_owned(),
        name: "Nightly".to_owned(),
        timeout_secs: 30,
        owner: None,
        declared_owner: None,
        entity: crate::jobs::ResolvedEntity::from((Some(entity), None)),
        release: contextos_workflows::JobFileRelease {
            version: contextos_workflows::JobVersion::from(contextos_workflows::JobVersionNumbers {
                major: 1,
                minor: 0,
                patch: 0,
            }),
            predecessor: None,
        },
        entry: "sync.rhai".to_owned(),
        calls: contextos_workflows::call_rules::CallRules::default(),
        inputs: contextos_workflows::ValidatedInputs::default(),
        declared_inputs: contextos_workflows::DeclaredInputs::default(),
        outputs: contextos_workflows::DeclaredOutputs::default(),
        workflow_log_level: None,
        log_level: None,
        ready: true,
        file_name: "nightly.toml".to_owned(),
        templates: contextos_workflows::DeclaredFiles::default(),
        modules: contextos_workflows::DeclaredModules::default(),
        libraries: Vec::new(),
    }
}

#[test]
fn a_run_record_header_carries_the_resolved_entity_and_no_literal_one() {
    for entity in ["personal", "vault", "acme-co"] {
        let job = job_with_entity(entity);
        let header = RunRecordHeader {
            job: &job,
            run_id: 3,
            trigger: Trigger::Scheduled,
            status: "running",
            started: time::macros::datetime!(2026-10-21 03:00:00 UTC),
            completed: None,
            dry_run: false,
            overrides: &[],
        };

        let frontmatter = frontmatter_of(&header);

        assert_eq!(
            frontmatter.get("entity"),
            Some(&Value::String(entity.to_owned())),
            "{entity}"
        );
    }
}

#[test]
fn a_run_record_header_names_the_package_workflow_and_job_of_the_run() {
    let job = job_with_entity("vault");
    let header = RunRecordHeader {
        job: &job,
        run_id: 3,
        trigger: Trigger::Scheduled,
        status: "running",
        started: time::macros::datetime!(2026-10-21 03:00:00 UTC),
        completed: None,
        dry_run: false,
        overrides: &[],
    };

    let frontmatter = frontmatter_of(&header);

    for (key, expected) in [
        ("package", "ops"),
        ("workflow", "sync"),
        ("title", "Run: ops/sync/nightly R-3"),
    ] {
        assert_eq!(frontmatter.get(key), Some(&Value::String(expected.to_owned())), "{key}");
    }
}

const FINISHED: time::OffsetDateTime = time::macros::datetime!(2026-10-21 03:00:05 UTC);

#[test]
fn a_new_run_record_has_a_code_field_beside_error_and_neither_has_a_value_yet() {
    let job = job_with_entity("vault");
    let header = RunRecordHeader {
        job: &job,
        run_id: 3,
        trigger: Trigger::Scheduled,
        status: "running",
        started: time::macros::datetime!(2026-10-21 03:00:00 UTC),
        completed: None,
        dry_run: false,
        overrides: &[],
    };

    let frontmatter = frontmatter_of(&header);

    assert_eq!(frontmatter.get("error"), Some(&Value::Null));
    assert_eq!(
        frontmatter.get("code"),
        Some(&Value::Null),
        "the failure code is a field of its own, empty until the run fails"
    );
}

#[test]
fn a_failed_runs_record_carries_the_stable_failure_code_as_its_own_field_beside_the_error_text() {
    let failure = contextos_workflows::RunFailure {
        kind: Some(contextos_workflows::WorkflowFailureKind::TimedOut),
        message: "the run exceeded its limit of 1 seconds".to_owned(),
    };

    let patch = completion_patch(&RunOutcome::Failed(failure), &[], FINISHED, true);

    assert_eq!(patch.get("status"), Some(&Value::String("failed".to_owned())));
    assert_eq!(
        patch.get("code"),
        Some(&Value::String("workflow/timed-out".to_owned())),
        "so a task for the failure never parses the error text"
    );
    assert_eq!(
        patch.get("error"),
        Some(&Value::String("the run exceeded its limit of 1 seconds".to_owned()))
    );
}

#[test]
fn a_failure_with_no_stable_kind_records_no_code_and_a_run_that_did_not_fail_records_none() {
    let kindless = RunOutcome::Failed("it broke".to_owned().into());

    let failed = completion_patch(&kindless, &[], FINISHED, true);
    let succeeded = completion_patch(&RunOutcome::Succeeded, &[], FINISHED, true);

    assert_eq!(
        failed.get("code"),
        Some(&Value::Null),
        "the field is present and empty, so a reader can tell none from not recorded"
    );
    assert_eq!(
        succeeded.get("code"),
        None,
        "a run that did not fail leaves the field alone"
    );
}

#[test]
fn a_new_record_says_its_script_has_not_started_and_the_completion_says_whether_it_did() {
    let job = job_with_entity("zenith");
    let header = RunRecordHeader {
        job: &job,
        run_id: 3,
        trigger: Trigger::Scheduled,
        status: "running",
        started: FINISHED,
        completed: None,
        dry_run: false,
        overrides: &[],
    };

    let frontmatter = frontmatter_of(&header);
    let began = completion_patch(&RunOutcome::Succeeded, &[], FINISHED, true);
    let refused = completion_patch(&RunOutcome::Failed("no entry".to_owned().into()), &[], FINISHED, false);

    assert_eq!(frontmatter.get("script_started"), Some(&Value::Bool(false)));
    assert_eq!(began.get("script_started"), Some(&Value::Bool(true)));
    assert_eq!(refused.get("script_started"), Some(&Value::Bool(false)));
}
