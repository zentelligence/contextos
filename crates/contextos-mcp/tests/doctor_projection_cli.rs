//! `contextos doctor` lists agent host projection findings, and `contextos doctor --resolve` repairs the auto-fixable
//! ones exactly as the `doctor_resolve` tool does, previews them with `--dry-run`, and leaves a configuration that
//! does not project alone.

mod support;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use contextos_mcp::{Config, ConfigLocation, ContextOsServer, ProjectionConfig};
use support::projection_doctor::{REVIEWER, snapshot};
use support::projection_world::{BoxError, ProjectionWorld, agent};

/// A world whose one vault is described by a configuration file on disk, so the command line and an in-process server
/// are the same configuration and own the same projected entries.
struct CliWorld {
    world: ProjectionWorld,
    fixture: tempfile::TempDir,
    config: PathBuf,
}

impl CliWorld {
    fn new() -> Result<Self, BoxError> {
        let world = ProjectionWorld::new(1)?;
        world.install_hosts()?;
        let fixture = tempfile::Builder::new().prefix("fixture").tempdir()?;
        let config = fixture.path().join("config.toml");
        let cli = Self { world, fixture, config };
        cli.write_config(true)?;
        Ok(cli)
    }

    fn state(&self) -> PathBuf {
        self.fixture.path().join("state")
    }

    fn write_config(&self, projecting: bool) -> Result<(), BoxError> {
        let projection = if projecting {
            "[projection]\nenabled = true\n"
        } else {
            ""
        };
        std::fs::write(
            &self.config,
            format!(
                "[[vault]]\npath = {}\nstate_directory = {}\n[vault.index_md]\nenabled = false\n[vault.git]\nenabled = false\n{projection}",
                toml::Value::String(self.world.vault(0).to_string_lossy().into_owned()),
                toml::Value::String(self.state().to_string_lossy().into_owned())
            ),
        )?;
        Ok(())
    }

    /// The same configuration as the file, in process: projecting it records entries the command line then owns.
    fn project(&self) -> Result<(), BoxError> {
        let mut config: Config = self.world.config_from_file(
            ConfigLocation::Named,
            self.config.clone(),
            Some(ProjectionConfig {
                enabled: Some(true),
                ..ProjectionConfig::default()
            }),
        )?;
        for vault in &mut config.vaults {
            vault.state_directory = Some(self.state());
            vault.git.enabled = false;
            vault.index_md.enabled = false;
        }
        drop(ContextOsServer::try_from(config)?);
        Ok(())
    }

    fn run(&self, arguments: &[&str]) -> Result<(Output, String), BoxError> {
        let output = Command::new(env!("CARGO_BIN_EXE_contextos"))
            .args(["--config", self.config.to_str().ok_or("non-UTF-8 config")?])
            .args(arguments)
            .env("HOME", self.world.home())
            .env("USERPROFILE", self.world.home())
            .output()?;
        let stdout = String::from_utf8(output.stdout.clone())?;
        Ok((output, stdout))
    }

    /// Projects reviewer, critic, retired, and mine, then drifts them: reviewer is stale, the Claude critic is edited
    /// by hand, retired is orphaned, and the operator's own file sits at the Claude slug mine.
    fn drift(&self) -> Result<(), BoxError> {
        let world = &self.world;
        world.write_vault(0, REVIEWER, &agent("Review it."))?;
        world.write_vault(0, "registry/agents/critic.md", &agent("Criticise it."))?;
        world.write_vault(0, "registry/agents/retired.md", &agent("Retire it."))?;
        world.write_vault(0, "registry/agents/mine.md", &agent("Mine."))?;
        std::fs::create_dir_all(world.home().join(".claude/agents"))?;
        std::fs::write(world.claude_agent("mine"), "The operator's own agent.\n")?;
        self.project()?;
        assert!(world.claude_agent("critic").is_file(), "the fixture was not projected");
        world.write_vault(0, REVIEWER, &agent("Review it differently."))?;
        std::fs::write(world.claude_agent("critic"), "edited by hand\n")?;
        std::fs::remove_file(world.vault(0).join("registry/agents/retired.md"))?;
        Ok(())
    }
}

