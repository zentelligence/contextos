use super::*;

fn dir(name: &str, children: Vec<DirectoryTreeNode>) -> DirectoryTreeNode {
    DirectoryTreeNode {
        name: name.to_owned(),
        kind: "dir".to_owned(),
        children,
    }
}

fn file(name: &str) -> DirectoryTreeNode {
    DirectoryTreeNode {
        name: name.to_owned(),
        kind: "file".to_owned(),
        children: Vec::new(),
    }
}

#[test]
fn build_folder_assembles_relative_paths_and_marks_markdown_files() -> Result<(), Box<dyn std::error::Error>> {
    let tree = dir(
        "zenith",
        vec![dir("finance", vec![file("overview.md"), file("export.csv")])],
    );
    let root = build_folder(&tree, "");
    let finance = &root.folders[0];
    assert_eq!(finance.relative_path, "finance");
    let export = finance
        .files
        .iter()
        .find(|f| f.label == "export.csv")
        .ok_or("export.csv should be listed")?;
    assert!(!export.is_markdown);
    let overview = finance
        .files
        .iter()
        .find(|f| f.label == "overview")
        .ok_or("overview.md should be listed")?;
    assert!(overview.is_markdown);
    assert_eq!(overview.relative_path, "finance/overview.md");
    Ok(())
}

#[test]
fn build_folder_sorts_folders_and_files_alphabetically() {
    let tree = dir(
        "zenith",
        vec![
            file("zebra.md"),
            file("apple.md"),
            dir("sales", vec![]),
            dir("legal", vec![]),
        ],
    );
    let root = build_folder(&tree, "");
    let folder_labels: Vec<&str> = root.folders.iter().map(|f| f.label.as_str()).collect();
    assert_eq!(folder_labels, vec!["legal", "sales"]);
    let file_labels: Vec<&str> = root.files.iter().map(|f| f.label.as_str()).collect();
    assert_eq!(file_labels, vec!["apple", "zebra"]);
}

#[test]
fn find_folder_walks_down_matching_segments() -> Result<(), Box<dyn std::error::Error>> {
    let tree = dir("zenith", vec![dir("finance", vec![dir("tax", vec![])])]);
    let root = build_folder(&tree, "");
    let found = find_folder(&root, "finance/tax").ok_or("folder should be found")?;
    assert_eq!(found.relative_path, "finance/tax");
    Ok(())
}

#[test]
fn find_folder_returns_none_for_a_missing_segment() {
    let tree = dir("zenith", vec![dir("finance", vec![])]);
    let root = build_folder(&tree, "");
    assert!(find_folder(&root, "finance/nonexistent").is_none());
}

#[test]
fn find_folder_returns_root_for_an_empty_relative_dir() -> Result<(), Box<dyn std::error::Error>> {
    let tree = dir("zenith", vec![]);
    let root = build_folder(&tree, "");
    let found = find_folder(&root, "").ok_or("root should be found")?;
    assert_eq!(found.relative_path, "");
    Ok(())
}

#[test]
fn classify_delete_failure_maps_known_codes_and_defaults_unknown_ones_to_rejected() {
    assert!(matches!(
        classify_delete_failure("path/not-found"),
        DeleteEntityPathError::NotFound
    ));
    assert!(matches!(
        classify_delete_failure("io/directory-not-empty"),
        DeleteEntityPathError::NotEmpty
    ));
    assert!(matches!(
        classify_delete_failure("something/else"),
        DeleteEntityPathError::Rejected
    ));
}

#[test]
fn default_title_is_always_sluggable() {
    use time::macros::datetime;
    let title = default_title(datetime!(2026-09-16 07:15 UTC));
    assert_eq!(title, "File 2026-09-16 0715");
    assert!(
        !crate::slug::slugify(&title).is_empty(),
        "`crate::slug::slugify(&title)` should not be empty"
    );
}
