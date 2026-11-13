use contextos_core::VaultSet;
use contextos_mcp::{
    CliHttp, Config, ConfigEnvironment, ConfigError, ConfigLoadInput, ConfigLocation, EmbeddingProvider,
    GraphBackendConfig, LogLevel, Transport,
};
use tempfile::tempdir;

#[test]
fn configuration_applies_documented_defaults_and_builds_vault_set() -> Result<(), Box<dyn std::error::Error>> {
    // A prefixed builder, not the bare `tempdir()` default, so the
    // directory's basename is a valid RFC 3986 scheme token: building the
    // `VaultSet` derives the vault's name from that basename when the TOML
    // omits an explicit `name`, as it does here.
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let source = format!(
        r"
        [[vault]]
        path = {:?}
        ",
        vault.path()
    );

    let config = Config::try_from(source.as_str())?;
    let roots = VaultSet::try_from(&config)?;

    assert_eq!(config.server.transports, vec![Transport::Stdio]);
    assert_eq!(config.server.log_level, LogLevel::Info);
    assert_eq!(config.server.http.bind, "127.0.0.1:3737");
    assert_eq!(config.vaults[0].limits.max_read_mb, 5);
    assert_eq!(config.vaults[0].limits.max_batch_files, 50);
    assert!(config.vaults[0].managed);
    assert_eq!(
        config.vaults[0].git.restore_exclude,
        [
            "memory/log",
            "memory/sessions",
            "memory/coding",
            "memory/jobs",
            "memory/events"
        ]
        .map(std::path::PathBuf::from)
        .to_vec()
    );
    assert_eq!(config.vaults[0].search.embedding.provider, EmbeddingProvider::Local);
    assert_eq!(config.vaults[0].search.graph_backend, GraphBackendConfig::Sqlite);
    assert_eq!(config.vaults[0].search.embedding.model_directory, None);
    assert_eq!(config.server.resource_link_threshold_kb, 5);
    assert_eq!(config.vaults[0].search.rebuild_budget_seconds, 25);
    assert_eq!(roots.len(), 1);
    Ok(())
}

/// `[vault.search] graph_backend` accepts exactly `"serde"`, `"fjall"`,
/// and `"sqlite"`, additively alongside the existing `graph` boolean
/// rather than nested under it, so a config that sets `graph` without
/// `graph_backend` (the default test above) keeps parsing unchanged.
#[test]
fn graph_backend_accepts_every_documented_value() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    for (toml_value, expected) in [
        ("serde", GraphBackendConfig::Serde),
        ("fjall", GraphBackendConfig::Fjall),
        ("sqlite", GraphBackendConfig::Sqlite),
    ] {
        let source = format!(
            r#"
            [[vault]]
            path = {:?}

            [vault.search]
            graph_backend = "{toml_value}"
            "#,
            vault.path()
        );

        let config = Config::try_from(source.as_str())?;
        assert_eq!(config.vaults[0].search.graph_backend, expected);
    }

    Ok(())
}

/// A `graph_backend` value outside the three documented options is a hard
/// configuration error at parse time, not a silently ignored or defaulted
/// field, as an unknown key is refused.
#[test]
fn an_unrecognised_graph_backend_value_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let source = format!(
        r#"
        [[vault]]
        path = {:?}

        [vault.search]
        graph_backend = "leveldb"
        "#,
        vault.path()
    );

    let Err(_error) = Config::try_from(source.as_str()) else {
        return Err("expected an unrecognised graph_backend value to be rejected".into());
    };

    Ok(())
}

/// An explicit `name` in TOML threads through `VaultConfig` into the
/// resolved `VaultRoot`, not just the default-from-basename case the test
/// above covers.
#[test]
fn an_explicit_vault_name_threads_through_to_the_resolved_root() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let source = format!(
        r#"
        [[vault]]
        path = {:?}
        name = "mine"
        "#,
        vault.path()
    );

    let config = Config::try_from(source.as_str())?;
    let roots = VaultSet::try_from(&config)?;

    assert_eq!(config.vaults[0].name.as_deref(), Some("mine"));
    let (_, root) = roots.root_by_name("mine").ok_or("mine should resolve")?;
    assert_eq!(root.name(), "mine");
    Ok(())
}

