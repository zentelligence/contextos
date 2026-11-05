use tempfile::tempdir;

use super::*;
use crate::FakeCommandRunner;
use crate::test_identities::{MCP, WEB};

fn linux_host(dir: &Path) -> ServiceHost {
    ServiceHost {
        os: "linux",
        binary_path: dir.join("bin"),
        home_dir: dir.join("home"),
        config_dir: dir.join("config"),
        system_dir: PathBuf::new(),
    }
}

fn request(identity: ServiceIdentity, dir: &Path, file: &str) -> ServiceRequest {
    ServiceRequest {
        identity,
        config_path: dir.join(file),
        run_args: vec!["--serve".to_owned()],
    }
}

#[test]
fn every_action_reports_in_the_name_of_the_program_it_acts_for() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let host = linux_host(dir.path());
    for (identity, file) in [(WEB, "web.toml"), (MCP, "config.toml")] {
        let request = request(identity, dir.path(), file);
        let program = identity.program;
        let runner = FakeCommandRunner::new();

        assert_eq!(
            run_on(ServiceAction::Status, &request, &host, &runner)?,
            format!("The {program} service is not installed.")
        );
        assert_eq!(
            run_on(ServiceAction::Uninstall, &request, &host, &runner)?,
            format!("The {program} service was not installed.")
        );
        assert_eq!(
            run_on(ServiceAction::Start, &request, &host, &runner)?,
            format!("The {program} service is not installed; run `{program} service install`.")
        );
        assert_eq!(
            run_on(
                ServiceAction::Install { scheduled_task: false },
                &request,
                &host,
                &runner
            )?,
            format!("Installed and started the {program} service.")
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn install_registers_the_requested_arguments_after_the_absolute_config_path() -> Result<(), Box<dyn std::error::Error>>
{
    let dir = tempdir()?;
    let host = linux_host(dir.path());

    run_on(
        ServiceAction::Install { scheduled_task: false },
        &request(MCP, dir.path(), "config.toml"),
        &host,
        &FakeCommandRunner::new(),
    )?;

    let unit = std::fs::read_to_string(host.config_dir.join("systemd/user/contextos.service"))?;
    assert!(
        unit.contains(&format!(
            "--config \"{}\" \"--serve\"\n",
            dir.path().join("config.toml").display()
        )),
        "{unit}"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_relative_config_path_is_registered_as_an_absolute_one() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let host = linux_host(dir.path());
    let request = ServiceRequest {
        identity: MCP,
        config_path: PathBuf::from("relative.toml"),
        run_args: Vec::new(),
    };

    run_on(
        ServiceAction::Install { scheduled_task: false },
        &request,
        &host,
        &FakeCommandRunner::new(),
    )?;

    let unit = std::fs::read_to_string(host.config_dir.join("systemd/user/contextos.service"))?;
    let registered = std::path::absolute("relative.toml")?;
    assert!(
        unit.contains(&format!("--config \"{}\"", registered.display())),
        "{unit}"
    );
    Ok(())
}

#[test]
fn an_unsupported_platform_is_refused_naming_the_program() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let host = ServiceHost {
        os: "plan9",
        ..linux_host(dir.path())
    };

    let error = run_on(
        ServiceAction::Status,
        &request(MCP, dir.path(), "config.toml"),
        &host,
        &FakeCommandRunner::new(),
    )
    .err()
    .ok_or("an unsupported platform is refused")?;

    assert!(error.to_string().starts_with("contextos service management"), "{error}");
    Ok(())
}
