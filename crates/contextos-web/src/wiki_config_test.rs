use super::*;

#[test]
fn an_empty_file_defaults_to_the_scaffold_directory_and_right_side() -> Result<(), Box<dyn std::error::Error>> {
    let config: WikiConfig = toml::from_str("")?;
    assert_eq!(config, WikiConfig::default());
    assert_eq!(config.wiki_dir, "memory/wiki");
    assert_eq!(config.tree_side, PanelSide::Right);
    Ok(())
}

#[test]
fn a_configured_file_overrides_both_fields() -> Result<(), Box<dyn std::error::Error>> {
    let config: WikiConfig = toml::from_str(
        "wiki_dir = \"wiki\"\n\
         tree_side = \"left\"\n",
    )?;
    assert_eq!(config.wiki_dir, "wiki");
    assert_eq!(config.tree_side, PanelSide::Left);
    Ok(())
}

#[test]
fn a_partially_configured_file_defaults_only_the_omitted_field() -> Result<(), Box<dyn std::error::Error>> {
    let config: WikiConfig = toml::from_str("wiki_dir = \"wiki\"\n")?;
    assert_eq!(config.wiki_dir, "wiki");
    assert_eq!(config.tree_side, PanelSide::Right);
    Ok(())
}

#[test]
fn an_unknown_key_fails_to_parse_rather_than_being_silently_ignored() {
    let result: Result<WikiConfig, _> = toml::from_str("wiki_dir = \"wiki\"\nunknown_key = 1\n");
    assert!(result.is_err());
}

#[test]
fn an_empty_file_defaults_the_metadata_panel_to_the_right() -> Result<(), Box<dyn std::error::Error>> {
    let config: WikiConfig = toml::from_str("")?;
    assert_eq!(config.metadata_panel_side, PanelSide::Right);
    Ok(())
}

#[test]
fn a_configured_file_overrides_the_metadata_panel_side_independently_of_the_tree_side()
-> Result<(), Box<dyn std::error::Error>> {
    let config: WikiConfig = toml::from_str(
        "tree_side = \"right\"\n\
         metadata_panel_side = \"left\"\n",
    )?;
    assert_eq!(config.tree_side, PanelSide::Right);
    assert_eq!(config.metadata_panel_side, PanelSide::Left);
    Ok(())
}
