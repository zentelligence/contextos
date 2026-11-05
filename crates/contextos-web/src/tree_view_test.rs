use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
struct TestLeaf {
    label: String,
    relative_path: String,
}

struct TestFolder {
    label: String,
    relative_path: String,
    folders: Vec<TestFolder>,
    leaves: Vec<TestLeaf>,
}

impl TreeFolder for TestFolder {
    type Leaf = TestLeaf;

    fn relative_path(&self) -> &str {
        &self.relative_path
    }

    fn label(&self) -> &str {
        &self.label
    }

    fn subfolders(&self) -> &[Self] {
        &self.folders
    }

    fn leaves(&self) -> &[Self::Leaf] {
        &self.leaves
    }
}

fn folder(label: &str, relative_path: &str, folders: Vec<TestFolder>, leaves: Vec<TestLeaf>) -> TestFolder {
    TestFolder {
        label: label.to_owned(),
        relative_path: relative_path.to_owned(),
        folders,
        leaves,
    }
}

fn leaf(label: &str, relative_path: &str) -> TestLeaf {
    TestLeaf {
        label: label.to_owned(),
        relative_path: relative_path.to_owned(),
    }
}

fn identity_view(leaf: &TestLeaf) -> TestLeaf {
    leaf.clone()
}

#[test]
fn flatten_of_an_empty_tree_is_empty() {
    let root = folder("", "", Vec::new(), Vec::new());
    assert!(
        flatten(&root, "", &mut identity_view).is_empty(),
        "`flatten(&root, \"\", &mut identity_view)` should be empty, got {:?}",
        flatten(&root, "", &mut identity_view)
    );
}

#[test]
fn flatten_places_a_root_level_leaf_before_any_folder_is_opened() {
    let root = folder(
        "",
        "",
        vec![folder("business", "business", Vec::new(), Vec::new())],
        vec![leaf("readme", "readme.md")],
    );
    let events = flatten(&root, "", &mut identity_view);
    assert!(matches!(&events[0], TreeEvent::Leaf(leaf) if leaf.label == "readme"));
    assert!(matches!(&events[1], TreeEvent::OpenFolder(label, _) if label == "business"));
}

#[test]
fn flatten_pairs_every_open_folder_with_exactly_one_later_close_in_order() {
    let root = folder(
        "",
        "",
        vec![folder(
            "business",
            "business",
            vec![folder(
                "finance",
                "business/finance",
                Vec::new(),
                vec![leaf("tax", "business/finance/tax.md")],
            )],
            Vec::new(),
        )],
        Vec::new(),
    );
    let events = flatten(&root, "business/finance", &mut identity_view);
    let opens = events
        .iter()
        .filter(|event| matches!(event, TreeEvent::OpenFolder(..)))
        .count();
    let closes = events
        .iter()
        .filter(|event| matches!(event, TreeEvent::CloseFolder))
        .count();
    assert_eq!(opens, 2, "{events:?}");
    assert_eq!(closes, 2, "{events:?}");

    let mut depth = 0i32;
    for event in &events {
        match event {
            TreeEvent::OpenFolder(..) => depth += 1,
            TreeEvent::CloseFolder => {
                depth -= 1;
                assert!(depth >= 0, "closed a folder that was never opened: {events:?}");
            }
            TreeEvent::Leaf(_) => {}
        }
    }
    assert_eq!(depth, 0, "every opened folder must be closed: {events:?}");
}

#[test]
fn flatten_opens_every_folder_on_the_anchor_dirs_own_path_and_no_others() {
    let root = folder(
        "",
        "",
        vec![folder(
            "business",
            "business",
            vec![
                folder("finance", "business/finance", Vec::new(), Vec::new()),
                folder("marketing", "business/marketing", Vec::new(), Vec::new()),
            ],
            Vec::new(),
        )],
        Vec::new(),
    );
    let events = flatten(&root, "business/finance", &mut identity_view);
    let open_states: Vec<(String, bool)> = events
        .iter()
        .filter_map(|event| match event {
            TreeEvent::OpenFolder(label, is_open) => Some((label.clone(), *is_open)),
            _ => None,
        })
        .collect();
    assert_eq!(
        open_states,
        vec![
            ("business".to_owned(), true),
            ("finance".to_owned(), true),
            ("marketing".to_owned(), false),
        ]
    );
}

#[test]
fn flatten_opens_the_currently_browsed_folder_itself_when_nothing_is_selected() {
    let root = folder(
        "",
        "",
        vec![folder("business", "business", Vec::new(), Vec::new())],
        Vec::new(),
    );
    let events = flatten(&root, "business", &mut identity_view);
    assert!(matches!(&events[0], TreeEvent::OpenFolder(label, true) if label == "business"));
}

#[test]
fn flatten_converts_every_real_leaf_through_the_caller_supplied_view() {
    let root = folder("", "", Vec::new(), vec![leaf("a", "a.md"), leaf("b", "b.md")]);
    let mut seen = Vec::new();
    let events = flatten(&root, "", &mut |item: &TestLeaf| {
        seen.push(item.relative_path.clone());
        item.label.to_ascii_uppercase()
    });
    assert_eq!(seen, vec!["a.md".to_owned(), "b.md".to_owned()]);
    let labels: Vec<&String> = events
        .iter()
        .filter_map(|event| match event {
            TreeEvent::Leaf(label) => Some(label),
            _ => None,
        })
        .collect();
    assert_eq!(labels, vec!["A", "B"]);
}
