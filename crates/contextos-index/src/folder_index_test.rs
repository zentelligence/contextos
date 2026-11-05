use super::*;

#[test]
fn both_index_note_names_are_folder_indexes_and_nothing_else_is() {
    assert!(is_folder_index("index.md"));
    assert!(is_folder_index("_index.md"));
    for other in ["notes.md", "index.markdown", "my-index.md", "index.md.bak", ""] {
        assert!(!is_folder_index(other), "{other}");
    }
}
