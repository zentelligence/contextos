use super::*;
use crate::{VaultRoot, VaultRootInput};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn resolved(path: &Path) -> Result<ResolvedPath, std::io::Error> {
    ResolvedPath::try_from(path)
}

#[test]
fn two_spellings_of_one_existing_folder_resolve_equal() -> TestResult {
    let directory = tempfile::tempdir()?;
    std::fs::create_dir(directory.path().join("inner"))?;
    let plain = directory.path().to_path_buf();
    let roundabout = directory.path().join("inner").join("..");

    assert_eq!(resolved(&plain)?, resolved(&roundabout)?);
    Ok(())
}

#[test]
fn a_folder_that_does_not_exist_yet_resolves_beside_the_part_that_does() -> TestResult {
    let directory = tempfile::tempdir()?;
    std::fs::create_dir(directory.path().join("inner"))?;
    let roundabout = directory.path().join("inner").join("..").join("later").join("agents");
    let plain = directory.path().join("later").join("agents");

    assert_eq!(resolved(&plain)?, resolved(&roundabout)?);
    Ok(())
}

#[test]
fn a_target_and_the_vault_root_naming_the_same_folder_resolve_equal() -> TestResult {
    let directory = tempfile::tempdir()?;
    std::fs::create_dir(directory.path().join("inner"))?;
    let root = VaultRoot::try_from(VaultRootInput {
        path: directory.path().to_path_buf(),
        managed: false,
        name: Some("alpha".to_owned()),
    })?;
    let target = directory.path().join("inner").join("..");

    assert_eq!(resolved(root.path())?, resolved(&target)?);
    Ok(())
}

#[test]
fn a_folder_contains_itself_and_what_lies_below_it_but_not_a_sibling_with_a_shared_prefix() -> TestResult {
    let directory = tempfile::tempdir()?;
    let parent = directory.path().join("agents");
    let child = parent.join("nested").join("deeper");
    let sibling = directory.path().join("agents-extra");
    std::fs::create_dir_all(&child)?;
    std::fs::create_dir_all(&sibling)?;

    assert!(resolved(&parent)?.contains(&resolved(&parent)?));
    assert!(resolved(&parent)?.contains(&resolved(&child)?));
    assert!(!resolved(&parent)?.contains(&resolved(&sibling)?));
    assert!(!resolved(&child)?.contains(&resolved(&parent)?));
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_link_resolves_to_the_folder_it_names() -> TestResult {
    let directory = tempfile::tempdir()?;
    let real = directory.path().join("real");
    std::fs::create_dir(&real)?;
    let link = directory.path().join("link");
    std::os::unix::fs::symlink(&real, &link)?;

    assert_eq!(resolved(&link)?, resolved(&real)?);
    assert!(resolved(&real)?.contains(&resolved(&link.join("below"))?));
    Ok(())
}

#[cfg(windows)]
#[test]
fn a_verbatim_spelling_resolves_equal_to_the_plain_spelling() -> TestResult {
    let directory = tempfile::tempdir()?;
    let plain = dunce::canonicalize(directory.path())?;
    let verbatim = PathBuf::from(format!(r"\\?\{}", plain.display()));

    assert_eq!(resolved(&plain)?, resolved(&verbatim)?);
    assert!(resolved(&plain)?.contains(&resolved(&verbatim.join("agents"))?));
    Ok(())
}
