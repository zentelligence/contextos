use proptest::prelude::*;
use serde_json::json;

use super::*;

/// A fixture is always a JSON object literal; anything else yields an empty map, which the calling test's own
/// assertions then fail on.
fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

fn names(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

fn fixed(template: Value, fixed_fields: &[&str]) -> Result<FixedFields, SeriesTemplateError> {
    let template = object(template);
    FixedFields::try_from(SeriesTemplateInput {
        template: &template,
        fixed_fields: &names(fixed_fields),
    })
}

fn unusable_field(template: Value, fixed_fields: &[&str]) -> Option<String> {
    match fixed(template, fixed_fields) {
        Err(SeriesTemplateError::NotLiteral { field } | SeriesTemplateError::Missing { field }) => Some(field),
        Ok(_) => None,
    }
}

#[test]
fn type_and_each_named_field_with_a_literal_value_are_the_fixed_fields() -> Result<(), SeriesTemplateError> {
    let fields = fixed(
        json!({"type": "task", "task_type": "action", "rank": 3, "urgent": false}),
        &["task_type", "rank", "urgent"],
    )?;

    assert_eq!(
        fields.fields(),
        &object(json!({"type": "task", "task_type": "action", "rank": 3, "urgent": false}))
    );
    Ok(())
}

#[test]
fn template_defaults_are_never_fixed_whether_literal_or_not() -> Result<(), SeriesTemplateError> {
    let fields = fixed(
        json!({
            "type": "task",
            "task_type": "action",
            "state": "next",
            "priority": "normal",
            "status": "adopted",
            "entity": "{{entity}}",
            "created": "<% tp.date.now() %>",
            "menu": "a|b",
        }),
        &["task_type"],
    )?;

    assert_eq!(fields.fields(), &object(json!({"type": "task", "task_type": "action"})));
    Ok(())
}

#[test]
fn a_named_field_holding_a_placeholder_templater_expression_menu_empty_value_list_or_map_makes_the_template_unusable_naming_the_field()
 {
    for value in [
        json!("{{requirement_type}}"),
        json!("<% tp.date.now() %>"),
        json!("functional|non-functional"),
        json!(""),
        json!(null),
        json!(["functional"]),
        json!({"a": 1}),
    ] {
        let template = json!({"type": "requirement", "requirement_type": value});
        assert_eq!(
            unusable_field(template.clone(), &["requirement_type"]).as_deref(),
            Some("requirement_type"),
            "{template}"
        );
    }
}

#[test]
fn a_template_without_a_literal_type_is_unusable() {
    for template in [
        json!({"task_type": "action"}),
        json!({"type": "{{type}}"}),
        json!({"type": "<% tp.x %>"}),
        json!({"type": "a|b"}),
        json!({"type": ""}),
        json!({"type": ["task"]}),
    ] {
        assert_eq!(
            unusable_field(template.clone(), &[]).as_deref(),
            Some("type"),
            "{template}"
        );
    }
}

#[test]
fn a_named_field_the_template_does_not_declare_is_not_fixed() -> Result<(), SeriesTemplateError> {
    let fields = fixed(json!({"type": "task"}), &["task_type"])?;

    assert_eq!(fields.fields(), &object(json!({"type": "task"})));
    Ok(())
}

#[test]
fn a_literal_number_and_boolean_keep_their_yaml_types() -> Result<(), SeriesTemplateError> {
    let fields = fixed(json!({"type": "t", "level": 1, "flag": true}), &["level", "flag"])?;

    assert_eq!(fields.fields().get("level"), Some(&json!(1)));
    assert_eq!(fields.fields().get("flag"), Some(&json!(true)));
    Ok(())
}

proptest! {
    #[test]
    fn a_string_containing_a_placeholder_templater_or_menu_marker_is_never_literal(
        before in "[a-z ]{0,6}",
        after in "[a-z ]{0,6}",
        marker in prop_oneof!["\\{\\{", "\\}\\}", "<%", "%>", "\\|"],
    ) {
        let marked = format!("{before}{marker}{after}");
        prop_assert_eq!(
            unusable_field(json!({"type": "task", "kind": marked}), &["kind"]),
            Some("kind".to_owned())
        );

        let plain = format!("a{before}{after}z");
        let fields = fixed(json!({"type": "task", "kind": plain}), &["kind"]);
        prop_assert_eq!(fields.map(|fields| fields.fields().get("kind").cloned()), Ok(Some(json!(plain))));
    }
}
