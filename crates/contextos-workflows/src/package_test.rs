use super::*;

fn valid_digest() -> &'static str {
    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
}

#[test]
fn a_manifest_with_no_workflow_and_no_export_declares_nothing() {
    let source = format!(
        "name = \"sync-crm\"\nversion = \"1.0.0\"\ndigest = {:?}\n",
        valid_digest()
    );
    let result = PackageManifest::parse(&source);
    assert!(matches!(result, Err(PackageError::NothingDeclared)), "{result:?}");
}

#[test]
fn a_digest_with_the_wrong_prefix_is_rejected() {
    assert!(matches!(
        ContentDigest::try_new("md5:aaaa"),
        Err(PackageError::MalformedDigest(_))
    ));
}

#[test]
fn a_digest_with_uppercase_hex_is_rejected() {
    let uppercase = format!("sha256:{}", "A".repeat(64));
    assert!(matches!(
        ContentDigest::try_new(uppercase),
        Err(PackageError::MalformedDigest(_))
    ));
}

#[test]
fn a_digest_with_the_wrong_length_is_rejected() {
    assert!(matches!(
        ContentDigest::try_new("sha256:aaaa"),
        Err(PackageError::MalformedDigest(_))
    ));
}

#[test]
fn a_valid_digest_round_trips_through_as_str() -> Result<(), Box<dyn std::error::Error>> {
    let digest = ContentDigest::try_new(valid_digest())?;
    assert_eq!(digest.as_str(), valid_digest());
    Ok(())
}

#[test]
fn a_missing_required_top_level_field_is_a_schema_violation() {
    let result = PackageManifest::parse("name = \"sync-crm\"\n");
    assert!(matches!(result, Err(PackageError::Malformed(_))), "{result:?}");
}

// ---------------------------------------------------------------------
// validate_and_convert
// ---------------------------------------------------------------------

#[test]
fn scalar_values_pass_through_validate_and_convert_unchanged() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        validate_and_convert(&serde_json::json!("hello"), &FieldKind::String)?,
        serde_json::json!("hello")
    );
    assert_eq!(
        validate_and_convert(&serde_json::json!(42), &FieldKind::Number)?,
        serde_json::json!(42)
    );
    assert_eq!(
        validate_and_convert(&serde_json::json!(true), &FieldKind::Bool)?,
        serde_json::json!(true)
    );
    assert_eq!(
        validate_and_convert(&serde_json::Value::Null, &FieldKind::Null)?,
        serde_json::Value::Null
    );
    Ok(())
}

#[test]
fn a_scalar_value_of_the_wrong_json_type_is_a_type_mismatch() {
    let result = validate_and_convert(&serde_json::json!("not a number"), &FieldKind::Number);
    assert!(matches!(result, Err(PackageError::TypeMismatch { .. })), "{result:?}");
}

#[test]
fn a_valid_date_string_normalises_through_validate_and_convert() -> Result<(), Box<dyn std::error::Error>> {
    let converted = validate_and_convert(&serde_json::json!("2026-09-23"), &FieldKind::Date)?;
    assert_eq!(converted, serde_json::json!("2026-09-23"));
    Ok(())
}

#[test]
fn a_malformed_date_string_is_an_invalid_temporal_value() {
    let result = validate_and_convert(&serde_json::json!("23/09/2026"), &FieldKind::Date);
    assert!(
        matches!(result, Err(PackageError::InvalidTemporalValue { kind: "date", .. })),
        "{result:?}"
    );
}

#[test]
fn a_valid_time_string_normalises_through_validate_and_convert() -> Result<(), Box<dyn std::error::Error>> {
    let converted = validate_and_convert(&serde_json::json!("09:30:00"), &FieldKind::Time)?;
    assert_eq!(converted, serde_json::json!("09:30:00"));
    Ok(())
}

#[test]
fn a_datetime_with_a_numeric_offset_normalises_to_the_z_suffix() -> Result<(), Box<dyn std::error::Error>> {
    let converted = validate_and_convert(&serde_json::json!("2026-09-23T10:00:00+00:00"), &FieldKind::DateTime)?;
    assert_eq!(converted, serde_json::json!("2026-09-23T10:00:00Z"));
    Ok(())
}

#[test]
fn a_malformed_datetime_string_is_an_invalid_temporal_value() {
    let result = validate_and_convert(&serde_json::json!("not-a-datetime"), &FieldKind::DateTime);
    assert!(
        matches!(result, Err(PackageError::InvalidTemporalValue { kind: "datetime", .. })),
        "{result:?}"
    );
}

#[test]
fn a_whole_second_timestamp_validates_and_passes_through() -> Result<(), Box<dyn std::error::Error>> {
    let converted = validate_and_convert(&serde_json::json!(1_758_600_000_i64), &FieldKind::Timestamp)?;
    assert_eq!(converted, serde_json::json!(1_758_600_000_i64));
    Ok(())
}

#[test]
fn a_non_integer_timestamp_is_a_type_mismatch() {
    let result = validate_and_convert(&serde_json::json!(1_758_600_000.5), &FieldKind::Timestamp);
    assert!(matches!(result, Err(PackageError::TypeMismatch { .. })), "{result:?}");
}

#[test]
fn an_out_of_range_timestamp_is_an_invalid_temporal_value() {
    let result = validate_and_convert(&serde_json::json!(i64::MAX), &FieldKind::Timestamp);
    assert!(
        matches!(
            result,
            Err(PackageError::InvalidTemporalValue { kind: "timestamp", .. })
        ),
        "{result:?}"
    );
}

#[test]
fn every_array_element_is_validated_and_converted_against_the_declared_items_kind()
-> Result<(), Box<dyn std::error::Error>> {
    let kind = FieldKind::Array {
        items: Box::new(FieldKind::DateTime),
    };
    let converted = validate_and_convert(
        &serde_json::json!(["2026-09-23T10:00:00+00:00", "2026-09-24T00:00:00Z"]),
        &kind,
    )?;
    assert_eq!(
        converted,
        serde_json::json!(["2026-09-23T10:00:00Z", "2026-09-24T00:00:00Z"])
    );
    Ok(())
}

#[test]
fn one_invalid_array_element_fails_the_whole_conversion() {
    let kind = FieldKind::Array {
        items: Box::new(FieldKind::Number),
    };
    let result = validate_and_convert(&serde_json::json!([1, "two", 3]), &kind);
    assert!(matches!(result, Err(PackageError::TypeMismatch { .. })), "{result:?}");
}

#[test]
fn declared_object_properties_are_validated_and_converted_in_place() -> Result<(), Box<dyn std::error::Error>> {
    let kind = FieldKind::Object {
        properties: vec![TypedField {
            required: true,
            name: "created".to_owned(),
            kind: FieldKind::DateTime,
        }],
    };
    let converted = validate_and_convert(
        &serde_json::json!({"created": "2026-09-23T10:00:00+00:00", "note": "kept as-is"}),
        &kind,
    )?;
    assert_eq!(
        converted,
        serde_json::json!({"created": "2026-09-23T10:00:00Z", "note": "kept as-is"})
    );
    Ok(())
}

#[test]
fn a_missing_declared_object_property_is_rejected() {
    let kind = FieldKind::Object {
        properties: vec![TypedField {
            required: true,
            name: "email".to_owned(),
            kind: FieldKind::String,
        }],
    };
    let result = validate_and_convert(&serde_json::json!({}), &kind);
    assert!(matches!(result, Err(PackageError::MissingProperty(_))), "{result:?}");
}
