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
fn build_folder_assembles_relative_paths_at_every_depth() {
    let tree = dir(
        "wiki",
        vec![dir(
            "business",
            vec![dir("client-acquisition", vec![file("cold-traffic.md")])],
        )],
    );
    let root = build_folder(&tree, "");
    assert_eq!(root.relative_path, "");
    let business = &root.folders[0];
    assert_eq!(business.relative_path, "business");
    let topic = &business.folders[0];
    assert_eq!(topic.relative_path, "business/client-acquisition");
    let page = &topic.pages[0];
    assert_eq!(page.relative_path, "business/client-acquisition/cold-traffic.md");
    assert_eq!(page.label, "cold-traffic");
}

#[test]
fn build_folder_sorts_folders_and_pages_alphabetically() {
    let tree = dir(
        "wiki",
        vec![
            file("zebra.md"),
            file("apple.md"),
            dir("marketing", vec![]),
            dir("finance", vec![]),
        ],
    );
    let root = build_folder(&tree, "");
    let folder_labels: Vec<&str> = root.folders.iter().map(|f| f.label.as_str()).collect();
    assert_eq!(folder_labels, vec!["finance", "marketing"]);
    let page_labels: Vec<&str> = root.pages.iter().map(|p| p.label.as_str()).collect();
    assert_eq!(page_labels, vec!["apple", "zebra"]);
}

#[test]
fn find_folder_walks_down_matching_segments() -> Result<(), Box<dyn std::error::Error>> {
    let tree = dir("wiki", vec![dir("business", vec![dir("finance", vec![])])]);
    let root = build_folder(&tree, "");
    let found = find_folder(&root, "business/finance").ok_or("folder should be found")?;
    assert_eq!(found.relative_path, "business/finance");
    Ok(())
}

#[test]
fn find_folder_returns_none_for_a_missing_segment() {
    let tree = dir("wiki", vec![dir("business", vec![])]);
    let root = build_folder(&tree, "");
    assert!(find_folder(&root, "business/nonexistent").is_none());
}

#[test]
fn find_folder_returns_root_for_an_empty_relative_dir() -> Result<(), Box<dyn std::error::Error>> {
    let tree = dir("wiki", vec![]);
    let root = build_folder(&tree, "");
    let found = find_folder(&root, "").ok_or("root should be found")?;
    assert_eq!(found.relative_path, "");
    Ok(())
}

#[test]
fn depth_of_counts_non_empty_segments() {
    assert_eq!(depth_of(""), 0);
    assert_eq!(depth_of("business"), 1);
    assert_eq!(depth_of("business/finance"), 2);
    assert_eq!(depth_of("business/finance/tax"), 3);
}

#[test]
fn depth_of_ignores_leading_and_trailing_slashes() {
    assert_eq!(depth_of("/business/finance/"), 2);
}

#[test]
fn filename_without_extension_strips_the_extension_and_every_folder_segment() {
    assert_eq!(
        filename_without_extension("business/finance/payment-terms.md"),
        "payment-terms"
    );
    assert_eq!(filename_without_extension("payment-terms.md"), "payment-terms");
}

#[test]
fn load_wiki_page_error_from_read_wiki_page_error_preserves_not_found() {
    assert!(matches!(
        LoadWikiPageError::from(ReadWikiPageError::NotFound),
        LoadWikiPageError::NotFound
    ));
}

#[test]
fn classify_delete_failure_maps_known_codes_and_defaults_unknown_ones_to_rejected() {
    assert!(matches!(
        classify_delete_failure("path/not-found"),
        DeleteWikiPathError::NotFound
    ));
    assert!(matches!(
        classify_delete_failure("io/directory-not-empty"),
        DeleteWikiPathError::NotEmpty
    ));
    assert!(matches!(
        classify_delete_failure("something/else"),
        DeleteWikiPathError::Rejected
    ));
}
