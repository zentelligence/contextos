use std::path::PathBuf;

use tempfile::tempdir;

use super::*;
use crate::windows_process::RegisteredCommand;
use crate::windows_process::tests::{POWERSHELL, registration_for, script_of, server_line, windows_spec, write_record};
use crate::{FakeCommandRunner, RunState};

const REG: &str = "C:\\Windows\\System32\\reg.exe";
const RUN_KEY_ARGS: [&str; 3] = [
    "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
    "/v",
    "ContextOS Web",
];

fn args(call: &(String, Vec<String>)) -> Vec<&str> {
    call.1.iter().map(String::as_str).collect()
}

#[test]
fn install_registers_the_headless_logon_command_for_this_user_and_starts_it() -> Result<(), Box<dyn std::error::Error>>
{
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    let runner = FakeCommandRunner::new();

    StartupAppBackend.install(&runner, &spec)?;

    let calls = runner.calls();
    assert_eq!(calls[0].0, REG, "by full path");
    let command = logon_command(&spec);
    assert_eq!(
        args(&calls[0]),
        [
            "add",
            RUN_KEY_ARGS[0],
            "/v",
            "ContextOS Web",
            "/t",
            "REG_SZ",
            "/d",
            command.as_str(),
            "/f"
        ],
        "HKCU, so no Administrator is needed"
    );
    assert_eq!(runner.spawned().len(), 1, "started now, not at the next logon");
    Ok(())
}

#[test]
fn installing_over_a_running_server_leaves_it_running() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    write_record(&spec, 4242)?;
    let runner = FakeCommandRunner::new();
    runner.push_success("The operation completed successfully."); // reg add
    runner.push_success(&server_line(&spec));

    StartupAppBackend.install(&runner, &spec)?;

    assert!(runner.spawned().is_empty(), "{:?}", runner.spawned());
    Ok(())
}

#[test]
fn a_command_longer_than_a_run_entry_may_be_is_refused_before_registering() {
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

    let result = StartupAppBackend.install(&runner, &spec);

    assert!(
        matches!(result, Err(ServiceError::CommandTooLong { limit: 260, .. })),
        "{result:?}"
    );
    assert!(runner.calls().is_empty(), "{:?}", runner.calls());
}

#[test]
fn the_registration_is_read_through_powershell_in_utf8() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success(&registration_for(&spec));

    assert_eq!(
        StartupAppBackend::registration(&runner, &spec)?,
        Some(logon_command(&spec))
    );
    let call = &runner.calls()[0];
    assert_eq!(call.0, POWERSHELL);
    assert_eq!(
        script_of(call),
        "$v = Get-ItemProperty -LiteralPath 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Run' -Name \
         'ContextOS Web' -ErrorAction SilentlyContinue; if ($v) { $v.'ContextOS Web' }"
    );
    Ok(())
}

#[test]
fn status_is_not_installed_without_the_run_entry() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let runner = FakeCommandRunner::new();
    runner.push_success(""); // no Run entry

    let status = StartupAppBackend.status(&runner, &windows_spec(dir.path()))?;

    assert_eq!(status, ServiceStatus::NotInstalled);
    Ok(())
}

#[test]
fn status_reports_a_crash_from_a_record_whose_server_is_gone() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    write_record(&spec, 4242)?;
    let runner = FakeCommandRunner::new();
    runner.push_success(&registration_for(&spec));
    runner.push_success(""); // no such process

    assert_eq!(
        StartupAppBackend.status(&runner, &spec)?,
        ServiceStatus::Installed(RunState::Crashed)
    );
    Ok(())
}

#[test]
fn start_launches_the_server_and_stop_ends_it() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success(&registration_for(&spec));
    StartupAppBackend.start(&runner, &spec)?;
    assert!(runner.spawned()[0].0.ends_with("conhost.exe"));

    write_record(&spec, 4242)?;
    let runner = FakeCommandRunner::new();
    runner.push_success(&server_line(&spec));
    StartupAppBackend.stop(&runner, &spec)?;
    assert!(runner.calls()[1].0.ends_with("taskkill.exe"));
    Ok(())
}

#[test]
fn uninstall_ends_the_server_then_removes_the_run_entry() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    write_record(&spec, 4242)?;
    let runner = FakeCommandRunner::new();
    runner.push_success(&registration_for(&spec));
    runner.push_success(&server_line(&spec));
    runner.push_success("SUCCESS"); // taskkill
    runner.push_success(""); // exited
    runner.push_success("The operation completed successfully."); // reg delete

    let outcome = StartupAppBackend.uninstall(&runner, &spec)?;

    assert_eq!(outcome, UninstallOutcome::Removed);
    let calls = runner.calls();
    assert!(calls[2].0.ends_with("taskkill.exe"));
    assert_eq!(calls[4].0, REG);
    assert_eq!(
        args(&calls[4]),
        [&["delete"][..], &RUN_KEY_ARGS[..], &["/f"][..]].concat()
    );
    Ok(())
}

#[test]
fn uninstall_without_the_run_entry_changes_nothing() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let runner = FakeCommandRunner::new();
    runner.push_success(""); // no Run entry

    let outcome = StartupAppBackend.uninstall(&runner, &windows_spec(dir.path()))?;

    assert_eq!(outcome, UninstallOutcome::NotInstalled);
    assert_eq!(runner.calls().len(), 1);
    Ok(())
}

#[test]
fn start_runs_the_registered_command_whatever_arguments_this_command_was_given()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let mut registered = windows_spec(dir.path());
    registered.run_args = ["--http", "127.0.0.1:7331", "--no-stdio"].map(str::to_owned).to_vec();
    let mut asked = registered.clone();
    asked.run_args = ["--http", "--no-stdio"].map(str::to_owned).to_vec();
    let runner = FakeCommandRunner::new();
    runner.push_success(&registration_for(&registered));

    StartupAppBackend.start(&runner, &asked)?;

    let expected = RegisteredCommand::try_from(logon_command(&registered).as_str())?;
    assert_eq!(runner.spawned(), vec![(expected.program, expected.args)]);
    Ok(())
}

#[test]
fn start_without_a_run_entry_is_refused_as_not_installed() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success("");

    let error = StartupAppBackend
        .start(&runner, &spec)
        .err()
        .ok_or("a start with nothing registered should be refused")?;

    assert!(matches!(error, ServiceError::NotRegistered { .. }), "{error:?}");
    assert_eq!(runner.spawned(), Vec::new());
    Ok(())
}
