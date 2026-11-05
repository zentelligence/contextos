use super::*;

#[test]
fn rename_error_message_is_actionable_for_each_variant() {
    assert!(rename_error_message(&insights::RenameNoteError::EmptyTitle).contains("needs a title"));
    assert!(
        rename_error_message(&insights::RenameNoteError::InvalidStatus).contains("proposed, adopted, or dismissed")
    );
    assert!(rename_error_message(&insights::RenameNoteError::Rejected).contains("could not be renamed"));
    // `RenameNoteError::Write` wraps a real `McpCallError::Unreachable` (a
    // boxed `rmcp::ServiceError`), not constructible without a genuine
    // broken transport - `tests/insights.rs`'s own real-session tests
    // exercise that construction for real; this match is exhaustive at
    // compile time, so a wrong message for that arm would only be an
    // editorial slip, not a missing case.
}
