//! `current_appearance`: the theme, font, size, system name, and logo the shell reads from `[server.ui]`.

use std::fs;

use tempfile::tempdir;

use super::*;

fn write(dir: &std::path::Path, name: &str, contents: &str) -> std::io::Result<std::path::PathBuf> {
    let path = dir.join(name);
    fs::write(&path, contents)?;
    Ok(path)
}

#[test]
fn current_appearance_reads_theme_font_and_size_from_server_ui() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(
        dir.path(),
        "web.toml",
        "[server.ui]\ntheme = \"dark\"\nfont = \"serif\"\nsize = \"large\"\n",
    )?;

    let appearance = current_appearance(&path);

    assert_eq!(appearance.theme.as_deref(), Some("dark"));
    assert_eq!(appearance.font.as_deref(), Some("serif"));
    assert_eq!(appearance.size.as_deref(), Some("large"));
    Ok(())
}

#[test]
fn current_appearance_ignores_a_non_string_value() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(dir.path(), "web.toml", "[server.ui]\ntheme = 42\n")?;

    let appearance = current_appearance(&path);

    assert_eq!(appearance.theme, None);
    Ok(())
}

#[test]
fn current_appearance_degrades_to_default_when_web_toml_is_unreadable() {
    let appearance = current_appearance(std::path::Path::new("/does/not/exist/web.toml"));
    assert_eq!(appearance, Appearance::default());
}

#[test]
fn current_appearance_is_empty_when_no_server_ui_table_is_present() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(dir.path(), "web.toml", "")?;

    let appearance = current_appearance(&path);

    assert_eq!(appearance, Appearance::default());
    Ok(())
}

#[test]
fn current_appearance_reads_system_name_from_server_ui() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(dir.path(), "web.toml", "[server.ui]\nsystem_name = \"Ops Desk\"\n")?;

    let appearance = current_appearance(&path);

    assert_eq!(appearance.system_name, "Ops Desk");
    Ok(())
}

#[test]
fn current_appearance_defaults_system_name_to_command_centre_when_unset() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(dir.path(), "web.toml", "")?;

    let appearance = current_appearance(&path);

    assert_eq!(appearance.system_name, "Command Centre");
    Ok(())
}

#[test]
fn current_appearance_ignores_a_non_string_system_name_and_falls_back_to_the_default()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(dir.path(), "web.toml", "[server.ui]\nsystem_name = 42\n")?;

    let appearance = current_appearance(&path);

    assert_eq!(appearance.system_name, "Command Centre");
    Ok(())
}

#[test]
fn current_appearance_reads_logo_from_server_ui() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(dir.path(), "web.toml", "[server.ui]\nlogo = \"acme-logo.png\"\n")?;

    let appearance = current_appearance(&path);

    assert_eq!(appearance.logo, "acme-logo.png");
    Ok(())
}

#[test]
fn current_appearance_defaults_logo_to_the_bundled_filename_when_unset() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = write(dir.path(), "web.toml", "")?;

    let appearance = current_appearance(&path);

    assert_eq!(appearance.logo, "logo.png");
    Ok(())
}

#[test]
fn current_appearance_ignores_a_non_string_logo_and_falls_back_to_the_default() -> Result<(), Box<dyn std::error::Error>>
{
    let dir = tempdir()?;
    let path = write(dir.path(), "web.toml", "[server.ui]\nlogo = 42\n")?;

    let appearance = current_appearance(&path);

    assert_eq!(appearance.logo, "logo.png");
    Ok(())
}
