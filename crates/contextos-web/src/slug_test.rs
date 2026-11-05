use super::*;

#[test]
fn slugify_lowercases_and_hyphenates_non_alphanumeric_runs() {
    assert_eq!(slugify("Cold Traffic Specificity"), "cold-traffic-specificity");
}

#[test]
fn slugify_collapses_repeated_separators_and_trims_a_trailing_hyphen() {
    assert_eq!(slugify("  Sales -- Call!!  "), "sales-call");
}

#[test]
fn slugify_of_an_entirely_unsluggable_title_is_empty() {
    assert_eq!(slugify("!!!"), "");
}
