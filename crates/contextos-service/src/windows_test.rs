use std::path::PathBuf;

use tempfile::tempdir;

use super::*;
use crate::windows_process::tests::{POWERSHELL, registration_for, script_of, server_line, windows_spec, write_record};
use crate::{FakeCommandRunner, RunState};

const SCHTASKS: &str = "C:\\Windows\\System32\\schtasks.exe";
/// The exact `schtasks` stderr an operator-tested standard (non-administrator) account produced: no UAC prompt, an
/// outright ACL refusal from Task Scheduler itself.
const ACCESS_DENIED: &str = "ERROR: Access is denied.\r\r\n";

/// Each call as `(program, first argument)`.
fn steps(runner: &FakeCommandRunner) -> Vec<(String, String)> {
    runner
        .calls()
        .into_iter()
        .map(|(program, args)| (program, args.first().cloned().unwrap_or_default()))
        .collect()
}

#[test]
fn install_creates_the_task_with_a_logon_trigger_and_runs_it_now() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    let runner = FakeCommandRunner::new();

    ScheduledTaskBackend.install(&runner, &spec)?;

    let calls = runner.calls();
    let (program, args) = &calls[0];
    assert_eq!(program, SCHTASKS, "by full path");
    assert_eq!(args[0], "/Create");
    for expected in ["ContextOS Web", "ONLOGON", "LIMITED", "/F"] {
        assert!(args.contains(&expected.to_owned()), "{expected}: {args:?}");
    }
    let tr = args
        .iter()
        .position(|a| a == "/TR")
        .ok_or("expected /TR in the schtasks arguments")?
        + 1;
    assert_eq!(args[tr], super::super::windows_process::logon_command(&spec));
    assert!(
        args[tr].starts_with("\"C:\\Windows\\System32\\conhost.exe\" --headless \""),
        "no console window: {}",
        args[tr]
    );
    assert_eq!(
        steps(&runner)[1],
        (SCHTASKS.to_owned(), "/Run".to_owned()),
        "started now, not at the next logon"
    );
    Ok(())
}

#[test]
fn install_fails_when_schtasks_reports_a_non_zero_exit() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let runner = FakeCommandRunner::new();
    runner.push_failure("something else went wrong");

    let result = ScheduledTaskBackend.install(&runner, &windows_spec(dir.path()));

    assert!(matches!(result, Err(ServiceError::CommandFailed { .. })));
    Ok(())
}

#[test]
fn creating_deleting_or_running_the_task_without_an_administrator_says_so() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());

    let runner = FakeCommandRunner::new();
    runner.push_failure(ACCESS_DENIED);
    let Err(error) = ScheduledTaskBackend.install(&runner, &spec) else {
        return Err("expected install to fail".into());
    };
    assert!(matches!(error, ServiceError::ElevationRequired { .. }));
    assert!(error.to_string().contains("Administrator"), "{error}");

    let runner = FakeCommandRunner::new();
    runner.push_success(&registration_for(&spec));
    runner.push_failure("ERROR: The task is not running."); // /End
    runner.push_failure(ACCESS_DENIED); // /Delete
    let result = ScheduledTaskBackend.uninstall(&runner, &spec);
    assert!(
        matches!(result, Err(ServiceError::ElevationRequired { .. })),
        "{result:?}"
    );

    let runner = FakeCommandRunner::new();
    runner.push_failure(ACCESS_DENIED); // /Run
    let result = ScheduledTaskBackend.start(&runner, &spec);
    assert!(
        matches!(result, Err(ServiceError::ElevationRequired { .. })),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn the_task_is_read_through_powershell_in_utf8() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success(&registration_for(&spec));

    assert_eq!(
        ScheduledTaskBackend::registration(&runner, &spec)?,
        Some(super::super::windows_process::logon_command(&spec))
    );
    let call = &runner.calls()[0];
    assert_eq!(call.0, POWERSHELL);
    assert_eq!(
        script_of(call),
        "$t = Get-ScheduledTask -TaskName 'ContextOS Web' -ErrorAction SilentlyContinue; if ($t) { foreach ($a in \
         $t.Actions) { '{0} {1}' -f $a.Execute, $a.Arguments } }"
    );
    Ok(())
}

