//! A disposable run started with `--vault` must never change the operator's own projections: nothing in the host
//! folders, the instructions files, or the ownership records changes, whatever the configuration file says.

mod support;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use support::projection_doctor::{GLOBAL_INSTRUCTIONS, REVIEWER, snapshot};
use support::projection_world::{BoxError, ProjectionWorld, agent};

type Snapshot = BTreeMap<PathBuf, Option<Vec<u8>>>;

/// The operator's own projections: what both hosts hold and the ownership records, but not the per-vault index and
/// lock state a started server keeps under the state root.
fn operators_projections(world: &ProjectionWorld) -> Result<Vec<Snapshot>, BoxError> {
    [".claude", ".codex", ".agents", ".contextos/projection"]
        .iter()
        .map(|folder| {
            let path = world.home().join(folder);
            if path.exists() {
                snapshot(&path)
            } else {
                Ok(Snapshot::new())
            }
        })
        .collect()
}

/// A home whose configuration at the default location has projected an agent, a skill, and an orientation block, as
/// an operator's real one would have.
fn projected_home() -> Result<ProjectionWorld, BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
    world.write_vault(0, REVIEWER, &agent("Review it."))?;
    world.write_vault(
        0,
        "registry/skills/writing/SKILL.md",
        &support::projection_world::skill("writing"),
    )?;
    world.write_vault(0, GLOBAL_INSTRUCTIONS, "Orientation.\n")?;
    drop(world.projecting_server()?);
    assert!(world.claude_agent("reviewer").is_file(), "the fixture did not project");
    assert!(world.claude_skill("writing").join("SKILL.md").is_file());
    assert!(std::fs::read_to_string(world.home().join(".claude/CLAUDE.md"))?.contains("Orientation."));
    assert!(world.home().join(".contextos/projection").is_dir());
    Ok(world)
}

/// Runs the binary against `vault` with the temporary home, as `contextos --vault <vault> [subcommand]`. With no
/// subcommand it composes the server (which refreshes projections at start-up) and exits when its input closes.
fn run_with_vault(
    world: &ProjectionWorld,
    vault: &std::path::Path,
    subcommand: Option<&str>,
) -> Result<String, BoxError> {
    let output = Command::new(env!("CARGO_BIN_EXE_contextos"))
        .args(["--vault", vault.to_str().ok_or("non-UTF-8 vault")?])
        .args(subcommand)
        .stdin(Stdio::null())
        .env("HOME", world.home())
        .env("USERPROFILE", world.home())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("CONTEXTOS_MCP_CONFIG")
        .output()?;
    // A different, empty vault: whether the command succeeds is beside the point; what it leaves behind is not.
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[test]
fn a_command_line_vault_run_changes_nothing_in_the_operators_hosts_or_ownership_records() -> Result<(), BoxError> {
    let world = projected_home()?;
    let other = tempfile::Builder::new().prefix("scratch-vault").tempdir()?;
    let before = operators_projections(&world)?;

    run_with_vault(&world, other.path(), None)?;

    assert_eq!(
        operators_projections(&world)?,
        before,
        "a `--vault` run changed the operator's projections"
    );
    Ok(())
}

#[test]
fn a_command_line_vault_run_ignores_a_configuration_file_that_enables_projection() -> Result<(), BoxError> {
    let world = projected_home()?;
    let config_dir = world.home().join(".config/contextos");
    std::fs::create_dir_all(&config_dir)?;
    std::fs::write(config_dir.join("config.toml"), "[projection]\nenabled = true\n")?;
    let other = tempfile::Builder::new().prefix("scratch-vault").tempdir()?;
    let before = operators_projections(&world)?;

    run_with_vault(&world, other.path(), None)?;

    assert_eq!(
        operators_projections(&world)?,
        before,
        "a `--vault` run changed the operator's projections although the file enabled projection"
    );
    Ok(())
}

#[test]
fn doctor_with_a_command_line_vault_reports_no_projection_findings_about_the_operators_hosts() -> Result<(), BoxError> {
    let world = projected_home()?;
    let other = tempfile::Builder::new().prefix("scratch-vault").tempdir()?;

    let report = run_with_vault(&world, other.path(), Some("doctor"))?;

    assert!(
        !report.contains("Projection ") && !report.contains("Orientation block"),
        "doctor judged the operator's projections against a disposable vault:\n{report}"
    );
    Ok(())
}
