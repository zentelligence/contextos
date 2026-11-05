use super::*;

fn wiki(text: &str) -> LinkTarget {
    LinkTarget::from(LinkDestination {
        text,
        percent_encoded: false,
    })
}

fn standard(text: &str) -> LinkTarget {
    LinkTarget::from(LinkDestination {
        text,
        percent_encoded: true,
    })
}

fn vault(path: &str, fragment: Option<Fragment>) -> LinkTarget {
    LinkTarget::Vault {
        path: path.to_owned(),
        fragment,
    }
}

#[test]
fn a_rooted_destination_is_a_vault_file() {
    assert_eq!(wiki("/wiki/Foo.md"), vault("wiki/Foo.md", None));
}

#[test]
fn a_heading_or_block_fragment_is_split_off_the_path() {
    assert_eq!(
        wiki("/wiki/Foo.md#Intro part"),
        vault("wiki/Foo.md", Some(Fragment::Heading("Intro part".to_owned())))
    );
    assert_eq!(
        wiki("/wiki/Foo.md#^b-1"),
        vault("wiki/Foo.md", Some(Fragment::Block("b-1".to_owned())))
    );
}

#[test]
fn a_standard_destination_is_percent_decoded_and_a_wikilink_is_not() {
    assert_eq!(
        standard("/a%20b/100%25.md#My%20Heading"),
        vault("a b/100%.md", Some(Fragment::Heading("My Heading".to_owned())))
    );
    assert_eq!(wiki("/a/100%20.md"), vault("a/100%20.md", None));
}

#[test]
fn the_markers_are_recognised_before_anything_else_and_keep_their_text_whole() {
    assert_eq!(wiki("/:!typo#x?y"), LinkTarget::NotFound("typo#x?y".to_owned()));
    assert_eq!(
        standard("/:?dup%20name"),
        LinkTarget::Ambiguous("dup%20name".to_owned())
    );
}

#[test]
fn web_and_mail_destinations_are_external() {
    for text in [
        "https://example.org/a",
        "HTTP://x",
        "mailto:a@b.c",
        "//cdn.example.org/x",
    ] {
        assert_eq!(standard(text), LinkTarget::External(text.to_owned()), "{text}");
    }
}

#[test]
fn any_other_scheme_is_unresolved() {
    for text in [
        "javascript:alert(1)",
        "JaVaScRiPt:x",
        "data:text/html,x",
        "obsidian://open",
        "harbour://a.md",
    ] {
        assert_eq!(standard(text), LinkTarget::Unresolved(text.to_owned()), "{text}");
    }
}

#[test]
fn an_anchor_names_a_place_on_the_same_page() {
    assert_eq!(
        standard("#Some%20Heading"),
        LinkTarget::Anchor(Fragment::Heading("Some Heading".to_owned()))
    );
    assert_eq!(wiki("#^blk"), LinkTarget::Anchor(Fragment::Block("blk".to_owned())));
}

#[test]
fn a_relative_or_empty_destination_is_unresolved() {
    assert_eq!(standard("Foo.md"), LinkTarget::Unresolved("Foo.md".to_owned()));
    assert_eq!(standard(""), LinkTarget::Unresolved(String::new()));
    assert_eq!(wiki("/"), LinkTarget::Unresolved("/".to_owned()));
}

#[test]
fn leading_and_trailing_whitespace_is_ignored() {
    assert_eq!(wiki("  /wiki/Foo.md "), vault("wiki/Foo.md", None));
}

#[test]
fn a_fragment_names_the_element_id_its_page_gives_it() {
    assert_eq!(Fragment::Heading("Some Part".to_owned()).anchor(), "some-part");
    assert_eq!(Fragment::Block("b-1".to_owned()).anchor(), "^b-1");
}
