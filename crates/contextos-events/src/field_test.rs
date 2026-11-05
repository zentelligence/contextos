use serde_json::json;

use super::*;

#[test]
fn a_field_name_is_lower_case_letters_digits_underscores_and_hyphens_led_by_a_letter() {
    for valid in ["run_id", "from", "x1", "a-b"] {
        assert!(FieldName::try_from(valid).is_ok(), "{valid}");
    }
    for invalid in ["", "1x", "_x", "-x", "Run", "a b", "a.b", "a:b", &"a".repeat(65)] {
        assert_eq!(
            FieldName::try_from(invalid).err().map(|error| error.code()),
            Some("events/invalid-fields"),
            "{invalid:?}"
        );
    }
}

#[test]
fn names_reserved_by_the_event_shape_are_listed() {
    for name in [
        "type",
        "kind",
        "severity",
        "at",
        "entity",
        "subject",
        "subject_id",
        "actor",
    ] {
        assert!(FieldName::is_event_field(name), "{name}");
    }
    for name in ["title", "created", "updated", "tags", "aliases", "description"] {
        assert!(FieldName::is_note_field(name), "{name}");
    }
    for name in ["at", "path", "filename"] {
        assert!(FieldName::is_server_stamped(name), "{name}");
    }
    assert!(!FieldName::is_event_field("run_id") && !FieldName::is_note_field("run_id"));
}

#[test]
fn json_scalars_and_lists_of_scalars_become_field_values() {
    assert_eq!(FieldValue::try_from(&json!("x")), Ok(FieldValue::from("x")));
    assert_eq!(FieldValue::try_from(&json!(4)), Ok(FieldValue::from(4_i64)));
    assert_eq!(FieldValue::try_from(&json!(true)), Ok(FieldValue::from(true)));
    assert_eq!(
        FieldValue::try_from(&json!(["a", 2])),
        Ok(FieldValue::List(vec![Scalar::from("a"), Scalar::from(2_i64)]))
    );
}

#[test]
fn nulls_objects_nested_lists_and_multi_line_strings_are_refused() {
    for value in [
        json!(null),
        json!({"a": 1}),
        json!([[1]]),
        json!([null]),
        json!("a\nb"),
        json!(["a\rb"]),
    ] {
        assert_eq!(
            FieldValue::try_from(&value),
            Err(FieldRejection::InvalidValue),
            "{value}"
        );
    }
}
