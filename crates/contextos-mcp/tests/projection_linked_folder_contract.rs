//! A projection agents or skills folder that is a symbolic link stops the configuration loading, so the server does
//! not start; `contextos doctor` reports it as a failed configuration check instead of aborting, and still runs every
//! vault check.
#![cfg(unix)]

mod support;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use contextos_mcp::{ConfigLocation, ContextOsServer};
use support::projection_world::{BoxError, ProjectionWorld};

/// A world whose Claude Code agents folder is a link to a folder elsewhere, and the link and its destination.
struct Linked {
    world: ProjectionWorld,
    link: PathBuf,
    destination: PathBuf,
    config: PathBuf,
    _fixture: tempfile::TempDir,
}

fn linked() -> Result<Linked, BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    let destination = world.home().join("elsewhere");
    std::fs::create_dir_all(&destination)?;
    let link = world.home().join(".claude/agents");
    std::os::unix::fs::symlink(&destination, &link)?;
    let fixture = tempfile::Builder::new().prefix("fixture").tempdir()?;
    let config = fixture.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "[[vault]]\npath = {}\nstate_directory = {}\n[vault.index_md]\nenabled = false\n[vault.git]\nenabled = \
             false\n[projection]\nenabled = true\n",
            toml::Value::String(world.vault(0).to_string_lossy().into_owned()),
            toml::Value::String(fixture.path().join("state").to_string_lossy().into_owned())
        ),
    )?;
    Ok(Linked {
        world,
        link,
        destination,
        config,
        _fixture: fixture,
    })
}

fn run(linked: &Linked, arguments: &[&str]) -> Result<Output, BoxError> {
    Ok(Command::new(env!("CARGO_BIN_EXE_contextos"))
        .args(["--config", linked.config.to_str().ok_or("non-UTF-8 config")?])
        .args(arguments)
        .stdin(Stdio::null())
        .env("HOME", linked.world.home())
        .env("USERPROFILE", linked.world.home())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("CONTEXTOS_MCP_CONFIG")
        .output()?)
}

fn names(text: &str, link: &Path, destination: &Path) -> bool {
    text.contains(&link.display().to_string()) && text.contains(&destination.display().to_string())
}

#[test]
fn a_server_is_not_built_over_a_linked_agents_folder() -> Result<(), BoxError> {
    let linked = linked()?;

    let built = ContextOsServer::try_from(linked.world.config(ConfigLocation::PlatformDefault, None)?);

    let Err(error) = built else {
        return Err("a server was built over a linked agents folder".into());
    };
    let message = error.to_string();
    assert!(names(&message, &linked.link, &linked.destination), "{message}");
    assert!(message.contains("enabled = false"), "{message}");
    assert!(
        std::fs::read_dir(&linked.destination)?.next().is_none(),
        "something was written through the link"
    );
    Ok(())
}

#[test]
fn the_binary_does_not_start_serving_over_a_linked_agents_folder() -> Result<(), BoxError> {
    let linked = linked()?;

    let output = run(&linked, &[])?;

    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(names(&stderr, &linked.link, &linked.destination), "{stderr}");
    Ok(())
}

#[test]
fn doctor_reports_a_linked_folder_as_a_failed_configuration_check_and_still_runs_the_vault_checks()
-> Result<(), BoxError> {
    let linked = linked()?;

    let output = run(&linked, &["doctor"])?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !output.status.success(),
        "a failed check must fail the command: {stdout}"
    );
    let configuration = stdout
        .lines()
        .find(|line| line.starts_with("Configuration | FAIL"))
        .ok_or_else(|| format!("no failed configuration check: {stdout}"))?;
    assert!(
        names(configuration, &linked.link, &linked.destination),
        "{configuration}"
    );
    assert!(configuration.contains("enabled = false"), "{configuration}");
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("Vault ") && line.contains("PASS")),
        "the vault checks did not run: {stdout}"
    );
    assert!(
        std::fs::read_dir(&linked.destination)?.next().is_none(),
        "something was written through the link"
    );
    Ok(())
}

#[test]
fn doctor_resolve_on_the_command_line_does_not_abort_on_a_linked_folder_either() -> Result<(), BoxError> {
    let linked = linked()?;

    let output = run(&linked, &["doctor", "--resolve"])?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Configuration | FAIL"),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(std::fs::read_dir(&linked.destination)?.next().is_none());
    Ok(())
}
