use tempfile::tempdir;

use super::*;
use crate::FakeCommandRunner;
use crate::windows_process::tests::{POWERSHELL, registration_for, windows_spec};

const SCHTASKS: &str = "C:\\Windows\\System32\\schtasks.exe";
const REG: &str = "C:\\Windows\\System32\\reg.exe";
const ACCESS_DENIED: &str = "ERROR: Access is denied.\r\r\n";

/// One service command, run against a scripted runner.
type Command<'a> = &'a dyn Fn(&FakeCommandRunner) -> Result<(), ServiceError>;

/// Each call as `(program, first argument)`, PowerShell's reads shown as `read`.
fn steps(runner: &FakeCommandRunner) -> Vec<(String, String)> {
    runner
        .calls()
        .into_iter()
        .map(|(program, args)| {
            if program == POWERSHELL {
                ("powershell".to_owned(), "read".to_owned())
            } else {
                (program, args.first().cloned().unwrap_or_default())
            }
        })
        .collect()
}

fn step(program: &str, first: &str) -> (String, String) {
    (program.to_owned(), first.to_owned())
}

fn read() -> (String, String) {
    step("powershell", "read")
}

fn another_config(dir: &std::path::Path) -> ServiceSpec {
    ServiceSpec {
        identity: crate::test_identities::WEB,
        run_args: Vec::new(),
        config_path: dir.join("registered.toml"),
        ..windows_spec(dir)
    }
}

#[test]
fn a_plain_install_registers_the_startup_app() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let runner = FakeCommandRunner::new();
    runner.push_success(""); // no Run entry
    runner.push_success(""); // no task

    WindowsBackend::from(WindowsMechanism::StartupApp).install(&runner, &windows_spec(dir.path()))?;

    assert_eq!(steps(&runner)[2], step(REG, "add"));
    assert_eq!(runner.spawned().len(), 1);
    Ok(())
}

#[test]
fn switching_to_the_startup_app_removes_the_scheduled_task_first() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success(""); // no Run entry
    runner.push_success(&registration_for(&spec)); // the task is installed
    runner.push_success(&registration_for(&spec)); // its uninstall's own read
    runner.push_failure("ERROR: The task is not running."); // /End
    runner.push_success("SUCCESS"); // /Delete

    WindowsBackend::from(WindowsMechanism::StartupApp).install(&runner, &spec)?;

    let calls = steps(&runner);
    let delete = calls.iter().position(|call| *call == step(SCHTASKS, "/Delete"));
    let add = calls.iter().position(|call| *call == step(REG, "add"));
    assert!(
        delete.is_some() && add.is_some() && delete < add,
        "two mechanisms would start two servers at logon: {calls:?}"
    );
    Ok(())
}

#[test]
fn a_task_that_needs_an_administrator_to_remove_stops_the_switch_before_anything_is_registered()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success("");
    runner.push_success(&registration_for(&spec));
    runner.push_success(&registration_for(&spec));
    runner.push_failure("ERROR: The task is not running.");
    runner.push_failure(ACCESS_DENIED);

    let result = WindowsBackend::from(WindowsMechanism::StartupApp).install(&runner, &spec);

    assert!(matches!(result, Err(ServiceError::ElevationRequired { .. })));
    assert!(!steps(&runner).contains(&step(REG, "add")));
    Ok(())
}

#[test]
fn switching_to_the_scheduled_task_registers_it_before_removing_the_startup_app_then_starts_it()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success(""); // no task yet
    runner.push_success(&registration_for(&spec)); // the Run entry
    runner.push_success("SUCCESS"); // /Create
    runner.push_success(&registration_for(&spec)); // the Startup App's uninstall's own read
    runner.push_success("The operation completed successfully."); // reg delete

    WindowsBackend::from(WindowsMechanism::ScheduledTask).install(&runner, &spec)?;

    assert_eq!(
        steps(&runner),
        [
            read(),
            read(),
            step(SCHTASKS, "/Create"),
            read(),
            step(REG, "delete"),
            step(SCHTASKS, "/Run"),
        ]
    );
    Ok(())
}

#[test]
fn a_scheduled_task_an_ordinary_terminal_cannot_register_leaves_the_startup_app_running()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success("");
    runner.push_success(&registration_for(&spec));
    runner.push_failure(ACCESS_DENIED); // /Create

    let result = WindowsBackend::from(WindowsMechanism::ScheduledTask).install(&runner, &spec);

    assert!(matches!(result, Err(ServiceError::ElevationRequired { .. })));
    assert_eq!(
        steps(&runner),
        [read(), read(), step(SCHTASKS, "/Create")],
        "nothing else touched"
    );
    Ok(())
}

