use super::*;
use time::macros::datetime;

use crate::logging::DegradedReasons;

fn record() -> RunRecord {
    RunRecord {
        run_id: 7,
        workflow: "copy-notes".to_owned(),
        job: "nightly".to_owned(),
        package: "acme".to_owned(),
        version: Version::new(1, 2, 0),
        digest: "sha256:abc".to_owned(),
        libraries: Vec::new(),
        trigger: RunTrigger::Manual,
        inputs: vec!["source_vault".to_owned(), "destination_vault".to_owned()],
        overrides: Vec::new(),
        outputs: Vec::new(),
        status: RunStatus::Running,
        started: datetime!(2026-10-01 03:00:00 UTC),
        completed: None,
        error: None,
        calls: Vec::new(),
        calls_omitted: 0,
        dry_run: false,
    }
}

fn call(tool: &str, outcome: CallOutcome) -> CallEntry {
    CallEntry {
        server: "contextos".to_owned(),
        tool: tool.to_owned(),
        outcome,
        duration_ms: 12,
    }
}

fn text(record: &RunRecord) -> String {
    String::from(record)
}

#[test]
fn a_new_record_renders_its_identity_and_the_names_of_its_inputs() {
    let rendered = text(&record());

    for expected in [
        "type: job-run\n",
        "id: R-7\n",
        "workflow: copy-notes\n",
        "job: nightly\n",
        "package: acme\n",
        "version: 1.2.0\n",
        "digest: \"sha256:abc\"\n",
        "trigger: manual\n",
        "status: running\n",
        "started: 2026-10-01T03:00:00Z\n",
        "inputs: [\"source_vault\",\"destination_vault\"]\n",
    ] {
        assert!(rendered.contains(expected), "missing {expected:?} in:\n{rendered}");
    }
    assert!(
        rendered.contains("completed:\n") && rendered.contains("error:\n"),
        "an unfinished record has empty completed and error:\n{rendered}"
    );
}

#[test]
fn each_call_is_listed_in_order_with_its_outcome_and_duration() {
    let mut record = record();
    record.record_call(call("vault_info", CallOutcome::Succeeded));
    record.record_call(call(
        "fs_write_file",
        CallOutcome::Failed(WorkflowFailureKind::ToolError),
    ));
    record.record_call(call(
        "fs_delete_file",
        CallOutcome::Failed(WorkflowFailureKind::EffectDenied),
    ));
    record.record_call(call(
        "note_create",
        CallOutcome::Failed(WorkflowFailureKind::VaultOutOfScope),
    ));
    record.record_call(call("git_commit", CallOutcome::AbandonedInFlight));

    let rendered = text(&record);

    let rows = [
        "| 1 | contextos | vault_info | succeeded | 12 ms |",
        "| 2 | contextos | fs_write_file | failed: workflow/tool-error | 12 ms |",
        "| 3 | contextos | fs_delete_file | refused: workflow/effect-denied | 12 ms |",
        "| 4 | contextos | note_create | refused: workflow/vault-out-of-scope | 12 ms |",
        "| 5 | contextos | git_commit | abandoned in flight, outcome unknown | 12 ms |",
    ];
    let positions: Vec<_> = rows.iter().map(|row| rendered.find(row)).collect();
    assert!(
        positions.iter().all(Option::is_some),
        "a row is missing in:\n{rendered}"
    );
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "rows are out of order"
    );
}

#[test]
fn a_name_with_table_syntax_cannot_break_out_of_its_cell() {
    let mut record = record();
    record.record_call(call("a|b\nc", CallOutcome::Succeeded));

    let rendered = text(&record);

    assert!(
        rendered.contains("| 1 | contextos | a\\|b c | succeeded | 12 ms |"),
        "{rendered}"
    );
}

#[test]
fn calls_beyond_the_bound_are_counted_and_the_log_says_it_is_truncated() {
    let mut record = record();
    for _ in 0..(MAX_RECORDED_CALLS + 5) {
        record.record_call(call("vault_info", CallOutcome::Succeeded));
    }

    assert_eq!(record.calls.len(), MAX_RECORDED_CALLS);
    assert_eq!(record.calls_omitted, 5);
    assert!(
        text(&record).contains("5 further calls were not logged."),
        "the truncation is stated"
    );
}

#[test]
fn a_finished_run_carries_the_status_completion_and_error_of_its_outcome() {
    let at = datetime!(2026-10-01 03:00:09 UTC);
    let cases = [
        (RunOutcome::Succeeded, RunStatus::Succeeded, None),
        (
            RunOutcome::Degraded(DegradedReasons {
                kept: vec!["a source note was skipped".to_owned()],
                omitted: 0,
            }),
            RunStatus::Degraded,
            None,
        ),
        (RunOutcome::Cancelled, RunStatus::Cancelled, None),
        (
            RunOutcome::Failed(RunFailure {
                kind: Some(WorkflowFailureKind::TimedOut),
                message: "workflow/timed-out: workflow run exceeded its timeout".to_owned(),
            }),
            RunStatus::Failed,
            Some("workflow/timed-out: workflow run exceeded its timeout"),
        ),
    ];
    for (outcome, status, error) in cases {
        let mut record = record();
        record.finish(&outcome, at);

        assert_eq!(record.status, status);
        assert_eq!(record.completed, Some(at));
        assert_eq!(record.error.as_deref(), error);
    }
}