#[test]
fn rebuild_budget_seconds_is_configurable() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let source = format!(
        r"
        [[vault]]
        path = {:?}
        [vault.search]
        rebuild_budget_seconds = 60
        ",
        vault.path()
    );

    let config = Config::try_from(source.as_str())?;

    assert_eq!(config.vaults[0].search.rebuild_budget_seconds, 60);
    Ok(())
}

#[test]
fn embedding_model_directory_is_configurable() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let source = format!(
        r#"
        [[vault]]
        path = {:?}
        [vault.search.embedding]
        provider = "local"
        model_directory = "/opt/contextos/models/all-MiniLM-L6-v2"
        "#,
        vault.path()
    );

    let config = Config::try_from(source.as_str())?;

    assert_eq!(
        config.vaults[0].search.embedding.model_directory,
        Some(std::path::PathBuf::from("/opt/contextos/models/all-MiniLM-L6-v2"))
    );
    Ok(())
}

#[test]
fn search_exclusions_default_independently_of_index_md_and_are_configurable() -> Result<(), Box<dyn std::error::Error>>
{
    let vault = tempdir()?;
    let default_source = format!(
        r"
        [[vault]]
        path = {:?}
        ",
        vault.path()
    );
    let default_config = Config::try_from(default_source.as_str())?;
    assert_eq!(
        default_config.vaults[0].search.exclude,
        default_config.vaults[0].index_md.exclude
    );

    let source = format!(
        r#"
        [[vault]]
        path = {:?}
        [vault.index_md]
        exclude = ["private-notes"]
        [vault.search]
        exclude = ["not-searchable"]
        "#,
        vault.path()
    );
    let config = Config::try_from(source.as_str())?;

    assert_eq!(config.vaults[0].index_md.exclude, ["private-notes"]);
    assert_eq!(config.vaults[0].search.exclude, ["not-searchable"]);
    Ok(())
}

#[test]
fn git_restore_exclusions_are_configurable_as_an_active_list() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let source = format!(
        r#"
        [[vault]]
        path = {:?}
        [vault.git]
        restore_exclude = ["journal/private", "memory/log"]
        "#,
        vault.path()
    );

    let config = Config::try_from(source.as_str())?;

    assert_eq!(
        config.vaults[0].git.restore_exclude,
        ["journal/private", "memory/log"].map(std::path::PathBuf::from).to_vec()
    );
    Ok(())
}

#[test]
fn resource_link_threshold_kb_is_configurable() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let source = format!(
        r"
        [[vault]]
        path = {:?}
        [server]
        resource_link_threshold_kb = 64
        ",
        vault.path()
    );

    let config = Config::try_from(source.as_str())?;

    assert_eq!(config.server.resource_link_threshold_kb, 64);
    Ok(())
}

#[test]
fn configuration_rejects_git_restore_exclusions_that_are_not_portable_relative_paths()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let source = format!(
        r#"
        [[vault]]
        path = {:?}
        [vault.git]
        restore_exclude = ["../outside"]
        "#,
        vault.path()
    );

    let result = Config::try_from(source.as_str());

    assert!(matches!(
        result,
        Err(ConfigError::InvalidRelativePath {
            field: "vault.git.restore_exclude",
            ..
        })
    ));
    Ok(())
}

/// Name validity and uniqueness are checked when the resolved `VaultSet`
/// is built, not by `Config::try_from`'s own `.validate()` (which only
/// checks limits and portable-relative-path fields); this is the error
/// path's own end-to-end proof, complementing the explicit-name threading
/// test above.
#[test]
fn duplicate_explicit_vault_names_are_rejected_when_the_vault_set_is_built() -> Result<(), Box<dyn std::error::Error>> {
    let first = tempdir()?;
    let second = tempdir()?;
    let source = format!(
        r#"
        [[vault]]
        path = {:?}
        name = "mine"
        [[vault]]
        path = {:?}
        name = "Mine"
        "#,
        first.path(),
        second.path()
    );

    let config = Config::try_from(source.as_str())?;
    let result = VaultSet::try_from(&config);

    assert!(matches!(result, Err(ConfigError::VaultSet { .. })));
    Ok(())
}

