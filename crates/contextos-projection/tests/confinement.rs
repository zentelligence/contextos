//! Confinement and atomicity of the projection write surface, against real temporary folders.

#[cfg(unix)]
use std::cell::RefCell;
use std::fs;
use std::io;
use std::path::Path;

use contextos_core::ContentHash;
use contextos_fs::host_file::{FolderReplaceObserver, ReplaceStep};
use contextos_projection::{FileExtension, ProjectionError, ProjectionTarget, SkillFilePath, SkillFolder, Slug};
use tempfile::tempdir;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn slug(value: &str) -> Result<Slug, ProjectionError> {
    Slug::try_from(value)
}

fn md() -> Result<FileExtension, ProjectionError> {
    FileExtension::try_from("md")
}

fn skill(files: &[(&str, &str)]) -> Result<SkillFolder, ProjectionError> {
    let mut entries = Vec::new();
    for (name, content) in files {
        entries.push((SkillFilePath::try_from(*name)?, content.as_bytes().to_vec()));
    }
    SkillFolder::try_from(entries)
}

fn listing(dir: &Path) -> Result<Vec<String>, io::Error> {
    let mut names: Vec<String> = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    Ok(names)
}

#[test]
fn an_agent_file_round_trips_with_its_hash() -> TestResult {
    let root = tempdir()?;
    let target = ProjectionTarget::try_from(root.path())?;

    let written = target.write_agent_file(&slug("reviewer")?, &md()?, b"# reviewer")?;

    assert_eq!(fs::read(root.path().join("reviewer.md"))?, b"# reviewer");
    assert_eq!(
        target.agent_file_hash(&slug("reviewer")?, &md()?)?,
        Some(written.clone())
    );
    assert_eq!(written, ContentHash::from(sha2_digest(b"# reviewer")));
    assert_eq!(target.agent_file_hash(&slug("absent")?, &md()?)?, None);
    assert!(target.remove_agent_file(&slug("reviewer")?, &md()?)?);
    assert!(!target.remove_agent_file(&slug("reviewer")?, &md()?)?);
    assert_eq!(listing(root.path())?, Vec::<String>::new());
    Ok(())
}

fn sha2_digest(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).into()
}

#[test]
fn a_skill_folder_round_trips_and_its_hash_matches_the_written_set() -> TestResult {
    let root = tempdir()?;
    let target = ProjectionTarget::try_from(root.path())?;
    let folder = skill(&[("SKILL.md", "body"), ("scripts/run.sh", "echo")])?;

    let written = target.replace_skill_folder(&slug("deploy")?, &folder)?;

    assert_eq!(fs::read_to_string(root.path().join("deploy/scripts/run.sh"))?, "echo");
    assert_eq!(written, ContentHash::from(&folder));
    assert_eq!(target.read_skill_folder(&slug("deploy")?)?, Some(folder));
    assert_eq!(listing(root.path())?, vec!["deploy".to_owned()]);

    let smaller = skill(&[("SKILL.md", "v2")])?;
    target.replace_skill_folder(&slug("deploy")?, &smaller)?;
    assert!(!root.path().join("deploy/scripts").exists());
    assert!(target.remove_skill_folder(&slug("deploy")?)?);
    assert_eq!(target.read_skill_folder(&slug("deploy")?)?, None);
    Ok(())
}

struct Interrupt;
impl FolderReplaceObserver for Interrupt {
    fn reached(&self, step: ReplaceStep) -> io::Result<()> {
        if step == ReplaceStep::PreviousMovedAside {
            return Err(io::Error::other("interrupted"));
        }
        Ok(())
    }
}
#[test]
fn an_interrupted_skill_replacement_is_repaired_by_recovery() -> TestResult {
    let root = tempdir()?;
    let target = ProjectionTarget::try_from(root.path())?;
    let old = skill(&[("SKILL.md", "old")])?;
    target.replace_skill_folder(&slug("deploy")?, &old)?;

    assert!(
        target
            .replace_skill_folder_observed(&slug("deploy")?, &skill(&[("SKILL.md", "new")])?, &Interrupt)
            .is_err()
    );
    assert!(!root.path().join("deploy").exists());

    let recovery = target.recover_skill_folder(&slug("deploy")?)?;

    assert!(recovery.restored_previous);
    assert_eq!(target.read_skill_folder(&slug("deploy")?)?, Some(old));
    Ok(())
}

#[cfg(unix)]
struct PlantLink<'a> {
    root: &'a Path,
    outside: &'a Path,
    seen: RefCell<Vec<ReplaceStep>>,
}
#[cfg(unix)]
impl FolderReplaceObserver for PlantLink<'_> {
    fn reached(&self, step: ReplaceStep) -> io::Result<()> {
        self.seen.borrow_mut().push(step);
        if step == ReplaceStep::NewFolderBuilt {
            std::os::unix::fs::symlink(self.outside, self.root.join("deploy"))?;
        }
        Ok(())
    }
}
/// Replaces the existing skill folder with a link once the new folder is built, as another process could.
#[cfg(unix)]
struct SwapForLink<'a> {
    root: &'a Path,
    outside: &'a Path,
}
#[cfg(unix)]
impl FolderReplaceObserver for SwapForLink<'_> {
    fn reached(&self, step: ReplaceStep) -> io::Result<()> {
        if step == ReplaceStep::NewFolderBuilt {
            fs::remove_dir_all(self.root.join("deploy"))?;
            std::os::unix::fs::symlink(self.outside, self.root.join("deploy"))?;
        }
        Ok(())
    }
}
#[cfg(unix)]
mod links {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn a_root_that_is_itself_a_link_is_allowed_and_resolved_once() -> TestResult {
        let real = tempdir()?;
        let holder = tempdir()?;
        let link = holder.path().join("agents");
        symlink(real.path(), &link)?;
        let target = ProjectionTarget::try_from(link.as_path())?;

