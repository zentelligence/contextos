use serde_json::json;

use super::*;
use crate::BaseSidecar;

/// A fixture is always a JSON object literal; anything else yields an empty map, which the
/// calling test's own assertions then fail on.
fn frontmatter(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// The declared fields of a tasks-style sidecar.
fn fields() -> Result<Vec<LinkField>, crate::BaseSidecarError> {
    let sidecar = BaseSidecar::try_from(&frontmatter(json!({
        "key_format": "FILEPATH",
        "last_id": 0,
        "last_create": "t0",
        "last_update": "t0",
        "link_fields": {
            "related": {"inverse": "related"},
            "parent": {"single": true},
        },
    })))?;
    Ok(sidecar.link_fields().to_vec())
}

fn field(name: &str) -> Result<LinkField, Box<dyn std::error::Error>> {
    fields()?
        .into_iter()
        .find(|candidate| candidate.name() == name)
        .ok_or_else(|| format!("no {name} field").into())
}

fn link(target: &str, display: &str) -> Result<WikiLink, LinkError> {
    WikiLink::try_from(WikiLinkInput { target, display })
}

/// Treats two entries as the same link when they name the same target text.
fn same_as(target: &'static str) -> impl Fn(&str) -> bool {
    move |entry| LinkTarget::try_from(entry).is_ok_and(|found| found.as_str() == target)
}

// ---------------------------------------------------------------------------
// Wikilink text
// ---------------------------------------------------------------------------

#[test]
fn a_link_renders_as_a_wikilink_with_display_text() -> Result<(), LinkError> {
    assert_eq!(link("alpha", "Alpha")?.as_ref(), "[[alpha|Alpha]]");
    assert_eq!(
        link("memory/tasks/alpha", "Alpha")?.as_ref(),
        "[[memory/tasks/alpha|Alpha]]"
    );
    Ok(())
}

#[test]
fn text_that_would_break_wikilink_syntax_is_refused() {
    for (target, display) in [
        ("a]]b", "x"),
        ("a[[b", "x"),
        ("a|b", "x"),
        ("a#b", "x"),
        ("a^b", "x"),
        ("a\nb", "x"),
        ("", "x"),
        ("a", "x]]"),
        ("a", "x|y"),
        ("a", "x\ny"),
        ("a", ""),
    ] {
        let result = link(target, display);

        assert!(
            matches!(result, Err(LinkError::UnlinkableText { .. })),
            "{target:?} / {display:?} must be refused, got {result:?}"
        );
    }
}

#[test]
fn the_target_of_a_stored_link_is_read_back() {
    for (entry, expected) in [
        ("[[alpha|Alpha]]", "alpha"),
        ("[[memory/tasks/alpha|Alpha]]", "memory/tasks/alpha"),
        ("  [[alpha]]  ", "alpha"),
        ("[[alpha#heading|A]]", "alpha"),
    ] {
        let target = LinkTarget::try_from(entry);

        assert_eq!(target.as_ref().map(LinkTarget::as_str), Ok(expected), "{entry:?}");
    }
}

#[test]
fn a_value_that_is_not_exactly_one_plain_wikilink_has_no_target() {
    for entry in [
        "alpha",
        "",
        "[[a]] and [[b]]",
        "![[a]]",
        "text [[a]]",
        "[[a]] trailing",
        "[a](b)",
    ] {
        assert!(LinkTarget::try_from(entry).is_err(), "{entry:?}");
    }
}

// ---------------------------------------------------------------------------
// Reading a field
// ---------------------------------------------------------------------------

#[test]
fn a_missing_null_or_empty_field_has_no_entries() -> Result<(), Box<dyn std::error::Error>> {
    let related = field("related")?;

    for document in [
        json!({}),
        json!({"related": null}),
        json!({"related": []}),
        json!({"related": ""}),
    ] {
        assert_eq!(
            related.entries(&frontmatter(document.clone()))?,
            Vec::<String>::new(),
            "{document}"
        );
    }
    Ok(())
}

#[test]
fn a_scalar_or_list_field_reads_as_its_entries() -> Result<(), Box<dyn std::error::Error>> {
    let related = field("related")?;

    assert_eq!(
        related.entries(&frontmatter(json!({"related": "[[a]]"})))?,
        vec!["[[a]]".to_owned()]
    );
    assert_eq!(
        related.entries(&frontmatter(json!({"related": ["[[a]]", "[[b]]"]})))?,
        vec!["[[a]]".to_owned(), "[[b]]".to_owned()]
    );
    Ok(())
}

#[test]
fn a_field_holding_anything_else_is_malformed() -> Result<(), Box<dyn std::error::Error>> {
    let related = field("related")?;

    for value in [
        json!(5),
        json!(true),
        json!({"a": 1}),
        json!(["[[a]]", 3]),
        json!([["[[a]]"]]),
    ] {
        let result = related.entries(&frontmatter(json!({ "related": value.clone() })));

        assert!(
            matches!(result, Err(LinkError::MalformedField { .. })),
            "{value} must be malformed, got {result:?}"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Adding
// ---------------------------------------------------------------------------

#[test]
fn adding_to_a_missing_list_field_creates_the_list() -> Result<(), Box<dyn std::error::Error>> {
    let edit = field("related")?.with_link(&frontmatter(json!({})), &link("one", "One")?, &same_as("one"))?;

    assert_eq!(edit.outcome, LinkOutcome::Added);
    assert_eq!(edit.value, Some(json!(["[[one|One]]"])));
    Ok(())
}

#[test]
fn adding_to_a_populated_list_appends_and_keeps_the_existing_entries_as_written()
-> Result<(), Box<dyn std::error::Error>> {
    let document = frontmatter(json!({"related": ["[[memory/tasks/nine|Nine]]"]}));

    let edit = field("related")?.with_link(&document, &link("one", "One")?, &same_as("one"))?;

    assert_eq!(edit.outcome, LinkOutcome::Added);
    assert_eq!(edit.value, Some(json!(["[[memory/tasks/nine|Nine]]", "[[one|One]]"])));
    Ok(())
}

#[test]
fn adding_a_scalar_valued_list_field_upgrades_it_to_a_list() -> Result<(), Box<dyn std::error::Error>> {
    let document = frontmatter(json!({"related": "[[nine|Nine]]"}));

    let edit = field("related")?.with_link(&document, &link("one", "One")?, &same_as("one"))?;

    assert_eq!(edit.value, Some(json!(["[[nine|Nine]]", "[[one|One]]"])));
    Ok(())
}

#[test]
fn adding_a_link_already_present_changes_nothing_however_it_was_written() -> Result<(), Box<dyn std::error::Error>> {
    let document = frontmatter(json!({"related": ["[[one|the first]]"]}));

    let edit = field("related")?.with_link(&document, &link("one", "One")?, &same_as("one"))?;

    assert_eq!(edit.outcome, LinkOutcome::AlreadyPresent);
    assert_eq!(edit.value, None);
    Ok(())
}

#[test]
fn adding_to_a_single_valued_field_sets_it_when_empty_and_refuses_a_different_value()
-> Result<(), Box<dyn std::error::Error>> {
    let parent = field("parent")?;

    let set = parent.with_link(&frontmatter(json!({})), &link("one", "One")?, &same_as("one"))?;
    assert_eq!(set.outcome, LinkOutcome::Added);
    assert_eq!(set.value, Some(json!("[[one|One]]")));

    let blank = parent.with_link(
        &frontmatter(json!({"parent": ""})),
        &link("one", "One")?,
        &same_as("one"),
    )?;
    assert_eq!(blank.outcome, LinkOutcome::Added);

    let same = parent.with_link(
        &frontmatter(json!({"parent": "[[one|One]]"})),
        &link("one", "One")?,
        &same_as("one"),
    )?;
    assert_eq!(same.outcome, LinkOutcome::AlreadyPresent);
    assert_eq!(same.value, None);

    let different = parent.with_link(
        &frontmatter(json!({"parent": "[[nine|Nine]]"})),
        &link("one", "One")?,
        &same_as("one"),
    );
    assert!(matches!(
        different,
        Err(LinkError::SingleValued { field, existing }) if field == "parent" && existing == "[[nine|Nine]]"
    ));
    Ok(())
}

// ---------------------------------------------------------------------------
// Removing
// ---------------------------------------------------------------------------

#[test]
fn removing_a_link_keeps_the_others_and_leaves_an_empty_list_rather_than_deleting_the_field()
-> Result<(), Box<dyn std::error::Error>> {
    let related = field("related")?;
    let document = frontmatter(json!({"related": ["[[one|One]]", "[[two|Two]]"]}));

    let removed = related.without_links(&document, &same_as("one"))?;
    assert_eq!(removed.outcome, LinkOutcome::Removed);
    assert_eq!(removed.value, Some(json!(["[[two|Two]]"])));

    let last = related.without_links(&frontmatter(json!({"related": ["[[one|One]]"]})), &same_as("one"))?;
    assert_eq!(last.value, Some(json!([])));
    Ok(())
}

#[test]
fn removing_a_link_that_is_not_there_is_not_an_error() -> Result<(), Box<dyn std::error::Error>> {
    let related = field("related")?;

    for document in [json!({}), json!({"related": []}), json!({"related": ["[[two|Two]]"]})] {
        let edit = related.without_links(&frontmatter(document.clone()), &same_as("one"))?;

        assert_eq!(edit.outcome, LinkOutcome::Absent, "{document}");
        assert_eq!(edit.value, None, "{document}");
    }
    Ok(())
}

#[test]
fn removing_from_a_single_valued_field_deletes_it() -> Result<(), Box<dyn std::error::Error>> {
    let parent = field("parent")?;

    let removed = parent.without_links(&frontmatter(json!({"parent": "[[one|One]]"})), &same_as("one"))?;
    assert_eq!(removed.outcome, LinkOutcome::Removed);
    assert_eq!(removed.value, Some(Value::Null));

    let other = parent.without_links(&frontmatter(json!({"parent": "[[nine|Nine]]"})), &same_as("one"))?;
    assert_eq!(other.outcome, LinkOutcome::Absent);
    Ok(())
}

#[test]
fn every_link_error_has_a_stable_link_or_format_code_and_a_remediation() {
    let errors = [
        (
            LinkError::UndeclaredField {
                field: "x".to_owned(),
                declared: vec![],
            },
            "link/undeclared-field",
        ),
        (LinkError::NoLinkFields, "link/no-link-fields"),
        (LinkError::AmbiguousBase { bases: vec![] }, "link/ambiguous-base"),
        (
            LinkError::TargetNotFound { target: "x".to_owned() },
            "link/target-not-found",
        ),
        (
            LinkError::TargetAmbiguous {
                target: "x".to_owned(),
                matches: vec![],
            },
            "link/target-ambiguous",
        ),
        (LinkError::SelfReference, "link/self-reference"),
        (
            LinkError::SingleValued {
                field: "x".to_owned(),
                existing: "y".to_owned(),
            },
            "link/single-valued",
        ),
        (
            LinkError::MalformedField { field: "x".to_owned() },
            "link/malformed-field",
        ),
        (
            LinkError::UnlinkableText { text: "x".to_owned() },
            "link/unlinkable-text",
        ),
    ];
    for (error, code) in errors {
        assert_eq!(error.code(), code);
        assert!(!error.remediation().is_empty(), "{code}");
        assert!(!error.to_string().is_empty(), "{code}");
    }
}