#[test]
fn configuration_rejects_unknown_keys() {
    let result = Config::try_from(
        r#"
        unexpected = true
        [[vault]]
        path = "/tmp"
        "#,
    );

    assert!(matches!(result, Err(ConfigError::Toml { .. })));
}

#[test]
fn configuration_permits_zero_configured_vaults() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::try_from("[server]\nlog_level = \"info\"")?;

    assert!(
        config.vaults.is_empty(),
        "`config.vaults` should be empty, got {:?}",
        config.vaults
    );
    Ok(())
}

#[test]
fn configuration_requires_at_least_one_transport() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let source = format!(
        r"
        [server]
        transports = []
        [[vault]]
        path = {:?}
        ",
        vault.path()
    );

    let result = Config::try_from(source.as_str());

    assert!(matches!(result, Err(ConfigError::NoTransports)));
    Ok(())
}

#[test]
fn configuration_rejects_zero_limits() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let source = format!(
        r"
        [[vault]]
        path = {:?}
        [vault.limits]
        max_read_mb = 0
        max_batch_files = 0
        ",
        vault.path()
    );

    let result = Config::try_from(source.as_str());

    assert!(matches!(result, Err(ConfigError::InvalidLimit { .. })));
    Ok(())
}

/// The inline threshold caps every byte part of `fs_read_text_file`, so a threshold of 0 would leave no file
/// readable in parts: it is refused when the configuration loads, never replaced by a default.
#[test]
fn a_zero_resource_link_threshold_is_refused_at_load() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let source = format!(
        r"
        [server]
        resource_link_threshold_kb = 0
        [[vault]]
        path = {:?}
        ",
        vault.path()
    );

    let result = Config::try_from(source.as_str());

    match result {
        Err(error @ ConfigError::InvalidLimit { field }) => {
            assert_eq!(field, "server.resource_link_threshold_kb");
            assert_eq!(error.code(), "config/invalid-limit");
            Ok(())
        }
        other => Err(format!("a zero threshold was not refused as an invalid limit: {other:?}").into()),
    }
}

#[test]
fn a_one_kilobyte_resource_link_threshold_is_accepted() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let source = format!(
        r"
        [server]
        resource_link_threshold_kb = 1
        [[vault]]
        path = {:?}
        ",
        vault.path()
    );

    let config = Config::try_from(source.as_str())?;

    assert_eq!(config.server.resource_link_threshold_kb, 1);
    Ok(())
}

#[test]
fn size_rotation_without_a_log_file_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let source = format!(
        r"
        [server]
        log_max_size_mb = 10
        [[vault]]
        path = {:?}
        ",
        vault.path()
    );

    let result = Config::try_from(source.as_str());

    assert!(matches!(result, Err(ConfigError::LogRotationWithoutFile)));
    Ok(())
}

#[test]
fn daily_rotation_without_a_log_file_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let source = format!(
        r"
        [server]
        log_rotate_daily = true
        [[vault]]
        path = {:?}
        ",
        vault.path()
    );

    let result = Config::try_from(source.as_str());

    assert!(matches!(result, Err(ConfigError::LogRotationWithoutFile)));
    Ok(())
}

#[test]
fn retention_without_a_log_file_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let source = format!(
        r"
        [server]
        log_retention_files = 5
        [[vault]]
        path = {:?}
        ",
        vault.path()
    );

    let result = Config::try_from(source.as_str());

    assert!(matches!(result, Err(ConfigError::LogRotationWithoutFile)));
    Ok(())
}