        target.write_agent_file(&slug("reviewer")?, &md()?, b"x")?;

        assert_eq!(fs::read(real.path().join("reviewer.md"))?, b"x");
        Ok(())
    }

    #[test]
    fn a_skill_folder_that_is_a_link_is_rejected_and_the_destination_is_untouched() -> TestResult {
        let root = tempdir()?;
        let outside = tempdir()?;
        fs::write(outside.path().join("keep.txt"), "keep")?;
        symlink(outside.path(), root.path().join("deploy"))?;
        let target = ProjectionTarget::try_from(root.path())?;

        let result = target.replace_skill_folder(&slug("deploy")?, &skill(&[("SKILL.md", "x")])?);

        assert!(matches!(result, Err(ProjectionError::LinkBelowRoot { .. })));
        assert_eq!(listing(outside.path())?, vec!["keep.txt".to_owned()]);
        assert!(matches!(
            target.remove_skill_folder(&slug("deploy")?),
            Err(ProjectionError::LinkBelowRoot { .. })
        ));
        assert!(matches!(
            target.read_skill_folder(&slug("deploy")?),
            Err(ProjectionError::LinkBelowRoot { .. })
        ));
        Ok(())
    }

    #[test]
    fn an_agent_file_that_is_a_link_is_rejected_for_every_operation() -> TestResult {
        let root = tempdir()?;
        let outside = tempdir()?;
        let victim = outside.path().join("victim.md");
        fs::write(&victim, "keep")?;
        symlink(&victim, root.path().join("reviewer.md"))?;
        let target = ProjectionTarget::try_from(root.path())?;

        assert!(matches!(
            target.write_agent_file(&slug("reviewer")?, &md()?, b"x"),
            Err(ProjectionError::LinkBelowRoot { .. })
        ));
        assert!(matches!(
            target.remove_agent_file(&slug("reviewer")?, &md()?),
            Err(ProjectionError::LinkBelowRoot { .. })
        ));
        assert!(matches!(
            target.agent_file_hash(&slug("reviewer")?, &md()?),
            Err(ProjectionError::LinkBelowRoot { .. })
        ));
        assert_eq!(fs::read_to_string(&victim)?, "keep");
        Ok(())
    }

    #[test]
    fn a_link_planted_after_the_target_was_built_is_caught_by_the_recheck() -> TestResult {
        let root = tempdir()?;
        let outside = tempdir()?;
        let target = ProjectionTarget::try_from(root.path())?;
        // The first replacement succeeds, so the target and its skill folder were validated and built once.
        target.replace_skill_folder(&slug("deploy")?, &skill(&[("SKILL.md", "old")])?)?;
        let observer = SwapForLink {
            root: root.path(),
            outside: outside.path(),
        };

        // Only the re-check made after the new folder is built can catch a link that appears in that window.
        let result = target.replace_skill_folder_observed(&slug("deploy")?, &skill(&[("SKILL.md", "new")])?, &observer);

        assert!(
            matches!(result, Err(ProjectionError::LinkBelowRoot { .. })),
            "{result:?}"
        );
        assert_eq!(
            listing(outside.path())?,
            Vec::<String>::new(),
            "nothing may be written through the link"
        );
        Ok(())
    }

    #[test]
    fn a_link_planted_between_building_and_installing_a_skill_folder_is_caught() -> TestResult {
        let root = tempdir()?;
        let outside = tempdir()?;
        let target = ProjectionTarget::try_from(root.path())?;

        let observer = PlantLink {
            root: root.path(),
            outside: outside.path(),
            seen: RefCell::new(Vec::new()),
        };

        let result = target.replace_skill_folder_observed(&slug("deploy")?, &skill(&[("SKILL.md", "x")])?, &observer);

        assert!(matches!(result, Err(ProjectionError::LinkBelowRoot { .. })));
        assert_eq!(*observer.seen.borrow(), vec![ReplaceStep::NewFolderBuilt]);
        assert_eq!(
            listing(outside.path())?,
            Vec::<String>::new(),
            "nothing may be written through the link"
        );
        Ok(())
    }

    #[test]
    fn a_link_inside_a_skill_folder_is_rejected_when_reading_it() -> TestResult {
        let root = tempdir()?;
        let outside = tempdir()?;
        let target = ProjectionTarget::try_from(root.path())?;
        target.replace_skill_folder(&slug("deploy")?, &skill(&[("SKILL.md", "x")])?)?;
        symlink(outside.path(), root.path().join("deploy/escape"))?;

        assert!(matches!(
            target.read_skill_folder(&slug("deploy")?),
            Err(ProjectionError::UnsupportedEntry { .. } | ProjectionError::LinkBelowRoot { .. })
        ));
        Ok(())
    }

    #[test]
    fn replacing_a_skill_folder_discards_a_link_planted_inside_it_without_writing_through_it() -> TestResult {
        let root = tempdir()?;
        let outside = tempdir()?;
        let target = ProjectionTarget::try_from(root.path())?;
        target.replace_skill_folder(&slug("deploy")?, &skill(&[("SKILL.md", "x")])?)?;
        symlink(outside.path(), root.path().join("deploy/scripts"))?;

        // Replacement swaps the whole folder, so the planted inner link is discarded with the old folder and nothing
        // is written through it.
        target.replace_skill_folder(&slug("deploy")?, &skill(&[("scripts/run.sh", "echo")])?)?;

        assert_eq!(listing(outside.path())?, Vec::<String>::new());
        assert_eq!(fs::read_to_string(root.path().join("deploy/scripts/run.sh"))?, "echo");
        Ok(())
    }
}
