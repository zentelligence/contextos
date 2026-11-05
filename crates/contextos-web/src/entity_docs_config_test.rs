use super::*;

#[test]
fn an_empty_file_defaults_to_the_right_side() -> Result<(), Box<dyn std::error::Error>> {
    let config: EntityDocsConfig = toml::from_str("")?;
    assert_eq!(config, EntityDocsConfig::default());
    assert_eq!(config.folder_side, PanelSide::Right);
    Ok(())
}

#[test]
fn a_configured_file_overrides_the_side() -> Result<(), Box<dyn std::error::Error>> {
    let config: EntityDocsConfig = toml::from_str("folder_side = \"left\"\n")?;
    assert_eq!(config.folder_side, PanelSide::Left);
    Ok(())
}

#[test]
fn an_unknown_key_fails_to_parse_rather_than_being_silently_ignored() {
    let result: Result<EntityDocsConfig, _> = toml::from_str("folder_side = \"left\"\nunknown_key = 1\n");
    assert!(result.is_err());
}

#[test]
fn an_empty_file_defaults_the_metadata_panel_to_the_right() -> Result<(), Box<dyn std::error::Error>> {
    let config: EntityDocsConfig = toml::from_str("")?;
    assert_eq!(config.metadata_panel_side, PanelSide::Right);
    Ok(())
}

#[test]
fn a_configured_file_overrides_the_metadata_panel_side_independently_of_the_folder_side()
-> Result<(), Box<dyn std::error::Error>> {
    let config: EntityDocsConfig = toml::from_str(
        "folder_side = \"right\"\n\
         metadata_panel_side = \"left\"\n",
    )?;
    assert_eq!(config.folder_side, PanelSide::Right);
    assert_eq!(config.metadata_panel_side, PanelSide::Left);
    Ok(())
}
