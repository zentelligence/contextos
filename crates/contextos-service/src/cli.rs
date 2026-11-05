//! `<program> service ...`: resolves this host's paths into a [`ServiceSpec`], runs the requested action against the
//! platform's [`ServiceBackend`](super::ServiceBackend), and returns the sentence the command prints. Blocking: it
//! runs platform commands and reads files, so a binary calls it on a blocking thread.

use std::io;
use std::path::{Path, PathBuf};

use clap::Subcommand;
use thiserror::Error;

use super::{
    CommandRunner, ControlAction, InstallChoice, ServiceError, ServiceIdentity, ServiceSpec, SystemCommandRunner,
    UninstallOutcome, WindowsMechanism, control, process_record,
};

/// A `<program> service` subcommand.
#[derive(Clone, Copy, Debug, Subcommand)]
pub enum ServiceAction {
    /// Install (or re-install) and start the service.
    Install {
        /// Windows only: register a Scheduled Task instead of a Startup App. Needs an Administrator terminal, to
        /// install and to uninstall.
        #[arg(long)]
        scheduled_task: bool,
    },
    /// Stop and remove the service.
    Uninstall,
    /// Report whether the service is installed and running, or stopped unexpectedly.
    Status,
    /// Start the installed service, for example after it stopped unexpectedly.
    Start,
    /// Stop the installed service until it is next started or you next log in.
    Stop,
    /// Stop the installed service if it is running, then start it.
    Restart,
}

/// Which service a command acts on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceRequest {
    /// Who the service is.
    pub identity: ServiceIdentity,
    /// The configuration file the service loads; made absolute before it is registered.
    pub config_path: PathBuf,
    /// The arguments the service runs with after `--config <path>`.
    pub run_args: Vec<String>,
}

/// The host a command runs on: the paths and platform every backend is anchored to. Passed in, never resolved by a
/// backend, so a test points it at a temporary directory instead of the operator's home.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServiceHost {
    /// The platform, as `std::env::consts::OS` names it.
    pub os: &'static str,
    /// The binary the service runs.
    pub binary_path: PathBuf,
    /// The user's home directory.
    pub home_dir: PathBuf,
    /// The user's configuration directory.
    pub config_dir: PathBuf,
    /// Windows' `System32` folder; empty elsewhere.
    pub system_dir: PathBuf,
}

/// Failures resolving this host's paths, or carrying out the action.
#[derive(Debug, Error)]
pub enum ServiceCliError {
    #[error("could not resolve the {program} executable or the {config_file} path: {source}")]
    Path {
        program: &'static str,
        config_file: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("could not determine the current user's home directory")]
    NoHomeDirectory,
    #[error(transparent)]
    Service(#[from] ServiceError),
}

impl ServiceHost {
    /// The host this process is running on.
    ///
    /// # Errors
    ///
    /// [`ServiceCliError::Path`] when this process's executable cannot be named, and
    /// [`ServiceCliError::NoHomeDirectory`] when the user has no home directory.
    pub fn detect(identity: &ServiceIdentity) -> Result<Self, ServiceCliError> {
        let folders = contextos_host::HostFolders::detect().ok_or(ServiceCliError::NoHomeDirectory)?;
        Ok(Self {
            os: std::env::consts::OS,
            binary_path: std::env::current_exe().map_err(|source| ServiceCliError::Path {
                program: identity.program,
                config_file: identity.config_file,
                source,
            })?,
            home_dir: folders.home().to_path_buf(),
            config_dir: folders.config().to_path_buf(),
            // Where the Windows backends find every tool they run; unused off Windows.
            system_dir: std::env::var_os("SystemRoot")
                .map_or_else(PathBuf::new, |root| PathBuf::from(root).join("System32")),
        })
    }
}

/// Carries out `action` for the service described by `request` on this host, returning what to print.
///
/// # Errors
///
/// [`ServiceCliError`] when a path cannot be resolved, or the action fails.
pub fn run(action: ServiceAction, request: &ServiceRequest) -> Result<String, ServiceCliError> {
    run_on(
        action,
        request,
        &ServiceHost::detect(&request.identity)?,
        &SystemCommandRunner,
    )
}

/// As [`run`], on `host` and through `runner`.
///
/// # Errors
///
/// [`ServiceCliError`] when a path cannot be resolved, or the action fails.
pub fn run_on(
    action: ServiceAction,
    request: &ServiceRequest,
    host: &ServiceHost,
    runner: &dyn CommandRunner,
) -> Result<String, ServiceCliError> {
    let identity = request.identity;
    let windows = WindowsMechanism::try_from(InstallChoice {
        identity,
        os: host.os,
        scheduled_task: matches!(action, ServiceAction::Install { scheduled_task: true }),
    })?;
    let config_path = absolute(&request.config_path, &identity)?;
    let spec = ServiceSpec {
        identity,
        binary_path: host.binary_path.clone(),
        process_record: process_record::path_for(&config_path),
        config_path,
        run_args: request.run_args.clone(),
        home_dir: host.home_dir.clone(),
        config_dir: host.config_dir.clone(),
        system_dir: host.system_dir.clone(),
    };
    let backend = super::backend_for_os(identity, host.os, windows)?;
    let program = identity.program;
    Ok(match action {
        ServiceAction::Install { .. } => {
            backend.install(runner, &spec)?;
            format!("Installed and started the {program} service.")
        }
        ServiceAction::Uninstall => match backend.uninstall(runner, &spec)? {
            UninstallOutcome::Removed => format!("Removed the {program} service."),
            UninstallOutcome::NotInstalled => format!("The {program} service was not installed."),
        },
        ServiceAction::Status => backend.status(runner, &spec)?.message(&identity),
        ServiceAction::Start => control(&*backend, runner, &spec, ControlAction::Start)?.message(&identity),
        ServiceAction::Stop => control(&*backend, runner, &spec, ControlAction::Stop)?.message(&identity),
        ServiceAction::Restart => control(&*backend, runner, &spec, ControlAction::Restart)?.message(&identity),
    })
}

fn absolute(config_path: &Path, identity: &ServiceIdentity) -> Result<PathBuf, ServiceCliError> {
    std::path::absolute(config_path).map_err(|source| ServiceCliError::Path {
        program: identity.program,
        config_file: identity.config_file,
        source,
    })
}

#[cfg(test)]
#[path = "cli_test.rs"]
mod tests;
