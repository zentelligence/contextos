use super::*;

#[test]
fn an_empty_or_blank_folder_names_the_top_of_the_tasks_folder() -> Result<(), TaskFolderError> {
    for raw in ["", "   "] {
        let folder = TaskFolder::try_from(raw)?;
        assert_eq!(folder, TaskFolder::default(), "{raw:?}");
        assert_eq!(folder.as_str(), "");
        assert_eq!(folder.note_path("a-001.md"), "memory/tasks/a-001.md");
    }
    Ok(())
}

#[test]
fn a_nested_folder_is_kept_without_its_surrounding_whitespace_or_trailing_slash() -> Result<(), TaskFolderError> {
    let folder = TaskFolder::try_from(" clients/acme/ ")?;
    assert_eq!(folder.as_str(), "clients/acme");
    assert_eq!(folder.note_path("a-001.md"), "memory/tasks/clients/acme/a-001.md");
    Ok(())
}

#[test]
fn every_form_that_would_leave_the_tasks_folder_is_refused_with_its_reason() {
    let cases = [
        (
            "../decisions",
            TaskFolderError::Path(VaultRelativePathError::InvalidSegment),
        ),
        (
            "inbox/../../notes",
            TaskFolderError::Path(VaultRelativePathError::InvalidSegment),
        ),
        (
            "clients//acme",
            TaskFolderError::Path(VaultRelativePathError::InvalidSegment),
        ),
        ("/memory/tasks", TaskFolderError::Path(VaultRelativePathError::Absolute)),
        (
            "clients\\acme",
            TaskFolderError::Path(VaultRelativePathError::Backslash),
        ),
        ("C:/clients", TaskFolderError::Path(VaultRelativePathError::Colon)),
        ("clients:stream", TaskFolderError::Path(VaultRelativePathError::Colon)),
        (".trash", TaskFolderError::Hidden),
        ("clients/.archive", TaskFolderError::Hidden),
        ("a#b", TaskFolderError::Character),
        ("a|b", TaskFolderError::Character),
        ("a^b", TaskFolderError::Character),
        ("[a]", TaskFolderError::Character),
        ("x\n- fake line", TaskFolderError::Character),
        ("a\tb", TaskFolderError::Character),
        ("a<b>", TaskFolderError::Character),
        ("a\"b", TaskFolderError::Character),
        ("a?b", TaskFolderError::Character),
        ("a*b", TaskFolderError::Character),
    ];
    for (raw, expected) in cases {
        assert_eq!(TaskFolder::try_from(raw), Err(expected), "{raw:?}");
    }
}

#[test]
fn a_submitted_value_must_be_text_or_null() -> Result<(), TaskFolderError> {
    assert_eq!(TaskFolder::try_from(&serde_json::json!("clients"))?.as_str(), "clients");
    assert_eq!(TaskFolder::try_from(&serde_json::Value::Null)?, TaskFolder::default());
    for value in [
        serde_json::json!(["clients"]),
        serde_json::json!(7),
        serde_json::json!({"a": 1}),
    ] {
        assert_eq!(TaskFolder::try_from(&value), Err(TaskFolderError::NotText), "{value}");
    }
    Ok(())
}

#[test]
fn every_rejection_carries_a_distinct_stable_code() {
    let codes = [
        TaskFolderError::Path(VaultRelativePathError::Absolute).code(),
        TaskFolderError::Path(VaultRelativePathError::InvalidSegment).code(),
        TaskFolderError::Hidden.code(),
        TaskFolderError::Character.code(),
        TaskFolderError::NotText.code(),
    ];
    let unique: BTreeSet<_> = codes.iter().collect();
    assert_eq!(unique.len(), codes.len());
}

#[test]
fn a_task_note_path_reads_as_its_folder_and_file_name() -> Result<(), TaskNoteLocationError> {
    let nested = TaskNoteLocation::try_from("memory/tasks/clients/acme/a-001.md")?;
    assert_eq!(nested.folder.as_str(), "clients/acme");
    assert_eq!(nested.file_name, "a-001.md");

    let top = TaskNoteLocation::try_from("memory/tasks/a-002.md")?;
    assert_eq!(top.folder, TaskFolder::default());
    assert_eq!(top.file_name, "a-002.md");
    Ok(())
}

#[test]
fn a_note_outside_the_tasks_folder_has_no_task_location() {
    for path in [
        "memory/notes/a-001.md",
        "memory/tasksx/a-001.md",
        "memory/tasks",
        "memory/tasks/",
        "a-001.md",
    ] {
        assert_eq!(
            TaskNoteLocation::try_from(path),
            Err(TaskNoteLocationError::OutsideTasks),
            "{path:?}"
        );
    }
}

#[test]
fn an_existing_folder_that_typed_input_cannot_name_has_no_task_location() {
    for path in ["memory/tasks/ inbox/a-001.md", "memory/tasks/inbox /a-001.md"] {
        assert_eq!(
            TaskNoteLocation::try_from(path),
            Err(TaskNoteLocationError::NotAsTyped),
            "{path:?}"
        );
    }
}

#[test]
fn moving_a_note_keeps_its_file_name_in_the_new_folder() -> Result<(), Box<dyn std::error::Error>> {
    let location = TaskNoteLocation::try_from("memory/tasks/inbox/a-001.md")?;
    assert_eq!(
        location.moved_to(&TaskFolder::try_from("clients/acme")?),
        "memory/tasks/clients/acme/a-001.md"
    );
    assert_eq!(location.moved_to(&TaskFolder::default()), "memory/tasks/a-001.md");
    Ok(())
}

#[test]
fn known_folders_list_every_folder_holding_a_task_and_its_ancestors_once_in_order() {
    let folders = known_task_folders([
        "memory/tasks/a-001.md",
        "memory/tasks/clients/acme/a-002.md",
        "memory/tasks/clients/acme/a-003.md",
        "memory/tasks/internal/a-004.md",
        "memory/notes/elsewhere.md",
    ]);
    assert_eq!(folders, ["clients", "clients/acme", "internal"]);
}
