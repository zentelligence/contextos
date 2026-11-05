use super::*;

fn file_link(label: &str, relative_path: &str) -> ProfileFileLink {
    ProfileFileLink {
        label: label.to_owned(),
        relative_path: relative_path.to_owned(),
    }
}

#[test]
fn file_links_are_unaffected_by_construction_order() {
    let a = file_link("Who Am I", "who-am-i.md");
    let b = file_link("Who Am I", "who-am-i.md");
    assert_eq!(a, b);
}
