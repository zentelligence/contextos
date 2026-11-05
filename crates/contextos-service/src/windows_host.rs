//! The Windows service: whichever of the two Windows mechanisms is installed, so `uninstall`, `status`, `start`,
//! `stop`, and `restart` need no reminder of which one the operator chose. `install` registers the chosen one and
//! removes the other: both registered would start two servers at logon, the second failing on the same port. The step
//! that needs an Administrator terminal (registering or removing the Scheduled Task) always comes first, so refusing
//! it leaves the working installation as it was.
//!
//! Every command first checks that an installed registration runs the server for this command's configuration file: a command
//! run with another `--config` would otherwise look for a server it cannot find, and an `install` would replace the
//! registration while that other server kept running with nothing left to stop it.

use super::windows_process::{registered_for, server_state};
use super::{
    CommandRunner, RunState, ScheduledTaskBackend, ServiceBackend, ServiceError, ServiceIdentity, ServiceSpec,
    ServiceStatus, StartupAppBackend, UninstallOutcome,
};

/// How a Windows install registers the service.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WindowsMechanism {
    /// A per-user Startup App: no Administrator needed.
    #[default]
    StartupApp,
    /// A per-user Scheduled Task: installing or removing it needs an Administrator terminal.
    ScheduledTask,
}

impl WindowsMechanism {
    const fn other(self) -> Self {
        match self {
            Self::StartupApp => Self::ScheduledTask,
            Self::ScheduledTask => Self::StartupApp,
        }
    }

    const fn backend(self) -> &'static dyn ServiceBackend {
        match self {
            Self::StartupApp => &StartupAppBackend,
            Self::ScheduledTask => &ScheduledTaskBackend,
        }
    }

    /// The mechanism's registration, as Windows reports it, or `None` when it is not installed.
    fn registration(self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<Option<String>, ServiceError> {
        match self {
            Self::StartupApp => StartupAppBackend::registration(runner, spec),
            Self::ScheduledTask => ScheduledTaskBackend::registration(runner, spec),
        }
    }
}

/// What `<program> service` was asked for: whether `--scheduled-task` was given, on which platform
/// (`std::env::consts::OS`).
#[derive(Clone, Copy, Debug)]
pub struct InstallChoice<'a> {
    pub identity: ServiceIdentity,
    pub os: &'a str,
    pub scheduled_task: bool,
}

impl TryFrom<InstallChoice<'_>> for WindowsMechanism {
    type Error = ServiceError;

    /// The Startup App unless `--scheduled-task` was given, which only Windows has.
    ///
    /// # Errors
    ///
    /// [`ServiceError::ScheduledTaskOffWindows`] for `--scheduled-task` on any other platform.
    fn try_from(choice: InstallChoice<'_>) -> Result<Self, Self::Error> {
        match (choice.scheduled_task, choice.os) {
            (false, _) => Ok(Self::StartupApp),
            (true, "windows") => Ok(Self::ScheduledTask),
            (true, os) => Err(ServiceError::ScheduledTaskOffWindows {
                program: choice.identity.program,
                os: os.to_owned(),
            }),
        }
    }
}

/// The Windows [`ServiceBackend`]: installs as its [`WindowsMechanism`], and acts on whichever is installed.
#[derive(Clone, Copy, Debug)]
pub struct WindowsBackend {
    install_as: WindowsMechanism,
}

impl From<WindowsMechanism> for WindowsBackend {
    fn from(install_as: WindowsMechanism) -> Self {
        Self { install_as }
    }
}

/// The installed mechanism, the Startup App first when (unexpectedly) both are, once its registration is confirmed
/// to run the server for `spec`'s configuration file.
fn installed(runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<Option<WindowsMechanism>, ServiceError> {
    for mechanism in [WindowsMechanism::StartupApp, WindowsMechanism::ScheduledTask] {
        if let Some(registration) = mechanism.registration(runner, spec)? {
            registered_for(spec, &registration)?;
            return Ok(Some(mechanism));
        }
    }
    Ok(None)
}

impl ServiceBackend for WindowsBackend {
    fn install(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<(), ServiceError> {
        let other = self.install_as.other();
        let mut other_installed = false;
        for mechanism in [self.install_as, other] {
            if let Some(registration) = mechanism.registration(runner, spec)? {
                if registered_for(spec, &registration).is_err() {
                    return Err(ServiceError::InstalledForAnotherConfig {
                        program: spec.identity.program,
                        config_file: spec.identity.config_file,
                        registration,
                    });
                }
                other_installed |= mechanism == other;
            }
        }
        match self.install_as {
            // Removing the task needs an Administrator; refused, nothing has changed yet.
            WindowsMechanism::StartupApp => {
                if other_installed {
                    other.backend().uninstall(runner, spec)?;
                }
                self.install_as.backend().install(runner, spec)
            }
            // Registering the task needs an Administrator; refused, the Startup App still runs the server.
            WindowsMechanism::ScheduledTask => {
                ScheduledTaskBackend::register(runner, spec)?;
                if other_installed {
                    other.backend().uninstall(runner, spec)?;
                }
                if server_state(runner, spec)? == RunState::Running {
                    return Ok(());
                }
                self.install_as.backend().start(runner, spec)
            }
        }
    }

    fn uninstall(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<UninstallOutcome, ServiceError> {
        if installed(runner, spec)?.is_none() {
            return Ok(UninstallOutcome::NotInstalled);
        }
        for mechanism in [WindowsMechanism::StartupApp, WindowsMechanism::ScheduledTask] {
            mechanism.backend().uninstall(runner, spec)?;
        }
        Ok(UninstallOutcome::Removed)
    }

    fn status(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<ServiceStatus, ServiceError> {
        match installed(runner, spec)? {
            Some(mechanism) => mechanism.backend().status(runner, spec),
            None => Ok(ServiceStatus::NotInstalled),
        }
    }

    fn start(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<(), ServiceError> {
        installed(runner, spec)?.map_or(Ok(()), |mechanism| mechanism.backend().start(runner, spec))
    }

    fn stop(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<(), ServiceError> {
        installed(runner, spec)?.map_or(Ok(()), |mechanism| mechanism.backend().stop(runner, spec))
    }
}

#[cfg(test)]
#[path = "windows_host_test.rs"]
mod tests;