#[test]
fn an_error_is_written_as_one_quoted_line_whatever_it_contains() {
    let mut record = record();
    record.finish(
        &RunOutcome::Failed(RunFailure::from(
            "line one\nline two: \"quoted\" # not a comment".to_owned(),
        )),
        datetime!(2026-10-01 03:00:09 UTC),
    );

    let rendered = text(&record);

    assert!(
        rendered.contains("error: \"line one\\nline two: \\\"quoted\\\" # not a comment\"\n"),
        "{rendered}"
    );
    let header = RunHeader::try_from(rendered.as_str());
    assert!(header.is_ok(), "{header:?}");
}

#[test]
fn a_record_header_reads_back_what_was_rendered() -> Result<(), RunRecordError> {
    let mut finished = record();
    finished.finish(&RunOutcome::Cancelled, datetime!(2026-10-01 03:00:09 UTC));

    let header = RunHeader::try_from(text(&finished).as_str())?;

    assert_eq!(header.status, RunStatus::Cancelled);
    assert_eq!(header.started, datetime!(2026-10-01 03:00:00 UTC));
    assert_eq!(header.completed, Some(datetime!(2026-10-01 03:00:09 UTC)));
    Ok(())
}

#[test]
fn a_header_without_a_status_is_rejected_not_repaired() {
    let header = RunHeader::try_from("---\ntype: job-run\nstarted: 2026-10-01T03:00:00Z\n---\n");

    assert!(
        matches!(header, Err(RunRecordError::MissingField("status"))),
        "{header:?}"
    );
}

#[test]
fn a_header_with_an_unknown_status_is_rejected() {
    let header = RunHeader::try_from("---\nstatus: paused\nstarted: 2026-10-01T03:00:00Z\n---\n");

    assert!(matches!(header, Err(RunRecordError::UnknownStatus(_))), "{header:?}");
}

#[test]
fn a_running_record_is_marked_interrupted_and_nothing_else_changes() -> Result<(), RunRecordError> {
    let mut running = record();
    running.record_call(call("vault_info", CallOutcome::Succeeded));
    let before = text(&running);
    let at = datetime!(2026-10-01 04:00:00 UTC);

    let after = mark_interrupted(&before, at)?.ok_or(RunRecordError::MissingField("status"))?;

    let header = RunHeader::try_from(after.as_str())?;
    assert_eq!(header.status, RunStatus::Interrupted);
    assert_eq!(header.completed, Some(at));
    assert!(after.contains("may or may not have taken effect"), "{after}");
    assert!(after.contains("| 1 | contextos | vault_info | succeeded | 12 ms |"));
    assert!(after.contains("id: R-7\n") && after.contains("started: 2026-10-01T03:00:00Z\n"));
    Ok(())
}

#[test]
fn a_record_that_is_not_running_is_left_alone() -> Result<(), RunRecordError> {
    let mut finished = record();
    finished.finish(&RunOutcome::Succeeded, datetime!(2026-10-01 03:00:09 UTC));

    let marked = mark_interrupted(&text(&finished), datetime!(2026-10-01 04:00:00 UTC))?;

    assert_eq!(marked, None);
    Ok(())
}

#[test]
fn a_status_reads_as_what_it_renders_as() {
    for status in [
        RunStatus::Running,
        RunStatus::Succeeded,
        RunStatus::Degraded,
        RunStatus::Failed,
        RunStatus::Cancelled,
        RunStatus::Interrupted,
        RunStatus::Skipped,
    ] {
        assert_eq!(RunStatus::try_from(status.as_str()).ok(), Some(status));
    }
}

#[test]
fn a_failure_message_is_cut_so_a_record_stays_small_and_readable() -> Result<(), RunRecordError> {
    let mut record = record();
    record.finish(
        &RunOutcome::Failed(RunFailure::from("secret ".repeat(300_000))),
        datetime!(2026-10-01 03:00:09 UTC),
    );

    let rendered = text(&record);

    assert!(rendered.len() < MAX_ERROR_BYTES * 2 + 2_000, "{} bytes", rendered.len());
    assert!(rendered.contains(" [cut]\"\n"), "the cut is marked");
    assert_eq!(RunHeader::try_from(rendered.as_str())?.status, RunStatus::Failed);
    Ok(())
}

#[test]
fn a_header_with_its_lines_in_another_order_is_still_marked_interrupted() -> Result<(), RunRecordError> {
    let reordered = "---\nerror:\ncompleted:\nstarted: 2026-10-01T03:00:00Z\nstatus: running\n---\n\n## Calls\n";

    let marked = mark_interrupted(reordered, datetime!(2026-10-01 04:00:00 UTC))?
        .ok_or(RunRecordError::MissingField("status"))?;

    assert_eq!(RunHeader::try_from(marked.as_str())?.status, RunStatus::Interrupted);
    Ok(())
}

