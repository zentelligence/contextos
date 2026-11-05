use super::*;

#[test]
fn list_markers_are_recognised_for_bullets_and_numbers_only_when_followed_by_white_space() {
    assert!(starts_list_item("- item"));
    assert!(starts_list_item("12. item"));
    assert!(starts_list_item("3) item"));
    assert!(starts_list_item("*"));
    assert!(!starts_list_item("---"));
    assert!(!starts_list_item("-item"));
    assert!(!starts_list_item("1.5 items"));
}

#[test]
fn a_tab_indents_to_the_next_multiple_of_four() {
    assert_eq!(indentation("\tcode"), 4);
    assert_eq!(indentation("  \tcode"), 4);
    assert_eq!(indentation("   x"), 3);
}