#[test]
fn a_zero_size_rotation_threshold_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let source = format!(
        r#"
        [server]
        log_file = "contextos.log"
        log_max_size_mb = 0
        [[vault]]
        path = {:?}
        "#,
        vault.path()
    );

    let result = Config::try_from(source.as_str());

    assert!(matches!(result, Err(ConfigError::InvalidLogRotation { .. })));
    Ok(())
}

#[test]
fn retention_with_no_rotation_trigger_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let source = format!(
        r#"
        [server]
        log_file = "contextos.log"
        log_retention_days = 7
        [[vault]]
        path = {:?}
        "#,
        vault.path()
    );

    let result = Config::try_from(source.as_str());

    assert!(matches!(result, Err(ConfigError::InvalidLogRotation { .. })));
    Ok(())
}

#[test]
fn size_and_daily_rotation_with_retention_is_accepted() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let source = format!(
        r#"
        [server]
        log_file = "contextos.log"
        log_max_size_mb = 10
        log_rotate_daily = true
        log_retention_days = 30
        log_retention_files = 10
        [[vault]]
        path = {:?}
        "#,
        vault.path()
    );

    let config = Config::try_from(source.as_str())?;

    assert_eq!(config.server.log_max_size_mb, Some(10));
    assert!(config.server.log_rotate_daily);
    assert_eq!(config.server.log_retention_days, Some(30));
    assert_eq!(config.server.log_retention_files, Some(10));
    Ok(())
}

#[test]
fn unmanaged_vault_retains_flag_in_domain_root() -> Result<(), Box<dyn std::error::Error>> {
    // See the same rationale in `configuration_applies_documented_defaults_and_builds_vault_set`.
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let source = format!(
        r"
        [[vault]]
        path = {:?}
        managed = false
        ",
        vault.path()
    );
    let config = Config::try_from(source.as_str())?;
    let roots = VaultSet::try_from(&config)?;

    let managed = roots.iter().next().map(contextos_core::VaultRoot::managed);
    assert_eq!(managed, Some(false));
    Ok(())
}

#[test]
fn environment_values_override_file_values() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault)?;
    let config_path = fixture.path().join("config.toml");
    std::fs::write(
        &config_path,
        format!("[server]\nlog_level = \"warn\"\n[server.http]\ntoken = \"file-token\"\n[[vault]]\npath = {vault:?}\n"),
    )?;

    let config = Config::try_from(ConfigLoadInput {
        cli_config_path: Some(config_path),
        environment: ConfigEnvironment {
            token: Some("environment-token".to_owned()),
            log_level: Some("debug".to_owned()),
            ..ConfigEnvironment::default()
        },
        ..ConfigLoadInput::default()
    })?;

    assert_eq!(config.server.log_level, LogLevel::Debug);
    assert_eq!(config.server.http.token, "environment-token");
    Ok(())
}

#[test]
fn cli_log_level_overrides_environment_value() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault)?;

    let config = Config::try_from(ConfigLoadInput {
        cli_vaults: vec![vault],
        cli_log_level: Some(LogLevel::Trace),
        environment: ConfigEnvironment {
            log_level: Some("debug".to_owned()),
            ..ConfigEnvironment::default()
        },
        ..ConfigLoadInput::default()
    })?;

    assert_eq!(config.server.log_level, LogLevel::Trace);
    Ok(())
}

#[test]
fn missing_config_file_is_ignored_when_cli_supplies_a_vault() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault)?;

    let config = Config::try_from(ConfigLoadInput {
        cli_config_path: Some(fixture.path().join("missing.toml")),
        cli_vaults: vec![vault.clone()],
        ..ConfigLoadInput::default()
    })?;

    assert_eq!(config.vaults.len(), 1);
    assert_eq!(config.vaults[0].path, vault);
    Ok(())
}

#[test]
fn missing_config_file_without_another_vault_source_starts_with_zero_vaults() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = tempdir()?;

    let config = Config::try_from(ConfigLoadInput {
        cli_config_path: Some(fixture.path().join("missing.toml")),
        ..ConfigLoadInput::default()
    })?;

    assert!(
        config.vaults.is_empty(),
        "`config.vaults` should be empty, got {:?}",
        config.vaults
    );
    Ok(())
}

