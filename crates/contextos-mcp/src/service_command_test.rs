use std::cell::RefCell;
use std::fs;
use std::io;
use std::path::Path;

use contextos_service::CommandOutput;
use tempfile::tempdir;

use super::*;

/// A runner that records what it is asked to run and says every command succeeded, so no real `systemctl` is touched.
#[derive(Default)]
struct RecordingRunner {
    calls: RefCell<Vec<String>>,
}

impl CommandRunner for RecordingRunner {
    fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutput, io::Error> {
        self.calls.borrow_mut().push(format!("{program} {}", args.join(" ")));
        Ok(CommandOutput {
            success: true,
            code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        })
    }

    fn spawn_detached(&self, program: &str, args: &[&str]) -> Result<(), io::Error> {
        self.calls
            .borrow_mut()
            .push(format!("detached {program} {}", args.join(" ")));
        Ok(())
    }
}

/// A Linux host rooted in `dir`, so nothing under the operator's home or configuration directory is read.
fn linux_host(dir: &Path) -> ServiceHost {
    ServiceHost {
        os: "linux",
        binary_path: dir.join("bin").join("contextos"),
        home_dir: dir.join("home"),
        config_dir: dir.join("config"),
        system_dir: std::path::PathBuf::new(),
    }
}

fn unit_path(dir: &Path) -> std::path::PathBuf {
    dir.join("config")
        .join("systemd")
        .join("user")
        .join("contextos.service")
}

fn install() -> ServiceAction {
    ServiceAction::Install { scheduled_task: false }
}

fn command(dir: &Path, http: Option<CliHttp>, config: &str) -> Result<ServiceCommand, io::Error> {
    let config_path = dir.join("config.toml");
    fs::write(&config_path, config)?;
    Ok(ServiceCommand { config_path, http })
}

#[cfg(unix)]
#[test]
fn install_registers_the_server_to_serve_http_alone_on_the_configured_bind() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let command = command(dir.path(), None, "")?;
    let runner = RecordingRunner::default();

    let report = run_service_command_on(install(), &command, &linux_host(dir.path()), &runner)?;

    assert_eq!(report, "Installed and started the contextos service.");
    let unit = fs::read_to_string(unit_path(dir.path()))?;
    let expected = format!(
        "ExecStart=\"{}\" --config \"{}\" \"--http\" \"--no-stdio\"\n",
        dir.path().join("bin").join("contextos").display(),
        command.config_path.display()
    );
    assert!(unit.contains(&expected), "{unit}");
    assert!(unit.contains("Description=ContextOS MCP server"), "{unit}");
    assert_eq!(
        *runner.calls.borrow(),
        [
            "systemctl --user daemon-reload",
            "systemctl --user enable --now contextos.service"
        ]
    );
    Ok(())
}

#[test]
fn install_with_an_address_registers_that_address() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let command = command(dir.path(), Some(CliHttp::Bind("127.0.0.1:9100".to_owned())), "")?;

    run_service_command_on(
        install(),
        &command,
        &linux_host(dir.path()),
        &RecordingRunner::default(),
    )?;

    let unit = fs::read_to_string(unit_path(dir.path()))?;
    assert!(
        unit.contains("\"--http\" \"127.0.0.1:9100\" \"--no-stdio\"\n"),
        "{unit}"
    );
    Ok(())
}

#[test]
fn install_refuses_a_configuration_that_cannot_load_and_registers_nothing() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let command = command(dir.path(), None, "this is not = [valid toml")?;
    let runner = RecordingRunner::default();

    let result = run_service_command_on(install(), &command, &linux_host(dir.path()), &runner);

    let message = result.err().ok_or("a broken configuration is refused")?.to_string();
    assert!(message.contains("was not installed"), "{message}");
    assert!(
        message.contains(&command.config_path.display().to_string()),
        "{message}"
    );
    assert!(!unit_path(dir.path()).exists());
    assert!(runner.calls.borrow().is_empty());
    Ok(())
}

