//! An installed host whose agents (or skills) folder does not exist has only that folder's entries left unassessed:
//! its other folder and its orientation block are still assessed by `contextos doctor` and repaired by
//! `contextos doctor --resolve`, and nothing is created.

mod support;
use std::path::PathBuf;
use std::process::Command;

use contextos_mcp::{Config, ConfigLocation, ContextOsServer, ProjectionConfig};
use support::projection_doctor::{GLOBAL_INSTRUCTIONS, REVIEWER};
use support::projection_world::{BoxError, ProjectionWorld, agent, skill};

const SKILL: &str = "registry/skills/writing/SKILL.md";

struct Cli {
    world: ProjectionWorld,
    fixture: tempfile::TempDir,
    config: PathBuf,
}

impl Cli {
    fn state(&self) -> PathBuf {
        self.fixture.path().join("state")
    }

    fn run(&self, arguments: &[&str]) -> Result<(bool, String, String), BoxError> {
        let output = Command::new(env!("CARGO_BIN_EXE_contextos"))
            .args(["--config", self.config.to_str().ok_or("non-UTF-8 config")?])
            .args(arguments)
            .env("HOME", self.world.home())
            .env("USERPROFILE", self.world.home())
            .output()?;
        Ok((
            output.status.success(),
            String::from_utf8(output.stdout)?,
            String::from_utf8(output.stderr)?,
        ))
    }
}

/// Everything projected to both hosts by the configuration the binary then loads, then Claude Code's agents folder
/// removed, the skill changed in the vault, and the global instructions changed.
fn without_claude_agents_folder() -> Result<Cli, BoxError> {
    let world = ProjectionWorld::new(1)?;
    world.install_hosts()?;
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
    let cli = Cli { world, fixture, config };
    let world = &cli.world;
    world.write_vault(0, REVIEWER, &agent("Review it."))?;
    world.write_vault(0, SKILL, &skill("writing"))?;
    world.write_vault(0, GLOBAL_INSTRUCTIONS, "First orientation.\n")?;
    let mut projecting: Config = world.config_from_file(
        ConfigLocation::Named,
        cli.config.clone(),
        Some(ProjectionConfig {
            enabled: Some(true),
            ..ProjectionConfig::default()
        }),
    )?;
    for vault in &mut projecting.vaults {
        vault.state_directory = Some(cli.state());
        vault.git.enabled = false;
        vault.index_md.enabled = false;
    }
    drop(ContextOsServer::try_from(projecting)?);
    assert!(
        world.claude_skill("writing").join("SKILL.md").is_file(),
        "the fixture was not projected"
    );
    std::fs::remove_dir_all(world.home().join(".claude/agents"))?;
    world.write_vault(
        0,
        SKILL,
        &skill("writing").replace("How to writing.", "How to write well."),
    )?;
    world.write_vault(0, GLOBAL_INSTRUCTIONS, "Second orientation.\n")?;
    Ok(cli)
}

fn failing_lines<'a>(stdout: &'a str, words: &[&str]) -> Vec<&'a str> {
    stdout
        .lines()
        .filter(|line| line.contains("FAIL") && words.iter().all(|word| line.to_lowercase().contains(word)))
        .collect()
}

#[test]
fn doctor_still_assesses_the_hosts_skills_and_orientation_block_and_creates_nothing() -> Result<(), BoxError> {
    let cli = without_claude_agents_folder()?;

    let (_, stdout, stderr) = cli.run(&["doctor"])?;

    assert_eq!(
        failing_lines(&stdout, &["stale", "writing", "claude"]).len(),
        1,
        "{stdout}"
    );
    assert_eq!(failing_lines(&stdout, &["orientation", "claude"]).len(), 1, "{stdout}");
    assert!(failing_lines(&stdout, &["reviewer", "claude"]).is_empty(), "{stdout}");
    assert!(
        !cli.world.home().join(".claude/agents").exists(),
        "doctor created the folder"
    );
    assert!(
        !stderr.contains("could not be prepared"),
        "a doctor run logged a warning about folders that could not be prepared: {stderr}"
    );
    Ok(())
}

#[test]
fn doctor_resolve_still_repairs_the_hosts_skills_and_orientation_block_and_creates_nothing() -> Result<(), BoxError> {
    let cli = without_claude_agents_folder()?;

    let (_, stdout, stderr) = cli.run(&["doctor", "--resolve"])?;

    let resolved = stdout
        .lines()
        .filter(|line| line.starts_with("Resolved: ") && line.to_lowercase().contains("claude"))
        .count();
    assert_eq!(resolved, 2, "the skill and the orientation block: {stdout}");
    assert!(std::fs::read_to_string(cli.world.claude_skill("writing").join("SKILL.md"))?.contains("write well"));
    assert!(std::fs::read_to_string(cli.world.home().join(".claude/CLAUDE.md"))?.contains("Second orientation."));
    assert!(
        !cli.world.home().join(".claude/agents").exists(),
        "doctor --resolve created the folder"
    );
    assert!(!stderr.contains("could not be prepared"), "{stderr}");
    Ok(())
}
