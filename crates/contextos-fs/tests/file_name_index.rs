use contextos_core::{VaultPath, VaultPathInput, VaultRoot, VaultRootInput, VaultSet};
use contextos_fs::{FileNameIndex, Filesystem, FilesystemConfig, FsError, FsLimits, default_hidden_patterns};
use tempfile::tempdir;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn index_over(
    vault: &std::path::Path,
    max_files: usize,
    hidden: Vec<String>,
) -> Result<FileNameIndex, Box<dyn std::error::Error>> {
    let root = VaultRoot::try_from(VaultRootInput {
        path: vault.to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?;
    let roots = VaultSet::try_from(vec![root])?;
    let filesystem = Filesystem::try_from(FilesystemConfig {
        roots: roots.clone(),
        limits: vec![FsLimits {
            max_read_bytes: 1024,
            max_batch_files: 50,
        }],
        hidden: vec![hidden],
        atomic_write_guard: None,
    })?;
    let root_path = VaultPath::try_from(VaultPathInput {
        roots: &roots,
        raw: ".",
    })?;
    Ok(FileNameIndex::new(filesystem, root_path, max_files))
}

fn write(vault: &std::path::Path, relative: &str) -> TestResult {
    let path = vault.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, "x")?;
    Ok(())
}

#[test]
fn files_of_every_type_are_found_by_their_trailing_segments_ignoring_case() -> TestResult {
    let vault = tempdir()?;
    for file in [
        "wiki/Foo.md",
        "other/deep/foo.md",
        "xfoo.md",
        "img/Pic.PNG",
        "Foo.md",
        "wiki/Foo2.md",
    ] {
        write(vault.path(), file)?;
    }
    let index = index_over(vault.path(), 100, default_hidden_patterns())?;

    assert_eq!(
        index.files_ending_with("FOO.md")?,
        vec!["Foo.md", "other/deep/foo.md", "wiki/Foo.md"]
    );
    assert_eq!(index.files_ending_with("wiki/foo.md")?, vec!["wiki/Foo.md"]);
    assert_eq!(index.files_ending_with("pic.png")?, vec!["img/Pic.PNG"]);
    assert!(
        index.files_ending_with("missing.md")?.is_empty(),
        "`index.files_ending_with(\"missing.md\")?` should be empty, got {:?}",
        index.files_ending_with("missing.md")?
    );
    assert!(
        index.files_ending_with("")?.is_empty(),
        "`index.files_ending_with(\"\")?` should be empty, got {:?}",
        index.files_ending_with("")?
    );
    Ok(())
}

#[test]
fn directories_hidden_paths_and_symlinks_are_not_indexed() -> TestResult {
    let vault = tempdir()?;
    std::fs::create_dir_all(vault.path().join("topic.md"))?;
    write(vault.path(), "secret/Hidden.md")?;
    write(vault.path(), ".git/Config.md")?;
    write(vault.path(), "real.md")?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(vault.path().join("real.md"), vault.path().join("alias.md"))?;
    let mut hidden = default_hidden_patterns();
    hidden.push("secret/**".to_owned());
    let index = index_over(vault.path(), 100, hidden)?;

    for name in ["topic.md", "hidden.md", "config.md", "alias.md"] {
        assert!(index.files_ending_with(name)?.is_empty(), "{name}");
    }
    assert_eq!(index.files_ending_with("real.md")?, vec!["real.md"]);
    Ok(())
}

#[test]
fn the_index_reflects_the_vault_as_of_its_last_walk_until_it_is_rescanned() -> TestResult {
    let vault = tempdir()?;
    write(vault.path(), "old/Gone.md")?;
    let index = index_over(vault.path(), 100, default_hidden_patterns())?;
    assert_eq!(index.files_ending_with("gone.md")?, vec!["old/Gone.md"]);

    std::fs::remove_file(vault.path().join("old/Gone.md"))?;
    write(vault.path(), "new/Fresh.md")?;

    assert_eq!(
        index.files_ending_with("gone.md")?,
        vec!["old/Gone.md"],
        "not yet rescanned"
    );
    assert!(index.files_ending_with("fresh.md")?.is_empty(), "not yet rescanned");
    assert_eq!(index.rescan()?, 1);
    assert!(
        index.files_ending_with("gone.md")?.is_empty(),
        "`index.files_ending_with(\"gone.md\")?` should be empty, got {:?}",
        index.files_ending_with("gone.md")?
    );
    assert_eq!(index.files_ending_with("fresh.md")?, vec!["new/Fresh.md"]);
    Ok(())
}

#[test]
fn a_vault_above_the_file_cap_fails_and_a_failed_rescan_keeps_the_previous_index() -> TestResult {
    let vault = tempdir()?;
    write(vault.path(), "a.md")?;
    let index = index_over(vault.path(), 2, default_hidden_patterns())?;
    assert_eq!(index.files_ending_with("a.md")?, vec!["a.md"]);

    write(vault.path(), "b.md")?;
    write(vault.path(), "c.md")?;
    let error = index.rescan().err().ok_or("a rescan above the cap must fail")?;

    assert!(matches!(error, FsError::TooManyFiles { maximum: 2, .. }), "{error:?}");
    assert_eq!(index.files_ending_with("a.md")?, vec!["a.md"]);
    assert!(
        index.files_ending_with("b.md")?.is_empty(),
        "`index.files_ending_with(\"b.md\")?` should be empty, got {:?}",
        index.files_ending_with("b.md")?
    );
    Ok(())
}

#[test]
fn lookups_racing_rescans_always_see_a_complete_snapshot() -> TestResult {
    let vault = tempdir()?;
    for name in ["a.md", "sub/a.md", "sub/b.md"] {
        write(vault.path(), name)?;
    }
    let index = index_over(vault.path(), 100, default_hidden_patterns())?;

    std::thread::scope(|scope| {
        for worker in 0..8 {
            let index = &index;
            scope.spawn(move || {
                for round in 0..25 {
                    if (worker + round) % 4 == 0 {
                        assert_eq!(index.rescan().ok(), Some(3));
                    } else {
                        assert_eq!(
                            index.files_ending_with("a.md").ok(),
                            Some(vec!["a.md".to_owned(), "sub/a.md".to_owned()])
                        );
                    }
                }
            });
        }
    });
    Ok(())
}
