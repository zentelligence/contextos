use super::*;

#[test]
fn a_note_declares_the_base_its_base_property_links_to() {
    let declared = DeclaredBase::try_from("---\nbase: \"[[Tasks.base]]\"\n---\nbody\n");

    assert_eq!(declared.as_ref().map(DeclaredBase::target), Ok("Tasks.base"));
}

#[test]
fn a_note_without_a_base_property_or_with_one_that_is_not_a_link_declares_nothing() {
    for source in [
        "no frontmatter\n",
        "---\ntitle: x\n---\n",
        "---\nbase: Tasks.base\n---\n",
        "---\nbase: 3\n---\n",
    ] {
        assert_eq!(DeclaredBase::try_from(source), Err(NoDeclaredBase), "{source:?}");
    }
}

#[test]
fn only_a_base_file_counts_as_the_base_a_note_declares() {
    assert!(DeclaredBase::is_base_file("bases/Tasks.base"));
    assert!(DeclaredBase::is_base_file("Tasks.BASE"));
    assert!(!DeclaredBase::is_base_file("Tasks.md"));
}

#[test]
fn a_base_confines_its_records_to_the_folder_its_filters_name() {
    let records = BaseRecords::try_from("filters:\n  and:\n    - file.inFolder(\"projects/tasks\")\n");

    assert_eq!(
        records,
        Ok(BaseRecords {
            path: "projects/tasks".to_owned(),
            includes_subfolders: true,
        })
    );
}

#[test]
fn a_base_with_no_folder_filter_or_unreadable_text_has_no_records_folder() {
    assert_eq!(BaseRecords::try_from("views: []\n"), Err(NoRecordsFolder));
    assert_eq!(BaseRecords::try_from(": : :"), Err(NoRecordsFolder));
}
