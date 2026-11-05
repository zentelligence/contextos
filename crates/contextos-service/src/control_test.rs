use std::cell::RefCell;
use std::path::PathBuf;

use super::*;
use crate::test_identities::WEB;
use crate::{FakeCommandRunner, ServiceStatus, UninstallOutcome};

/// A backend whose status is scripted and whose start/stop/restart calls are recorded.
struct ScriptedBackend {
    status: ServiceStatus,
    calls: RefCell<Vec<&'static str>>,
}

impl ScriptedBackend {
    fn new(status: ServiceStatus) -> Self {
        Self {
            status,
            calls: RefCell::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<&'static str> {
        self.calls.borrow().clone()
    }
}

impl ServiceBackend for ScriptedBackend {
    fn install(&self, _: &dyn CommandRunner, _: &ServiceSpec) -> Result<(), ServiceError> {
        Ok(())
    }

    fn uninstall(&self, _: &dyn CommandRunner, _: &ServiceSpec) -> Result<UninstallOutcome, ServiceError> {
        Ok(UninstallOutcome::Removed)
    }

    fn status(&self, _: &dyn CommandRunner, _: &ServiceSpec) -> Result<ServiceStatus, ServiceError> {
        Ok(self.status)
    }

    fn start(&self, _: &dyn CommandRunner, _: &ServiceSpec) -> Result<(), ServiceError> {
        self.calls.borrow_mut().push("start");
        Ok(())
    }

    fn stop(&self, _: &dyn CommandRunner, _: &ServiceSpec) -> Result<(), ServiceError> {
        self.calls.borrow_mut().push("stop");
        Ok(())
    }
}

fn spec() -> ServiceSpec {
    ServiceSpec {
        identity: crate::test_identities::WEB,
        run_args: Vec::new(),
        binary_path: PathBuf::from("/opt/contextos-web"),
        config_path: PathBuf::from("/home/op/web.toml"),
        home_dir: PathBuf::from("/home/op"),
        config_dir: PathBuf::from("/home/op/.config"),
        process_record: PathBuf::from("/home/op/web.toml.process.json"),
        system_dir: PathBuf::new(),
    }
}

fn run(status: ServiceStatus, action: ControlAction) -> Result<(ControlOutcome, Vec<&'static str>), ServiceError> {
    let backend = ScriptedBackend::new(status);
    let outcome = control(&backend, &FakeCommandRunner::new(), &spec(), action)?;
    Ok((outcome, backend.calls()))
}

#[test]
fn starting_a_stopped_server_starts_it() -> Result<(), ServiceError> {
    let (outcome, calls) = run(ServiceStatus::Installed(RunState::Stopped), ControlAction::Start)?;
    assert_eq!(outcome, ControlOutcome::Started { recovered: false });
    assert_eq!(calls, ["start"]);
    Ok(())
}

#[test]
fn starting_after_a_crash_starts_it_and_says_it_recovered() -> Result<(), ServiceError> {
    let (outcome, calls) = run(ServiceStatus::Installed(RunState::Crashed), ControlAction::Start)?;
    assert_eq!(outcome, ControlOutcome::Started { recovered: true });
    assert_eq!(calls, ["start"]);
    Ok(())
}

#[test]
fn starting_a_running_server_changes_nothing() -> Result<(), ServiceError> {
    let (outcome, calls) = run(ServiceStatus::Installed(RunState::Running), ControlAction::Start)?;
    assert_eq!(outcome, ControlOutcome::AlreadyRunning);
    assert!(calls.is_empty(), "{calls:?}");
    Ok(())
}

#[test]
fn stopping_a_running_server_stops_it() -> Result<(), ServiceError> {
    let (outcome, calls) = run(ServiceStatus::Installed(RunState::Running), ControlAction::Stop)?;
    assert_eq!(outcome, ControlOutcome::Stopped);
    assert_eq!(calls, ["stop"]);
    Ok(())
}

#[test]
fn stopping_after_a_crash_clears_what_the_crash_left_and_reports_nothing_was_running() -> Result<(), ServiceError> {
    let (outcome, calls) = run(ServiceStatus::Installed(RunState::Crashed), ControlAction::Stop)?;
    assert_eq!(outcome, ControlOutcome::NotRunning);
    assert_eq!(calls, ["stop"], "the backend clears a stale record");
    Ok(())
}

#[test]
fn stopping_a_stopped_server_changes_nothing() -> Result<(), ServiceError> {
    let (outcome, calls) = run(ServiceStatus::Installed(RunState::Stopped), ControlAction::Stop)?;
    assert_eq!(outcome, ControlOutcome::NotRunning);
    assert!(calls.is_empty(), "{calls:?}");
    Ok(())
}

#[test]
fn restarting_stops_then_starts_whatever_state_it_was_in() -> Result<(), ServiceError> {
    for state in [RunState::Running, RunState::Stopped, RunState::Crashed] {
        let (outcome, calls) = run(ServiceStatus::Installed(state), ControlAction::Restart)?;
        assert_eq!(outcome, ControlOutcome::Restarted, "{state:?}");
        assert_eq!(calls, ["stop", "start"], "{state:?}");
    }
    Ok(())
}

#[test]
fn nothing_is_started_stopped_or_restarted_when_the_service_is_not_installed() -> Result<(), ServiceError> {
    for action in [ControlAction::Start, ControlAction::Stop, ControlAction::Restart] {
        let (outcome, calls) = run(ServiceStatus::NotInstalled, action)?;
        assert_eq!(outcome, ControlOutcome::NotInstalled, "{action:?}");
        assert!(calls.is_empty(), "{action:?}");
    }
    Ok(())
}

#[test]
fn each_outcome_reads_as_a_sentence_naming_the_next_step_where_there_is_one() {
    assert_eq!(
        ControlOutcome::Started { recovered: true }.message(&WEB),
        "The contextos-web service had stopped unexpectedly; started it again."
    );
    assert_eq!(
        ControlOutcome::NotInstalled.message(&WEB),
        "The contextos-web service is not installed; run `contextos-web service install`."
    );
    assert_eq!(
        ServiceStatus::Installed(RunState::Crashed).message(&WEB),
        "The contextos-web service is installed but stopped unexpectedly; run `contextos-web service start`."
    );
}
