use super::*;

#[test]
fn a_write_failure_is_logged_by_its_code_and_never_by_its_text() {
    let failure = EventWriteError::Write(ToolError::Filesystem(contextos_fs::FsError::NotFound {
        path: "/home/someone/vault/memory/events/x.md".into(),
    }));
    assert!(
        failure.to_string().contains("/home/someone"),
        "the premise: the text names a path"
    );
    assert_eq!(String::from(failure), "path/not-found");
    assert_eq!(String::from(EventWriteError::PathsExhausted), "events/paths-exhausted");
}
