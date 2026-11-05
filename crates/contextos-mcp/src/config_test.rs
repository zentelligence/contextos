use super::*;
use crate::config_load::{ConfigEnvironment, ConfigLoadInput};

/// `memory/jobs` is structured, high-volume, machine-written run
/// telemetry: excluded from managed `index.md` generation and the search
/// corpus (text, graph, and semantic together, there being no
/// semantic-only knob) by default, matching `memory/log`'s existing
/// treatment rather than `memory/coding`'s.
#[test]
fn default_content_excludes_covers_memory_jobs() {
    let excludes = default_content_excludes();
    assert!(
        excludes.iter().any(|path| path == "memory/jobs"),
        "expected \"memory/jobs\" in the default content-exclude list, got {excludes:?}"
    );
}

#[test]
fn index_md_config_default_excludes_memory_jobs() {
    assert!(
        IndexMdConfig::default()
            .exclude
            .iter()
            .any(|path| path == "memory/jobs")
    );
}

#[test]
fn search_config_default_excludes_memory_jobs() {
    assert!(SearchConfig::default().exclude.iter().any(|path| path == "memory/jobs"));
}

/// `memory/jobs/{{job-name}}/archive.md` is an append-only retention
/// ledger, the same shape as `memory/log`'s daily oplog files
/// and `memory/coding`'s diary: a `git_restore` that rolled it back to an
/// older committed version would silently discard every ledger line
/// appended since, the same corruption `restore_exclude` already guards
/// against for those two stores.
#[test]
fn default_git_restore_excludes_covers_memory_jobs() {
    let excludes = default_git_restore_excludes();
    assert!(
        excludes.iter().any(|path| path == std::path::Path::new("memory/jobs")),
        "expected \"memory/jobs\" in the default git restore-exclude list, got {excludes:?}"
    );
}

#[test]
fn git_config_default_excludes_memory_jobs_from_restore() {
    assert!(
        GitConfig::default()
            .restore_exclude
            .iter()
            .any(|path| path == std::path::Path::new("memory/jobs"))
    );
}

/// Event notes under `memory/events` are history, not knowledge: they stay out of the search corpus and get no managed
/// `index.md` per month folder, as `memory/log` does.
#[test]
fn default_content_excludes_cover_memory_events() {
    let excludes = default_content_excludes();
    assert!(
        excludes.iter().any(|path| path == "memory/events"),
        "expected \"memory/events\" in the default content-exclude list, got {excludes:?}"
    );
    assert!(
        IndexMdConfig::default()
            .exclude
            .iter()
            .any(|path| path == "memory/events")
    );
    assert!(
        SearchConfig::default()
            .exclude
            .iter()
            .any(|path| path == "memory/events")
    );
}

/// A `git_restore` that rolled `memory/events` back would erase history the Signals screen and the attention indicators
/// rely on, so it is excluded from restore by default, as `memory/jobs` is.
#[test]
fn default_git_restore_excludes_cover_memory_events() {
    let excludes = GitConfig::default().restore_exclude;
    assert!(
        excludes
            .iter()
            .any(|path| path == std::path::Path::new("memory/events")),
        "expected \"memory/events\" in the default git restore-exclude list, got {excludes:?}"
    );
}

#[test]
fn a_configured_iana_time_zone_is_accepted() -> Result<(), ConfigError> {
    let config = Config::try_from("[server]\ntimezone = \"Australia/Sydney\"\n")?;

    assert_eq!(config.server.timezone.as_deref(), Some("Australia/Sydney"));
    Ok(())
}

#[test]
fn no_time_zone_is_configured_by_default() -> Result<(), ConfigError> {
    let config = Config::try_from("[server]\n")?;

    assert_eq!(config.server.timezone, None);
    Ok(())
}

#[test]
fn an_unknown_time_zone_is_refused_naming_the_value() {
    let error = Config::try_from("[server]\ntimezone = \"Australia/Atlantis\"\n").err();

    assert!(
        matches!(&error, Some(ConfigError::TimeZone { source }) if source.code() == "config/time-zone-unknown"),
        "{error:?}"
    );
    assert!(
        error.is_some_and(|error| error.to_string().contains("Australia/Atlantis")),
        "the message names the refused value"
    );
}

#[test]
fn a_fixed_offset_is_refused_as_a_time_zone() {
    let error = Config::try_from("[server]\ntimezone = \"+10:00\"\n").err();

    assert!(
        matches!(&error, Some(ConfigError::TimeZone { source }) if source.code() == "config/time-zone-malformed"),
        "{error:?}"
    );
}

#[test]
fn the_default_http_body_cap_is_the_shared_request_body_limit() {
    assert_eq!(
        HttpConfig::default().max_body_kb * 1024,
        contextos_core::MAX_REQUEST_BODY_BYTES
    );
}

mod command_line_vaults {
    use std::path::PathBuf;

    use super::*;
    use crate::projection_setup::ProjectionSetup;

    struct Fixture {
        _directory: tempfile::TempDir,
        home: PathBuf,
        vault: PathBuf,
        file: PathBuf,
    }

