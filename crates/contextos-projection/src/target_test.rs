use tempfile::tempdir;

use super::*;
use crate::TargetLimit;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn a_relative_root_is_rejected() {
    assert!(matches!(
        ProjectionTarget::try_from(Path::new("relative/agents")),
        Err(ProjectionError::TargetRootRejected {
            reason: TargetRootRejection::NotAbsolute
        })
    ));
}

#[test]
fn a_missing_root_and_a_file_root_are_rejected() -> TestResult {
    let dir = tempdir()?;
    assert!(matches!(
        ProjectionTarget::try_from(dir.path().join("missing").as_path()),
        Err(ProjectionError::TargetRootUnresolvable { .. })
    ));
    let file = dir.path().join("file");
    std::fs::write(&file, "x")?;
    assert!(matches!(
        ProjectionTarget::try_from(file.as_path()),
        Err(ProjectionError::TargetRootRejected {
            reason: TargetRootRejection::NotFolder
        })
    ));
    Ok(())
}

#[test]
fn the_root_is_resolved_once_at_construction() -> TestResult {
    let dir = tempdir()?;
    let resolved = dunce::canonicalize(dir.path())?;
    let target = ProjectionTarget::try_from(dir.path().join(".").as_path())?;
    assert_eq!(<&Path>::from(&target), resolved.as_path());
    Ok(())
}

fn slug(text: &str) -> Result<Slug, ProjectionError> {
    Slug::try_from(text)
}

fn sparse_file(path: &Path, length: u64) -> TestResult {
    let file = std::fs::File::create(path)?;
    file.set_len(length)?;
    Ok(())
}

