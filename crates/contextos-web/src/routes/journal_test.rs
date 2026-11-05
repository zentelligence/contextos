use super::*;

#[test]
fn a_section_index_is_a_plain_decimal_number() {
    assert_eq!(section_index("0"), Some(0));
    assert_eq!(section_index("12"), Some(12));
    for refused in ["", "abc", "-1", "+1", "01", " 1", "1.0", "99999999999999999999999"] {
        assert_eq!(section_index(refused), None, "{refused:?}");
    }
}

#[test]
fn a_tab_is_labelled_with_its_heading_or_a_stand_in() {
    let entry = JournalEntry::from(EntryInput {
        text: "## Gym\n\n##\n",
        template: "",
    });
    assert_eq!(section_label(&entry, 0), "Gym");
    assert_eq!(section_label(&entry, 1), "Untitled");
    assert_eq!(section_label(&entry, 2), "");
    let whole = JournalEntry::from(EntryInput {
        text: "Just prose.",
        template: "",
    });
    assert_eq!(section_label(&whole, 0), "Entry");
}
