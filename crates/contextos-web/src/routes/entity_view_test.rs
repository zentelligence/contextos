use super::*;

fn folder(label: &str, relative_path: &str, folders: Vec<EntityFolder>, files: Vec<EntityFileLink>) -> EntityFolder {
    EntityFolder {
        label: label.to_owned(),
        relative_path: relative_path.to_owned(),
        folders,
        files,
    }
}

fn file(label: &str, relative_path: &str, is_markdown: bool) -> EntityFileLink {
    EntityFileLink {
        label: label.to_owned(),
        relative_path: relative_path.to_owned(),
        is_markdown,
    }
}

#[test]
fn flatten_of_an_empty_tree_is_empty() {
    let root = folder("", "", Vec::new(), Vec::new());
    assert!(
        flatten("vault", "zenith", &root, "", None).is_empty(),
        "`flatten(\"vault\", \"zenith\", &root, \"\", None)` should be empty, got {:?}",
        flatten("vault", "zenith", &root, "", None)
    );
}

#[test]
fn flatten_opens_every_folder_on_the_anchor_dirs_own_path() {
    let root = folder(
        "",
        "",
        vec![folder(
            "finance",
            "finance",
            vec![folder("tax", "finance/tax", Vec::new(), Vec::new())],
            Vec::new(),
        )],
        Vec::new(),
    );
    let events = flatten("vault", "zenith", &root, "finance/tax", None);
    let open_states: Vec<(String, bool)> = events
        .iter()
        .filter_map(|event| match event {
            TreeEvent::OpenFolder(label, is_open) => Some((label.clone(), *is_open)),
            _ => None,
        })
        .collect();
    assert_eq!(
        open_states,
        vec![("finance".to_owned(), true), ("tax".to_owned(), true)]
    );
}

#[test]
fn flatten_gives_a_markdown_file_its_own_read_view_href() {
    let root = folder(
        "",
        "",
        vec![folder(
            "finance",
            "finance",
            Vec::new(),
            vec![file("overview", "finance/overview.md", true)],
        )],
        Vec::new(),
    );
    let events = flatten("vault", "zenith", &root, "finance", Some("finance/overview.md"));
    let files: Vec<&TreeFile> = events
        .iter()
        .filter_map(|event| match event {
            TreeEvent::Leaf(file) => Some(file),
            _ => None,
        })
        .collect();
    assert_eq!(files.len(), 1);
    assert!(files[0].active);
    assert!(files[0].is_markdown);
    assert_eq!(files[0].href, "/vault/entity/zenith/finance/overview.md");
}

#[test]
fn flatten_gives_a_non_markdown_file_the_general_vault_content_route_and_never_marks_it_active() {
    let root = folder(
        "",
        "",
        vec![folder(
            "finance",
            "finance",
            Vec::new(),
            vec![file("export", "finance/export.csv", false)],
        )],
        Vec::new(),
    );
    let events = flatten("vault", "zenith", &root, "finance", Some("finance/export.csv"));
    let files: Vec<&TreeFile> = events
        .iter()
        .filter_map(|event| match event {
            TreeEvent::Leaf(file) => Some(file),
            _ => None,
        })
        .collect();
    assert_eq!(files.len(), 1);
    assert!(!files[0].is_markdown);
    assert_eq!(files[0].href, "/vault/zenith/finance/export.csv");
}

#[test]
fn flatten_marks_a_file_active_only_when_its_own_relative_path_matches_the_selection() {
    let root = folder(
        "",
        "",
        Vec::new(),
        vec![file("a", "a.md", true), file("b", "b.md", true)],
    );
    let events = flatten("vault", "zenith", &root, "", Some("b.md"));
    let files: Vec<&TreeFile> = events
        .iter()
        .filter_map(|event| match event {
            TreeEvent::Leaf(file) => Some(file),
            _ => None,
        })
        .collect();
    assert!(!files[0].active);
    assert!(files[1].active);
}
