use super::*;

#[test]
fn validate_note_path_accepts_a_plain_dated_relative_path() {
    assert_eq!(
        validate_note_path("2026/09/2026-09-14-0705.md"),
        Some("2026/09/2026-09-14-0705.md".to_owned())
    );
}

#[test]
fn validate_note_path_rejects_traversal() {
    assert_eq!(validate_note_path("../../etc/passwd"), None);
}

#[test]
fn validate_note_path_rejects_an_empty_value() {
    assert_eq!(validate_note_path(""), None);
    assert_eq!(validate_note_path("   "), None);
}

#[test]
fn validate_note_path_rejects_a_value_not_ending_in_md() {
    assert_eq!(validate_note_path("2026/09"), None);
    assert_eq!(validate_note_path("2026/09/note.txt"), None);
}

#[test]
fn weekday_and_iso_week_computes_both_from_a_valid_date_field() {
    let (weekday, iso_week) = weekday_and_iso_week(Some("2026-09-14"));
    assert_eq!(weekday, "Monday");
    assert_eq!(iso_week, "week 38");
}

#[test]
fn weekday_and_iso_week_is_blank_for_an_absent_or_malformed_date_field() {
    assert_eq!(weekday_and_iso_week(None), (String::new(), String::new()));
    assert_eq!(weekday_and_iso_week(Some("not a date")), (String::new(), String::new()));
}

fn note_state(relative_path: &str) -> NoteState {
    NoteState {
        relative_path: relative_path.to_owned(),
        title: "A title".to_owned(),
        status: Some("draft".to_owned()),
        date: Some("2026-09-14".to_owned()),
        time: Some("07:05".to_owned()),
        tags: vec!["note".to_owned()],
        frontmatter: serde_json::Map::new(),
        body: "# A title\n\nSome text.\n".to_owned(),
        expected_hash: Some("abc123".to_owned()),
    }
}

#[test]
fn selected_note_view_derives_the_rename_value_from_the_current_basename_without_its_extension() {
    let view = selected_note_view(&note_state("2026/09/2026-09-14-0705.md"));
    assert_eq!(view.rename_value, "2026-09-14-0705");
}

#[test]
fn selected_note_view_derives_the_rename_value_correctly_even_with_no_directory_component() {
    let view = selected_note_view(&note_state("just-a-name.md"));
    assert_eq!(view.rename_value, "just-a-name");
}

#[test]
fn selected_note_view_computes_the_weekday_and_iso_week_from_the_date_field() {
    let view = selected_note_view(&note_state("2026/09/x.md"));
    assert_eq!(view.weekday, "Monday");
    assert_eq!(view.iso_week, "week 38");
}

#[test]
fn selected_note_view_falls_back_expected_hash_to_empty() {
    let mut state = note_state("x.md");
    state.expected_hash = None;
    let view = selected_note_view(&state);
    assert_eq!(view.expected_hash, "");
}

// `metadata_field_views` and `metadata_patch` moved to `crate::metadata_panel`
// (`metadata_panel_test.rs` covers them there) once every screen editing frontmatter as
// real fields, not just Notes, needed them - neither function has any behaviour specific
// to a note as opposed to any other frontmatter-bearing item.

#[test]
fn load_error_message_is_actionable_for_each_variant() {
    assert!(load_error_message(&LoadNoteError::NotFound).contains("could not be found"));
    assert!(load_error_message(&LoadNoteError::Malformed).contains("frontmatter"));
    // `LoadNoteError::Write` wraps a real `McpCallError::Unreachable`
    // (a boxed `rmcp::ServiceError`), not constructible without a genuine
    // broken transport - `tests/notes.rs`'s killed-session tests exercise
    // that construction for real; this match is exhaustive at compile time
    // (`rust-quality.md`: "prefer exhaustive matches"), so a wrong message
    // for that arm would only be an editorial slip, not a missing case.
}

#[test]
fn a_base_canvas_or_diagram_may_be_shown_but_never_written() {
    for path in ["2026/board.canvas", "2026/list.base", "2026/flow.mermaid"] {
        assert_eq!(validate_note_path(path), None, "{path}");
        assert_eq!(validate_shown_path(path), Some(path.to_owned()), "{path}");
    }
    assert_eq!(validate_shown_path("2026/note.md"), Some("2026/note.md".to_owned()));
    assert_eq!(validate_shown_path("../board.canvas"), None);
    assert_eq!(validate_shown_path("2026/report.pdf"), None);
}