#[test]
fn status_is_not_installed_without_the_task() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let runner = FakeCommandRunner::new();
    runner.push_success(""); // no task

    assert_eq!(
        ScheduledTaskBackend.status(&runner, &windows_spec(dir.path()))?,
        ServiceStatus::NotInstalled
    );
    Ok(())
}

#[test]
fn status_reads_whether_the_server_runs_from_its_process_record_not_schtasks_localised_text()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success(&registration_for(&spec));
    assert_eq!(
        ScheduledTaskBackend.status(&runner, &spec)?,
        ServiceStatus::Installed(RunState::Stopped)
    );

    write_record(&spec, 4242)?;
    let runner = FakeCommandRunner::new();
    runner.push_success(&registration_for(&spec));
    runner.push_success(&server_line(&spec));
    assert_eq!(
        ScheduledTaskBackend.status(&runner, &spec)?,
        ServiceStatus::Installed(RunState::Running)
    );
    Ok(())
}

#[test]
fn stop_ends_the_task_and_the_recorded_server() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    write_record(&spec, 4242)?;
    let runner = FakeCommandRunner::new();
    runner.push_success("SUCCESS"); // /End
    runner.push_success(&server_line(&spec));
    runner.push_success("SUCCESS"); // taskkill

    ScheduledTaskBackend.stop(&runner, &spec)?;

    let steps = steps(&runner);
    assert_eq!(steps[0], (SCHTASKS.to_owned(), "/End".to_owned()));
    assert!(steps[2].0.ends_with("taskkill.exe"), "{steps:?}");
    assert!(!spec.process_record.exists());
    Ok(())
}

#[test]
fn uninstall_without_the_task_changes_nothing() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let runner = FakeCommandRunner::new();
    runner.push_success(""); // no task

    let outcome = ScheduledTaskBackend.uninstall(&runner, &windows_spec(dir.path()))?;

    assert_eq!(outcome, UninstallOutcome::NotInstalled);
    assert_eq!(
        runner.calls().len(),
        1,
        "must not call /Delete when the task does not exist"
    );
    Ok(())
}

#[test]
fn uninstall_stops_then_deletes_the_task() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success(&registration_for(&spec));
    runner.push_failure("ERROR: The task is not running."); // /End
    runner.push_success("SUCCESS"); // /Delete

    let outcome = ScheduledTaskBackend.uninstall(&runner, &spec)?;

    assert_eq!(outcome, UninstallOutcome::Removed);
    let steps = steps(&runner);
    assert_eq!(steps[1], (SCHTASKS.to_owned(), "/End".to_owned()));
    assert_eq!(steps[2], (SCHTASKS.to_owned(), "/Delete".to_owned()));
    Ok(())
}

#[test]
fn a_task_command_longer_than_schtasks_accepts_is_refused_before_registering() {
    let long = format!("C:\\{}", "deeply-nested-folder\\".repeat(12));
    let spec = ServiceSpec {
        identity: crate::test_identities::WEB,
        run_args: Vec::new(),
        binary_path: PathBuf::from(format!("{long}contextos-web.exe")),
        config_path: PathBuf::from(format!("{long}web.toml")),
        home_dir: PathBuf::from("C:\\Users\\pj"),
        config_dir: PathBuf::from("C:\\Users\\pj\\AppData\\Roaming"),
        process_record: PathBuf::from(format!("{long}web.toml.process.json")),
        system_dir: PathBuf::from("C:\\Windows\\System32"),
    };
    let runner = FakeCommandRunner::new();

    let result = ScheduledTaskBackend.install(&runner, &spec);

    assert!(
        matches!(result, Err(ServiceError::CommandTooLong { length, limit: 261, .. }) if length > 261),
        "{result:?}"
    );
    assert!(runner.calls().is_empty(), "nothing is registered: {:?}", runner.calls());
}
