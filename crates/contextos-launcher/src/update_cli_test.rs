use super::*;

#[test]
fn decode_hex_32_accepts_a_valid_lowercase_key() {
    let hex = "0".repeat(64);

    assert_eq!(decode_hex_32(&hex), Some([0_u8; 32]));
}

#[test]
fn decode_hex_32_rejects_the_wrong_length() {
    assert!(decode_hex_32("deadbeef").is_none());
}

#[test]
fn decode_hex_32_rejects_non_hex_characters() {
    let bad = "g".repeat(64);

    assert!(decode_hex_32(&bad).is_none());
}

#[test]
fn the_embedded_production_public_key_is_well_formed() {
    assert!(decode_hex_32(PRODUCTION_PUBLIC_KEY_HEX).is_some());
}

#[test]
fn trusted_keys_always_includes_the_production_key() -> CliResult<()> {
    let keys = trusted_keys()?;

    assert!(!keys.is_empty());
    Ok(())
}

#[test]
fn declared_commands_lists_both_public_binaries() {
    assert_eq!(declared_commands(), vec!["contextos", "contextos-web"]);
}

#[test]
fn display_optional_reports_none_for_an_absent_version() {
    assert_eq!(display_optional(None), "none");
}

#[test]
fn display_optional_reports_the_version_string_when_present() -> CliResult<()> {
    let version = ProductVersion::try_from("1.10.0")?;

    assert_eq!(display_optional(Some(&version)), "1.10.0");
    Ok(())
}

#[test]
fn the_default_manifest_url_is_the_public_release_repository() {
    assert_eq!(
        DEFAULT_UPDATE_URL,
        "https://github.com/zentelligence/contextos/releases/latest/download/latest.json"
    );
}
