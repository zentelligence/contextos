//! `contextos service install|uninstall|status|start|stop|restart`: registers one per-user background `contextos` that
//! serves MCP over HTTP alone. The registered command is `contextos --config <path> --http [ADDR] --no-stdio`.

use std::path::PathBuf;

use contextos_service::cli::{ServiceAction, ServiceCliError, ServiceHost, ServiceRequest, run_on};
use contextos_service::{CommandRunner, ServiceIdentity};
use thiserror::Error;

use crate::config::Config;
use crate::config_load::{CliHttp, ConfigEnvironment, ConfigLoadInput};
use crate::http::{HttpTransportError, validate_bind};

/// The names `contextos` registers as a background service. They are what an installed service is found by, so they
/// never change.
pub const SERVICE_IDENTITY: ServiceIdentity = ServiceIdentity {
    program: "contextos",
    description: "ContextOS MCP server",
    unit_name: "contextos.service",
    launchd_label: "au.zentelligence.contextos",
    windows_name: "ContextOS MCP",
    config_file: "config.toml",
};

/// What a `contextos service` command acts on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceCommand {
    /// The configuration the service loads, resolved as the server resolves it.
    pub config_path: PathBuf,
    /// The global `--http [ADDR]` given with the command, if any.
    pub http: Option<CliHttp>,
}

/// Failures registering or controlling the service.
#[derive(Debug, Error)]
pub enum ServiceCommandError {
    #[error(
        "there is no configuration file at {path}, so the service was not installed; create it with `contextos \
         config`, or name the one to use with `--config`",
        path = path.display()
    )]
    NoConfigFile { path: PathBuf },
    #[error(
        "the configuration at {path} cannot serve HTTP alone, so the service was not installed: {reason}",
        path = path.display()
    )]
    ConfigRefused { path: PathBuf, reason: String },
    #[error(
        "the service was not installed: {source}. The service does not inherit this shell's environment, so a token \
         for a non-loopback address must be in the configuration, not in `CONTEXTOS_MCP_TOKEN`"
    )]
    BindRefused {
        #[source]
        source: HttpTransportError,
    },
    #[error(transparent)]
    Service(#[from] ServiceCliError),
}

/// The arguments the service runs with after `--config <path>`: HTTP alone, on the address given or else on the
/// configured bind.
impl From<&CliHttp> for Vec<String> {
    fn from(http: &CliHttp) -> Self {
        let mut args = vec!["--http".to_owned()];
        if let CliHttp::Bind(bind) = http {
            args.push(bind.clone());
        }
        args.push("--no-stdio".to_owned());
        args
    }
}

impl ServiceCommand {
    /// Refuses a service the server would not start as: the configuration is loaded exactly as the service will load
    /// it (this file, the HTTP override, no stdio), and its bind must pass the server's own check. The environment is
    /// left out on purpose: a service manager does not hand the service this shell's variables, so a token only this
    /// shell has would let a non-loopback bind pass here and fail at every start.
    fn check_registrable(&self) -> Result<(), ServiceCommandError> {
        if !self.config_path.is_file() {
            return Err(ServiceCommandError::NoConfigFile {
                path: self.config_path.clone(),
            });
        }
        let config = Config::try_from(ConfigLoadInput {
            cli_config_path: Some(self.config_path.clone()),
            cli_http: Some(self.http.clone().unwrap_or(CliHttp::ConfiguredBind)),
            cli_no_stdio: true,
            environment: ConfigEnvironment::default(),
            ..ConfigLoadInput::default()
        })
        .map_err(|error| ServiceCommandError::ConfigRefused {
            path: self.config_path.clone(),
            // The parse failure that says which line is wrong is the error's source, which a bare message omits.
            reason: std::error::Error::source(&error)
                .map_or_else(|| error.to_string(), |cause| format!("{error}: {cause}")),
        })?;
        // The service reads the variable `token_env` names from its own environment at every start and refuses to
        // start without it, so a declared variable counts as a configured token here.
        let http = &config.server.http;
        let token = if http.token.is_empty() {
            &http.token_env
        } else {
            &http.token
        };
        validate_bind(&http.bind, token).map_err(|source| ServiceCommandError::BindRefused { source })
    }
}

/// Carries out `action` on this host.
///
/// # Errors
///
/// [`ServiceCommandError`] when the action fails.
pub fn run_service_command(action: ServiceAction, command: &ServiceCommand) -> Result<String, ServiceCommandError> {
    let host = ServiceHost::detect(&SERVICE_IDENTITY)?;
    run_service_command_on(action, command, &host, &contextos_service::SystemCommandRunner)
}

/// As [`run_service_command`], on `host` and through `runner`.
///
/// # Errors
///
/// [`ServiceCommandError`] when the action fails.
pub fn run_service_command_on(
    action: ServiceAction,
    command: &ServiceCommand,
    host: &ServiceHost,
    runner: &dyn CommandRunner,
) -> Result<String, ServiceCommandError> {
    if matches!(action, ServiceAction::Install { .. }) {
        command.check_registrable()?;
    }
    let request = ServiceRequest {
        identity: SERVICE_IDENTITY,
        config_path: command.config_path.clone(),
        run_args: Vec::<String>::from(&command.http.clone().unwrap_or(CliHttp::ConfiguredBind)),
    };
    Ok(run_on(action, &request, host, runner)?)
}

/// Claims the process record for the process serving HTTP.
///
/// # Errors
///
/// The underlying [`std::io::Error`].
pub fn claim_process_record(
    config_path: &std::path::Path,
) -> std::io::Result<contextos_service::process_record::ProcessRecordGuard> {
    contextos_service::process_record::ProcessRecordGuard::claim(
        contextos_service::process_record::path_for(config_path),
        SERVICE_IDENTITY.program,
    )
}

#[cfg(test)]
#[path = "service_command_test.rs"]
mod tests;
