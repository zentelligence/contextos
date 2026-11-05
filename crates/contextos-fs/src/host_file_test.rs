use std::cell::Cell;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tempfile::tempdir;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct FailAt {
    step: ReplaceStep,
    reached: Cell<bool>,
}

impl FailAt {
    fn new(step: ReplaceStep) -> Self {
        Self {
            step,
            reached: Cell::new(false),
        }
    }
}

impl FolderReplaceObserver for FailAt {
    fn reached(&self, step: ReplaceStep) -> io::Result<()> {
        if step == self.step {
            self.reached.set(true);
            return Err(io::Error::other("interrupted"));
        }
        Ok(())
    }
}

fn names(dir: &Path) -> Result<Vec<String>, io::Error> {
    let mut found: Vec<String> = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    found.sort();
    Ok(found)
}

fn fill(content: &'static str) -> impl FnOnce(&Path) -> io::Result<()> {
    move |folder| {
        fs::create_dir_all(folder.join("nested"))?;
        fs::write(folder.join("SKILL.md"), content)?;
        fs::write(folder.join("nested").join("a.txt"), content)
    }
}

fn seed(target: &Path, content: &'static str) -> TestResult {
    fs::create_dir_all(target)?;
    fill(content)(target)?;
    Ok(())
}

#[test]
fn file_write_creates_the_parent_and_the_file() -> TestResult {
    let dir = tempdir()?;
    let path = dir.path().join("a").join("b").join("target.txt");
    write_file_atomically(&path, b"hello")?;
    assert_eq!(fs::read(&path)?, b"hello");
    Ok(())
}

#[test]
fn file_replace_refuses_a_missing_folder_without_creating_it() -> TestResult {
    let dir = tempdir()?;
    let folder = dir.path().join("missing");
    let result = replace_file_atomically(&folder.join("target.txt"), b"hello");
    assert!(matches!(
        result,
        Err(FsError::CreateTemporary { ref source, .. }) if source.kind() == io::ErrorKind::NotFound
    ));
    assert!(!folder.exists());
    Ok(())
}

#[test]
fn file_write_replaces_existing_content_and_leaves_no_temporary_file() -> TestResult {
    let dir = tempdir()?;
    let path = dir.path().join("target.txt");
    fs::write(&path, b"old")?;
    write_file_atomically(&path, b"new")?;
    assert_eq!(fs::read(&path)?, b"new");
    assert_eq!(names(dir.path())?, vec!["target.txt".to_owned()]);
    Ok(())
}

#[test]
fn file_write_rejects_a_path_without_a_parent_folder() {
    let result = write_file_atomically(Path::new("bare.txt"), b"x");
    assert!(matches!(result, Err(FsError::CreateParent { .. })));
}

#[test]
fn file_write_fails_when_the_parent_is_a_file() -> TestResult {
    let dir = tempdir()?;
    let blocker = dir.path().join("blocker");
    fs::write(&blocker, b"x")?;
    let result = write_file_atomically(&blocker.join("target.txt"), b"x");
    assert!(matches!(result, Err(FsError::CreateParent { .. })));
    Ok(())
}

#[test]
fn replacing_a_missing_target_installs_the_new_folder() -> TestResult {
    let dir = tempdir()?;
    let target = dir.path().join("skill");
    replace_folder(&target, fill("new"), &NoFolderFaults)?;
    assert_eq!(fs::read_to_string(target.join("nested").join("a.txt"))?, "new");
    assert_eq!(names(dir.path())?, vec!["skill".to_owned()]);
    Ok(())
}

#[test]
fn replacing_an_existing_target_swaps_content_and_removes_siblings() -> TestResult {
    let dir = tempdir()?;
    let target = dir.path().join("skill");
    seed(&target, "old")?;
    fs::write(target.join("stale.txt"), "stale")?;
    replace_folder(&target, fill("new"), &NoFolderFaults)?;
    assert_eq!(fs::read_to_string(target.join("SKILL.md"))?, "new");
    assert!(
        !target.join("stale.txt").exists(),
        "files absent from the new folder must not survive"
    );
    assert_eq!(names(dir.path())?, vec!["skill".to_owned()]);
    Ok(())
}