#[test]
fn cli_config_path_precedes_environment_and_platform_paths() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let cli_vault = fixture.path().join("cli-vault");
    let environment_vault = fixture.path().join("environment-vault");
    let platform_vault = fixture.path().join("platform-vault");
    for vault in [&cli_vault, &environment_vault, &platform_vault] {
        std::fs::create_dir(vault)?;
    }
    let cli_path = fixture.path().join("cli.toml");
    let environment_path = fixture.path().join("environment.toml");
    let platform_path = fixture.path().join("platform.toml");
    std::fs::write(&cli_path, format!("[[vault]]\npath = {cli_vault:?}\n"))?;
    std::fs::write(&environment_path, format!("[[vault]]\npath = {environment_vault:?}\n"))?;
    std::fs::write(&platform_path, format!("[[vault]]\npath = {platform_vault:?}\n"))?;

    let config = Config::try_from(ConfigLoadInput {
        cli_config_path: Some(cli_path),
        default_config_path: Some(platform_path),
        environment: ConfigEnvironment {
            config_path: Some(environment_path),
            ..ConfigEnvironment::default()
        },
        ..ConfigLoadInput::default()
    })?;

    assert_eq!(config.vaults[0].path, cli_vault);
    Ok(())
}

#[test]
fn invalid_environment_log_level_is_a_typed_startup_error() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault)?;

    let result = Config::try_from(ConfigLoadInput {
        cli_vaults: vec![vault],
        environment: ConfigEnvironment {
            log_level: Some("verbose".to_owned()),
            ..ConfigEnvironment::default()
        },
        ..ConfigLoadInput::default()
    });

    assert!(matches!(
        result,
        Err(ConfigError::InvalidEnvironmentValue {
            variable: "CONTEXTOS_MCP_LOG_LEVEL",
            ..
        })
    ));
    Ok(())
}

#[test]
fn documented_example_configuration_parses_under_deny_unknown_fields() -> Result<(), Box<dyn std::error::Error>> {
    let example_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/config.example.toml");
    let source = std::fs::read_to_string(example_path)?;

    let config = Config::try_from(source.as_str())?;

    assert_eq!(config.vaults.len(), 2);
    Ok(())
}

#[test]
fn documented_projection_section_states_exactly_the_defaults() -> Result<(), Box<dyn std::error::Error>> {
    let example_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/config.example.toml");
    let source = std::fs::read_to_string(example_path)?;

    let config = Config::try_from(source.as_str())?;

    let documented = config.projection.ok_or("the example has no [projection] section")?;
    let mut defaults = contextos_mcp::ProjectionConfig {
        enabled: Some(true),
        ..contextos_mcp::ProjectionConfig::default()
    };
    // The example spells out each Codex tier's effort, which the defaults leave to the tier.
    defaults.codex.tiers.fast.effort = Some("low".to_owned());
    defaults.codex.tiers.balanced.effort = Some("medium".to_owned());
    defaults.codex.tiers.deep.effort = Some("high".to_owned());
    assert_eq!(documented, defaults);
    Ok(())
}

fn provenance_fixture(
    cli: bool,
    environment: bool,
    default: bool,
) -> Result<(Config, std::path::PathBuf, std::path::PathBuf, tempfile::TempDir), Box<dyn std::error::Error>> {
    let directory = tempdir()?;
    let fixture = directory.path().to_path_buf();
    let vault = fixture.join("vault");
    std::fs::create_dir(&vault)?;
    let body = format!("[[vault]]\npath = {:?}\n", vault.display().to_string());
    let named = |name: &str| -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
        let path = fixture.join(name);
        std::fs::write(&path, &body)?;
        Ok(path)
    };
    let home = fixture.join("home");
    let cli_path = named("cli.toml")?;
    let environment_path = named("environment.toml")?;
    let default_path = named("default.toml")?;
    let config = Config::try_from(ConfigLoadInput {
        cli_config_path: cli.then(|| cli_path.clone()),
        default_config_path: default.then(|| default_path.clone()),
        environment: ConfigEnvironment {
            config_path: environment.then(|| environment_path.clone()),
            home: Some(home.clone()),
            ..ConfigEnvironment::default()
        },
        ..ConfigLoadInput::default()
    })?;
    let chosen = if cli {
        cli_path
    } else if environment {
        environment_path
    } else {
        default_path
    };
    Ok((config, chosen, home, directory))
}

