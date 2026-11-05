use super::*;

#[test]
fn a_base_path_becomes_a_plain_wikilink_to_the_base_file() {
    let link = BaseLink::try_from(BaseLinkInput {
        path: "memory/tasks.base",
    });

    assert_eq!(link.as_ref().map(AsRef::as_ref), Ok("[[memory/tasks.base]]"));
}

#[test]
fn only_a_base_file_can_be_linked_as_a_base() {
    for path in ["memory/tasks.md", "memory/tasks", "", "memory/.base"] {
        assert!(BaseLink::try_from(BaseLinkInput { path }).is_err(), "{path:?}");
    }
}

#[test]
fn a_path_that_cannot_be_written_in_a_wikilink_is_refused() {
    for path in ["a[b].base", "a|b.base", "a#b.base", "a^b.base", "a\nb.base"] {
        assert!(BaseLink::try_from(BaseLinkInput { path }).is_err(), "{path:?}");
    }
}

#[test]
fn a_stored_value_reads_back_as_the_vault_path_of_the_base() {
    for stored in [
        "[[memory/tasks.base]]",
        "  [[memory/tasks.base]]  ",
        "[[memory/tasks.base|Tasks]]",
        "[[memory/tasks.base#Open]]",
    ] {
        assert_eq!(
            BaseLinkTarget::try_from(stored).map(|target| target.as_str().to_owned()),
            Ok("memory/tasks.base".to_owned()),
            "{stored:?}"
        );
    }
}

#[test]
fn a_stored_value_that_is_not_one_link_to_a_base_file_is_refused() {
    for stored in [
        "memory/tasks.base",
        "[[memory/tasks]]",
        "[[memory/tasks.md]]",
        "[[a.base]] [[b.base]]",
        "![[memory/tasks.base]]",
        "",
        "[[]]",
    ] {
        assert!(BaseLinkTarget::try_from(stored).is_err(), "{stored:?}");
    }
}

#[test]
fn a_leading_slash_is_a_vault_root_marker_not_part_of_the_path() {
    assert_eq!(
        BaseLinkTarget::try_from("[[/memory/tasks.base]]").map(|target| target.as_str().to_owned()),
        Ok("memory/tasks.base".to_owned())
    );
}