    fn fixture(file_contents: &str) -> Result<Fixture, Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let base = dunce::canonicalize(directory.path())?;
        let home = base.join("home");
        let vault = base.join("vault");
        std::fs::create_dir_all(home.join(".config/contextos"))?;
        std::fs::create_dir_all(&vault)?;
        let file = home.join(".config/contextos/config.toml");
        std::fs::write(&file, file_contents)?;
        Ok(Fixture {
            _directory: directory,
            home,
            vault,
            file,
        })
    }

    fn load(fixture: &Fixture, named: bool) -> Result<Config, ConfigError> {
        Config::try_from(ConfigLoadInput {
            cli_config_path: named.then(|| fixture.file.clone()),
            default_config_path: Some(fixture.file.clone()),
            cli_vaults: vec![fixture.vault.clone()],
            cli_log_level: None,
            cli_http: None,
            cli_no_stdio: false,
            environment: ConfigEnvironment {
                home: Some(fixture.home.clone()),
                ..ConfigEnvironment::default()
            },
        })
    }

    #[test]
    fn vaults_from_the_command_line_disable_projection_for_a_default_location_file_without_a_section()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = fixture("")?;

        let setup = ProjectionSetup::try_from(&load(&fixture, false)?)?;

        assert!(matches!(setup, ProjectionSetup::Disabled), "{setup:?}");
        Ok(())
    }

    #[test]
    fn vaults_from_the_command_line_disable_projection_even_when_the_file_enables_it()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = fixture("[projection]\nenabled = true\n")?;

        for named in [false, true] {
            let setup = ProjectionSetup::try_from(&load(&fixture, named)?)?;
            assert!(matches!(setup, ProjectionSetup::Disabled), "named={named}: {setup:?}");
        }
        Ok(())
    }

    #[test]
    fn vaults_from_the_file_still_project_by_default_at_the_default_location() -> Result<(), Box<dyn std::error::Error>>
    {
        let fixture = fixture("")?;
        std::fs::write(
            &fixture.file,
            format!(
                "[[vault]]\npath = {}\n",
                toml::Value::String(fixture.vault.display().to_string())
            ),
        )?;
        let config = Config::try_from(ConfigLoadInput {
            cli_config_path: None,
            default_config_path: Some(fixture.file.clone()),
            cli_vaults: Vec::new(),
            cli_log_level: None,
            cli_http: None,
            cli_no_stdio: false,
            environment: ConfigEnvironment {
                home: Some(fixture.home.clone()),
                ..ConfigEnvironment::default()
            },
        })?;

        let setup = ProjectionSetup::try_from(&config)?;

        assert!(matches!(setup, ProjectionSetup::Enabled(_)), "{setup:?}");
        Ok(())
    }
}

mod default_location_without_a_vault_set {
    use std::path::PathBuf;

    use super::*;
    use crate::projection_setup::ProjectionSetup;

    fn load(home: &std::path::Path, default_file: PathBuf) -> Result<ProjectionSetup, Box<dyn std::error::Error>> {
        let config = Config::try_from(ConfigLoadInput {
            cli_config_path: None,
            default_config_path: Some(default_file),
            cli_vaults: Vec::new(),
            cli_log_level: None,
            cli_http: None,
            cli_no_stdio: false,
            environment: ConfigEnvironment {
                home: Some(home.to_path_buf()),
                ..ConfigEnvironment::default()
            },
        })?;
        Ok(ProjectionSetup::try_from(&config)?)
    }

    #[test]
    fn no_file_at_the_default_location_means_projection_is_off() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let home = dunce::canonicalize(directory.path())?;

        let setup = load(&home, home.join(".config/contextos/config.toml"))?;

        assert!(matches!(setup, ProjectionSetup::Disabled), "{setup:?}");
        Ok(())
    }

    #[test]
    fn a_file_with_no_vault_set_still_projects_when_it_asks_to() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let home = dunce::canonicalize(directory.path())?;
        let file = home.join(".config/contextos/config.toml");
        std::fs::create_dir_all(home.join(".config/contextos"))?;
        std::fs::write(&file, "[projection]\nenabled = true\n")?;

        let setup = load(&home, file)?;

        assert!(matches!(setup, ProjectionSetup::Enabled(_)), "{setup:?}");
        Ok(())
    }
}

/// A vault configuration whose `[vault.search.embedding]` table holds `embedding`.
fn embedding_config(embedding: &str) -> Result<Config, ConfigError> {
    let vault = std::env::temp_dir();
    Config::try_from(format!("[[vault]]\npath = {vault:?}\n[vault.search.embedding]\n{embedding}").as_str())
}

#[test]
fn an_embedding_key_written_in_the_file_is_accepted() -> Result<(), ConfigError> {
    let config = embedding_config("provider = \"openai-compatible\"\napi_key = \"sk-in-the-file\"\n")?;

    assert_eq!(config.vaults[0].search.embedding.api_key, "sk-in-the-file");
    assert_eq!(config.vaults[0].search.embedding.api_key_env, "");
    Ok(())
}

