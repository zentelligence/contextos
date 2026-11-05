use crate::test_identities::{MCP, WEB};

#[test]
fn the_two_identities_share_no_name_a_host_registers() {
    assert_ne!(WEB.unit_name, MCP.unit_name);
    assert_ne!(WEB.launchd_label, MCP.launchd_label);
    assert_ne!(WEB.windows_name, MCP.windows_name);
}

use std::path::{Path, PathBuf};

use tempfile::tempdir;

use crate::windows_process::tests::{POWERSHELL, registration_for, script_of, windows_spec};
use crate::{
    FakeCommandRunner, LaunchdBackend, ScheduledTaskBackend, ServiceBackend, ServiceError, ServiceSpec,
    StartupAppBackend,
};

/// A `contextos` service: its identity, serving HTTP alone on a given address.
fn mcp_spec(base: ServiceSpec) -> ServiceSpec {
    ServiceSpec {
        identity: MCP,
        run_args: ["--http", "127.0.0.1:9100", "--no-stdio"].map(str::to_owned).to_vec(),
        ..base
    }
}

fn base_spec(dir: &Path) -> ServiceSpec {
    ServiceSpec {
        identity: WEB,
        binary_path: dir.join("contextos"),
        config_path: dir.join("config.toml"),
        run_args: Vec::new(),
        home_dir: dir.join("home"),
        config_dir: dir.join("config"),
        process_record: dir.join("config.toml.process.json"),
        system_dir: PathBuf::new(),
    }
}

#[cfg(unix)]
#[test]
fn systemd_registers_the_contextos_unit_running_http_alone() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = mcp_spec(base_spec(dir.path()));
    let runner = FakeCommandRunner::new();

    crate::SystemdUserBackend.install(&runner, &spec)?;

    let unit = std::fs::read_to_string(spec.config_dir.join("systemd/user/contextos.service"))?;
    assert!(unit.contains("Description=ContextOS MCP server\n"), "{unit}");
    let expected = format!(
        "ExecStart=\"{}\" --config \"{}\" \"--http\" \"127.0.0.1:9100\" \"--no-stdio\"\n",
        spec.binary_path.display(),
        spec.config_path.display()
    );
    assert!(unit.contains(&expected), "{unit}");
    assert_eq!(runner.calls()[1].1, ["--user", "enable", "--now", "contextos.service"]);
    Ok(())
}

#[test]
fn launchd_registers_the_contextos_agent_running_http_alone() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = mcp_spec(base_spec(dir.path()));
    let runner = FakeCommandRunner::new();
    runner.push_success("501");

    LaunchdBackend.install(&runner, &spec)?;

    let plist = std::fs::read_to_string(
        spec.home_dir
            .join("Library/LaunchAgents/au.zentelligence.contextos.plist"),
    )?;
    assert!(plist.contains("<string>au.zentelligence.contextos</string>"), "{plist}");
    assert!(
        plist.contains(
            "\t\t<string>--config</string>\n\t\t<string>{config}</string>\n\
             \t\t<string>--http</string>\n\t\t<string>127.0.0.1:9100</string>\n\t\t<string>--no-stdio</string>\n\
             \t</array>"
                .replace("{config}", &spec.config_path.display().to_string())
                .as_str()
        ),
        "{plist}"
    );
    assert_eq!(runner.calls()[1].1, ["bootout", "gui/501/au.zentelligence.contextos"]);
    Ok(())
}

#[test]
fn the_startup_app_registers_the_contextos_value_with_http_alone_arguments() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = mcp_spec(windows_spec(dir.path()));
    let runner = FakeCommandRunner::new();

    StartupAppBackend.install(&runner, &spec)?;

    let (program, args) = &runner.calls()[0];
    assert_eq!(program, "C:\\Windows\\System32\\reg.exe");
    assert_eq!(args[3], "ContextOS MCP");
    let command = args.get(7).ok_or("the Run entry's command")?;
    assert!(
        command.ends_with(" \"--http\" \"127.0.0.1:9100\" \"--no-stdio\""),
        "{command}"
    );
    let binary = spec.binary_path.display().to_string();
    let config = spec.config_path.display().to_string();
    assert_eq!(
        runner.spawned()[0].1,
        [
            "--headless",
            &binary,
            "--config",
            &config,
            "--http",
            "127.0.0.1:9100",
            "--no-stdio"
        ]
    );
    Ok(())
}

#[test]
fn the_scheduled_task_is_named_for_contextos_and_found_by_that_name() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = mcp_spec(windows_spec(dir.path()));
    let runner = FakeCommandRunner::new();

    ScheduledTaskBackend.install(&runner, &spec)?;

    let (_, args) = &runner.calls()[0];
    assert_eq!(args[2], "ContextOS MCP");
    let runner = FakeCommandRunner::new();
    runner.push_success(&registration_for(&spec));
    let _ = ScheduledTaskBackend.status(&runner, &spec);
    let calls = runner.calls();
    assert_eq!(calls[0].0, POWERSHELL);
    let script = script_of(&calls[0]);
    assert!(
        script.contains("Get-ScheduledTask -TaskName 'ContextOS MCP'"),
        "{script}"
    );
    Ok(())
}

#[test]
fn a_registration_for_another_config_names_contextos_and_config_toml() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let spec = mcp_spec(windows_spec(dir.path()));

    let error = crate::windows_process::registered_for(&spec, "\"conhost\" --config \"C:\\other.toml\"")
        .err()
        .ok_or("another config is refused")?;

    let message = error.to_string();
    assert!(matches!(error, ServiceError::RegisteredForAnotherConfig { .. }));
    assert!(message.contains("the contextos service is registered"), "{message}");
    assert!(message.contains("naming the registered config.toml"), "{message}");
    assert!(message.contains("`contextos service install`"), "{message}");
    assert!(!message.contains("contextos-web"), "{message}");
    Ok(())
}
