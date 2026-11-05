use time::macros::datetime;

use super::*;

const DECISION_TEMPLATE: &str = "---\nid: VD-{{NNN}}\ndate: <% tp.date.now(\"YYYY-MM-DD\") %>\n---\n\n# VD-{{NNN}}: {{Decision title}}\n\n## Context\n\n[The situation.]\n\n<!--\nFilename: memory/decisions/<id>-<slug>.md.\n-->\n";

#[test]
fn a_template_seed_drops_its_frontmatter_heading_and_authoring_comment() {
    let (seed, guidance) = seed_from_template(DECISION_TEMPLATE, datetime!(2026-10-08 09:00 UTC));
    assert_eq!(seed, "## Context\n\n[The situation.]\n");
    assert_eq!(guidance.as_deref(), Some("Filename: memory/decisions/<id>-<slug>.md."));
}

#[test]
fn a_template_without_frontmatter_or_comment_is_kept_whole() {
    let (seed, guidance) = seed_from_template("## Question\n\n[Ask it.]\n", datetime!(2026-10-08 09:00 UTC));
    assert_eq!(seed, "## Question\n\n[Ask it.]\n");
    assert_eq!(guidance, None);
}

#[test]
fn a_form_must_be_a_flat_object_of_text() {
    assert!(
        parse_form(&Bytes::from_static(
            br#"{"title": "T", "body": "B", "expected_hash": "h"}"#
        ))
        .is_some()
    );
    assert!(parse_form(&Bytes::from_static(br#"{"title": ["T"]}"#)).is_none());
    assert!(parse_form(&Bytes::from_static(b"not json")).is_none());
}

#[test]
fn a_current_value_outside_the_offered_options_is_kept_selected() {
    let shown = options([("a".to_owned(), "A".to_owned())], "legacy", true);
    let values: Vec<(&str, bool)> = shown
        .iter()
        .map(|option| (option.value.as_str(), option.selected))
        .collect();
    assert_eq!(values, [("", false), ("a", false), ("legacy", true)]);
}