#[test]
fn an_embedding_key_and_its_environment_variable_together_are_refused_naming_both() {
    let error = embedding_config("api_key = \"sk-in-the-file\"\napi_key_env = \"OPENAI_API_KEY\"\n").err();

    assert_eq!(error.as_ref().map(ConfigError::code), Some("config/conflicting-secret"));
    let message = error.map(|error| error.to_string()).unwrap_or_default();
    assert!(message.contains("api_key and api_key_env"), "{message}");
    assert!(
        !message.contains("sk-in-the-file"),
        "the refusal never echoes the key: {message}"
    );
}

#[test]
fn debug_output_of_a_configuration_never_shows_its_secrets() -> Result<(), ConfigError> {
    let mut config = embedding_config("api_key = \"sk-debug-must-not-appear\"\n")?;
    config.server.http.token = "token-debug-must-not-appear".to_owned();

    let debug = format!("{config:?}");

    assert!(!debug.contains("sk-debug-must-not-appear"), "{debug}");
    assert!(!debug.contains("token-debug-must-not-appear"), "{debug}");
    assert!(
        debug.contains("api_key_env"),
        "the rest of the configuration is still shown: {debug}"
    );
    Ok(())
}

#[test]
fn an_http_token_and_its_environment_variable_together_are_refused_naming_both() {
    let error = Config::try_from("[server.http]\ntoken = \"tok-in-the-file\"\ntoken_env = \"SOME_TOKEN\"\n").err();

    assert_eq!(error.as_ref().map(ConfigError::code), Some("config/conflicting-secret"));
    let message = error.map(|error| error.to_string()).unwrap_or_default();
    assert!(message.contains("token and token_env"), "{message}");
    assert!(
        !message.contains("tok-in-the-file"),
        "the refusal never echoes the token: {message}"
    );
}

#[test]
fn an_http_token_written_in_the_file_is_used_as_it_is() -> Result<(), ConfigError> {
    let config = Config::try_from("[server.http]\ntoken = \"tok-in-the-file\"\n")?;

    assert_eq!(config.server.http.resolve_token(|_name| None)?, "tok-in-the-file");
    Ok(())
}

#[test]
fn an_http_token_variable_is_read_from_the_environment_it_is_resolved_in() -> Result<(), ConfigError> {
    let config = Config::try_from("[server.http]\ntoken_env = \"SOME_TOKEN\"\n")?;

    let token = config
        .server
        .http
        .resolve_token(|name| (name == "SOME_TOKEN").then(|| "tok-from-the-variable".to_owned()))?;

    assert_eq!(token, "tok-from-the-variable");
    Ok(())
}

#[test]
fn an_http_token_variable_that_is_unset_or_empty_is_refused_naming_it() -> Result<(), ConfigError> {
    let config = Config::try_from("[server.http]\ntoken_env = \"SOME_TOKEN\"\n")?;

    for lookup in [None, Some(String::new())] {
        let error = config.server.http.resolve_token(|_name| lookup.clone()).err();

        assert_eq!(
            error.as_ref().map(ConfigError::code),
            Some("config/missing-secret-variable")
        );
        assert!(
            error.is_some_and(|error| error.to_string().contains("SOME_TOKEN")),
            "the refusal names the variable"
        );
    }
    Ok(())
}

#[test]
fn no_http_token_at_all_resolves_to_none() -> Result<(), ConfigError> {
    let config = Config::try_from("[server.http]\n")?;

    assert_eq!(config.server.http.resolve_token(|_name| None)?, "");
    Ok(())
}

fn events_config(events: &str) -> Result<Config, ConfigError> {
    let vault = std::env::temp_dir();
    Config::try_from(format!("[[vault]]\npath = {vault:?}\n{events}").as_str())
}

#[test]
fn events_default_to_ninety_days_and_keeping_attention() -> Result<(), ConfigError> {
    let config = events_config("")?;

    assert_eq!(config.vaults[0].events.retention_days, 90);
    assert!(config.vaults[0].events.keep_attention);
    assert_eq!(EventsConfig::default(), config.vaults[0].events);
    Ok(())
}

#[test]
fn events_settings_are_read_from_the_vault_events_table() -> Result<(), ConfigError> {
    let config = events_config("[vault.events]\nretention_days = 30\nkeep_attention = false\n")?;

    assert_eq!(config.vaults[0].events.retention_days, 30);
    assert!(!config.vaults[0].events.keep_attention);
    Ok(())
}

#[test]
fn a_zero_retention_window_is_refused_with_its_own_code() {
    let refused = events_config("[vault.events]\nretention_days = 0\n").err();

    assert_eq!(refused.map(|error| error.code()), Some("config/invalid-retention"));
}

#[test]
fn an_unknown_events_setting_is_refused_rather_than_ignored() {
    assert!(matches!(
        events_config("[vault.events]\nretain = 5\n"),
        Err(ConfigError::Toml { .. })
    ));
}
