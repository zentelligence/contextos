use super::*;

#[test]
fn a_slug_lowers_and_hyphenates() {
    assert_eq!(slug("Intro Part"), "intro-part");
    assert_eq!(slug("  What's new? (2026) "), "what-s-new-2026");
    assert_eq!(slug("Ünïcode Wörds"), "ünïcode-wörds");
    assert_eq!(slug("!!!"), "");
}

#[test]
fn anchors_are_unique_across_a_page_even_when_a_heading_looks_like_a_suffix() {
    let mut anchors = Anchors::default();
    assert_eq!(anchors.heading("Notes"), "notes");
    assert_eq!(anchors.heading("Notes"), "notes-1");
    assert_eq!(anchors.heading("Notes 1"), "notes-1-1");
    assert_eq!(anchors.heading("Notes"), "notes-2");
    assert_eq!(anchors.heading("???"), "section");
}