#[test]
fn a_configuration_named_by_flag_or_environment_is_recorded_as_named() -> Result<(), Box<dyn std::error::Error>> {
    for (cli, environment) in [(true, false), (false, true), (true, true)] {
        let (config, chosen, home, _directory) = provenance_fixture(cli, environment, true)?;

        assert_eq!(config.provenance.location, ConfigLocation::Named);
        assert_eq!(config.provenance.path, Some(chosen));
        assert_eq!(config.provenance.home, Some(home));
    }
    Ok(())
}

#[test]
fn a_configuration_found_only_at_the_platform_default_is_recorded_as_such() -> Result<(), Box<dyn std::error::Error>> {
    let (config, chosen, _, _directory) = provenance_fixture(false, false, true)?;

    assert_eq!(config.provenance.location, ConfigLocation::PlatformDefault);
    assert_eq!(config.provenance.path, Some(chosen));
    Ok(())
}

#[test]
fn a_configuration_built_from_command_line_vaults_alone_has_no_provenance() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = tempdir()?;
    let vault = fixture.path().join("vault");
    std::fs::create_dir(&vault)?;

    let config = Config::try_from(ConfigLoadInput {
        cli_vaults: vec![vault],
        ..ConfigLoadInput::default()
    })?;

    assert_eq!(config.provenance.location, ConfigLocation::Unspecified);
    assert_eq!(config.provenance.path, None);
    Ok(())
}

#[test]
fn a_projection_section_in_the_file_is_read_and_its_unknown_keys_are_refused() -> Result<(), Box<dyn std::error::Error>>
{
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let valid = format!(
        "[[vault]]\npath = {:?}\n\n[projection]\nenabled = true\n\n[projection.claude]\nmcp_server = \"vault-os\"\n",
        vault.path()
    );
    let unknown = format!("{valid}\n[projection.claude.tiers]\nsmall = \"haiku\"\n");

    let config = Config::try_from(valid.as_str())?;
    let projection = config.projection.ok_or("the projection section was not read")?;
    let refused = Config::try_from(unknown.as_str());

    assert_eq!(projection.enabled, Some(true));
    assert_eq!(projection.claude.mcp_server, "vault-os");
    assert_eq!(projection.claude.agents, std::path::PathBuf::from("~/.claude/agents"));
    assert!(matches!(refused, Err(ConfigError::Toml { .. })));
    Ok(())
}

#[test]
fn every_configuration_error_carries_a_stable_code() {
    assert_eq!(ConfigError::NoVaults.code(), "config/no-vaults");
    assert_eq!(
        ConfigError::ProjectionHomeUnavailable.code(),
        "projection/home-unavailable"
    );
}

#[test]
fn http_on_the_command_line_adds_the_http_transport_on_the_configured_bind() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::try_from(ConfigLoadInput {
        cli_http: Some(CliHttp::ConfiguredBind),
        ..ConfigLoadInput::default()
    })?;

    assert_eq!(config.server.transports, vec![Transport::Stdio, Transport::Http]);
    assert_eq!(config.server.http.bind, "127.0.0.1:3737");
    Ok(())
}

#[test]
fn http_on_the_command_line_with_an_address_replaces_the_configured_bind() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::try_from(ConfigLoadInput {
        cli_http: Some(CliHttp::from("127.0.0.1:9000".to_owned())),
        ..ConfigLoadInput::default()
    })?;

    assert_eq!(config.server.transports, vec![Transport::Stdio, Transport::Http]);
    assert_eq!(config.server.http.bind, "127.0.0.1:9000");
    Ok(())
}

