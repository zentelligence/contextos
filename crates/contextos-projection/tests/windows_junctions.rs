//! A junction (a Windows directory link that needs no administrator rights) is refused wherever a link is, and
//! nothing at the junction's destination is ever changed.
//!
//! These tests exist only for Windows; on any other platform the target compiles to nothing.
#![cfg(windows)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{REVIEWER, TestResult, codes, reviewer, skill, world};
use contextos_core::OpKind;
use contextos_projection::{FileExtension, ProjectionTarget, SkillFilePath, SkillFolder, Slug};
use tempfile::tempdir;

fn junction(link: &Path, destination: &Path) -> TestResult {
    // `mklink` reads a forward slash as a switch, so every separator must be the native one.
    let status = Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(link.components().collect::<PathBuf>())
        .arg(destination.components().collect::<PathBuf>())
        .output()?;
    assert!(
        status.status.success(),
        "mklink /J failed: {} {}",
        String::from_utf8_lossy(&status.stdout),
        String::from_utf8_lossy(&status.stderr)
    );
    Ok(())
}

fn slug(text: &str) -> Result<Slug, Box<dyn std::error::Error>> {
    Ok(Slug::try_from(text)?)
}

fn one_file_skill() -> Result<SkillFolder, Box<dyn std::error::Error>> {
    Ok(SkillFolder::try_from(vec![(
        SkillFilePath::try_from("SKILL.md")?,
        b"skill".to_vec(),
    )])?)
}

fn snapshot(folder: &Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(folder)? {
        names.push(entry?.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    Ok(names)
}

#[test]
fn a_junction_at_an_agent_files_location_is_refused_and_its_destination_is_untouched() -> TestResult {
    let dir = tempdir()?;
    let root = dir.path().join("agents");
    let destination = dir.path().join("elsewhere");
    fs::create_dir_all(&root)?;
    fs::create_dir_all(&destination)?;
    fs::write(destination.join("keep.txt"), "keep")?;
    junction(&root.join("reviewer.md"), &destination)?;
    let target = ProjectionTarget::try_from(root.as_path())?;
    let extension = FileExtension::try_from("md")?;

    let written = target.write_agent_file(&slug("reviewer")?, &extension, b"agent");
    let removed = target.remove_agent_file(&slug("reviewer")?, &extension);

    let error = written.err().ok_or("the write went through the junction")?;
    assert_eq!(error.code(), "projection/path-rejected", "{error:?}");
    let error = removed.err().ok_or("the removal went through the junction")?;
    assert_eq!(error.code(), "projection/path-rejected", "{error:?}");
    assert_eq!(snapshot(&destination)?, ["keep.txt"]);
    assert_eq!(fs::read_to_string(destination.join("keep.txt"))?, "keep");
    Ok(())
}

#[test]
fn a_junction_at_a_skill_folder_is_refused_and_its_destination_is_untouched() -> TestResult {
    let dir = tempdir()?;
    let root = dir.path().join("skills");
    let destination = dir.path().join("elsewhere");
    fs::create_dir_all(&root)?;
    fs::create_dir_all(&destination)?;
    fs::write(destination.join("keep.txt"), "keep")?;
    junction(&root.join("triage"), &destination)?;
    let target = ProjectionTarget::try_from(root.as_path())?;

    let replaced = target.replace_skill_folder(&slug("triage")?, &one_file_skill()?);
    let read = target.read_skill_folder(&slug("triage")?);
    let removed = target.remove_skill_folder(&slug("triage")?);

    for result in [replaced.map(|_| ()), read.map(|_| ()), removed.map(|_| ())] {
        let error = result.err().ok_or("the junction was followed")?;
        assert_eq!(error.code(), "projection/path-rejected", "{error:?}");
    }
    assert_eq!(snapshot(&destination)?, ["keep.txt"]);
    Ok(())
}

#[test]
fn a_junction_inside_a_projected_skill_folder_is_refused_and_its_destination_is_untouched() -> TestResult {
    let dir = tempdir()?;
    let root = dir.path().join("skills");
    let destination = dir.path().join("elsewhere");
    fs::create_dir_all(root.join("triage"))?;
    fs::create_dir_all(&destination)?;
    fs::write(destination.join("keep.txt"), "keep")?;
    fs::write(root.join("triage/SKILL.md"), "skill")?;
    junction(&root.join("triage/inner"), &destination)?;
    let target = ProjectionTarget::try_from(root.as_path())?;

    let read = target.read_skill_folder(&slug("triage")?);

    let error = read.err().ok_or("the junction inside the skill was followed")?;
    assert_eq!(error.code(), "projection/path-rejected", "{error:?}");
    assert_eq!(snapshot(&destination)?, ["keep.txt"]);
    Ok(())
}

#[test]
fn a_junction_inside_a_source_skill_folder_is_refused_and_nothing_is_projected() -> TestResult {
    let world = world(1)?;
    let service = world.service("config-one")?;
    let destination = tempdir()?;
    fs::write(destination.path().join("secret.txt"), "secret")?;
    world.write_vault(0, "registry/skills/triage/SKILL.md", &skill("triage"))?;
    junction(&world.vault_file(0, "registry/skills/triage/inner"), destination.path())?;

    let warnings = world.project(&service, OpKind::Create, &[(0, "registry/skills/triage/SKILL.md")])?;

    assert!(codes(&warnings).contains(&"projection/path-rejected"), "{warnings:?}");
    assert!(!world.claude_skill("triage").exists());
    assert!(!world.codex_skill("triage").exists());
    assert_eq!(snapshot(destination.path())?, ["secret.txt"]);
    Ok(())
}

#[test]
fn a_target_root_re_linked_to_a_junction_after_start_is_refused_and_both_destinations_are_untouched() -> TestResult {
    let dir = tempdir()?;
    let original = dir.path().join("dotfiles-agents");
    let replacement = dir.path().join("vault-folder");
    fs::create_dir_all(&original)?;
    fs::create_dir_all(&replacement)?;
    let link = dir.path().join("agents");
    junction(&link, &original)?;
    let target = ProjectionTarget::try_from(link.as_path())?;
    let extension = FileExtension::try_from("md")?;
    target.write_agent_file(&slug("first")?, &extension, b"one")?;
    assert!(original.join("first.md").is_file());

    fs::remove_dir(&link)?;
    junction(&link, &replacement)?;
    let result = target.write_agent_file(&slug("second")?, &extension, b"two");

    let error = result.err().ok_or("the write followed the re-linked junction")?;
    assert_eq!(error.code(), "projection/path-rejected", "{error:?}");
    let replaced = snapshot(&replacement)?;
    assert!(replaced.is_empty(), "{replaced:?}");
    assert_eq!(snapshot(&original)?, ["first.md"]);
    Ok(())
}

#[test]
fn a_junction_at_an_agent_definition_in_a_vault_is_not_projected() -> TestResult {
    let world = world(1)?;
    let service = world.service("config-one")?;
    let destination = tempdir()?;
    fs::write(destination.path().join("secret.txt"), "secret")?;
    world.write_vault(0, "registry/agents/placeholder.md", &reviewer("x"))?;
    fs::remove_file(world.vault_file(0, "registry/agents/placeholder.md"))?;
    junction(&world.vault_file(0, REVIEWER), destination.path())?;

    // A refresh and not an event: an event cannot be built for a path that leaves the vault.
    let warnings = service.refresh();

    assert!(!world.claude_agent("reviewer").exists(), "{warnings:?}");
    assert_eq!(snapshot(destination.path())?, ["secret.txt"]);
    Ok(())
}
