use super::*;

#[test]
fn an_empty_file_defaults_to_the_scaffold_directory_and_template_path() -> Result<(), Box<dyn std::error::Error>> {
    let config: JournalConfig = toml::from_str("")?;
    assert_eq!(config, JournalConfig::default());
    assert_eq!(config.journal_dir, "journal");
    assert_eq!(config.template_path, "templates/daily-journal.md");
    Ok(())
}

#[test]
fn a_configured_file_overrides_both_fields() -> Result<(), Box<dyn std::error::Error>> {
    let config: JournalConfig = toml::from_str(
        "journal_dir = \"diary\"\n\
         template_path = \"templates/diary-entry.md\"\n",
    )?;
    assert_eq!(config.journal_dir, "diary");
    assert_eq!(config.template_path, "templates/diary-entry.md");
    Ok(())
}

#[test]
fn a_partially_configured_file_defaults_only_the_omitted_field() -> Result<(), Box<dyn std::error::Error>> {
    let config: JournalConfig = toml::from_str("journal_dir = \"diary\"\n")?;
    assert_eq!(config.journal_dir, "diary");
    assert_eq!(config.template_path, "templates/daily-journal.md");
    Ok(())
}

#[test]
fn an_unknown_key_fails_to_parse_rather_than_being_silently_ignored() {
    let result: Result<JournalConfig, _> = toml::from_str("journal_dir = \"diary\"\nunknown_key = 1\n");
    assert!(result.is_err());
}

#[test]
fn an_empty_file_defaults_the_date_format_and_folder_pane_side_to_the_shipped_convention()
-> Result<(), Box<dyn std::error::Error>> {
    let config: JournalConfig = toml::from_str("")?;
    assert_eq!(config.date_format, DEFAULT_DATE_FORMAT);
    assert_eq!(config.date_structure_side, DateStructureSide::Right);
    Ok(())
}

#[test]
fn a_configured_file_overrides_the_date_format_and_folder_pane_side() -> Result<(), Box<dyn std::error::Error>> {
    let config: JournalConfig = toml::from_str(
        "date_format = \"YYYY-MM-DD\"\n\
         date_structure_side = \"left\"\n",
    )?;
    assert_eq!(config.date_format, "YYYY-MM-DD");
    assert_eq!(config.date_structure_side, DateStructureSide::Left);
    Ok(())
}

#[test]
fn journal_config_has_no_date_structure_active_field_unlike_notes() {
    let result: Result<JournalConfig, _> = toml::from_str("date_structure_active = true\n");
    assert!(
        result.is_err(),
        "journal_dir has no open-ended, operator-filed use for a dated/regular split to separate"
    );
}

#[test]
fn an_empty_file_logs_no_task_events_and_has_no_read_only_sections() -> Result<(), Box<dyn std::error::Error>> {
    let config: JournalConfig = toml::from_str("")?;
    assert_eq!(config.task_opened_section, None);
    assert_eq!(config.task_closed_section, None);
    assert!(config.read_only_sections.is_empty(), "{:?}", config.read_only_sections);
    assert_eq!(config.task_section(TaskEvent::Opened), None);
    assert_eq!(config.task_section(TaskEvent::Closed), None);
    assert!(!config.is_read_only("Activity Log"));
    Ok(())
}

#[test]
fn configured_task_sections_and_read_only_sections_are_read() -> Result<(), Box<dyn std::error::Error>> {
    let config: JournalConfig = toml::from_str(
        "task_opened_section = \"New actions\"\n\
         task_closed_section = \"Completed actions\"\n\
         read_only_sections = [\"Activity Log\"]\n",
    )?;
    assert_eq!(config.task_section(TaskEvent::Opened), Some("New actions"));
    assert_eq!(config.task_section(TaskEvent::Closed), Some("Completed actions"));
    assert!(config.is_read_only("Activity Log"));
    assert!(
        config.is_read_only("  activity LOG "),
        "headings match as section headings do"
    );
    assert!(!config.is_read_only("Working notes"));
    Ok(())
}

#[test]
fn a_blank_task_section_logs_nothing() -> Result<(), Box<dyn std::error::Error>> {
    let config: JournalConfig = toml::from_str("task_opened_section = \"  \"\n")?;
    assert_eq!(config.task_section(TaskEvent::Opened), None);
    Ok(())
}

#[test]
fn unset_optional_settings_are_left_out_when_written() -> Result<(), Box<dyn std::error::Error>> {
    let written = toml::to_string(&JournalConfig::default())?;
    assert!(!written.contains("task_opened_section"), "{written}");
    assert!(!written.contains("task_closed_section"), "{written}");
    assert!(!written.contains("read_only_sections"), "{written}");
    Ok(())
}