#[test]
fn http_on_the_command_line_with_no_address_names_the_configured_bind() {
    assert_eq!(CliHttp::from(String::new()), CliHttp::ConfiguredBind);
    assert_eq!(
        CliHttp::from("127.0.0.1:9000".to_owned()),
        CliHttp::Bind("127.0.0.1:9000".to_owned())
    );
}

#[test]
fn http_on_the_command_line_is_not_added_twice_when_the_file_already_serves_it()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempdir()?;
    let config_path = directory.path().join("config.toml");
    std::fs::write(&config_path, "[server]\ntransports = [\"stdio\", \"http\"]\n")?;

    let config = Config::try_from(ConfigLoadInput {
        cli_config_path: Some(config_path),
        cli_http: Some(CliHttp::ConfiguredBind),
        ..ConfigLoadInput::default()
    })?;

    assert_eq!(config.server.transports, vec![Transport::Stdio, Transport::Http]);
    Ok(())
}

#[test]
fn no_stdio_with_http_on_the_command_line_serves_http_alone() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::try_from(ConfigLoadInput {
        cli_http: Some(CliHttp::ConfiguredBind),
        cli_no_stdio: true,
        ..ConfigLoadInput::default()
    })?;

    assert_eq!(config.server.transports, vec![Transport::Http]);
    Ok(())
}

#[test]
fn no_stdio_keeps_the_http_transport_the_file_configures() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempdir()?;
    let config_path = directory.path().join("config.toml");
    std::fs::write(&config_path, "[server]\ntransports = [\"stdio\", \"http\"]\n")?;

    let config = Config::try_from(ConfigLoadInput {
        cli_config_path: Some(config_path),
        cli_no_stdio: true,
        ..ConfigLoadInput::default()
    })?;

    assert_eq!(config.server.transports, vec![Transport::Http]);
    Ok(())
}

#[test]
fn no_stdio_with_no_http_transport_is_refused_with_how_to_serve_http() -> Result<(), Box<dyn std::error::Error>> {
    let error = Config::try_from(ConfigLoadInput {
        cli_no_stdio: true,
        ..ConfigLoadInput::default()
    })
    .err()
    .ok_or("--no-stdio with nothing else to serve should be refused")?;

    assert!(matches!(error, ConfigError::NoStdioWithoutHttp), "{error:?}");
    assert_eq!(error.code(), "config/no-transports");
    let message = error.to_string();
    assert!(message.contains("--no-stdio"), "{message}");
    assert!(message.contains("--http"), "{message}");
    Ok(())
}

fn load_with_environment_token(http: &str) -> Result<Config, contextos_mcp::ConfigError> {
    let fixture = tempdir().map_err(|source| contextos_mcp::ConfigError::Read {
        path: std::path::PathBuf::new(),
        source,
    })?;
    let config_path = fixture.path().join("config.toml");
    std::fs::write(&config_path, format!("[server.http]\n{http}")).map_err(|source| {
        contextos_mcp::ConfigError::Read {
            path: config_path.clone(),
            source,
        }
    })?;
    Config::try_from(ConfigLoadInput {
        cli_config_path: Some(config_path),
        environment: ConfigEnvironment {
            token: Some("environment-token".to_owned()),
            ..ConfigEnvironment::default()
        },
        ..ConfigLoadInput::default()
    })
}

#[test]
fn the_documented_token_variable_overrides_a_token_env_in_the_file() -> Result<(), Box<dyn std::error::Error>> {
    let config = load_with_environment_token("token_env = \"SOME_UNSET_TOKEN_VARIABLE\"\n")?;

    assert_eq!(config.server.http.resolve_token(|_name| None)?, "environment-token");
    Ok(())
}

#[test]
fn a_file_setting_both_token_and_token_env_is_refused_even_under_the_documented_variable() {
    let error = load_with_environment_token("token = \"file-token\"\ntoken_env = \"SOME_TOKEN\"\n").err();

    assert_eq!(
        error.as_ref().map(contextos_mcp::ConfigError::code),
        Some("config/conflicting-secret")
    );
}