#[test]
fn install_refuses_an_address_the_server_would_refuse_to_bind() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    for (http, expected) in [
        (Some(CliHttp::Bind("not an address".to_owned())), "not an address"),
        (Some(CliHttp::Bind("0.0.0.0:3737".to_owned())), "token"),
    ] {
        let command = command(dir.path(), http, "")?;
        let runner = RecordingRunner::default();

        let result = run_service_command_on(install(), &command, &linux_host(dir.path()), &runner);

        let message = result.err().ok_or("the address is refused")?.to_string();
        assert!(message.contains("was not installed"), "{message}");
        assert!(message.contains(expected), "{message}");
        assert!(!unit_path(dir.path()).exists());
        assert!(runner.calls.borrow().is_empty());
    }
    Ok(())
}

#[test]
fn install_refuses_a_configured_bind_that_needs_a_token_the_service_would_not_have()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let command = command(dir.path(), None, "[server.http]\nbind = \"0.0.0.0:3737\"\n")?;

    let result = run_service_command_on(
        install(),
        &command,
        &linux_host(dir.path()),
        &RecordingRunner::default(),
    );

    let message = result
        .err()
        .ok_or("a non-loopback bind without a token is refused")?
        .to_string();
    assert!(message.contains("0.0.0.0:3737"), "{message}");
    assert!(!unit_path(dir.path()).exists());
    Ok(())
}

#[test]
fn install_refuses_a_configuration_file_that_does_not_exist() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let command = ServiceCommand {
        config_path: dir.path().join("absent.toml"),
        http: None,
    };

    let result = run_service_command_on(
        install(),
        &command,
        &linux_host(dir.path()),
        &RecordingRunner::default(),
    );

    let message = result.err().ok_or("a missing configuration is refused")?.to_string();
    assert!(message.contains("absent.toml"), "{message}");
    assert!(message.contains("contextos config"), "{message}");
    assert!(!unit_path(dir.path()).exists());
    Ok(())
}

#[test]
fn status_on_a_host_with_nothing_installed_names_this_program_and_runs_nothing()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let command = ServiceCommand {
        config_path: dir.path().join("config.toml"),
        http: None,
    };
    let runner = RecordingRunner::default();

    let report = run_service_command_on(ServiceAction::Status, &command, &linux_host(dir.path()), &runner)?;

    assert_eq!(report, "The contextos service is not installed.");
    assert!(runner.calls.borrow().is_empty());
    Ok(())
}

#[test]
fn uninstall_and_start_on_a_host_with_nothing_installed_say_so() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let command = ServiceCommand {
        config_path: dir.path().join("config.toml"),
        http: None,
    };
    let host = linux_host(dir.path());
    let runner = RecordingRunner::default();

    assert_eq!(
        run_service_command_on(ServiceAction::Uninstall, &command, &host, &runner)?,
        "The contextos service was not installed."
    );
    assert_eq!(
        run_service_command_on(ServiceAction::Start, &command, &host, &runner)?,
        "The contextos service is not installed; run `contextos service install`."
    );
    Ok(())
}

#[test]
fn scheduled_task_is_refused_off_windows_naming_this_program() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let command = command(dir.path(), None, "")?;

    let result = run_service_command_on(
        ServiceAction::Install { scheduled_task: true },
        &command,
        &linux_host(dir.path()),
        &RecordingRunner::default(),
    );

    let message = result
        .err()
        .ok_or("--scheduled-task is refused off Windows")?
        .to_string();
    assert!(message.contains("`contextos service install`"), "{message}");
    Ok(())
}

#[test]
fn the_process_record_sits_beside_config_toml_under_its_name() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let config = dir.path().join("config.toml");

    let guard = claim_process_record(&config)?;

    let record = dir.path().join("config.toml.process.json");
    assert!(record.exists());
    drop(guard);
    assert!(!record.exists(), "a clean stop removes the record");
    Ok(())
}

/// The service reads the variable `token_env` names from its own environment at each start, and refuses to start
/// without it, so a declared variable counts as a configured token here even though this shell may not have it.
#[test]
fn install_accepts_a_non_loopback_bind_whose_token_comes_from_a_named_variable()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let command = command(
        dir.path(),
        None,
        "[server.http]\nbind = \"0.0.0.0:3737\"\ntoken_env = \"CONTEXTOS_TEST_UNSET_SERVICE_TOKEN\"\n",
    )?;

    run_service_command_on(
        install(),
        &command,
        &linux_host(dir.path()),
        &RecordingRunner::default(),
    )?;

    assert!(unit_path(dir.path()).exists());
    Ok(())
}
