//! A target root or instructions file re-linked at a vault folder after start-up is refused at the next write, so
//! nothing is ever written into a vault through a host path.
#![cfg(unix)]

mod common;

use std::fs;

use common::{REVIEWER, TestResult, codes, reviewer, world};
use contextos_core::OpKind;
use tempfile::tempdir;

#[test]
fn an_agents_folder_re_linked_at_a_vault_folder_after_start_is_refused_and_the_vault_is_untouched() -> TestResult {
    let world = world(1)?;
    let service = world.service("config-one")?;
    let agents = world.hosts.claude_agents.path().to_path_buf();
    let vault_folder = world.vaults[0].path().join("notes");
    fs::create_dir_all(&vault_folder)?;
    fs::remove_dir(&agents)?;
    std::os::unix::fs::symlink(&vault_folder, &agents)?;
    world.write_vault(0, REVIEWER, &reviewer("Review."))?;

    let warnings = world.project(&service, OpKind::Create, &[(0, REVIEWER)])?;

    assert!(
        codes(&warnings).contains(&"projection/path-rejected"),
        "the re-linked root was not refused: {warnings:?}"
    );
    assert_eq!(
        fs::read_dir(&vault_folder)?.count(),
        0,
        "something was written into the vault folder through the re-linked root"
    );
    Ok(())
}

#[test]
fn an_instructions_file_re_linked_at_a_vault_file_after_start_is_refused() -> TestResult {
    let world = world(1)?;
    let home = tempdir()?;
    fs::create_dir_all(home.path().join(".claude"))?;
    fs::create_dir_all(home.path().join(".codex"))?;
    let claude = home.path().join(".claude/CLAUDE.md");
    let codex = home.path().join(".codex/AGENTS.md");
    fs::write(&claude, "mine\n")?;
    let service = world.service_with_instructions("config-one", &claude, &codex)?;
    let vault_file = world.vaults[0].path().join("AGENTS.md");
    fs::write(&vault_file, "vault content\n")?;
    fs::remove_file(&claude)?;
    std::os::unix::fs::symlink(&vault_file, &claude)?;
    world.write_vault(0, "registry/instructions/global.md", "Orient.\n")?;

    let warnings = world.project(&service, OpKind::Create, &[(0, "registry/instructions/global.md")])?;

    assert!(
        codes(&warnings).contains(&"projection/path-rejected"),
        "the re-linked instructions file was not refused: {warnings:?}"
    );
    assert_eq!(fs::read_to_string(&vault_file)?, "vault content\n");
    Ok(())
}
