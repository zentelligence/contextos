//! `<program> service start`, `stop`, and `restart`: what each does given the service's current state, the same on
//! every platform, so a backend only has to know how to start and stop its own mechanism. Checking the state first is
//! what lets `start` leave a running server alone and say so, and lets `start` after a crash say it recovered.

use super::{CommandRunner, ServiceBackend, ServiceError, ServiceIdentity, ServiceSpec, ServiceStatus};

/// Whether an installed service's server is running.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunState {
    Running,
    /// Not running, and nothing says it stopped other than as asked.
    Stopped,
    /// Not running, and the platform or the server's process record says it ended without being asked to.
    Crashed,
}

/// A `start`, `stop`, or `restart` request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlAction {
    Start,
    Stop,
    Restart,
}

/// What a [`ControlAction`] did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlOutcome {
    /// Started; `recovered` when it had crashed.
    Started {
        recovered: bool,
    },
    AlreadyRunning,
    Stopped,
    NotRunning,
    Restarted,
    NotInstalled,
}

/// Carries out `action` against `backend`'s service.
///
/// # Errors
///
/// Returns [`ServiceError`] when the status cannot be read or the backend's start or stop fails.
pub fn control(
    backend: &dyn ServiceBackend,
    runner: &dyn CommandRunner,
    spec: &ServiceSpec,
    action: ControlAction,
) -> Result<ControlOutcome, ServiceError> {
    let ServiceStatus::Installed(state) = backend.status(runner, spec)? else {
        return Ok(ControlOutcome::NotInstalled);
    };
    match (action, state) {
        (ControlAction::Start, RunState::Running) => Ok(ControlOutcome::AlreadyRunning),
        (ControlAction::Start, RunState::Stopped | RunState::Crashed) => {
            backend.start(runner, spec)?;
            Ok(ControlOutcome::Started {
                recovered: state == RunState::Crashed,
            })
        }
        (ControlAction::Stop, RunState::Running) => {
            backend.stop(runner, spec)?;
            Ok(ControlOutcome::Stopped)
        }
        // Stopping clears whatever a crash left behind, so a later start begins clean.
        (ControlAction::Stop, RunState::Crashed) => {
            backend.stop(runner, spec)?;
            Ok(ControlOutcome::NotRunning)
        }
        (ControlAction::Stop, RunState::Stopped) => Ok(ControlOutcome::NotRunning),
        (ControlAction::Restart, _) => {
            backend.restart(runner, spec)?;
            Ok(ControlOutcome::Restarted)
        }
    }
}

impl ControlOutcome {
    /// The sentence the command prints for this outcome, naming `identity`'s program.
    #[must_use]
    pub fn message(self, identity: &ServiceIdentity) -> String {
        let program = identity.program;
        match self {
            Self::Started { recovered: false } => format!("Started the {program} service."),
            Self::Started { recovered: true } => {
                format!("The {program} service had stopped unexpectedly; started it again.")
            }
            Self::AlreadyRunning => format!("The {program} service is already running."),
            Self::Stopped => format!("Stopped the {program} service."),
            Self::NotRunning => format!("The {program} service was not running."),
            Self::Restarted => format!("Restarted the {program} service."),
            Self::NotInstalled => format!("The {program} service is not installed; run `{program} service install`."),
        }
    }
}

impl ServiceStatus {
    /// The sentence `status` prints, naming `identity`'s program.
    #[must_use]
    pub fn message(self, identity: &ServiceIdentity) -> String {
        let program = identity.program;
        match self {
            Self::Installed(RunState::Running) => format!("The {program} service is installed and running."),
            Self::Installed(RunState::Stopped) => {
                format!("The {program} service is installed but not running; run `{program} service start`.")
            }
            Self::Installed(RunState::Crashed) => {
                format!("The {program} service is installed but stopped unexpectedly; run `{program} service start`.")
            }
            Self::NotInstalled => format!("The {program} service is not installed."),
        }
    }
}

#[cfg(test)]
#[path = "control_test.rs"]
mod tests;
