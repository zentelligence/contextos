use super::*;

#[test]
fn sanitised_path_accepts_an_empty_string_as_the_root() {
    assert_eq!(sanitised_path(""), Some(String::new()));
}

#[test]
fn sanitised_path_accepts_a_real_nested_path() {
    assert_eq!(
        sanitised_path("business/finance/tax.md"),
        Some("business/finance/tax.md".to_owned())
    );
}

#[test]
fn sanitised_path_rejects_traversal() {
    assert_eq!(sanitised_path("../secrets"), None);
}

#[test]
fn as_page_path_requires_a_markdown_extension() {
    assert_eq!(as_page_path("business/finance/tax.md"), Some("business/finance/tax.md"));
    assert_eq!(as_page_path("business/finance"), None);
    assert_eq!(as_page_path("business/finance/tax.txt"), None);
}

#[test]
fn action_path_strips_a_trailing_reserved_suffix() {
    assert_eq!(
        action_path("business/finance/tax.md/edit", "/edit"),
        Some("business/finance/tax.md")
    );
    assert_eq!(action_path("business/finance", "/edit"), None);
}

#[test]
fn action_path_does_not_match_the_reserved_new_suffix_without_a_leading_folder_segment() {
    // The bare Wiki root's own `GET .../new` carries no leading folder segment at all, so
    // `action_path` (which always strips a leading `/`) never matches it - `get_dispatch`'s own
    // `path == "new"` check covers that case separately.
    assert_eq!(action_path("new", "/new"), None);
    assert_eq!(action_path("business/finance/new", "/new"), Some("business/finance"));
}

#[test]
fn new_folder_action_path_handles_the_root_and_a_nested_folder() {
    assert_eq!(new_folder_action_path("execute/new-folder"), Some(""));
    assert_eq!(
        new_folder_action_path("business/finance/execute/new-folder"),
        Some("business/finance")
    );
    assert_eq!(new_folder_action_path("business/finance"), None);
}

#[test]
fn new_folder_href_handles_the_root_and_a_nested_folder() {
    assert_eq!(new_folder_href("vault", ""), "/vault/wiki/execute/new-folder");
    assert_eq!(
        new_folder_href("vault", "business/finance"),
        "/vault/wiki/business/finance/execute/new-folder"
    );
}

#[test]
fn new_page_href_handles_the_root_and_a_nested_folder() {
    assert_eq!(new_page_href("vault", ""), "/vault/wiki/new");
    assert_eq!(
        new_page_href("vault", "business/finance"),
        "/vault/wiki/business/finance/new"
    );
}

#[test]
fn dir_href_omits_the_query_string_for_the_root() {
    assert_eq!(dir_href("vault", ""), "/vault/wiki/");
}

#[test]
fn dir_href_appends_the_path_directly_for_a_nested_folder() {
    assert_eq!(dir_href("vault", "business/finance"), "/vault/wiki/business/finance");
}

#[test]
fn page_at_finds_a_page_by_its_own_relative_path() {
    let folder = WikiFolder {
        label: "finance".to_owned(),
        relative_path: "business/finance".to_owned(),
        folders: Vec::new(),
        pages: vec![wiki::WikiPageLink {
            label: "tax".to_owned(),
            relative_path: "business/finance/tax.md".to_owned(),
        }],
    };
    assert!(page_at(&folder, "business/finance/tax.md").is_some());
    assert!(page_at(&folder, "business/finance/other.md").is_none());
}

fn folder_with_page() -> WikiFolder {
    WikiFolder {
        label: String::new(),
        relative_path: String::new(),
        folders: vec![WikiFolder {
            label: "business".to_owned(),
            relative_path: "business".to_owned(),
            folders: Vec::new(),
            pages: vec![wiki::WikiPageLink {
                label: "tax".to_owned(),
                relative_path: "business/tax.md".to_owned(),
            }],
        }],
        pages: Vec::new(),
    }
}

#[test]
fn resolve_target_finds_a_real_folder() {
    let root = folder_with_page();
    let found = resolve_target(&root, "business");
    assert!(matches!(found, Some((TargetKind::Folder, label)) if label == "business"));
}

#[test]
fn resolve_target_finds_a_real_page() {
    let root = folder_with_page();
    let found = resolve_target(&root, "business/tax.md");
    assert!(matches!(found, Some((TargetKind::Page, label)) if label == "tax"));
}

#[test]
fn resolve_target_is_none_for_a_path_naming_neither() {
    let root = folder_with_page();
    assert!(resolve_target(&root, "business/missing.md").is_none());
    assert!(resolve_target(&root, "nonexistent").is_none());
}

#[test]
fn wiki_layout_class_ignores_the_metadata_side_when_no_page_is_selected() {
    assert_eq!(wiki_layout_class(true, false, false), "");
    assert_eq!(wiki_layout_class(false, true, false), "notes-layout-panel-right");
}

#[test]
fn wiki_layout_class_is_the_plain_base_grid_when_both_panels_share_the_left() {
    assert_eq!(wiki_layout_class(true, true, true), "");
}

#[test]
fn wiki_layout_class_mirrors_the_base_grid_when_both_panels_share_the_right() {
    assert_eq!(wiki_layout_class(false, false, true), "notes-layout-panel-right");
}

#[test]
fn wiki_layout_class_dedicates_a_right_column_to_the_metadata_panel_when_the_tree_stays_left() {
    assert_eq!(wiki_layout_class(true, false, true), "notes-layout-metadata-right");
}

#[test]
fn wiki_layout_class_dedicates_a_left_column_to_the_metadata_panel_when_the_tree_moves_right() {
    assert_eq!(
        wiki_layout_class(false, true, true),
        "notes-layout-metadata-left-panel-right"
    );
}
