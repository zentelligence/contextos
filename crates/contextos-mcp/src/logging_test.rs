use tempfile::tempdir;

use super::*;

#[test]
fn an_empty_log_file_opens_stderr_only_with_no_rotating_writer() -> Result<(), Box<dyn std::error::Error>> {
    let config = ServerConfig::default();
    let opened = open(&config)?;
    assert!(opened.is_none());
    Ok(())
}

#[test]
fn a_configured_log_file_opens_a_rotating_writer() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let config = ServerConfig {
        log_file: dir.path().join("contextos.log").to_string_lossy().into_owned(),
        log_max_size_mb: Some(10),
        ..ServerConfig::default()
    };

    let opened = open(&config)?;

    assert!(opened.is_some());
    Ok(())
}
