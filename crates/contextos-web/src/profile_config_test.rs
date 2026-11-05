use super::*;

#[test]
fn an_empty_file_defaults_to_the_right_side_and_no_template() -> Result<(), Box<dyn std::error::Error>> {
    let config: ProfileConfig = toml::from_str("")?;
    assert_eq!(config, ProfileConfig::default());
    assert_eq!(config.side, PanelSide::Right);
    assert_eq!(config.entity_template_path, "");
    Ok(())
}

#[test]
fn a_configured_file_overrides_the_side_and_template_path() -> Result<(), Box<dyn std::error::Error>> {
    let config: ProfileConfig =
        toml::from_str("side = \"left\"\nentity_template_path = \"templates/entity-profile.md\"\n")?;
    assert_eq!(config.side, PanelSide::Left);
    assert_eq!(config.entity_template_path, "templates/entity-profile.md");
    Ok(())
}

#[test]
fn an_unknown_key_fails_to_parse_rather_than_being_silently_ignored() {
    let result: Result<ProfileConfig, _> = toml::from_str("side = \"left\"\nunknown_key = 1\n");
    assert!(result.is_err());
}
