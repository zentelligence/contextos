//! Windows backend, the default: a per-user Startup App, a value under the current user's `Run` registry key that
//! Windows runs at each logon. Registering it needs no elevation (unlike the Scheduled Task, `service::windows`), and
//! it runs as the user, like every other backend. Windows does not track the process it starts, so start, stop, and
//! status go through the server's process record (`service::windows_process`).

use super::windows_process::{
    RegisteredCommand, end_server, launch, logon_command, printed, server_state, system_tool, within_limit,
};
use super::{
    CommandRunner, RunState, ServiceBackend, ServiceError, ServiceSpec, ServiceStatus, UninstallOutcome, run_checked,
};

const RUN_KEY: &str = "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run";

/// The longest command Windows documents running from a `Run` entry, in UTF-16 code units.
const RUN_LIMIT: usize = 260;

/// The PowerShell that prints the `Run` entry's command, or nothing when there is none.
fn registration_script(spec: &ServiceSpec) -> String {
    let name = spec.identity.windows_name;
    format!(
        "$v = Get-ItemProperty -LiteralPath 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Run' -Name '{name}' \
         -ErrorAction SilentlyContinue; if ($v) {{ $v.'{name}' }}"
    )
}

/// Installs a binary as a per-user Windows Startup App.
#[derive(Clone, Copy, Debug, Default)]
pub struct StartupAppBackend;

impl StartupAppBackend {
    /// The `Run` entry's command, or `None` when there is none.
    pub(crate) fn registration(runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<Option<String>, ServiceError> {
        printed(runner, spec, &registration_script(spec))
    }

    /// Writes the `Run` entry without starting the server.
    pub(crate) fn register(runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<(), ServiceError> {
        let command = logon_command(spec);
        within_limit(spec, &command, RUN_LIMIT)?;
        run_checked(
            runner,
            &system_tool(spec, "reg.exe"),
            &[
                "add",
                RUN_KEY,
                "/v",
                spec.identity.windows_name,
                "/t",
                "REG_SZ",
                "/d",
                &command,
                "/f",
            ],
        )
    }
}

impl ServiceBackend for StartupAppBackend {
    fn install(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<(), ServiceError> {
        Self::register(runner, spec)?;
        // The entry first runs at the next logon; this starts the server now, leaving one already running alone.
        if server_state(runner, spec)? == RunState::Running {
            return Ok(());
        }
        launch(
            runner,
            spec,
            &RegisteredCommand::try_from(logon_command(spec).as_str())?,
        )
    }

    fn uninstall(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<UninstallOutcome, ServiceError> {
        if Self::registration(runner, spec)?.is_none() {
            return Ok(UninstallOutcome::NotInstalled);
        }
        end_server(runner, spec)?;
        run_checked(
            runner,
            &system_tool(spec, "reg.exe"),
            &["delete", RUN_KEY, "/v", spec.identity.windows_name, "/f"],
        )?;
        Ok(UninstallOutcome::Removed)
    }

    fn status(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<ServiceStatus, ServiceError> {
        if Self::registration(runner, spec)?.is_none() {
            return Ok(ServiceStatus::NotInstalled);
        }
        Ok(ServiceStatus::Installed(server_state(runner, spec)?))
    }

    /// Starts the command the `Run` entry holds, as Windows will at the next logon, not one rebuilt from the arguments
    /// this command was given, so a server registered with `--http ADDR` always starts on that address.
    fn start(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<(), ServiceError> {
        let registration = Self::registration(runner, spec)?.ok_or(ServiceError::NotRegistered {
            program: spec.identity.program,
        })?;
        launch(runner, spec, &RegisteredCommand::try_from(registration.as_str())?)
    }

    fn stop(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<(), ServiceError> {
        end_server(runner, spec)
    }
}

#[cfg(test)]
#[path = "startup_app_test.rs"]
mod tests;
