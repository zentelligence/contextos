use serde_json::json;

use super::*;

#[test]
fn a_folder_equality_names_the_records_folder() {
    let filters = json!("file.folder == \"memory/decisions\"");

    assert_eq!(
        records_folder(Some(&filters)),
        Some(RecordsFolder {
            path: "memory/decisions",
            includes_subfolders: false
        })
    );
}

#[test]
fn an_in_folder_call_alongside_an_index_exclusion_names_the_records_folder() {
    let filters = json!("file.inFolder(\"memory/tasks/\") && file.basename != \"index\"");

    assert_eq!(
        records_folder(Some(&filters)),
        Some(RecordsFolder {
            path: "memory/tasks",
            includes_subfolders: true
        })
    );
}

#[test]
fn a_folder_inside_a_structured_conjunction_names_the_records_folder() {
    let filters = json!({ "and": ["type == \"task\"", "file.inFolder(\"work/tasks\")"] });

    assert_eq!(
        records_folder(Some(&filters)),
        Some(RecordsFolder {
            path: "work/tasks",
            includes_subfolders: true
        })
    );
}

#[test]
fn filters_on_properties_alone_name_no_records_folder() {
    let filters = json!({ "and": ["type == \"task\"", "project == \"alpha\""] });

    assert_eq!(records_folder(Some(&filters)), None);
}

#[test]
fn a_single_file_path_names_no_records_folder() {
    let filters = json!("file.path == \"work/one.md\"");

    assert_eq!(records_folder(Some(&filters)), None);
}

#[test]
fn a_disjunction_over_different_folders_names_no_records_folder() {
    let filters = json!("file.inFolder(\"a\") || file.inFolder(\"b\")");

    assert_eq!(records_folder(Some(&filters)), None);
}

#[test]
fn no_filters_and_the_vault_root_name_no_records_folder() {
    assert_eq!(records_folder(None), None);
    assert_eq!(records_folder(Some(&json!("file.inFolder(\"/\")"))), None);
}

#[test]
fn a_folder_equality_holds_only_the_notes_directly_inside_it() {
    let folder = RecordsFolder {
        path: "memory/decisions",
        includes_subfolders: false,
    };

    assert!(folder.contains("memory/decisions/vd-001.md"));
    assert!(!folder.contains("memory/decisions/2026/vd-002.md"));
    assert!(!folder.contains("memory/decisions.md"));
}

#[test]
fn an_in_folder_call_holds_every_note_beneath_it() {
    let folder = RecordsFolder {
        path: "memory/tasks",
        includes_subfolders: true,
    };

    assert!(folder.contains("memory/tasks/a-1.md"));
    assert!(folder.contains("memory/tasks/deep/a-2.md"));
    assert!(!folder.contains("memory/tasks-archive/a-3.md"));
}
