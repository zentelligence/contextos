//! Loading a [`Config`] from the trusted startup sources, in their documented precedence: the configuration file, then
//! environment variables, then command-line flags.

use std::fs;
use std::path::PathBuf;

use crate::config::{Config, ConfigError, ConfigLocation, ConfigProvenance, LogLevel, Transport, VaultConfig};

/// Environment-provided configuration values captured at process startup.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConfigEnvironment {
    pub config_path: Option<PathBuf>,
    pub token: Option<String>,
    pub log_level: Option<String>,
    /// The operator's home directory. Injected so nothing under it is resolved from the process environment during a
    /// test.
    pub home: Option<PathBuf>,
}

/// Trusted startup sources in documented precedence order.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConfigLoadInput {
    pub cli_config_path: Option<PathBuf>,
    pub default_config_path: Option<PathBuf>,
    pub cli_vaults: Vec<PathBuf>,
    pub cli_log_level: Option<LogLevel>,
    /// `--http [ADDR]`: serve HTTP on this run as well as the configured transports.
    pub cli_http: Option<CliHttp>,
    /// `--no-stdio`: serve no stdio on this run, whatever the file configures.
    pub cli_no_stdio: bool,
    pub environment: ConfigEnvironment,
}

/// Where `--http` asks this run to serve HTTP.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CliHttp {
    /// `--http` with no address: the bind in `[server.http]`, or its default.
    ConfiguredBind,
    /// `--http ADDR`: this address instead of the configured bind.
    Bind(String),
}

/// The command line gives `--http` with no address as an empty value.
impl From<String> for CliHttp {
    fn from(value: String) -> Self {
        if value.is_empty() {
            Self::ConfiguredBind
        } else {
            Self::Bind(value)
        }
    }
}

impl TryFrom<ConfigLoadInput> for Config {
    type Error = ConfigError;

    fn try_from(value: ConfigLoadInput) -> Result<Self, Self::Error> {
        let named = value.cli_config_path.or(value.environment.config_path.clone());
        let location = match (&named, &value.default_config_path) {
            (Some(_), _) => ConfigLocation::Named,
            (None, Some(_)) => ConfigLocation::PlatformDefault,
            (None, None) => ConfigLocation::Unspecified,
        };
        let config_path = named.or(value.default_config_path);
        let mut provenance = ConfigProvenance {
            location,
            path: config_path.clone(),
            home: value.environment.home.clone(),
        };
        let mut config = match config_path {
            Some(path) => match fs::read_to_string(&path) {
                Ok(source) => toml::from_str(&source).map_err(|source| ConfigError::Toml {
                    source: Box::new(source),
                })?,
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                    // Projection is on by default only for a file at the default location that exists and names the
                    // vaults itself. Without the file there is nothing to say the operator chose this set of vaults.
                    if provenance.location == ConfigLocation::PlatformDefault {
                        provenance.location = ConfigLocation::Unspecified;
                    }
                    Self::default()
                }
                Err(source) => return Err(ConfigError::Read { path, source }),
            },
            None => Self::default(),
        };

        config.provenance = provenance;
        // Checked before the environment can replace the file's token, so a file naming both is refused either way.
        config.server.http.check_token_sources()?;
        if let Some(token) = value.environment.token {
            // The documented variable replaces the file's token source, whichever form it took.
            config.server.http.token = token;
            config.server.http.token_env.clear();
        }
        if let Some(log_level) = value.environment.log_level {
            config.server.log_level = LogLevel::try_from(log_level.as_str())?;
        }
        if let Some(log_level) = value.cli_log_level {
            config.server.log_level = log_level;
        }
        if !value.cli_vaults.is_empty() {
            config.vaults = value.cli_vaults.into_iter().map(VaultConfig::from).collect();
            // Vaults named on the command line are a one-off, often disposable, selection. Agent host folders are
            // shared by every configuration on the machine, so such a run neither projects nor withdraws anything:
            // the file's `[projection]` section is set aside and the load no longer counts as the default location.
            config.projection = None;
            config.provenance.location = ConfigLocation::Unspecified;
        }
        if let Some(http) = value.cli_http {
            if !config.server.transports.contains(&Transport::Http) {
                config.server.transports.push(Transport::Http);
            }
            if let CliHttp::Bind(bind) = http {
                config.server.http.bind = bind;
            }
        }
        if value.cli_no_stdio {
            config
                .server
                .transports
                .retain(|transport| *transport != Transport::Stdio);
            if config.server.transports.is_empty() {
                return Err(ConfigError::NoStdioWithoutHttp);
            }
        }

        config.validate()?;
        Ok(config)
    }
}
