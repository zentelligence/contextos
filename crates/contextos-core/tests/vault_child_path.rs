//! Building the path of one entry directly inside a directory already validated as part of a vault: the same path,
//! and the same refusals, as resolving that entry from the vault root, at the cost of inspecting only the entry.

use std::path::PathBuf;

use contextos_core::{PathError, VaultChildInput, VaultPath, VaultPathInput, VaultRoot, VaultSet};
use proptest::prelude::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A temporary directory whose name is a valid vault name, unlike tempfile's default `.tmp` prefix.
fn tempdir() -> std::io::Result<tempfile::TempDir> {
    tempfile::Builder::new().prefix("vault").tempdir()
}

fn sole_root(path: PathBuf) -> Result<VaultSet, PathError> {
    VaultSet::try_from(vec![VaultRoot::try_from(path)?])
}

fn full(roots: &VaultSet, raw: &str) -> Result<VaultPath, PathError> {
    VaultPath::try_from(VaultPathInput { roots, raw })
}

fn child(roots: &VaultSet, parent: &VaultPath, name: &str) -> Result<VaultPath, PathError> {
    VaultPath::try_from(VaultChildInput { roots, parent, name })
}

#[test]
fn a_file_in_a_validated_directory_has_the_path_full_resolution_gives() -> TestResult {
    let vault = tempdir()?;
    std::fs::create_dir_all(vault.path().join("notes/deep"))?;
    std::fs::write(vault.path().join("notes/deep/a.md"), "a")?;
    let roots = sole_root(vault.path().to_path_buf())?;
    let parent = full(&roots, "notes/deep")?;

    let built = child(&roots, &parent, "a.md")?;

    assert_eq!(built, full(&roots, "notes/deep/a.md")?);
    assert_eq!(built.portable_relative(), "notes/deep/a.md");
    Ok(())
}

#[test]
fn an_entry_not_yet_created_is_named_inside_its_directory() -> TestResult {
    let vault = tempdir()?;
    std::fs::create_dir(vault.path().join("notes"))?;
    let roots = sole_root(vault.path().to_path_buf())?;
    let parent = full(&roots, "notes")?;

    assert_eq!(child(&roots, &parent, "new.md")?, full(&roots, "notes/new.md")?);
    Ok(())
}

#[test]
fn a_child_of_the_vault_root_itself_resolves() -> TestResult {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("top.md"), "t")?;
    let roots = sole_root(vault.path().to_path_buf())?;
    let root = full(&roots, &vault.path().display().to_string())?;

    assert_eq!(child(&roots, &root, "top.md")?, full(&roots, "top.md")?);
    Ok(())
}

#[test]
fn a_name_that_is_not_one_plain_component_is_refused() -> TestResult {
    let vault = tempdir()?;
    std::fs::create_dir(vault.path().join("notes"))?;
    let roots = sole_root(vault.path().to_path_buf())?;
    let parent = full(&roots, "notes")?;

    for name in ["", "nul\0byte"] {
        assert!(
            matches!(child(&roots, &parent, name), Err(PathError::Invalid { .. })),
            "{name:?} must be refused as empty or holding a null byte"
        );
    }
    for name in [".", "a/b", "/abs"] {
        let refused = child(&roots, &parent, name);
        assert!(
            matches!(refused, Err(PathError::NotOneComponent { .. })),
            "{name:?} must be refused as not one plain component: {refused:?}"
        );
    }
    assert!(matches!(child(&roots, &parent, ".."), Err(PathError::Traversal { .. })));
    assert!(matches!(
        child(&roots, &parent, "note.md:stream"),
        Err(PathError::AlternateDataStream { .. })
    ));
    Ok(())
}

#[test]
fn a_path_through_a_file_is_refused_the_same_way_on_every_platform() -> TestResult {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("note.md"), "a")?;
    let roots = sole_root(vault.path().to_path_buf())?;

    let error = full(&roots, "note.md/child.md")
        .err()
        .ok_or("a file cannot hold a child")?;

    assert_eq!(error.code(), "path/invalid", "{error}");
    Ok(())
}

#[test]
fn an_entry_under_a_file_is_refused_the_same_way_on_every_platform() -> TestResult {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("note.md"), "a")?;
    let roots = sole_root(vault.path().to_path_buf())?;
    let file = full(&roots, "note.md")?;

    let error = child(&roots, &file, "child.md")
        .err()
        .ok_or("a file cannot hold a child")?;

    assert_eq!(error.code(), "path/invalid", "{error}");
    Ok(())
}

#[test]
fn a_path_from_the_root_of_the_drive_is_outside_the_vault() -> TestResult {
    let vault = tempdir()?;
    let roots = sole_root(vault.path().to_path_buf())?;

    let error = full(&roots, "/abs").err().ok_or("a rooted path is not in the vault")?;

    assert_eq!(error.code(), "path/outside-root", "{error}");
    Ok(())
}

