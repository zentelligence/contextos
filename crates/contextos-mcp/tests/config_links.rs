//! `[vault.links]`: `exclude` names the folders whose notes are not checked as link sources, and `ignore_targets` the
//! folders whose files are not checked as link targets. Both take plain vault-relative folders and refuse anything else
//! naming the entry.
use contextos_mcp::{Config, ConfigError};
use tempfile::tempdir;

#[test]
fn link_exclusions_default_to_the_folders_whose_notes_are_not_checked_as_link_sources()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let source = format!("[[vault]]\npath = {:?}\n", vault.path());

    let config = Config::try_from(source.as_str())?;

    assert_eq!(
        config.vaults[0].links.exclude,
        ["templates", "memory/events", "memory/jobs", ".contextos"]
    );
    Ok(())
}

#[test]
fn a_configured_link_exclusion_list_replaces_the_default() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let source = format!(
        "[[vault]]\npath = {:?}\n[vault.links]\nexclude = [\"drafts\", \"memory/raw\"]\n",
        vault.path()
    );

    let config = Config::try_from(source.as_str())?;

    assert_eq!(config.vaults[0].links.exclude, ["drafts", "memory/raw"]);
    Ok(())
}

#[test]
fn a_link_exclusion_that_is_not_a_plain_vault_relative_folder_is_refused_naming_it()
-> Result<(), Box<dyn std::error::Error>> {
    for bad in ["../outside", "/abs", "", "a//b", "templates/", "c:/x", "a\\\\b", "./x"] {
        let vault = tempdir()?;
        let source = format!(
            "[[vault]]\npath = {:?}\n[vault.links]\nexclude = [{bad:?}]\n",
            vault.path()
        );

        let result = Config::try_from(source.as_str());

        assert!(
            matches!(&result, Err(ConfigError::InvalidLinkExclude { entry }) if entry == bad),
            "{bad:?} must be refused naming it, got {result:?}"
        );
    }
    Ok(())
}

#[test]
fn an_unknown_link_setting_is_refused_rather_than_ignored() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let source = format!(
        "[[vault]]\npath = {:?}\n[vault.links]\nexclud = [\"x\"]\n",
        vault.path()
    );

    assert!(Config::try_from(source.as_str()).is_err());
    Ok(())
}

#[test]
fn link_target_ignores_default_to_none() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let source = format!("[[vault]]\npath = {:?}\n", vault.path());

    let config = Config::try_from(source.as_str())?;

    assert_eq!(config.vaults[0].links.ignore_targets, Vec::<String>::new());
    Ok(())
}

#[test]
fn configured_link_target_ignores_are_kept_as_written_with_nothing_added() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let source = format!(
        "[[vault]]\npath = {:?}\n[vault.search]\nexclude = [\"private\"]\n[vault.links]\nignore_targets = [\"inbox\", \"memory/scratch\"]\n",
        vault.path()
    );

    let config = Config::try_from(source.as_str())?;

    assert_eq!(config.vaults[0].links.ignore_targets, ["inbox", "memory/scratch"]);
    assert_eq!(
        config.vaults[0].links.exclude,
        ["templates", "memory/events", "memory/jobs", ".contextos"],
        "setting ignore_targets leaves the source exclusions at their default"
    );
    Ok(())
}

#[test]
fn a_link_target_ignore_that_is_not_a_plain_vault_relative_folder_is_refused_naming_it()
-> Result<(), Box<dyn std::error::Error>> {
    for bad in ["../outside", "/abs", "", "a//b", "inbox/", "c:/x", "a\\\\b", "./x"] {
        let vault = tempdir()?;
        let source = format!(
            "[[vault]]\npath = {:?}\n[vault.links]\nignore_targets = [{bad:?}]\n",
            vault.path()
        );

        let result = Config::try_from(source.as_str());

        assert!(
            matches!(&result, Err(ConfigError::InvalidLinkIgnoreTarget { entry }) if entry == bad),
            "{bad:?} must be refused naming it, got {result:?}"
        );
        assert_eq!(
            result.as_ref().err().map(ConfigError::code),
            Some("config/invalid-link-ignore-target")
        );
    }
    Ok(())
}
