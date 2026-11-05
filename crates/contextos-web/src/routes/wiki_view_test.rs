use super::*;

fn folder(
    label: &str,
    relative_path: &str,
    folders: Vec<WikiFolder>,
    pages: Vec<crate::wiki::WikiPageLink>,
) -> WikiFolder {
    WikiFolder {
        label: label.to_owned(),
        relative_path: relative_path.to_owned(),
        folders,
        pages,
    }
}

fn page(label: &str, relative_path: &str) -> crate::wiki::WikiPageLink {
    crate::wiki::WikiPageLink {
        label: label.to_owned(),
        relative_path: relative_path.to_owned(),
    }
}

#[test]
fn flatten_of_an_empty_tree_is_empty() {
    let root = folder("", "", Vec::new(), Vec::new());
    assert!(
        flatten("vault", &root, "", None).is_empty(),
        "`flatten(\"vault\", &root, \"\", None)` should be empty, got {:?}",
        flatten("vault", &root, "", None)
    );
}

#[test]
fn flatten_opens_every_folder_on_the_anchor_dirs_own_path_via_the_shared_tree_walk() {
    let root = folder(
        "",
        "",
        vec![folder(
            "business",
            "business",
            vec![folder("finance", "business/finance", Vec::new(), Vec::new())],
            Vec::new(),
        )],
        Vec::new(),
    );
    let events = flatten("vault", &root, "business/finance", None);
    let open_states: Vec<(String, bool)> = events
        .iter()
        .filter_map(|event| match event {
            TreeEvent::OpenFolder(label, is_open) => Some((label.clone(), *is_open)),
            _ => None,
        })
        .collect();
    assert_eq!(
        open_states,
        vec![("business".to_owned(), true), ("finance".to_owned(), true)]
    );
}

#[test]
fn flatten_marks_exactly_the_selected_page_active_with_a_query_encoded_href() {
    let root = folder(
        "",
        "",
        vec![folder(
            "business",
            "business",
            Vec::new(),
            vec![page("a", "business/a.md"), page("b", "business/b.md")],
        )],
        Vec::new(),
    );
    let events = flatten("vault", &root, "business", Some("business/b.md"));
    let pages: Vec<&TreePage> = events
        .iter()
        .filter_map(|event| match event {
            TreeEvent::Leaf(page) => Some(page),
            _ => None,
        })
        .collect();
    assert!(!pages[0].active);
    assert!(pages[1].active);
    assert_eq!(pages[1].href, "/vault/wiki/business/b.md");
}

#[test]
fn flatten_places_a_root_level_page_before_any_folder_is_opened() {
    let root = folder(
        "",
        "",
        vec![folder("business", "business", Vec::new(), Vec::new())],
        vec![page("readme", "readme.md")],
    );
    let events = flatten("vault", &root, "", None);
    assert!(matches!(&events[0], TreeEvent::Leaf(page) if page.label == "readme"));
    assert!(matches!(&events[1], TreeEvent::OpenFolder(label, _) if label == "business"));
}
