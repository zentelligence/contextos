use super::*;

#[test]
fn an_empty_file_defaults_to_the_scaffold_directory_names_and_karla() -> Result<(), Box<dyn std::error::Error>> {
    let config: InboxOutboxConfig = toml::from_str("")?;
    assert_eq!(config, InboxOutboxConfig::default());
    assert_eq!(config.curator_agent, "karla");
    assert_eq!(config.inbox_dir, "inbox");
    assert_eq!(config.outbox_dir, "outbox");
    assert_eq!(config.processed_dir, "processed");
    Ok(())
}

#[test]
fn a_configured_file_overrides_every_field() -> Result<(), Box<dyn std::error::Error>> {
    let config: InboxOutboxConfig = toml::from_str(
        "curator_agent = \"aria\"\n\
         inbox_dir = \"drop-zone\"\n\
         outbox_dir = \"ready-for-pickup\"\n\
         processed_dir = \"archive\"\n",
    )?;
    assert_eq!(config.curator_agent, "aria");
    assert_eq!(config.inbox_dir, "drop-zone");
    assert_eq!(config.outbox_dir, "ready-for-pickup");
    assert_eq!(config.processed_dir, "archive");
    Ok(())
}

#[test]
fn a_partially_configured_file_defaults_only_the_omitted_fields() -> Result<(), Box<dyn std::error::Error>> {
    let config: InboxOutboxConfig = toml::from_str("curator_agent = \"aria\"\n")?;
    assert_eq!(config.curator_agent, "aria");
    assert_eq!(config.inbox_dir, "inbox");
    Ok(())
}

#[test]
fn an_unknown_key_fails_to_parse_rather_than_being_silently_ignored() {
    let result: Result<InboxOutboxConfig, _> = toml::from_str("curator_agent = \"aria\"\nunknown_key = 1\n");
    assert!(result.is_err());
}
