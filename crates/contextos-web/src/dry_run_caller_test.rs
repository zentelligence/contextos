use serde_json::json;

use super::*;

#[test]
fn a_value_matching_its_schema_is_accepted() {
    let schema = json!({"type": "object", "required": ["path"], "properties": {"path": {"type": "string"}}});
    let value = json!({"path": "notes.md"});
    assert!(validate_against_schema(&value, &schema).is_ok());
}

#[test]
fn a_value_missing_a_required_field_is_rejected() {
    let schema = json!({"type": "object", "required": ["path"], "properties": {"path": {"type": "string"}}});
    let value = json!({});
    let result = validate_against_schema(&value, &schema);
    assert!(result.is_err());
}

#[test]
fn a_value_with_the_wrong_type_is_rejected() {
    let schema = json!({"type": "object", "properties": {"path": {"type": "string"}}});
    let value = json!({"path": 5});
    assert!(validate_against_schema(&value, &schema).is_err());
}