#[test]
fn a_projected_agent_file_far_beyond_the_size_limit_is_refused_not_read() -> TestResult {
    let dir = tempdir()?;
    let target = ProjectionTarget::try_from(dir.path())?;
    sparse_file(&dir.path().join("big.md"), 33 << 20)?;
    let extension = FileExtension::try_from("md")?;

    let result = target.agent_file_hash(&slug("big")?, &extension);

    assert!(
        matches!(
            result,
            Err(ProjectionError::TargetEntryOverLimit {
                limit: TargetLimit::Bytes,
                ..
            })
        ),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn a_skill_folder_in_a_target_with_too_many_files_is_refused() -> TestResult {
    let dir = tempdir()?;
    let target = ProjectionTarget::try_from(dir.path())?;
    let skill = dir.path().join("crowded");
    std::fs::create_dir(&skill)?;
    for index in 0..513 {
        std::fs::write(skill.join(format!("file-{index}.md")), "x")?;
    }

    let result = target.read_skill_folder(&slug("crowded")?);

    assert!(
        matches!(
            result,
            Err(ProjectionError::TargetEntryOverLimit {
                limit: TargetLimit::Files,
                ..
            })
        ),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn a_skill_folder_in_a_target_nested_too_deeply_is_refused() -> TestResult {
    let dir = tempdir()?;
    let target = ProjectionTarget::try_from(dir.path())?;
    let mut deep = dir.path().join("nested");
    for level in 0..9 {
        deep.push(format!("level-{level}"));
    }
    std::fs::create_dir_all(&deep)?;
    std::fs::write(deep.join("SKILL.md"), "x")?;

    let result = target.read_skill_folder(&slug("nested")?);

    assert!(
        matches!(
            result,
            Err(ProjectionError::TargetEntryOverLimit {
                limit: TargetLimit::Depth,
                ..
            })
        ),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn a_skill_folder_in_a_target_holding_too_many_bytes_is_refused() -> TestResult {
    let dir = tempdir()?;
    let target = ProjectionTarget::try_from(dir.path())?;
    let skill = dir.path().join("heavy");
    std::fs::create_dir(&skill)?;
    sparse_file(&skill.join("SKILL.md"), 33 << 20)?;

    let result = target.read_skill_folder(&slug("heavy")?);

    assert!(
        matches!(
            result,
            Err(ProjectionError::TargetEntryOverLimit {
                limit: TargetLimit::Bytes,
                ..
            })
        ),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn a_skill_folder_at_the_limits_is_still_read() -> TestResult {
    let dir = tempdir()?;
    let target = ProjectionTarget::try_from(dir.path())?;
    let mut deep = dir.path().join("edge");
    for level in 0..8 {
        deep.push(format!("level-{level}"));
    }
    std::fs::create_dir_all(&deep)?;
    std::fs::write(deep.join("SKILL.md"), "x")?;

    assert!(target.read_skill_folder(&slug("edge")?)?.is_some());
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_target_root_re_pointed_at_another_folder_after_start_is_refused_at_the_next_write() -> TestResult {
    let dir = tempdir()?;
    let root = dir.path().join("agents");
    std::fs::create_dir(&root)?;
    let target = ProjectionTarget::try_from(root.as_path())?;
    let elsewhere = dir.path().join("vault-folder");
    std::fs::create_dir(&elsewhere)?;
    std::fs::remove_dir(&root)?;
    std::os::unix::fs::symlink(&elsewhere, &root)?;

    let result = target.write_agent_file(&slug("reviewer")?, &FileExtension::try_from("md")?, b"projected");

    let error = result.err().ok_or("the write was not refused")?;
    assert_eq!(error.code(), "projection/path-rejected", "{error:?}");
    assert!(
        !elsewhere.join("reviewer.md").exists(),
        "the write went through the new link"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn an_operator_made_link_to_the_root_is_followed_once_but_refused_when_it_is_re_linked_later() -> TestResult {
    let dir = tempdir()?;
    let original = dir.path().join("dotfiles-agents");
    let replacement = dir.path().join("vault-folder");
    std::fs::create_dir(&original)?;
    std::fs::create_dir(&replacement)?;
    let link = dir.path().join("agents");
    std::os::unix::fs::symlink(&original, &link)?;
    let target = ProjectionTarget::try_from(link.as_path())?;
    target.write_agent_file(&slug("first")?, &FileExtension::try_from("md")?, b"one")?;
    assert!(original.join("first.md").is_file());

    std::fs::remove_file(&link)?;
    std::os::unix::fs::symlink(&replacement, &link)?;
    let result = target.write_agent_file(&slug("second")?, &FileExtension::try_from("md")?, b"two");

    let error = result.err().ok_or("the write was not refused")?;
    assert_eq!(error.code(), "projection/path-rejected", "{error:?}");
    assert!(!replacement.join("second.md").exists());
    assert!(!original.join("second.md").exists());
    Ok(())
}

fn class(is_link: bool, file_attributes: u32) -> EntryClass {
    EntryClass::from(EntryFlags {
        is_link,
        file_attributes,
    })
}

#[test]
fn a_link_is_classified_as_a_link_whatever_its_attributes() {
    assert_eq!(class(true, 0), EntryClass::Link);
    assert_eq!(class(true, FILE_ATTRIBUTE_REPARSE_POINT), EntryClass::Link);
    assert_eq!(class(true, 0x20 | FILE_ATTRIBUTE_REPARSE_POINT), EntryClass::Link);
}

#[test]
fn a_reparse_point_that_is_not_a_link_is_classified_as_a_reparse_point() {
    assert_eq!(class(false, FILE_ATTRIBUTE_REPARSE_POINT), EntryClass::ReparsePoint);
    assert_eq!(
        class(false, 0x20 | 0x1000 | FILE_ATTRIBUTE_REPARSE_POINT),
        EntryClass::ReparsePoint
    );
}

#[test]
fn an_entry_that_is_neither_a_link_nor_a_reparse_point_is_plain() {
    assert_eq!(class(false, 0), EntryClass::Plain);
    assert_eq!(class(false, 0x20), EntryClass::Plain);
    assert_eq!(class(false, 0x10 | 0x1000_0000), EntryClass::Plain);
}

#[test]
fn the_reparse_point_attribute_is_the_windows_value() {
    assert_eq!(FILE_ATTRIBUTE_REPARSE_POINT, 0x400);
}

#[cfg(unix)]
fn unreadable_sparse_file(path: &Path, length: u64) -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    sparse_file(path, length)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000))?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_target_skill_whose_listed_sizes_exceed_the_total_is_refused_before_any_file_is_read() -> TestResult {
    let dir = tempdir()?;
    let target = ProjectionTarget::try_from(dir.path())?;
    let skill = dir.path().join("heavy");
    std::fs::create_dir(&skill)?;
    // Neither file can be opened, so a read would fail with a read error instead of the limit error.
    unreadable_sparse_file(&skill.join("a.bin"), 20 << 20)?;
    unreadable_sparse_file(&skill.join("b.bin"), 20 << 20)?;

    let result = target.read_skill_folder(&slug("heavy")?);

    assert!(
        matches!(
            result,
            Err(ProjectionError::TargetEntryOverLimit {
                limit: TargetLimit::Bytes,
                ..
            })
        ),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn a_managed_index_inside_a_target_skill_is_neither_read_nor_counted() -> TestResult {
    let dir = tempdir()?;
    let target = ProjectionTarget::try_from(dir.path())?;
    let skill = dir.path().join("indexed");
    std::fs::create_dir_all(skill.join("scripts"))?;
    std::fs::write(skill.join("SKILL.md"), "x")?;
    std::fs::write(skill.join("scripts/run.sh"), "y")?;
    let without = target.read_skill_folder(&slug("indexed")?)?.ok_or("no skill")?;
    sparse_file(&skill.join("index.md"), 33 << 20)?;
    sparse_file(&skill.join("scripts/index.md"), 33 << 20)?;

    let with = target
        .read_skill_folder(&slug("indexed")?)?
        .ok_or("the skill with indexes was not read")?;

    let names: Vec<&str> = with.files().map(|(path, _)| <&str>::from(path)).collect();
    assert_eq!(names, ["SKILL.md", "scripts/run.sh"]);
    assert_eq!(ContentHash::from(&with), ContentHash::from(&without));
    Ok(())
}