#[test]
fn a_failing_builder_leaves_the_target_untouched_and_removes_the_new_folder() -> TestResult {
    let dir = tempdir()?;
    let target = dir.path().join("skill");
    seed(&target, "old")?;
    let result = replace_folder(&target, |_| Err(io::Error::other("build failed")), &NoFolderFaults);
    assert!(matches!(result, Err(FsError::ReplaceDirectory { .. })));
    assert_eq!(fs::read_to_string(target.join("SKILL.md"))?, "old");
    assert_eq!(names(dir.path())?, vec!["skill".to_owned()]);
    Ok(())
}

#[test]
fn a_replacement_interrupted_at_any_step_recovers_to_the_old_or_the_new_folder() -> TestResult {
    let cases = [
        (ReplaceStep::NewFolderBuilt, "old"),
        (ReplaceStep::PreviousMovedAside, "old"),
        (ReplaceStep::NewFolderInstalled, "new"),
        (ReplaceStep::PreviousRemoved, "new"),
    ];
    for (step, expected) in cases {
        let dir = tempdir()?;
        let target = dir.path().join("skill");
        seed(&target, "old")?;
        let fault = FailAt::new(step);

        let result = replace_folder(&target, fill("new"), &fault);

        assert!(fault.reached.get(), "{step:?} was never reached");
        assert!(result.is_err(), "{step:?} should abandon the replacement");
        recover_folder(&target)?;
        assert_eq!(fs::read_to_string(target.join("SKILL.md"))?, expected, "{step:?}");
        assert_eq!(
            fs::read_to_string(target.join("nested").join("a.txt"))?,
            expected,
            "{step:?}"
        );
        assert_eq!(names(dir.path())?, vec!["skill".to_owned()], "{step:?} left a sibling");
    }
    Ok(())
}

#[test]
fn a_first_install_interrupted_at_any_step_recovers_to_nothing_or_the_new_folder() -> TestResult {
    let cases = [
        (ReplaceStep::NewFolderBuilt, None),
        (ReplaceStep::NewFolderInstalled, Some("new")),
        (ReplaceStep::PreviousRemoved, Some("new")),
    ];
    for (step, expected) in cases {
        let dir = tempdir()?;
        let target = dir.path().join("skill");
        let fault = FailAt::new(step);
        let outcome = replace_folder(&target, fill("new"), &fault);
        assert!(outcome.is_err() || !fault.reached.get());
        recover_folder(&target)?;
        match expected {
            None => assert!(names(dir.path())?.is_empty(), "{step:?}"),
            Some(content) => {
                assert_eq!(fs::read_to_string(target.join("SKILL.md"))?, content, "{step:?}");
                assert_eq!(names(dir.path())?, vec!["skill".to_owned()], "{step:?}");
            }
        }
    }
    Ok(())
}

#[test]
fn recovery_reports_each_repair_it_made() -> TestResult {
    let dir = tempdir()?;
    let target = dir.path().join("skill");
    seed(&target, "old")?;
    seed(&dir.path().join(".skill.contextos-new"), "new")?;
    assert_eq!(
        recover_folder(&target)?,
        FolderRecovery {
            removed_incomplete: true,
            ..FolderRecovery::default()
        }
    );

    seed(&dir.path().join(".skill.contextos-old"), "older")?;
    assert_eq!(
        recover_folder(&target)?,
        FolderRecovery {
            removed_previous: true,
            ..FolderRecovery::default()
        }
    );

    fs::rename(&target, dir.path().join(".skill.contextos-old"))?;
    assert_eq!(
        recover_folder(&target)?,
        FolderRecovery {
            restored_previous: true,
            ..FolderRecovery::default()
        }
    );
    assert_eq!(recover_folder(&target)?, FolderRecovery::default());
    Ok(())
}

#[test]
fn recovery_of_a_target_that_never_existed_does_nothing() -> TestResult {
    let dir = tempdir()?;
    let target: PathBuf = dir.path().join("skill");
    assert_eq!(recover_folder(&target)?, FolderRecovery::default());
    Ok(())
}

#[test]
fn replacement_first_repairs_an_earlier_interruption() -> TestResult {
    let dir = tempdir()?;
    let target = dir.path().join("skill");
    seed(&target, "old")?;
    let fault = FailAt::new(ReplaceStep::PreviousMovedAside);
    assert!(replace_folder(&target, fill("middle"), &fault).is_err());

    replace_folder(&target, fill("new"), &NoFolderFaults)?;

    assert_eq!(fs::read_to_string(target.join("SKILL.md"))?, "new");
    assert_eq!(names(dir.path())?, vec!["skill".to_owned()]);
    Ok(())
}

