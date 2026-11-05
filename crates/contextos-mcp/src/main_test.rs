use std::fs;

use tempfile::tempdir;

use contextos_mcp::{ConfigEnvironment, ConfigLoadInput};

use super::{Config, LogLevel, default_config_path};

#[test]
fn default_config_path_is_under_the_home_directory_config_folder()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let resolved = default_config_path().ok_or("home directory unavailable")?;

    assert_eq!(
        resolved.strip_prefix(
            contextos_host::HostFolders::detect()
                .ok_or("home directory unavailable")?
                .home()
        )?,
        std::path::Path::new(".config/contextos/config.toml")
    );
    Ok(())
}

#[test]
fn cli_vaults_replace_file_vaults_without_discarding_server_settings()
-> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let fixture = tempdir()?;
    let file_vault = fixture.path().join("file-vault");
    let cli_vault = fixture.path().join("cli-vault");
    fs::create_dir_all(&file_vault)?;
    fs::create_dir_all(&cli_vault)?;
    let config_path = fixture.path().join("config.toml");
    fs::write(
        &config_path,
        format!("[server]\nlog_level = \"debug\"\n[[vault]]\npath = {file_vault:?}\n"),
    )?;

    let config = Config::try_from(ConfigLoadInput {
        cli_config_path: Some(config_path),
        cli_vaults: vec![cli_vault.clone()],
        environment: ConfigEnvironment::default(),
        ..ConfigLoadInput::default()
    })?;

    assert_eq!(config.server.log_level, LogLevel::Debug);
    assert_eq!(config.vaults.len(), 1);
    assert_eq!(config.vaults[0].path, cli_vault);
    Ok(())
}
