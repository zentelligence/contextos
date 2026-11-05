use super::*;

#[test]
fn an_empty_file_defaults_to_the_scaffold_directory_and_template_path() -> Result<(), Box<dyn std::error::Error>> {
    let config: NotesConfig = toml::from_str("")?;
    assert_eq!(config, NotesConfig::default());
    assert_eq!(config.notes_dir, "notes");
    assert_eq!(config.template_path, "templates/unique-note.md");
    Ok(())
}

#[test]
fn a_configured_file_overrides_both_fields() -> Result<(), Box<dyn std::error::Error>> {
    let config: NotesConfig = toml::from_str(
        "notes_dir = \"scratch\"\n\
         template_path = \"templates/scratch-note.md\"\n",
    )?;
    assert_eq!(config.notes_dir, "scratch");
    assert_eq!(config.template_path, "templates/scratch-note.md");
    Ok(())
}

#[test]
fn a_partially_configured_file_defaults_only_the_omitted_field() -> Result<(), Box<dyn std::error::Error>> {
    let config: NotesConfig = toml::from_str("notes_dir = \"scratch\"\n")?;
    assert_eq!(config.notes_dir, "scratch");
    assert_eq!(config.template_path, "templates/unique-note.md");
    Ok(())
}

#[test]
fn an_unknown_key_fails_to_parse_rather_than_being_silently_ignored() {
    let result: Result<NotesConfig, _> = toml::from_str("notes_dir = \"scratch\"\nunknown_key = 1\n");
    assert!(result.is_err());
}

#[test]
fn an_empty_file_defaults_the_date_structure_fields_to_the_shipped_convention() -> Result<(), Box<dyn std::error::Error>>
{
    let config: NotesConfig = toml::from_str("")?;
    assert_eq!(config.date_format, DEFAULT_DATE_FORMAT);
    assert!(config.date_structure_active);
    assert_eq!(config.date_structure_side, DateStructureSide::Right);
    Ok(())
}

#[test]
fn a_configured_file_overrides_every_date_structure_field() -> Result<(), Box<dyn std::error::Error>> {
    let config: NotesConfig = toml::from_str(
        "date_format = \"DD-MM-YYYY\"\n\
         date_structure_active = false\n\
         date_structure_side = \"left\"\n",
    )?;
    assert_eq!(config.date_format, "DD-MM-YYYY");
    assert!(!config.date_structure_active);
    assert_eq!(config.date_structure_side, DateStructureSide::Left);
    Ok(())
}

#[test]
fn an_empty_file_defaults_the_metadata_panel_to_the_right() -> Result<(), Box<dyn std::error::Error>> {
    let config: NotesConfig = toml::from_str("")?;
    assert_eq!(config.metadata_panel_side, PanelSide::Right);
    Ok(())
}

#[test]
fn a_configured_file_overrides_the_metadata_panel_side_independently_of_the_dated_pane_side()
-> Result<(), Box<dyn std::error::Error>> {
    let config: NotesConfig = toml::from_str(
        "date_structure_side = \"right\"\n\
         metadata_panel_side = \"left\"\n",
    )?;
    assert_eq!(config.date_structure_side, DateStructureSide::Right);
    assert_eq!(config.metadata_panel_side, PanelSide::Left);
    Ok(())
}
