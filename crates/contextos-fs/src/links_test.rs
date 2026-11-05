use std::fs;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn a_regular_file_and_a_folder_are_not_links() -> TestResult {
    let root = tempfile::tempdir()?;
    let file = root.path().join("a.toml");
    fs::write(&file, b"x")?;

    assert!(!is_link_or_reparse_point(&fs::symlink_metadata(&file)?));
    assert!(!is_link_or_reparse_point(&fs::symlink_metadata(root.path())?));
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_symbolic_link_to_a_file_or_a_folder_is_a_link_whether_or_not_it_dangles() -> TestResult {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir()?;
    let file = root.path().join("real.toml");
    fs::write(&file, b"x")?;
    let folder = root.path().join("folder");
    fs::create_dir(&folder)?;
    for (name, target) in [
        ("to-file", file.clone()),
        ("to-folder", folder.clone()),
        ("dangling", root.path().join("missing")),
    ] {
        let link = root.path().join(name);
        symlink(&target, &link)?;
        assert!(is_link_or_reparse_point(&fs::symlink_metadata(&link)?), "{name}");
    }
    Ok(())
}

/// A junction is the Windows folder link that needs no privilege to make, so it is the one an attacker can plant.
#[cfg(windows)]
#[test]
fn a_junction_is_a_link() -> TestResult {
    let root = tempfile::tempdir()?;
    let target = root.path().join("target");
    fs::create_dir(&target)?;
    let junction = root.path().join("junction");
    let made = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&junction)
        .arg(&target)
        .output()?;
    assert!(made.status.success(), "{made:?}");

    assert!(is_link_or_reparse_point(&fs::symlink_metadata(&junction)?));
    Ok(())
}

#[test]
fn a_flag_set_is_classified_as_plain_a_link_or_a_reparse_point_with_a_link_taking_precedence() {
    let class = |is_link, file_attributes| {
        EntryClass::from(EntryFlags {
            is_link,
            file_attributes,
        })
    };

    assert_eq!(class(false, 0), EntryClass::Plain);
    assert_eq!(
        class(false, 0x20),
        EntryClass::Plain,
        "an archive attribute alone is plain"
    );
    assert_eq!(class(true, 0), EntryClass::Link);
    assert_eq!(class(false, FILE_ATTRIBUTE_REPARSE_POINT), EntryClass::ReparsePoint);
    assert_eq!(
        class(false, FILE_ATTRIBUTE_REPARSE_POINT | 0x20),
        EntryClass::ReparsePoint
    );
    assert_eq!(
        class(true, FILE_ATTRIBUTE_REPARSE_POINT),
        EntryClass::Link,
        "a junction is reported as a link, and a link is named as one"
    );
}

#[test]
fn the_reparse_attribute_is_the_windows_documented_bit() {
    assert_eq!(FILE_ATTRIBUTE_REPARSE_POINT, 0x400);
}

#[test]
fn a_regular_file_and_a_folder_classify_as_plain() -> TestResult {
    let root = tempfile::tempdir()?;
    let file = root.path().join("a.toml");
    fs::write(&file, b"x")?;

    for path in [&file, root.path()] {
        let metadata = fs::symlink_metadata(path)?;
        assert_eq!(EntryClass::from(&metadata), EntryClass::Plain);
        assert!(!EntryFlags::from(&metadata).is_link);
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_symbolic_link_classifies_as_a_link_and_has_no_windows_attributes() -> TestResult {
    let root = tempfile::tempdir()?;
    let target = root.path().join("target");
    fs::create_dir(&target)?;
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&target, &link)?;
    let metadata = fs::symlink_metadata(&link)?;

    assert_eq!(
        EntryFlags::from(&metadata),
        EntryFlags {
            is_link: true,
            file_attributes: 0
        }
    );
    assert_eq!(EntryClass::from(&metadata), EntryClass::Link);
    Ok(())
}

/// A junction is the Windows folder link that needs no privilege to make, so it is the one an attacker can plant.
#[cfg(windows)]
#[test]
fn a_junction_classifies_as_a_link() -> TestResult {
    let root = tempfile::tempdir()?;
    let target = root.path().join("target");
    fs::create_dir(&target)?;
    let junction = root.path().join("junction");
    let made = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&junction)
        .arg(&target)
        .output()?;
    assert!(made.status.success(), "{made:?}");

    assert_eq!(EntryClass::from(&fs::symlink_metadata(&junction)?), EntryClass::Link);
    Ok(())
}