#[test]
fn removing_an_entry_deletes_a_file_a_folder_tree_or_a_link_without_following_it() -> TestResult {
    let dir = tempdir()?;
    let file = dir.path().join("agent.md");
    fs::write(&file, "x")?;
    assert!(remove_entry(&file)?);
    assert!(!file.exists());

    let folder = dir.path().join("skill");
    seed(&folder, "x")?;
    assert!(remove_entry(&folder)?);
    assert!(!folder.exists());

    assert!(!remove_entry(&folder)?, "an absent entry reports false");

    #[cfg(unix)]
    {
        let outside = dir.path().join("outside");
        seed(&outside, "keep")?;
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&outside, &link)?;
        assert!(remove_entry(&link)?);
        assert!(link.symlink_metadata().is_err());
        assert_eq!(fs::read_to_string(outside.join("SKILL.md"))?, "keep");
    }
    Ok(())
}

/// Records what the temporary file looks like at the moment just before it replaces the target.
#[cfg(unix)]
struct InspectTemporary {
    folder: PathBuf,
    target_name: &'static str,
    modes: std::cell::RefCell<Vec<u32>>,
}

#[cfg(unix)]
impl FileReplaceObserver for InspectTemporary {
    fn reached(&self, step: FileReplaceStep) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(step, FileReplaceStep::TemporaryReady);
        for entry in fs::read_dir(&self.folder)? {
            let entry = entry?;
            if entry.file_name() != self.target_name {
                self.modes
                    .borrow_mut()
                    .push(entry.metadata()?.permissions().mode() & 0o777);
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
#[test]
fn the_temporary_file_has_the_targets_permissions_before_it_replaces_the_target() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir()?;
    let target = dir.path().join("CLAUDE.md");
    fs::write(&target, "old")?;
    fs::set_permissions(&target, fs::Permissions::from_mode(0o640))?;
    let observer = InspectTemporary {
        folder: dir.path().to_path_buf(),
        target_name: "CLAUDE.md",
        modes: std::cell::RefCell::new(Vec::new()),
    };

    replace_file_observed(&target, b"new", &observer)?;

    assert_eq!(
        *observer.modes.borrow(),
        vec![0o640],
        "the temporary file must match the target before the rename"
    );
    assert_eq!(fs::read(&target)?, b"new");
    assert_eq!(fs::metadata(&target)?.permissions().mode() & 0o777, 0o640);
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_new_file_keeps_the_temporary_files_default_permissions() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir()?;
    let target = dir.path().join("new.md");

    replace_file_atomically(&target, b"content")?;

    assert_eq!(fs::metadata(&target)?.permissions().mode() & 0o777, 0o600);
    Ok(())
}

#[test]
fn an_observer_that_fails_before_the_rename_leaves_the_target_and_no_temporary_file() -> TestResult {
    struct Refuse;
    impl FileReplaceObserver for Refuse {
        fn reached(&self, _step: FileReplaceStep) -> io::Result<()> {
            Err(io::Error::other("interrupted"))
        }
    }
    let dir = tempdir()?;
    let target = dir.path().join("target.txt");
    fs::write(&target, "old")?;

    let result = replace_file_observed(&target, b"new", &Refuse);

    assert!(
        matches!(result, Err(FsError::AtomicWriteInterrupted { .. })),
        "{result:?}"
    );
    assert_eq!(fs::read(&target)?, b"old");
    assert_eq!(names(dir.path())?, vec!["target.txt".to_owned()]);
    Ok(())
}

/// Interrupts a file write just before its rename.
struct InterruptBeforeRename;

impl FileReplaceObserver for InterruptBeforeRename {
    fn reached(&self, _step: FileReplaceStep) -> io::Result<()> {
        Err(io::Error::other("interrupted"))
    }
}

#[test]
fn creating_a_file_that_does_not_exist_writes_exactly_the_bytes_given() -> TestResult {
    let dir = tempdir()?;
    let path = dir.path().join("job.toml");
    let bytes = b"# a comment\r\nname = \"Nightly\"\r\n";

    create_file_atomically(&path, bytes)?;

    assert_eq!(fs::read(&path)?, bytes);
    assert_eq!(names(dir.path())?, ["job.toml"], "no temporary file is left beside it");
    Ok(())
}

#[test]
fn creating_over_an_existing_file_is_refused_and_leaves_its_bytes_untouched() -> TestResult {
    let dir = tempdir()?;
    let path = dir.path().join("job.toml");
    fs::write(&path, b"original")?;

    let result = create_file_atomically(&path, b"replacement");

    assert!(
        matches!(result, Err(FsError::PersistTemporary { ref source, .. }) if source.kind() == io::ErrorKind::AlreadyExists),
        "{result:?}"
    );
    assert_eq!(fs::read(&path)?, b"original");
    assert_eq!(names(dir.path())?, ["job.toml"], "no temporary file is left beside it");
    Ok(())
}

#[cfg(unix)]
#[test]
fn creating_where_a_dangling_link_stands_is_refused_and_nothing_is_written_through_it() -> TestResult {
    let dir = tempdir()?;
    let outside = tempdir()?;
    let destination = outside.path().join("elsewhere.toml");
    let path = dir.path().join("job.toml");
    std::os::unix::fs::symlink(&destination, &path)?;

    let result = create_file_atomically(&path, b"content");

    assert!(
        matches!(result, Err(FsError::PersistTemporary { ref source, .. }) if source.kind() == io::ErrorKind::AlreadyExists),
        "{result:?}"
    );
    assert!(!destination.exists(), "nothing was written through the link");
    assert!(
        path.symlink_metadata()?.file_type().is_symlink(),
        "the link is untouched"
    );
    assert_eq!(names(dir.path())?, ["job.toml"], "no temporary file is left beside it");
    Ok(())
}

#[test]
fn a_create_interrupted_before_the_rename_leaves_no_file_and_no_temporary_file() -> TestResult {
    let dir = tempdir()?;
    let path = dir.path().join("job.toml");

    let result = create_file_observed(&path, b"content", &InterruptBeforeRename);

    assert!(
        matches!(result, Err(FsError::AtomicWriteInterrupted { .. })),
        "{result:?}"
    );
    assert!(!path.exists());
    assert!(names(dir.path())?.is_empty(), "no temporary file is left behind");
    Ok(())
}

#[test]
fn creating_in_a_folder_that_does_not_exist_is_reported_and_the_folder_is_not_made() -> TestResult {
    let dir = tempdir()?;
    let folder = dir.path().join("missing");

    let result = create_file_atomically(&folder.join("job.toml"), b"content");

    assert!(
        matches!(result, Err(FsError::CreateTemporary { ref source, .. }) if source.kind() == io::ErrorKind::NotFound),
        "{result:?}"
    );
    assert!(!folder.exists());
    Ok(())
}

/// What the umask gives a file created in `directory` by an ordinary `fs::write`, which is what a hand-placed file
/// gets.
#[cfg(unix)]
fn default_mode_in(directory: &Path) -> Result<u32, io::Error> {
    use std::os::unix::fs::PermissionsExt;
    let probe = directory.join("hand-placed.toml");
    fs::write(&probe, b"x")?;
    Ok(fs::metadata(&probe)?.permissions().mode() & 0o777)
}

#[cfg(unix)]
#[test]
fn a_created_file_has_the_umask_default_permissions_not_the_temporary_files_private_mode() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir()?;
    let expected = default_mode_in(dir.path())?;
    let path = dir.path().join("job.toml");

    create_file_atomically(&path, b"name = \"Nightly\"\n")?;

    assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, expected);
    Ok(())
}

