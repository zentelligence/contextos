use serde_json::json;

use super::*;
use crate::{BaseSidecar, BaseSidecarPatch};

const T0: &str = "2026-10-22T10:00:00+11:00";
const T1: &str = "2026-10-22T10:05:00+11:00";

/// A fixture is always a JSON object literal; anything else yields an empty map, which the calling test's own
/// assertions then fail on.
fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// A keyed sidecar with series `A`, `D`, and `Q`, plus the extra keys of one declaration.
fn keyed(extra: Value) -> Map<String, Value> {
    let mut document = object(json!({
        "key_format": "{key_prefix}-{key_id}",
        "key_prefixes": {"A": 4, "D": 2, "Q": 1},
        "last_create": T0,
        "last_update": T0,
    }));
    document.extend(object(extra));
    document
}

fn link_of(sidecar: &BaseSidecar, prefix: &str) -> Option<String> {
    sidecar
        .series_templates()
        .template_for(Some(prefix))
        .map(|link| link.as_ref().to_owned())
}

fn assert_malformed(document: &Map<String, Value>, key: &str) {
    let error = BaseSidecar::try_from(document).err();
    assert!(
        error.is_some(),
        "the sidecar declaring {key} should be malformed, but it parsed"
    );
    if let Some(error) = error {
        assert_eq!(error.code(), "format/base-sidecar", "{key}: {error}");
        assert!(
            error.to_string().contains(key),
            "the message should name {key}, got: {error}"
        );
    }
}

#[test]
fn one_template_link_applies_to_every_series() -> Result<(), BaseSidecarError> {
    let sidecar = BaseSidecar::try_from(&keyed(json!({"template": "[[templates/task|Task]]"})))?;

    for prefix in ["A", "D", "Q"] {
        assert_eq!(
            link_of(&sidecar, prefix).as_deref(),
            Some("[[templates/task|Task]]"),
            "{prefix}"
        );
    }
    Ok(())
}

#[test]
fn a_one_item_template_list_applies_to_every_series() -> Result<(), BaseSidecarError> {
    let sidecar = BaseSidecar::try_from(&keyed(json!({"template": ["[[templates/task|Task]]"]})))?;

    for prefix in ["A", "D", "Q"] {
        assert_eq!(
            link_of(&sidecar, prefix).as_deref(),
            Some("[[templates/task|Task]]"),
            "{prefix}"
        );
    }
    Ok(())
}

#[test]
fn a_series_template_overrides_the_every_series_template_for_its_series_only() -> Result<(), BaseSidecarError> {
    let sidecar = BaseSidecar::try_from(&keyed(json!({
        "template": "[[templates/task]]",
        "series_templates": {"D": "[[templates/task-decision|Decision task]]"},
    })))?;

    assert_eq!(
        link_of(&sidecar, "D").as_deref(),
        Some("[[templates/task-decision|Decision task]]")
    );
    assert_eq!(link_of(&sidecar, "A").as_deref(), Some("[[templates/task]]"));
    assert_eq!(link_of(&sidecar, "Q").as_deref(), Some("[[templates/task]]"));
    Ok(())
}

#[test]
fn a_template_list_of_several_links_gives_a_template_only_to_series_with_their_own_entry()
-> Result<(), BaseSidecarError> {
    let sidecar = BaseSidecar::try_from(&keyed(json!({
        "template": ["[[templates/a]]", "[[templates/b]]", "[[templates/c]]"],
        "series_templates": {"Q": "[[templates/question]]"},
    })))?;

    assert_eq!(link_of(&sidecar, "Q").as_deref(), Some("[[templates/question]]"));
    assert_eq!(link_of(&sidecar, "A"), None);
    assert_eq!(link_of(&sidecar, "D"), None);
    Ok(())
}

#[test]
fn a_sidecar_declaring_nothing_has_no_template_and_no_extra_fixed_fields() -> Result<(), BaseSidecarError> {
    // Guard: behaviour for a base with no declaration must stay exactly as before.
    let sidecar = BaseSidecar::try_from(&keyed(json!({})))?;

    assert_eq!(link_of(&sidecar, "A"), None);
    assert!(
        sidecar.series_templates().fixed_fields().is_empty(),
        "no fixed_fields declared, got {:?}",
        sidecar.series_templates().fixed_fields()
    );
    Ok(())
}

#[test]
fn fixed_fields_are_read_in_declaration_order() -> Result<(), BaseSidecarError> {
    let sidecar = BaseSidecar::try_from(&keyed(json!({
        "template": "[[templates/task]]",
        "fixed_fields": ["task_type", "kind"],
    })))?;

    assert_eq!(sidecar.series_templates().fixed_fields(), ["task_type", "kind"]);
    Ok(())
}

#[test]
fn a_series_template_for_an_undeclared_or_differently_cased_prefix_makes_the_sidecar_malformed() {
    for prefix in ["Z", "d"] {
        let document = keyed(json!({"series_templates": {prefix: "[[templates/task]]"}}));
        assert_malformed(&document, "series_templates");
    }
}

#[test]
fn series_templates_on_a_filepath_sidecar_makes_it_malformed() {
    let document = object(json!({
        "key_format": "FILEPATH",
        "last_id": 0,
        "last_create": T0,
        "last_update": T0,
        "series_templates": {"A": "[[templates/task]]"},
    }));

    assert_malformed(&document, "series_templates");
}

