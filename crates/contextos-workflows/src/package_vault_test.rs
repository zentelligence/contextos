use serde_json::json;

use super::*;

#[test]
fn a_vault_value_that_is_a_plain_name_is_accepted() -> Result<(), PackageError> {
    let converted = validate_and_convert(&json!("client-a"), &FieldKind::Vault)?;

    assert_eq!(converted, json!("client-a"));
    Ok(())
}

#[test]
fn a_vault_value_that_is_empty_or_looks_like_a_path_is_rejected() {
    for value in [
        "",
        "notes/a",
        "/etc",
        "C:\\vault",
        "client:a",
        "client-a://",
        ".hidden",
        "..",
        "a\\b",
    ] {
        let outcome = validate_and_convert(&json!(value), &FieldKind::Vault);

        assert!(
            matches!(outcome, Err(PackageError::InvalidVaultName { .. })),
            "{value:?} should not be accepted as a vault name, got {outcome:?}"
        );
    }
}

#[test]
fn a_vault_value_that_is_not_a_string_is_rejected() {
    let outcome = validate_and_convert(&json!(5), &FieldKind::Vault);

    assert!(
        matches!(outcome, Err(PackageError::TypeMismatch { .. })),
        "got {outcome:?}"
    );
}
