//! A skill folder swap interrupted by a previous process is completed or reversed by the next start-up refresh.

mod common;

use std::fs;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::{TestResult, World, codes, skill, world};
use contextos_fs::host_file::{FolderReplaceObserver, ReplaceStep};
use contextos_projection::{ProjectionTarget, SkillFilePath, SkillFolder, Slug};

/// Fails a folder swap at the given step, leaving the folders exactly as that step left them.
struct InterruptedAt(ReplaceStep);

impl FolderReplaceObserver for InterruptedAt {
    fn reached(&self, step: ReplaceStep) -> io::Result<()> {
        if step == self.0 {
            Err(io::Error::other("the previous process died here"))
        } else {
            Ok(())
        }
    }
}

fn siblings(folder: &Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(folder)? {
        names.push(entry?.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    Ok(names)
}

#[test]
fn a_start_up_refresh_completes_or_reverses_a_swap_left_by_a_previous_process() -> TestResult {
    for step in [
        ReplaceStep::NewFolderBuilt,
        ReplaceStep::PreviousMovedAside,
        ReplaceStep::NewFolderInstalled,
    ] {
        let world = world(1)?;
        world.write_vault(0, "registry/skills/writing/SKILL.md", &skill("writing"))?;
        let first = world.service("config-one")?;
        assert_eq!(codes(&first.refresh()), Vec::<&str>::new());

        // The operator edits the skill; a process projecting the edit was replacing the projected folder when it died.
        let edited = skill("writing").replace("How to writing.", "How to write well.");
        world.write_vault(0, "registry/skills/writing/SKILL.md", &edited)?;
        let target = ProjectionTarget::try_from(world.hosts.claude_skills.path())?;
        let replacement = SkillFolder::try_from(vec![(
            SkillFilePath::try_from("SKILL.md")?,
            edited.clone().into_bytes(),
        )])?;
        let interrupted =
            target.replace_skill_folder_observed(&Slug::try_from("writing")?, &replacement, &InterruptedAt(step));
        assert!(interrupted.is_err(), "{step:?}: the swap was not interrupted");

        let warnings = world.service("config-one")?.refresh();

        assert!(warnings.is_empty(), "{step:?}: {warnings:?}");
        assert_eq!(
            siblings(world.hosts.claude_skills.path())?,
            ["writing"],
            "{step:?}: leftovers of the swap remain"
        );
        assert_eq!(
            fs::read_to_string(world.claude_skill("writing").join("SKILL.md"))?,
            edited,
            "{step:?}: the folder is not the one the vault defines"
        );
    }
    Ok(())
}

/// Fails the first time a swap has installed the new folder, which leaves the previous folder beside it exactly as a
/// removal that failed would. Later swaps are left alone.
struct LeavesThePreviousFolderBehind {
    installed: AtomicUsize,
}

impl FolderReplaceObserver for LeavesThePreviousFolderBehind {
    fn reached(&self, step: ReplaceStep) -> io::Result<()> {
        if step == ReplaceStep::NewFolderInstalled && self.installed.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(io::Error::other("the previous folder could not be removed"))
        } else {
            Ok(())
        }
    }
}

/// Every entry in the two skill folders other than the installed `writing` folders, which are leftovers.
fn leftovers(world: &World) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut found = Vec::new();
    for folder in [world.hosts.claude_skills.path(), world.hosts.codex_skills.path()] {
        found.extend(siblings(folder)?.into_iter().filter(|name| name != "writing"));
    }
    Ok(found)
}

#[test]
fn a_previous_folder_that_could_not_be_removed_after_the_install_is_a_separate_warning_and_the_install_stands()
-> TestResult {
    let world = world(1)?;
    world.write_vault(0, "registry/skills/writing/SKILL.md", &skill("writing"))?;
    assert_eq!(codes(&world.service("config-one")?.refresh()), Vec::<&str>::new());
    let edited = skill("writing").replace("How to writing.", "How to write well.");
    world.write_vault(0, "registry/skills/writing/SKILL.md", &edited)?;
    let service = world
        .service("config-one")?
        .with_folder_observer(Arc::new(LeavesThePreviousFolderBehind {
            installed: AtomicUsize::new(0),
        }));

    let warnings = service.refresh();

    assert_eq!(
        codes(&warnings),
        ["projection/cleanup-incomplete"],
        "the install is not a failed write, and the leftover has its own warning: {warnings:?}"
    );
    assert!(warnings[0].message.contains("writing"), "{warnings:?}");
    for folder in [world.claude_skill("writing"), world.codex_skill("writing")] {
        assert_eq!(fs::read_to_string(folder.join("SKILL.md"))?, edited);
    }
    assert_eq!(
        leftovers(&world)?.len(),
        1,
        "exactly one previous folder was left behind"
    );
    let findings = service.assess().findings;
    assert!(
        findings.is_empty(),
        "the install is recorded, so it is neither hand edited nor foreign: {findings:?}"
    );
    Ok(())
}

#[test]
fn the_next_run_removes_the_previous_folder_a_failed_cleanup_left_behind() -> TestResult {
    let world = world(1)?;
    world.write_vault(0, "registry/skills/writing/SKILL.md", &skill("writing"))?;
    assert_eq!(codes(&world.service("config-one")?.refresh()), Vec::<&str>::new());
    let edited = skill("writing").replace("How to writing.", "How to write well.");
    world.write_vault(0, "registry/skills/writing/SKILL.md", &edited)?;
    let interrupted = world
        .service("config-one")?
        .with_folder_observer(Arc::new(LeavesThePreviousFolderBehind {
            installed: AtomicUsize::new(0),
        }));
    assert_eq!(codes(&interrupted.refresh()), ["projection/cleanup-incomplete"]);
    assert_eq!(leftovers(&world)?.len(), 1);

    let warnings = world.service("config-one")?.refresh();

    assert_eq!(codes(&warnings), Vec::<&str>::new(), "{warnings:?}");
    assert_eq!(leftovers(&world)?, Vec::<String>::new());
    assert_eq!(
        fs::read_to_string(world.claude_skill("writing").join("SKILL.md"))?,
        edited
    );
    Ok(())
}