#[cfg(windows)]
#[test]
fn a_backslash_separates_components_on_windows_and_is_refused() -> TestResult {
    let vault = tempdir()?;
    std::fs::create_dir(vault.path().join("notes"))?;
    let roots = sole_root(vault.path().to_path_buf())?;
    let parent = full(&roots, "notes")?;

    assert!(matches!(
        child(&roots, &parent, r"a\b"),
        Err(PathError::NotOneComponent { .. })
    ));
    Ok(())
}

#[cfg(windows)]
#[test]
fn a_windows_device_name_gets_the_same_answer_as_full_resolution() -> TestResult {
    let vault = tempdir()?;
    std::fs::create_dir(vault.path().join("notes"))?;
    let roots = sole_root(vault.path().to_path_buf())?;
    let parent = full(&roots, "notes")?;

    for name in [
        "CON",
        "nul.md",
        "COM1.txt",
        "lpt9",
        "COM\u{b9}",
        "LPT\u{b3}.md",
        "CONIN$",
        "CON .txt",
        "a.",
        "a ",
        "LONGFI~1.MD",
    ] {
        let built = child(&roots, &parent, name).map_err(|error| error.code());
        let resolved = full(&roots, &format!("notes/{name}")).map_err(|error| error.code());
        assert_eq!(built, resolved, "{name}");
    }
    Ok(())
}

#[test]
fn a_parent_from_a_root_that_is_not_configured_is_refused() -> TestResult {
    let first = tempdir()?;
    let second = tempdir()?;
    let both = VaultSet::try_from(vec![
        VaultRoot::try_from(first.path().to_path_buf())?,
        VaultRoot::try_from(second.path().to_path_buf())?,
    ])?;
    let parent = full(&both, &second.path().display().to_string())?;
    let only_first = sole_root(first.path().to_path_buf())?;

    assert!(matches!(
        child(&only_first, &parent, "a.md"),
        Err(PathError::OutsideRoot { .. })
    ));
    Ok(())
}

#[test]
fn a_parent_validated_under_a_different_root_at_the_same_position_is_refused() -> TestResult {
    let outer = tempdir()?;
    std::fs::create_dir_all(outer.path().join("sub/notes"))?;
    let inner_set = sole_root(outer.path().join("sub"))?;
    let parent = full(&inner_set, "notes")?;
    let outer_set = sole_root(outer.path().to_path_buf())?;

    // The same root position names a different directory in each set: the parent's relative path would be wrong.
    assert!(matches!(
        child(&outer_set, &parent, "a.md"),
        Err(PathError::OutsideRoot { .. })
    ));
    Ok(())
}