struct RecordSteps(std::cell::RefCell<Vec<FileRemoveStep>>);

impl FileRemoveObserver for RecordSteps {
    fn reached(&self, step: FileRemoveStep) {
        self.0.borrow_mut().push(step);
    }
}

#[test]
fn removing_a_file_flushes_the_folder_that_held_it_after_the_entry_is_gone() -> TestResult {
    let dir = tempdir()?;
    let path = dir.path().join("job.toml");
    fs::write(&path, b"x")?;
    let observer = RecordSteps(std::cell::RefCell::new(Vec::new()));

    let removed = remove_file_observed(&path, &observer)?;

    assert!(removed);
    assert!(!path.exists());
    assert_eq!(
        *observer.0.borrow(),
        [FileRemoveStep::Removed, FileRemoveStep::ParentSynced],
        "the folder is flushed once the entry is gone, and not before"
    );
    Ok(())
}

#[test]
fn removing_a_file_that_is_not_there_reports_nothing_removed_and_flushes_nothing() -> TestResult {
    let dir = tempdir()?;
    let observer = RecordSteps(std::cell::RefCell::new(Vec::new()));

    let removed = remove_file_observed(&dir.path().join("missing.toml"), &observer)?;

    assert!(!removed);
    assert!(observer.0.borrow().is_empty());
    Ok(())
}
