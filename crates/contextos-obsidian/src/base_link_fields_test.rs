use serde_json::json;

use super::*;
use crate::{BaseSidecar, BaseSidecarPatch};

/// A fixture is always a JSON object literal; anything else yields an empty map, which the
/// calling test's own assertions then fail on.
fn frontmatter(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

fn parse(link_fields: Value) -> Result<Vec<LinkField>, BaseSidecarError> {
    let mut document = Map::new();
    document.insert("link_fields".to_owned(), link_fields);
    link_fields_from(&document)
}

fn summary(fields: &[LinkField]) -> Vec<(String, bool, Option<String>)> {
    fields
        .iter()
        .map(|field| {
            (
                field.name().to_owned(),
                field.single(),
                field.inverse().map(str::to_owned),
            )
        })
        .collect()
}

#[test]
fn a_sidecar_declaring_no_link_fields_has_none() -> Result<(), BaseSidecarError> {
    assert!(
        link_fields_from(&frontmatter(json!({"key_format": "FILEPATH"})))?.is_empty(),
        "`link_fields_from(&frontmatter(json!({{\"key_format\": \"FILEPATH\"}})))?` should be empty, got {:?}",
        link_fields_from(&frontmatter(json!({"key_format": "FILEPATH"})))?
    );
    assert!(
        parse(json!({}))?.is_empty(),
        "`parse(json!({{}}))?` should be empty, got {:?}",
        parse(json!({}))?
    );
    Ok(())
}

#[test]
fn link_fields_are_read_in_declaration_order_with_arity_and_inverse() -> Result<(), BaseSidecarError> {
    let fields = parse(json!({
        "depends_on": {"inverse": "blocks"},
        "blocks": {"inverse": "depends_on"},
        "blocked_by": {},
        "related": {"inverse": "related"},
        "parent": {"single": true},
    }))?;

    assert_eq!(
        summary(&fields),
        vec![
            ("depends_on".to_owned(), false, Some("blocks".to_owned())),
            ("blocks".to_owned(), false, Some("depends_on".to_owned())),
            ("blocked_by".to_owned(), false, None),
            ("related".to_owned(), false, Some("related".to_owned())),
            ("parent".to_owned(), true, None),
        ]
    );
    Ok(())
}

#[test]
fn a_single_valued_field_may_have_a_list_valued_inverse() -> Result<(), BaseSidecarError> {
    let fields = parse(json!({
        "parent": {"single": true, "inverse": "children"},
        "children": {"inverse": "parent"},
    }))?;

    assert_eq!(fields.len(), 2);
    Ok(())
}

#[test]
fn a_field_name_that_is_not_a_plain_identifier_is_rejected() {
    for name in ["", "1st", "has space", "a-b", "a/b", "..", "\u{e9}", &"x".repeat(65)] {
        let result = parse(json!({ name: {} }));

        assert!(
            matches!(result, Err(BaseSidecarError::InvalidLinkField { .. })),
            "{name:?} must be rejected, got {result:?}"
        );
    }
}

#[test]
fn field_names_differing_only_by_case_are_rejected() {
    let result = parse(json!({"related": {}, "Related": {}}));

    assert!(matches!(result, Err(BaseSidecarError::DuplicateLinkField { .. })));
}

#[test]
fn an_inverse_that_is_not_declared_is_rejected() {
    let result = parse(json!({"blocks": {"inverse": "depends_on"}}));

    assert!(matches!(
        result,
        Err(BaseSidecarError::UndeclaredLinkInverse { field, inverse }) if field == "blocks" && inverse == "depends_on"
    ));
}

#[test]
fn an_inverse_that_does_not_point_back_is_rejected() {
    let result = parse(json!({
        "depends_on": {"inverse": "blocks"},
        "blocks": {},
    }));

    assert!(matches!(result, Err(BaseSidecarError::AsymmetricLinkInverse { .. })));

    let result = parse(json!({
        "depends_on": {"inverse": "blocks"},
        "blocks": {"inverse": "related"},
        "related": {"inverse": "blocks"},
    }));

    assert!(matches!(result, Err(BaseSidecarError::AsymmetricLinkInverse { .. })));
}

#[test]
fn a_malformed_declaration_is_rejected_as_the_wrong_type_or_an_unknown_option() {
    for (value, expect_wrong_type) in [
        (json!(["related"]), true),
        (json!("related"), true),
        (json!({"related": "list"}), true),
        (json!({"related": {"single": "yes"}}), true),
        (json!({"related": {"inverse": 7}}), true),
        (json!({"related": {"arity": "list"}}), false),
    ] {
        let result = parse(value.clone());

        if expect_wrong_type {
            assert!(
                matches!(result, Err(BaseSidecarError::WrongFieldType { field: "link_fields" })),
                "{value} must be a wrong-type error, got {result:?}"
            );
        } else {
            assert!(
                matches!(result, Err(BaseSidecarError::UnknownLinkFieldOption { .. })),
                "{value} must be an unknown-option error, got {result:?}"
            );
        }
    }
}

#[test]
fn the_sidecar_carries_its_link_fields_and_a_malformed_declaration_makes_it_malformed() -> Result<(), BaseSidecarError>
{
    let mut document = frontmatter(json!({
        "key_format": "FILEPATH",
        "last_id": 0,
        "last_create": "t0",
        "last_update": "t0",
        "link_fields": {"related": {"inverse": "related"}},
    }));

    let sidecar = BaseSidecar::try_from(&document)?;
    assert_eq!(
        summary(sidecar.link_fields()),
        vec![("related".to_owned(), false, Some("related".to_owned()))]
    );

    document.insert("link_fields".to_owned(), json!({"related": {"inverse": "nowhere"}}));
    assert!(matches!(
        BaseSidecar::try_from(&document),
        Err(BaseSidecarError::UndeclaredLinkInverse { .. })
    ));
    Ok(())
}

#[test]
fn link_fields_render_into_a_new_sidecar_and_are_never_part_of_a_patch() -> Result<(), BaseSidecarError> {
    let document = frontmatter(json!({
        "key_format": "FILEPATH",
        "last_id": 0,
        "last_create": "t0",
        "last_update": "t0",
        "link_fields": {"parent": {"single": true}, "related": {"inverse": "related"}},
    }));
    let sidecar = BaseSidecar::try_from(&document)?;

    let rendered = Map::from(&sidecar);
    assert_eq!(
        rendered.get("link_fields"),
        Some(&json!({"parent": {"single": true}, "related": {"inverse": "related"}}))
    );
    assert_eq!(BaseSidecar::try_from(&rendered)?, sidecar);

    let BaseSidecarPatch(patch) = BaseSidecarPatch::from(&sidecar);
    assert_eq!(
        patch.get("link_fields"),
        None,
        "a rewrite must leave the declaration exactly as the operator wrote it"
    );
    Ok(())
}

#[test]
fn every_link_field_error_is_a_malformed_sidecar_error() {
    for error in [
        BaseSidecarError::InvalidLinkField { name: "a-b".to_owned() },
        BaseSidecarError::DuplicateLinkField { name: "a".to_owned() },
        BaseSidecarError::UndeclaredLinkInverse {
            field: "a".to_owned(),
            inverse: "b".to_owned(),
        },
        BaseSidecarError::AsymmetricLinkInverse {
            field: "a".to_owned(),
            inverse: "b".to_owned(),
        },
        BaseSidecarError::UnknownLinkFieldOption {
            field: "a".to_owned(),
            option: "x".to_owned(),
        },
    ] {
        assert_eq!(error.code(), "format/base-sidecar");
        assert!(
            !error.remediation().is_empty(),
            "`error.remediation()` should not be empty"
        );
    }
}

#[test]
fn a_reserved_frontmatter_key_cannot_be_declared_as_a_link_field() {
    for name in [
        "id",
        "title",
        "updated",
        "Updated",
        "tags",
        "aliases",
        "template",
        "base",
        "Base",
        "link_fields",
        "key_prefix",
        "last_id",
    ] {
        let result = parse(json!({ name: {} }));

        assert!(
            matches!(result, Err(BaseSidecarError::InvalidLinkField { .. })),
            "{name:?} must be rejected, got {result:?}"
        );
    }
}

#[test]
fn a_link_field_may_not_be_named_for_a_series_template_declaration_key() {
    for name in ["series_templates", "fixed_fields"] {
        assert_eq!(
            parse(json!({name: {}})),
            Err(BaseSidecarError::InvalidLinkField { name: name.to_owned() }),
            "{name}"
        );
    }
}
