use super::*;

#[test]
fn a_note_beneath_the_tasks_folder_names_its_folder_relative_to_it() {
    assert_eq!(folder_beneath_tasks("memory/tasks/q1#review/t-001.md"), "q1#review");
    assert_eq!(folder_beneath_tasks("memory/tasks/clients/a#b/t-001.md"), "clients/a#b");
}

#[test]
fn a_note_directly_in_the_tasks_folder_names_the_top_level() {
    assert_eq!(folder_beneath_tasks("memory/tasks/t-001.md"), "");
}

#[test]
fn a_folder_that_only_shares_the_tasks_folder_name_as_a_prefix_keeps_its_whole_path() {
    assert_eq!(folder_beneath_tasks("memory/tasks-old/t-001.md"), "memory/tasks-old");
    assert_eq!(folder_beneath_tasks("memory/tasksx/a/t-001.md"), "memory/tasksx/a");
}
