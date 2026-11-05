use super::*;

fn files(paths: &[&str]) -> VaultFiles {
    paths.iter().map(|path| (*path).to_owned()).collect()
}

#[test]
fn a_path_is_a_file_only_when_it_matches_exactly() {
    let vault = files(&["notes/a.md", "img/Photo.PNG"]);

    assert_eq!(vault.is_file("notes/a.md"), Ok(true));
    assert_eq!(vault.is_file("notes/A.md"), Ok(false));
    assert_eq!(vault.is_file("notes"), Ok(false));
}

#[test]
fn names_are_matched_on_whole_path_segments_ignoring_case() {
    let vault = files(&["foo.md", "wiki/Foo.md", "xfoo.md", "deep/wiki/foo.md"]);

    assert_eq!(
        vault.files_ending_with("foo.md"),
        Ok(vec![
            "deep/wiki/foo.md".to_owned(),
            "foo.md".to_owned(),
            "wiki/Foo.md".to_owned()
        ])
    );
    assert_eq!(
        vault.files_ending_with("wiki/foo.md"),
        Ok(vec!["deep/wiki/foo.md".to_owned(), "wiki/Foo.md".to_owned()])
    );
}

#[test]
fn inserting_and_removing_report_whether_the_set_changed_and_keep_the_name_lookup_in_step() {
    let mut vault = VaultFiles::default();

    assert!(vault.insert("a/b.md"));
    assert!(!vault.insert("a/b.md"));
    assert_eq!(vault.files_ending_with("b.md"), Ok(vec!["a/b.md".to_owned()]));
    assert!(vault.remove("a/b.md"));
    assert!(!vault.remove("a/b.md"));
    assert_eq!(vault.files_ending_with("b.md"), Ok(Vec::new()));
    assert!(vault.is_empty());
}

#[test]
fn removing_a_folder_removes_the_files_under_it_and_nothing_with_a_similar_name() {
    let mut vault = files(&["dir/a.md", "dir/sub/b.png", "dir2/c.md", "dir.md"]);

    let removed = vault.remove_under("dir");

    assert_eq!(removed, ["dir/a.md", "dir/sub/b.png"]);
    assert_eq!(vault.len(), 2);
    assert_eq!(vault.is_file("dir2/c.md"), Ok(true));
    assert_eq!(vault.files_ending_with("a.md"), Ok(Vec::new()));
}