#[test]
fn a_series_template_that_is_not_one_string_makes_the_sidecar_malformed() {
    for value in [json!(["[[templates/task]]"]), json!(5), json!(null), json!({"x": 1})] {
        let document = keyed(json!({"series_templates": {"A": value}}));
        assert_malformed(&document, "series_templates");
    }
}

#[test]
fn fixed_fields_naming_a_server_field_a_sidecar_key_or_a_link_field_makes_the_sidecar_malformed() {
    for name in [
        "id",
        "base",
        "title",
        "entity",
        "created",
        "updated",
        "template",
        "last_id",
        "key_prefixes",
        "related",
    ] {
        let document = keyed(json!({
            "template": "[[templates/task]]",
            "link_fields": {"related": {"inverse": "related"}},
            "fixed_fields": [name],
        }));
        assert_malformed(&document, "fixed_fields");
    }
}

#[test]
fn listing_type_among_the_fixed_fields_is_allowed_and_changes_nothing() -> Result<(), BaseSidecarError> {
    let sidecar = BaseSidecar::try_from(&keyed(json!({
        "template": "[[templates/task]]",
        "fixed_fields": ["type", "task_type"],
    })))?;

    assert_eq!(link_of(&sidecar, "A").as_deref(), Some("[[templates/task]]"));
    Ok(())
}

#[test]
fn fixed_fields_that_is_empty_repeats_a_name_has_an_invalid_name_or_exceeds_sixteen_makes_the_sidecar_malformed() {
    let too_many: Vec<String> = (0..17).map(|index| format!("field_{index}")).collect();
    let long_name = format!("a{}", "b".repeat(64));
    for fixed in [
        json!([]),
        json!(["task_type", "task_type"]),
        json!(["1task"]),
        json!(["task type"]),
        json!(["task.type"]),
        json!([long_name]),
        json!(too_many),
        json!("task_type"),
        json!([5]),
    ] {
        let document = keyed(json!({"template": "[[templates/task]]", "fixed_fields": fixed}));
        assert_malformed(&document, "fixed_fields");
    }
}

#[test]
fn fixed_fields_may_use_letters_digits_underscores_and_hyphens() -> Result<(), BaseSidecarError> {
    let sidecar = BaseSidecar::try_from(&keyed(json!({
        "template": "[[templates/task]]",
        "fixed_fields": ["task-type", "kind_2"],
    })))?;

    assert_eq!(sidecar.series_templates().fixed_fields(), ["task-type", "kind_2"]);
    Ok(())
}

#[test]
fn a_template_that_is_neither_a_link_nor_a_list_of_links_makes_the_sidecar_malformed() {
    for value in [json!(5), json!(true), json!({"a": "[[x]]"}), json!(["[[x]]", 5])] {
        let document = keyed(json!({"template": value}));
        assert_malformed(&document, "template");
    }
}

#[test]
fn advancing_or_refreshing_a_sidecar_keeps_its_template_declaration_byte_for_byte() -> Result<(), BaseSidecarError> {
    let declaration = json!({
        "template": ["[[templates/a]]", "[[templates/b]]"],
        "series_templates": {"D": "[[templates/task-decision|Decision task]]"},
        "fixed_fields": ["task_type"],
    });
    let original = keyed(declaration.clone());
    let sidecar = BaseSidecar::try_from(&original)?;
    assert_eq!(
        link_of(&sidecar, "D").as_deref(),
        Some("[[templates/task-decision|Decision task]]")
    );

    for rewritten in [
        sidecar.advance(Some("A"), T1)?,
        sidecar.touch(T1),
        sidecar.caught_up_to(Some("A"), 9)?,
    ] {
        let mut document = original.clone();
        let BaseSidecarPatch(patch) = BaseSidecarPatch::from(&rewritten);
        for (key, value) in patch {
            if value.is_null() {
                document.shift_remove(&key);
            } else if declaration.get(&key).is_none() {
                document.insert(key, value);
            }
        }
        for key in ["template", "series_templates", "fixed_fields"] {
            assert_eq!(document.get(key), declaration.get(key), "{key}");
        }
        let reparsed = BaseSidecar::try_from(&document)?;
        assert_eq!(
            link_of(&reparsed, "D").as_deref(),
            Some("[[templates/task-decision|Decision task]]")
        );
        assert_eq!(reparsed.series_templates().fixed_fields(), ["task_type"]);
    }
    Ok(())
}

#[test]
fn a_patch_never_carries_the_template_declaration_keys() -> Result<(), BaseSidecarError> {
    // Guard: the server must never write these hand-authored keys, so a rewrite cannot disturb them.
    let sidecar = BaseSidecar::try_from(&keyed(json!({
        "template": "[[templates/task]]",
        "series_templates": {"D": "[[templates/task-decision]]"},
        "fixed_fields": ["task_type"],
    })))?;

    let BaseSidecarPatch(patch) = BaseSidecarPatch::from(&sidecar.advance(Some("A"), T1)?);
    for key in ["template", "series_templates", "fixed_fields"] {
        assert!(!patch.contains_key(key), "the patch should leave {key} alone");
    }
    Ok(())
}
