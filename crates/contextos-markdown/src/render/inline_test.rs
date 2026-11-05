use super::*;

#[test]
fn a_trailing_block_id_needs_a_space_before_it_and_a_valid_id() {
    assert_eq!(trailing_block_id("text ^ab-1 "), Some(("text", "ab-1")));
    assert_eq!(trailing_block_id("^solo"), Some(("", "solo")));
    assert_eq!(trailing_block_id("x^y"), None);
    assert_eq!(trailing_block_id("text ^bad_id"), None);
    assert_eq!(trailing_block_id("text ^"), None);
}
