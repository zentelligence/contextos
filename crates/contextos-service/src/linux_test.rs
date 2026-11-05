use tempfile::tempdir;

use super::*;
use crate::FakeCommandRunner;
use crate::RunState;

fn spec(dir: &std::path::Path) -> ServiceSpec {
    ServiceSpec {
        identity: crate::test_identities::WEB,
        run_args: Vec::new(),
        binary_path: dir.join("contextos-web"),
        config_path: dir.join("web.toml"),
        home_dir: dir.join("home"),
        config_dir: dir.join("config"),
        process_record: dir.join("web.toml.process.json"),
        system_dir: std::path::PathBuf::new(),
    }
}

#[cfg(unix)]
#[test]
fn install_writes_the_unit_file_and_enables_it() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success(""); // daemon-reload
    runner.push_success(""); // enable --now

    SystemdUserBackend.install(&runner, &spec)?;

    let unit_path = spec
        .config_dir
        .join("systemd")
        .join("user")
        .join("contextos-web.service");
    let contents = std::fs::read_to_string(&unit_path)?;
    assert!(contents.contains(&spec.binary_path.display().to_string()));
    assert!(contents.contains(&spec.config_path.display().to_string()));
    assert!(contents.contains("WantedBy=default.target"));
    assert!(contents.contains("KillMode=mixed"));
    assert!(contents.contains("TimeoutStopSec=30"));
    assert_eq!(
        runner.calls(),
        vec![
            (
                "systemctl".to_owned(),
                vec!["--user".to_owned(), "daemon-reload".to_owned()]
            ),
            (
                "systemctl".to_owned(),
                vec![
                    "--user".to_owned(),
                    "enable".to_owned(),
                    "--now".to_owned(),
                    "contextos-web.service".to_owned(),
                ]
            ),
        ]
    );
    Ok(())
}

#[test]
fn install_fails_when_enable_now_reports_a_non_zero_exit() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success(""); // daemon-reload
    runner.push_failure("unit not found");

    let result = SystemdUserBackend.install(&runner, &spec);

    assert!(matches!(result, Err(ServiceError::CommandFailed { .. })));
    Ok(())
}

#[test]
fn status_reports_not_installed_when_no_unit_file_exists() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();

    let status = SystemdUserBackend.status(&runner, &spec)?;

    assert_eq!(status, ServiceStatus::NotInstalled);
    assert!(
        runner.calls().is_empty(),
        "must not shell out when no unit file is present"
    );
    Ok(())
}

/// Installs the unit, so start/stop/restart/status have one to act on.
fn installed(dir: &std::path::Path) -> Result<(ServiceSpec, FakeCommandRunner), ServiceError> {
    let spec = spec(dir);
    let runner = FakeCommandRunner::new();
    SystemdUserBackend.install(&runner, &spec)?;
    Ok((spec, FakeCommandRunner::new()))
}

#[test]
fn a_failed_unit_reports_as_crashed() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let (spec, runner) = installed(dir.path())?;
    // `is-active` exits non-zero for both; its output is what tells a failed unit from an inactive one.
    runner.push_output(false, "failed\n");

    let status = SystemdUserBackend.status(&runner, &spec)?;

    assert_eq!(status, ServiceStatus::Installed(RunState::Crashed));
    Ok(())
}

#[test]
fn start_stop_and_restart_drive_the_user_unit() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let (spec, runner) = installed(dir.path())?;

    SystemdUserBackend.start(&runner, &spec)?;
    SystemdUserBackend.stop(&runner, &spec)?;
    SystemdUserBackend.restart(&runner, &spec)?;

    let calls: Vec<Vec<String>> = runner.calls().into_iter().map(|(_, args)| args).collect();
    assert_eq!(
        calls,
        [
            vec!["--user", "start", "contextos-web.service"],
            vec!["--user", "stop", "contextos-web.service"],
            vec!["--user", "reset-failed", "contextos-web.service"],
            vec!["--user", "restart", "contextos-web.service"],
        ]
        .map(|args| args.into_iter().map(str::to_owned).collect::<Vec<_>>())
    );
    Ok(())
}

#[test]
fn status_reports_running_from_is_active_exit_success() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success(""); // daemon-reload
    runner.push_success(""); // enable --now
    SystemdUserBackend.install(&runner, &spec)?;
    runner.push_success("active");

    let status = SystemdUserBackend.status(&runner, &spec)?;

    assert_eq!(status, ServiceStatus::Installed(RunState::Running));
    Ok(())
}

#[test]
fn status_reports_not_running_from_is_active_exit_failure() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success(""); // daemon-reload
    runner.push_success(""); // enable --now
    SystemdUserBackend.install(&runner, &spec)?;
    runner.push_output(false, "inactive\n");

    let status = SystemdUserBackend.status(&runner, &spec)?;

    assert_eq!(status, ServiceStatus::Installed(RunState::Stopped));
    Ok(())
}

#[test]
fn uninstall_reports_not_installed_without_shelling_out_when_no_unit_file_exists()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();

    let outcome = SystemdUserBackend.uninstall(&runner, &spec)?;

    assert_eq!(outcome, UninstallOutcome::NotInstalled);
    assert!(
        runner.calls().is_empty(),
        "`runner.calls()` should be empty, got {:?}",
        runner.calls()
    );
    Ok(())
}

#[test]
fn uninstall_disables_removes_the_unit_file_and_reloads() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success(""); // daemon-reload (install)
    runner.push_success(""); // enable --now (install)
    SystemdUserBackend.install(&runner, &spec)?;
    runner.push_success(""); // disable --now
    runner.push_success(""); // daemon-reload

    let outcome = SystemdUserBackend.uninstall(&runner, &spec)?;

    assert_eq!(outcome, UninstallOutcome::Removed);
    let unit_path = spec
        .config_dir
        .join("systemd")
        .join("user")
        .join("contextos-web.service");
    assert!(!unit_path.exists());
    Ok(())
}

#[test]
fn install_is_idempotent_over_an_existing_installation() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = spec(dir.path());
    let runner = FakeCommandRunner::new();
    runner.push_success("");
    runner.push_success("");
    SystemdUserBackend.install(&runner, &spec)?;

    runner.push_success("");
    runner.push_success("");
    let result = SystemdUserBackend.install(&runner, &spec);

    assert!(result.is_ok());
    Ok(())
}

#[test]
fn a_unit_waiting_to_restart_after_a_crash_counts_as_running() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let (spec, runner) = installed(dir.path())?;
    // `Restart=on-failure`'s wait before starting the server again reads `activating`.
    runner.push_output(false, "activating\n");

    assert_eq!(
        SystemdUserBackend.status(&runner, &spec)?,
        ServiceStatus::Installed(RunState::Running)
    );
    Ok(())
}
