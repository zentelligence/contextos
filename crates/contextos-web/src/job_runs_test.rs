use super::*;

fn a_summary(status: &str) -> RunSummary {
    RunSummary {
        path: "memory/jobs/ops/sync/nightly/2026/09/R-1.md".to_owned(),
        run_id: "R-1".to_owned(),
        job: String::new(),
        release: String::new(),
        dry_run: false,
        trigger: "manual".to_owned(),
        status: status.to_owned(),
        started: "2026-09-20T10:00:00Z".to_owned(),
        completed: String::new(),
        error: String::new(),
        code: None,
        script_started: ScriptStarted::Absent,
    }
}

#[test]
fn state_maps_every_documented_run_status_to_its_status_dot_class() {
    assert_eq!(a_summary("succeeded").state(), "done");
    assert_eq!(a_summary("failed").state(), "blocked");
    assert_eq!(a_summary("running").state(), "working");
    assert_eq!(a_summary("skipped").state(), "idle");
    assert_eq!(a_summary("degraded").state(), "degraded");
}

#[test]
fn state_falls_back_to_unknown_for_an_unrecognised_status() {
    assert_eq!(a_summary("").state(), "unknown");
    assert_eq!(a_summary("queued").state(), "unknown");
}

#[test]
fn attention_flags_only_failed_skipped_and_degraded_runs() {
    assert_eq!(a_summary("failed").attention(), Some("error"));
    assert_eq!(a_summary("skipped").attention(), Some("warning"));
    assert_eq!(a_summary("degraded").attention(), Some("warning"));
    assert_eq!(a_summary("running").attention(), None);
    assert_eq!(a_summary("succeeded").attention(), None);
}

fn a_row(path: &str, status: Option<&str>, started: Option<&str>) -> RunRow {
    RunRow {
        path: path.to_owned(),
        dry_run: None,
        trigger: Some("manual".to_owned()),
        status: status.map(str::to_owned),
        started: started.map(str::to_owned),
        completed: None,
        error: None,
        code: None,
        script_started: None,
        version: None,
    }
}

#[test]
fn a_well_formed_row_converts_into_a_run_summary() -> Result<(), Box<dyn std::error::Error>> {
    let summary = RunSummary::try_from(a_row(
        "memory/jobs/sync-crm-contacts/2026/09/R-47.md",
        Some("succeeded"),
        Some("2026-09-08T03:00:01Z"),
    ))?;
    assert_eq!(summary.run_id, "R-47");
    assert_eq!(summary.status, "succeeded");
    assert_eq!(summary.started, "2026-09-08T03:00:01Z");
    assert_eq!(summary.trigger, "manual");
    Ok(())
}

#[test]
fn a_row_with_no_status_is_rejected() {
    let result = RunSummary::try_from(a_row(
        "memory/jobs/j/2026/09/R-1.md",
        None,
        Some("2026-09-08T03:00:01Z"),
    ));
    assert!(result.is_err());
}

#[test]
fn a_row_with_no_start_time_is_rejected() {
    let result = RunSummary::try_from(a_row("memory/jobs/j/2026/09/R-1.md", Some("succeeded"), None));
    assert!(result.is_err());
}

#[test]
fn a_row_whose_path_is_not_a_numbered_run_record_is_rejected() {
    let result = RunSummary::try_from(a_row(
        "memory/jobs/j/2026/09/archive.md",
        Some("succeeded"),
        Some("2026-09-08T03:00:01Z"),
    ));
    assert!(result.is_err());
}

#[test]
fn a_row_with_an_empty_path_is_rejected() {
    let result = RunSummary::try_from(a_row("", Some("succeeded"), Some("2026-09-08T03:00:01Z")));
    assert!(result.is_err());
}

#[test]
fn run_number_parses_the_numbered_suffix_of_a_run_record_name() {
    assert_eq!(run_number("R-47.md"), Some(47));
    assert_eq!(run_number("archive.md"), None);
    assert_eq!(run_number(""), None);
}

#[test]
fn a_started_field_is_read_as_true_false_absent_or_invalid() {
    use serde_json::json;
    for (stored, expected) in [
        (Some(json!(true)), ScriptStarted::Yes),
        (Some(json!(false)), ScriptStarted::No),
        (None, ScriptStarted::Absent),
        (Some(Value::Null), ScriptStarted::Absent),
        (Some(json!("maybe")), ScriptStarted::Invalid),
        (Some(json!("true")), ScriptStarted::Invalid),
        (Some(json!(1)), ScriptStarted::Invalid),
    ] {
        assert_eq!(ScriptStarted::from(stored.as_ref()), expected, "{stored:?}");
    }
}

#[test]
fn a_row_whose_started_field_is_not_a_boolean_is_still_a_run_summary_so_the_rest_of_the_history_is_listed()
-> Result<(), Box<dyn std::error::Error>> {
    let row: RunRow = serde_json::from_value(serde_json::json!({
        "file.path": "memory/jobs/j/2026/09/R-1.md",
        "status": "failed",
        "started": "2026-09-08T03:00:01Z",
        "script_started": "maybe",
    }))?;

    let summary = RunSummary::try_from(row)?;

    assert_eq!(summary.script_started, ScriptStarted::Invalid);
    Ok(())
}

#[test]
fn a_row_carries_the_version_of_the_release_the_run_read_or_nothing_when_its_record_does_not_say()
-> Result<(), Box<dyn std::error::Error>> {
    let mut row = a_row(
        "memory/jobs/ops/sync/nightly/2026/09/R-3.md",
        Some("succeeded"),
        Some("2026-09-08T03:00:01Z"),
    );
    row.version = Some("1.4.2".to_owned());
    let without = a_row(
        "memory/jobs/ops/sync/nightly/2026/09/R-4.md",
        Some("succeeded"),
        Some("2026-09-08T03:00:01Z"),
    );

    assert_eq!(RunSummary::try_from(row)?.release, "1.4.2");
    assert_eq!(RunSummary::try_from(without)?.release, "");
    Ok(())
}
