//! One registry layout: a completed write and a full refresh agree on which paths under `registry/` are definitions.

mod common;

use std::fs;
use std::path::Path;

use common::{TestResult, World, codes, reviewer, skill, world};
use contextos_core::OpKind;

/// Every file below `folder`, relative to it and sorted.
fn files_below(folder: &Path) -> Result<Vec<String>, std::io::Error> {
    let mut found = Vec::new();
    let mut pending = vec![folder.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else if let Ok(relative) = path.strip_prefix(folder) {
                found.push(relative.to_string_lossy().into_owned());
            }
        }
    }
    found.sort();
    Ok(found)
}

/// What the four host folders hold, labelled by folder.
fn projected(world: &World) -> Result<Vec<String>, std::io::Error> {
    let mut all = Vec::new();
    for (label, folder) in [
        ("claude agents", world.hosts.claude_agents.path()),
        ("claude skills", world.hosts.claude_skills.path()),
        ("codex agents", world.hosts.codex_agents.path()),
        ("codex skills", world.hosts.codex_skills.path()),
    ] {
        all.extend(files_below(folder)?.into_iter().map(|file| format!("{label}: {file}")));
    }
    Ok(all)
}

#[test]
fn a_plain_file_directly_in_the_skills_folder_is_neither_projected_nor_warned_about() -> TestResult {
    let world = world(1)?;
    let service = world.service("config-one")?;
    let path = "registry/skills/readme.md";
    world.write_vault(0, path, "Notes about the skills, not a skill.\n")?;

    let warnings = world.project(&service, OpKind::Create, &[(0, path)])?;

    assert_eq!(codes(&warnings), Vec::<&str>::new(), "{warnings:?}");
    assert_eq!(projected(&world)?, Vec::<String>::new());
    Ok(())
}

#[test]
fn a_plain_file_moved_or_deleted_directly_in_the_skills_folder_is_not_warned_about_either() -> TestResult {
    let world = world(1)?;
    let service = world.service("config-one")?;
    let path = "registry/skills/readme.md";
    world.write_vault(0, path, "Notes.\n")?;
    fs::rename(
        world.vault_file(0, path),
        world.vault_file(0, "registry/skills/notes.txt"),
    )?;

    let moved = world.project(&service, OpKind::Move, &[(0, path), (0, "registry/skills/notes.txt")])?;
    world.remove_vault(0, "registry/skills/notes.txt")?;
    let deleted = world.project(&service, OpKind::Delete, &[(0, "registry/skills/notes.txt")])?;

    assert_eq!(codes(&moved), Vec::<&str>::new(), "{moved:?}");
    assert_eq!(codes(&deleted), Vec::<&str>::new(), "{deleted:?}");
    Ok(())
}

#[test]
fn a_write_event_and_a_full_refresh_treat_every_sample_path_alike() -> TestResult {
    let agent = reviewer("Review it.");
    let skill_file = skill("triage");
    let samples: [(&str, &str); 11] = [
        ("registry/agents/reviewer.md", agent.as_str()),
        ("registry/agents/index.md", "The managed index.\n"),
        ("registry/agents/notes.txt", "Not a definition.\n"),
        ("registry/agents/team/critic.md", agent.as_str()),
        ("registry/agents/Bad Name.md", agent.as_str()),
        ("registry/skills/triage/SKILL.md", skill_file.as_str()),
        ("registry/skills/triage/notes/extra.txt", "Extra.\n"),
        ("registry/skills/readme.md", "Notes about the skills.\n"),
        ("registry/skills/index.md", "The managed index.\n"),
        ("registry/instructions/other.md", "Not the global instructions.\n"),
        ("registry/roles/critic.md", "Not projected.\n"),
    ];
    let mut differences = Vec::new();
    for (path, content) in samples {
        let by_event = world(1)?;
        let event_service = by_event.service("config-event")?;
        by_event.write_vault(0, path, content)?;
        let event_warnings = by_event.project(&event_service, OpKind::Create, &[(0, path)])?;

        let by_refresh = world(1)?;
        let refresh_service = by_refresh.service("config-refresh")?;
        by_refresh.write_vault(0, path, content)?;
        let refresh_warnings = refresh_service.refresh();

        if codes(&event_warnings) != codes(&refresh_warnings) {
            differences.push(format!(
                "{path}: warnings differ, event {:?} against refresh {:?}",
                codes(&event_warnings),
                codes(&refresh_warnings)
            ));
        }
        if projected(&by_event)? != projected(&by_refresh)? {
            differences.push(format!("{path}: what is projected differs"));
        }
    }
    assert_eq!(differences, Vec::<String>::new());
    Ok(())
}