#[test]
fn names_that_only_look_like_whole_path_prefixes_agree_with_full_resolution() -> TestResult {
    let vault = tempdir()?;
    std::fs::create_dir(vault.path().join("notes"))?;
    let roots = sole_root(vault.path().to_path_buf())?;
    let parent = full(&roots, "notes")?;

    let names: &[&str] = if cfg!(windows) {
        &["c", "x.md", "~notes.md"]
    } else {
        &[
            r"\\x.md",
            r"\\?\x",
            r"\\.\pipe",
            r"a\b.md",
            r"..\x",
            "c:x",
            "~notes.md",
            "$money",
        ]
    };
    for name in names {
        let built = child(&roots, &parent, name).map_err(|error| error.code());
        let resolved = full(&roots, &format!("notes/{name}")).map_err(|error| error.code());
        assert_eq!(built, resolved, "{name:?}");
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn an_entry_that_is_a_symlink_into_another_configured_vault_resolves_into_that_vault() -> TestResult {
    use std::os::unix::fs::symlink;
    let first = tempdir()?;
    let second = tempdir()?;
    std::fs::create_dir(first.path().join("notes"))?;
    std::fs::write(second.path().join("shared.md"), "s")?;
    symlink(second.path().join("shared.md"), first.path().join("notes/shared.md"))?;
    let roots = VaultSet::try_from(vec![
        VaultRoot::try_from(first.path().to_path_buf())?,
        VaultRoot::try_from(second.path().to_path_buf())?,
    ])?;
    let parent = full(&roots, &first.path().join("notes").display().to_string())?;

    let built = child(&roots, &parent, "shared.md")?;

    assert_eq!(
        built,
        full(&roots, &first.path().join("notes/shared.md").display().to_string())?
    );
    assert_eq!(built.portable_relative(), "shared.md");
    Ok(())
}

#[cfg(unix)]
#[test]
fn an_entry_that_is_a_symlink_out_of_the_vault_is_refused() -> TestResult {
    use std::os::unix::fs::symlink;
    let vault = tempdir()?;
    let outside = tempdir()?;
    std::fs::write(outside.path().join("secret.md"), "private")?;
    std::fs::create_dir(vault.path().join("notes"))?;
    symlink(outside.path().join("secret.md"), vault.path().join("notes/escape.md"))?;
    symlink(outside.path(), vault.path().join("notes/linked"))?;
    let roots = sole_root(vault.path().to_path_buf())?;
    let parent = full(&roots, "notes")?;

    assert!(matches!(
        child(&roots, &parent, "escape.md"),
        Err(PathError::SymlinkEscape { .. })
    ));
    assert!(matches!(
        child(&roots, &parent, "linked"),
        Err(PathError::SymlinkEscape { .. })
    ));
    Ok(())
}

#[cfg(unix)]
#[test]
fn an_entry_that_is_a_symlink_within_the_vault_resolves_to_its_target_as_full_resolution_does() -> TestResult {
    use std::os::unix::fs::symlink;
    let vault = tempdir()?;
    std::fs::create_dir_all(vault.path().join("notes"))?;
    std::fs::create_dir_all(vault.path().join("archive"))?;
    std::fs::write(vault.path().join("archive/real.md"), "r")?;
    symlink(
        vault.path().join("archive/real.md"),
        vault.path().join("notes/alias.md"),
    )?;
    let roots = sole_root(vault.path().to_path_buf())?;
    let parent = full(&roots, "notes")?;

    let built = child(&roots, &parent, "alias.md")?;

    assert_eq!(built, full(&roots, "notes/alias.md")?);
    assert_eq!(built.portable_relative(), "archive/real.md");
    Ok(())
}

/// The kinds of entry a generated name can be given in the fixture directory.
#[derive(Clone, Copy, Debug)]
enum Entry {
    Missing,
    File,
    Directory,
    #[cfg(unix)]
    LinkInside,
    #[cfg(unix)]
    LinkOutside,
}

fn entry() -> impl Strategy<Value = Entry> {
    #[cfg(unix)]
    {
        prop_oneof![
            Just(Entry::Missing),
            Just(Entry::File),
            Just(Entry::Directory),
            Just(Entry::LinkInside),
            Just(Entry::LinkOutside),
        ]
    }
    #[cfg(not(unix))]
    {
        prop_oneof![Just(Entry::Missing), Just(Entry::File), Just(Entry::Directory)]
    }
}

proptest! {
    // A fixed seed keeps the gate deterministic; widen `cases` locally to search further.
    #![proptest_config(ProptestConfig {
        cases: 256,
        failure_persistence: None,
        rng_seed: proptest::test_runner::RngSeed::Fixed(0x5eed_c0de),
        ..ProptestConfig::default()
    })]

    /// Whatever the name and whatever the entry is, building it from its validated directory gives exactly what
    /// resolving it from the vault root gives: the same path, or a refusal with the same code.
    #[test]
    fn building_an_entry_from_its_directory_agrees_with_resolving_it_from_the_root(
        name in "[A-Za-z0-9 ._:~$\\\\\u{e9}\u{b9}-]{1,12}",
        kind in entry(),
    ) {
        let vault = tempdir().map_err(|error| TestCaseError::fail(error.to_string()))?;
        #[cfg(unix)]
        let outside = tempdir().map_err(|error| TestCaseError::fail(error.to_string()))?;
        let notes = vault.path().join("notes");
        std::fs::create_dir(&notes).map_err(|error| TestCaseError::fail(error.to_string()))?;
        let lexically_plain = !name.contains(':') && name != "." && name != ".." && !name.contains('/');
        if lexically_plain {
            let target = notes.join(&name);
            let made = match kind {
                Entry::Missing => Ok(()),
                Entry::File => std::fs::write(&target, "x"),
                Entry::Directory => std::fs::create_dir(&target),
                #[cfg(unix)]
                Entry::LinkInside => std::os::unix::fs::symlink(vault.path(), &target),
                #[cfg(unix)]
                Entry::LinkOutside => std::os::unix::fs::symlink(outside.path(), &target),
            };
            // A name the platform cannot create is not a case of this property.
            prop_assume!(made.is_ok());
        }
        let roots = sole_root(vault.path().to_path_buf()).map_err(|error| TestCaseError::fail(error.to_string()))?;
        let parent = full(&roots, "notes").map_err(|error| TestCaseError::fail(error.to_string()))?;

        let built = child(&roots, &parent, &name).map_err(|error| error.code());
        let resolved = full(&roots, &format!("notes/{name}")).map_err(|error| error.code());
        // Full resolution reads a name with a separator as a path, so a refusal by the narrower constructor is the
        // only acceptable disagreement, and only for such a name.
        if name.contains('\\') && cfg!(windows) || name == "." {
            prop_assert!(built.is_err());
        } else {
            prop_assert_eq!(built, resolved);
        }
    }
}