#[test]
fn a_scheduled_trigger_is_recorded_as_scheduled() {
    let mut scheduled = record();
    scheduled.trigger = RunTrigger::Scheduled;

    assert!(
        text(&scheduled).contains("trigger: scheduled\n"),
        "{}",
        text(&scheduled)
    );
    assert!(text(&record()).contains("trigger: manual\n"));
}

#[test]
fn a_skipped_run_is_a_finished_record_that_ran_nothing() -> Result<(), RunRecordError> {
    let mut skipped = record();
    skipped.trigger = RunTrigger::Scheduled;
    skipped.status = RunStatus::Skipped;
    skipped.completed = Some(skipped.started);
    skipped.error = Some("the job's previous run still held its claim".to_owned());

    let rendered = text(&skipped);

    assert!(rendered.contains("status: skipped\n"), "{rendered}");
    assert!(rendered.contains("No calls were logged."), "{rendered}");
    let header = RunHeader::try_from(rendered.as_str())?;
    assert_eq!(header.status, RunStatus::Skipped);
    assert_eq!(header.completed, Some(skipped.started));
    Ok(())
}

#[test]
fn a_skipped_record_is_not_a_run_that_can_be_interrupted() -> Result<(), RunRecordError> {
    let mut skipped = record();
    skipped.status = RunStatus::Skipped;
    skipped.completed = Some(skipped.started);

    assert_eq!(mark_interrupted(&text(&skipped), skipped.started)?, None);
    Ok(())
}

#[test]
fn a_record_flags_whether_the_run_was_a_dry_run() {
    let live = text(&record());
    let dry = text(&RunRecord {
        dry_run: true,
        ..record()
    });

    assert!(live.contains("dry_run: false\n"), "{live}");
    assert!(dry.contains("dry_run: true\n"), "{dry}");
}

#[test]
fn a_record_names_overridden_inputs_and_outputs_and_holds_no_value() {
    let rendered = text(&RunRecord {
        overrides: vec!["target".to_owned()],
        outputs: vec!["created".to_owned(), "plan".to_owned()],
        ..record()
    });

    assert!(rendered.contains("overrides: [\"target\"]\n"), "{rendered}");
    assert!(rendered.contains("outputs: [\"created\",\"plan\"]\n"), "{rendered}");
    let empty = text(&record());
    assert!(
        empty.contains("overrides: []\n") && empty.contains("outputs: []\n"),
        "{empty}"
    );
}

#[test]
fn a_record_written_before_these_fields_existed_reads_back_with_both_empty() -> Result<(), RunRecordError> {
    let written = text(&RunRecord {
        overrides: vec!["target".to_owned()],
        outputs: vec!["created".to_owned()],
        ..record()
    });
    let current = RunHeader::try_from(written.as_str())?;
    let kept: Vec<&str> = written
        .lines()
        .filter(|line| !line.starts_with("overrides:") && !line.starts_with("outputs:"))
        .collect();
    let before_the_fields = format!("{}\n", kept.join("\n"));
    let old = RunHeader::try_from(before_the_fields.as_str())?;

    assert_eq!(current.overrides, vec!["target".to_owned()]);
    assert_eq!(current.outputs, vec!["created".to_owned()]);
    assert!(old.overrides.is_empty() && old.outputs.is_empty(), "{old:?}");
    assert_eq!(old.status, RunStatus::Running, "the rest of an old record still reads");
    Ok(())
}

fn library(name: &str, version: &str) -> RecordedLibrary {
    RecordedLibrary {
        name: name.to_owned(),
        version: Version::parse(version).unwrap_or_else(|_| Version::new(0, 0, 0)),
        digest: format!("sha256:{name}"),
    }
}

#[test]
fn a_record_lists_the_libraries_the_run_resolved_with_their_release_and_digest_in_the_order_given() {
    let rendered = text(&RunRecord {
        libraries: vec![library("acme-core", "1.0.0"), library("acme-text", "1.4.2")],
        ..record()
    });

    assert!(
        rendered.contains(
            "libraries: [{\"name\":\"acme-core\",\"version\":\"1.0.0\",\"digest\":\"sha256:acme-core\"},\
             {\"name\":\"acme-text\",\"version\":\"1.4.2\",\"digest\":\"sha256:acme-text\"}]\n"
        ),
        "{rendered}"
    );
}

#[test]
fn a_record_of_a_run_that_resolved_no_library_has_no_libraries_key() {
    assert!(!text(&record()).contains("libraries"));
}

#[test]
fn a_record_with_libraries_still_reads_back_its_header() -> Result<(), RunRecordError> {
    let header = RunHeader::try_from(
        text(&RunRecord {
            libraries: vec![library("acme-text", "1.4.2")],
            ..record()
        })
        .as_str(),
    )?;

    assert_eq!(header.status, RunStatus::Running);
    Ok(())
}
