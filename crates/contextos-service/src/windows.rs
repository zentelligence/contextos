//! Windows backend for an operator who chooses it (`service install
//! --scheduled-task`; the Startup App, `service::startup_app`, is the
//! default): a per-user Scheduled Task with a logon trigger, run only while
//! the user is logged on. The registered task itself runs with
//! no elevated privileges (`/RL LIMITED`), like the Linux and macOS
//! backends' units never running as `LocalSystem` or another privileged
//! account, but *registering* it is a different matter: unlike those two
//! backends, `install`/`uninstall` here need an elevated (Administrator)
//! terminal. Windows Task Scheduler requires the invoking account to be an
//! administrator to create or delete a task at all, even one carrying no
//! elevated run level once installed, on every configuration this crate has
//! been operator-tested against (`schtasks /Create ... /RL LIMITED` from a
//! standard account: `ERROR: Access is denied.`, no UAC prompt, an outright
//! ACL refusal rather than something a flag can route around); see
//! `ServiceError::ElevationRequired`, which [`run_checked_elevatable`]
//! raises instead of a bare [`ServiceError::CommandFailed`] when this is
//! what happened.
//!
//! There is no separate service-definition file: Task Scheduler owns the
//! task's definition, so "installed" is answered by asking it for the task
//! (through PowerShell's `Get-ScheduledTask`, which reports the task's
//! command in UTF-8). Whether its server is running is read from the
//! server's process record (`service::windows_process`), as for the Startup
//! App: `schtasks`' own status text is localised.

use super::windows_process::{end_server, logon_command, printed, server_state, system_tool, within_limit};
use super::{
    CommandRunner, RunState, ServiceBackend, ServiceError, ServiceSpec, ServiceStatus, UninstallOutcome, run_checked,
};

/// The longest command `schtasks /Create /TR` accepts, in UTF-16 code units.
const TASK_RUN_LIMIT: usize = 261;

/// The PowerShell that prints the task's command (its program and arguments), or nothing when there is no task.
fn registration_script(spec: &ServiceSpec) -> String {
    format!(
        "$t = Get-ScheduledTask -TaskName '{}' -ErrorAction SilentlyContinue; \
         if ($t) {{ foreach ($a in $t.Actions) {{ '{{0}} {{1}}' -f $a.Execute, $a.Arguments }} }}",
        spec.identity.windows_name
    )
}

/// Installs a binary as a per-user Windows Scheduled Task.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScheduledTaskBackend;

impl ScheduledTaskBackend {
    /// The task's command, or `None` when there is no such task.
    pub(crate) fn registration(runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<Option<String>, ServiceError> {
        printed(runner, spec, &registration_script(spec))
    }

    /// Creates (or replaces) the task without starting it.
    pub(crate) fn register(runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<(), ServiceError> {
        let run_value = logon_command(spec);
        // `schtasks` refuses a longer command with a message that names no limit, so it is checked here first.
        within_limit(spec, &run_value, TASK_RUN_LIMIT)?;
        run_checked_elevatable(
            runner,
            &system_tool(spec, "schtasks.exe"),
            &[
                "/Create",
                "/TN",
                spec.identity.windows_name,
                "/TR",
                &run_value,
                "/SC",
                "ONLOGON",
                "/RL",
                "LIMITED",
                "/F",
            ],
        )
    }
}

/// As [`run_checked`], but rewrites a failure whose `stderr` carries
/// Windows' well-known "Access is denied" text into
/// [`ServiceError::ElevationRequired`], so the operator sees the actionable
/// "run this elevated" hint instead of a bare non-zero-exit message. Every
/// `schtasks` call that changes or runs the task uses it (`/Create`,
/// `/Delete`, and `/Run`, which a task registered from an Administrator
/// terminal may refuse from an ordinary one).
///
/// This is deliberately not a general string match on every possible
/// Windows access-control failure, only the exact case this module has been
/// operator-tested against. A mismatch (a differently-worded or localised
/// message) simply falls through to the original, still-correct
/// [`ServiceError::CommandFailed`]: the risk is losing the friendlier hint,
/// never misreporting success or swallowing a real failure.
fn run_checked_elevatable(runner: &dyn CommandRunner, program: &str, args: &[&str]) -> Result<(), ServiceError> {
    run_checked(runner, program, args).map_err(|error| match error {
        ServiceError::CommandFailed {
            program, args, stderr, ..
        } if stderr.to_ascii_lowercase().contains("access is denied") => {
            ServiceError::ElevationRequired { program, args, stderr }
        }
        other => other,
    })
}

impl ServiceBackend for ScheduledTaskBackend {
    fn install(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<(), ServiceError> {
        Self::register(runner, spec)?;
        // The logon trigger first fires at the next logon; this starts it now, leaving one already running alone.
        if server_state(runner, spec)? == RunState::Running {
            return Ok(());
        }
        self.start(runner, spec)
    }

    fn uninstall(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<UninstallOutcome, ServiceError> {
        if Self::registration(runner, spec)?.is_none() {
            return Ok(UninstallOutcome::NotInstalled);
        }
        self.stop(runner, spec)?;
        run_checked_elevatable(
            runner,
            &system_tool(spec, "schtasks.exe"),
            &["/Delete", "/TN", spec.identity.windows_name, "/F"],
        )?;
        Ok(UninstallOutcome::Removed)
    }

    fn status(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<ServiceStatus, ServiceError> {
        if Self::registration(runner, spec)?.is_none() {
            return Ok(ServiceStatus::NotInstalled);
        }
        Ok(ServiceStatus::Installed(server_state(runner, spec)?))
    }

    fn start(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<(), ServiceError> {
        run_checked_elevatable(
            runner,
            &system_tool(spec, "schtasks.exe"),
            &["/Run", "/TN", spec.identity.windows_name],
        )
    }

    fn stop(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<(), ServiceError> {
        // Best effort: `/End` fails when the task is not running, which is not a failure to stop. Ending the server
        // through its record covers a server the task scheduler no longer counts as the task's.
        let _ = runner.run(
            &system_tool(spec, "schtasks.exe"),
            &["/End", "/TN", spec.identity.windows_name],
        );
        end_server(runner, spec)
    }
}

#[cfg(test)]
#[path = "windows_test.rs"]
mod tests;