#[test]
fn installing_over_a_registration_for_another_web_toml_is_refused_before_anything_changes()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let runner = FakeCommandRunner::new();
    runner.push_success(&registration_for(&another_config(dir.path())));

    let result = WindowsBackend::from(WindowsMechanism::StartupApp).install(&runner, &windows_spec(dir.path()));

    assert!(
        matches!(
            &result,
            Err(ServiceError::InstalledForAnotherConfig { registration, .. }) if registration.contains("registered.toml")
        ),
        "its server would keep running with nothing left to stop it: {result:?}"
    );
    assert_eq!(steps(&runner), [read()]);
    assert!(runner.spawned().is_empty(), "{:?}", runner.spawned());
    Ok(())
}

#[test]
fn status_reports_whichever_mechanism_is_installed() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    // Chosen for install only; status finds the task.
    let backend = WindowsBackend::from(WindowsMechanism::StartupApp);

    let runner = FakeCommandRunner::new();
    runner.push_success(""); // no Run entry
    runner.push_success(&registration_for(&spec)); // the task, for this web.toml
    runner.push_success(&registration_for(&spec)); // the task's own status read
    assert_eq!(
        backend.status(&runner, &spec)?,
        ServiceStatus::Installed(RunState::Stopped)
    );

    let runner = FakeCommandRunner::new();
    runner.push_success("");
    runner.push_success("");
    assert_eq!(backend.status(&runner, &spec)?, ServiceStatus::NotInstalled);
    Ok(())
}

#[test]
fn with_both_mechanisms_registered_the_startup_app_is_reported_and_uninstall_removes_both()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    let backend = WindowsBackend::from(WindowsMechanism::StartupApp);

    let runner = FakeCommandRunner::new();
    runner.push_success(&registration_for(&spec)); // Run entry
    runner.push_success(&registration_for(&spec)); // the Startup App's own status read
    assert_eq!(
        backend.status(&runner, &spec)?,
        ServiceStatus::Installed(RunState::Stopped)
    );
    assert_eq!(runner.calls().len(), 2, "the task is not consulted");

    let runner = FakeCommandRunner::new();
    runner.push_success(&registration_for(&spec)); // found as installed
    runner.push_success(&registration_for(&spec)); // Startup App uninstall: read
    runner.push_success("The operation completed successfully."); // reg delete
    runner.push_success(&registration_for(&spec)); // task uninstall: read
    runner.push_failure("ERROR: The task is not running."); // /End
    runner.push_success("SUCCESS"); // /Delete
    assert_eq!(backend.uninstall(&runner, &spec)?, UninstallOutcome::Removed);
    let calls = steps(&runner);
    assert!(
        calls.contains(&step(REG, "delete")) && calls.contains(&step(SCHTASKS, "/Delete")),
        "{calls:?}"
    );
    Ok(())
}

#[test]
fn every_command_for_another_web_toml_is_refused_rather_than_acting_on_a_server_it_cannot_find()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = windows_spec(dir.path());
    let registered = registration_for(&another_config(dir.path()));
    let backend = WindowsBackend::from(WindowsMechanism::StartupApp);
    let commands: [(&str, Command<'_>); 4] = [
        ("status", &|runner| backend.status(runner, &spec).map(drop)),
        ("start", &|runner| backend.start(runner, &spec)),
        ("stop", &|runner| backend.stop(runner, &spec)),
        ("uninstall", &|runner| backend.uninstall(runner, &spec).map(drop)),
    ];
    for (name, command) in commands {
        let runner = FakeCommandRunner::new();
        runner.push_success(&registered);
        let result = command(&runner);
        assert!(
            matches!(
                &result,
                Err(ServiceError::RegisteredForAnotherConfig { registration, .. })
                    if registration.contains("registered.toml")
            ),
            "{name}: {result:?}"
        );
        assert_eq!(
            steps(&runner),
            [read()],
            "{name}: nothing beyond reading the registration"
        );
        assert!(runner.spawned().is_empty(), "{name}");
    }
    Ok(())
}

#[test]
fn scheduled_task_is_a_windows_only_choice() {
    let choose = |os, scheduled_task| {
        WindowsMechanism::try_from(InstallChoice {
            identity: crate::test_identities::WEB,
            os,
            scheduled_task,
        })
    };
    assert_eq!(choose("windows", false).ok(), Some(WindowsMechanism::StartupApp));
    assert_eq!(choose("linux", false).ok(), Some(WindowsMechanism::StartupApp));
    assert_eq!(choose("windows", true).ok(), Some(WindowsMechanism::ScheduledTask));
    for os in ["linux", "macos"] {
        assert!(
            matches!(choose(os, true), Err(ServiceError::ScheduledTaskOffWindows { os: named, .. }) if named == os),
            "{os}"
        );
    }
}
