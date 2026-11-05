//! Linux backend: a `systemd --user` unit, so no elevation is needed and
//! the service starts and stops with the user's own login session.

use std::path::PathBuf;

use super::{
    CommandRunner, RunState, ServiceBackend, ServiceError, ServiceSpec, ServiceStatus, UninstallOutcome, run_checked,
};
use crate::write_atomically;

/// Installs a binary as a `systemd --user` unit.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemdUserBackend;

fn unit_path(spec: &ServiceSpec) -> PathBuf {
    spec.config_dir
        .join("systemd")
        .join("user")
        .join(spec.identity.unit_name)
}

/// Quotes `value` as one systemd command-line word: in double quotes, with `\` and `"` escaped as systemd reads them.
fn quoted_word(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn unit_contents(spec: &ServiceSpec) -> String {
    let mut command = format!(
        "{} --config {}",
        quoted_word(&spec.binary_path.display().to_string()),
        quoted_word(&spec.config_path.display().to_string()),
    );
    for argument in &spec.run_args {
        command.push(' ');
        command.push_str(&quoted_word(argument));
    }
    format!(
        "[Unit]\n\
         Description={}\n\
         \n\
         [Service]\n\
         ExecStart={command}\n\
         KillMode=mixed\n\
         TimeoutStopSec=30\n\
         Restart=on-failure\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        spec.identity.description,
    )
}

impl ServiceBackend for SystemdUserBackend {
    fn install(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<(), ServiceError> {
        let path = unit_path(spec);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| ServiceError::Write {
                path: path.clone(),
                source,
            })?;
        }
        write_atomically(&path, unit_contents(spec).as_bytes()).map_err(|source| ServiceError::Write {
            path: path.clone(),
            source,
        })?;

        run_checked(runner, "systemctl", &["--user", "daemon-reload"])?;
        run_checked(
            runner,
            "systemctl",
            &["--user", "enable", "--now", spec.identity.unit_name],
        )
    }

    fn uninstall(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<UninstallOutcome, ServiceError> {
        let path = unit_path(spec);
        if !path.exists() {
            return Ok(UninstallOutcome::NotInstalled);
        }

        run_checked(
            runner,
            "systemctl",
            &["--user", "disable", "--now", spec.identity.unit_name],
        )?;
        std::fs::remove_file(&path).map_err(|source| ServiceError::Remove {
            path: path.clone(),
            source,
        })?;
        run_checked(runner, "systemctl", &["--user", "daemon-reload"])?;
        Ok(UninstallOutcome::Removed)
    }

    fn status(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<ServiceStatus, ServiceError> {
        let path = unit_path(spec);
        if !path.exists() {
            return Ok(ServiceStatus::NotInstalled);
        }

        let args = ["--user", "is-active", spec.identity.unit_name];
        let output = runner.run("systemctl", &args).map_err(|source| ServiceError::Spawn {
            program: "systemctl".to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            source,
        })?;
        // `is-active` exits non-zero unless active; its output names the state. `activating` includes the wait before
        // `Restart=on-failure` starts a crashed server again, so it counts as running: `stop` must stop that restart.
        Ok(ServiceStatus::Installed(match output.stdout.trim() {
            "active" | "activating" | "reloading" | "refreshing" | "deactivating" => RunState::Running,
            "failed" => RunState::Crashed,
            _ => RunState::Stopped,
        }))
    }

    fn start(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<(), ServiceError> {
        run_checked(runner, "systemctl", &["--user", "start", spec.identity.unit_name])
    }

    fn stop(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<(), ServiceError> {
        run_checked(runner, "systemctl", &["--user", "stop", spec.identity.unit_name])?;
        // Best effort: clears a crash's `failed` state so status reads stopped; there is none to clear otherwise.
        let _ = runner.run("systemctl", &["--user", "reset-failed", spec.identity.unit_name]);
        Ok(())
    }

    fn restart(&self, runner: &dyn CommandRunner, spec: &ServiceSpec) -> Result<(), ServiceError> {
        run_checked(runner, "systemctl", &["--user", "restart", spec.identity.unit_name])
    }
}

#[cfg(test)]
#[path = "linux_test.rs"]
mod tests;