fn lines_naming<'a>(stdout: &'a str, slug: &str, status: &str) -> Vec<&'a str> {
    stdout
        .lines()
        .filter(|line| line.contains(slug) && line.contains(status))
        .collect()
}

fn read(path: &Path) -> Result<String, BoxError> {
    Ok(std::fs::read_to_string(path)?)
}

#[test]
fn doctor_lists_each_projection_finding_and_changes_nothing() -> Result<(), BoxError> {
    let cli = CliWorld::new()?;
    cli.drift()?;
    let before = snapshot(cli.world.home())?;

    let (output, stdout) = cli.run(&["doctor"])?;

    assert!(!output.status.success(), "{stdout}");
    for slug in ["reviewer", "critic", "retired", "mine"] {
        assert!(
            !lines_naming(&stdout, slug, "FAIL").is_empty(),
            "no failing line names {slug}: {stdout}"
        );
    }
    assert_eq!(
        snapshot(cli.world.home())?,
        before,
        "doctor without --resolve must write nothing"
    );
    Ok(())
}

#[test]
fn doctor_resolve_repairs_the_auto_fixable_projection_findings_and_leaves_the_operators_file() -> Result<(), BoxError> {
    let cli = CliWorld::new()?;
    cli.drift()?;

    let (output, stdout) = cli.run(&["doctor", "--resolve"])?;

    let resolved = stdout.lines().filter(|line| line.starts_with("Resolved: ")).count();
    assert_eq!(
        resolved, 5,
        "stale for two hosts, hand-edited, orphaned for two hosts: {stdout}"
    );
    assert!(
        !stdout.lines().any(|line| line.starts_with("Not resolved: ")),
        "{stdout}"
    );
    assert!(read(&cli.world.claude_agent("reviewer"))?.contains("Review it differently."));
    assert!(read(&cli.world.codex_agent("reviewer"))?.contains("Review it differently."));
    assert!(read(&cli.world.claude_agent("critic"))?.contains("Criticise it."));
    assert!(!cli.world.claude_agent("retired").exists() && !cli.world.codex_agent("retired").exists());
    assert_eq!(read(&cli.world.claude_agent("mine"))?, "The operator's own agent.\n");
    assert!(
        !output.status.success() && !lines_naming(&stdout, "mine", "FAIL").is_empty(),
        "the collision needs the operator, so doctor still fails and names it: {stdout}"
    );
    for slug in ["reviewer", "critic", "retired"] {
        assert!(
            lines_naming(
                stdout.split("ContextOS MCP doctor").last().unwrap_or_default(),
                slug,
                "FAIL"
            )
            .is_empty(),
            "the report after the resolution still lists {slug}: {stdout}"
        );
    }
    Ok(())
}

#[test]
fn doctor_resolve_dry_run_previews_the_same_projection_repairs_and_writes_nothing() -> Result<(), BoxError> {
    let cli = CliWorld::new()?;
    cli.drift()?;
    let before = snapshot(cli.world.home())?;

    let (_, stdout) = cli.run(&["doctor", "--resolve", "--dry-run"])?;

    let planned = stdout
        .lines()
        .filter(|line| line.starts_with("Would resolve: "))
        .count();
    assert_eq!(planned, 5, "{stdout}");
    assert_eq!(snapshot(cli.world.home())?, before, "a dry run must write nothing");
    Ok(())
}

#[test]
fn doctor_resolve_leaves_the_host_folders_alone_when_the_configuration_does_not_project() -> Result<(), BoxError> {
    let cli = CliWorld::new()?;
    cli.drift()?;
    let (_, enabled) = cli.run(&["doctor"])?;
    assert!(
        !lines_naming(&enabled, "critic", "FAIL").is_empty(),
        "the fixture must show a finding when projection is on: {enabled}"
    );
    cli.write_config(false)?;
    let before = snapshot(cli.world.home())?;

    let (_, stdout) = cli.run(&["doctor", "--resolve"])?;

    assert!(!stdout.lines().any(|line| line.starts_with("Resolved: ")), "{stdout}");
    assert!(lines_naming(&stdout, "critic", "FAIL").is_empty(), "{stdout}");
    assert_eq!(snapshot(cli.world.home())?, before, "nothing in the home may change");
    Ok(())
}
