use std::fs;

use tempfile::tempdir;

use super::*;

fn write(dir: &std::path::Path, name: &str, contents: &str) -> std::io::Result<std::path::PathBuf> {
    let path = dir.join(name);
    fs::write(&path, contents)?;
    Ok(path)
}

const ONE_SERVER: &str = "[[mcp_server]]\ntransport = \"stdio\"\nname = \"contextos\"\ncommand = \"contextos-mcp\"\n";

#[test]
fn a_config_without_an_owner_table_has_no_owner() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(dir.path(), "web.toml", ONE_SERVER)?;

    assert_eq!(load_web_config(&path)?.owner, None);
    Ok(())
}

#[test]
fn an_owner_table_names_the_owner_of_shared_jobs() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        &format!("[owner]\nname = \"Piper\"\n{ONE_SERVER}"),
    )?;

    let owner = load_web_config(&path)?.owner;

    assert_eq!(owner.map(|owner| owner.name), Some("Piper".to_owned()));
    Ok(())
}

#[test]
fn an_owner_table_with_a_blank_name_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(dir.path(), "web.toml", &format!("[owner]\nname = \"  \"\n{ONE_SERVER}"))?;

    assert!(matches!(load_web_config(&path), Err(WebConfigError::BlankOwnerName)));
    Ok(())
}

#[test]
fn an_owner_table_with_an_unknown_key_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        &format!("[owner]\nname = \"Piper\"\nemail = \"p@example.com\"\n{ONE_SERVER}"),
    )?;

    assert!(matches!(load_web_config(&path), Err(WebConfigError::Toml { .. })));
    Ok(())
}
