use std::path::PathBuf;

use contextos_core::{VaultPath, VaultPathInput, VaultRoot, VaultRootInput, VaultSet};
use contextos_fs::{
    DirectoryTreeRequest, EntryKind, FileInfoRequest, Filesystem, FilesystemConfig, FsError, FsLimits, HiddenPaths,
    HiddenPathsInput, ListDirectoryRequest, ListDirectoryWithSizesRequest, ListFilesRequest, ReadTextRequest,
    SearchFilesRequest, SortBy, default_hidden_patterns,
};
use tempfile::tempdir;

fn filesystem(root: PathBuf, managed: bool) -> Result<(Filesystem, VaultSet), Box<dyn std::error::Error>> {
    filesystem_with_hidden(root, managed, default_hidden_patterns())
}

fn filesystem_with_hidden(
    root: PathBuf,
    managed: bool,
    hidden: Vec<String>,
) -> Result<(Filesystem, VaultSet), Box<dyn std::error::Error>> {
    let vault_root = VaultRoot::try_from(VaultRootInput {
        path: root,
        managed,
        name: Some("vault".to_owned()),
    })?;
    let roots = VaultSet::try_from(vec![vault_root])?;
    let filesystem = Filesystem::try_from(FilesystemConfig {
        roots: roots.clone(),
        limits: vec![FsLimits {
            max_read_bytes: 1024,
            max_batch_files: 50,
        }],
        hidden: vec![hidden],
        atomic_write_guard: None,
    })?;
    Ok((filesystem, roots))
}

fn path(roots: &VaultSet, raw: &str) -> Result<VaultPath, Box<dyn std::error::Error>> {
    Ok(VaultPath::try_from(VaultPathInput { roots, raw })?)
}

#[test]
fn lists_entries_with_kind_markers() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::create_dir(vault.path().join("notes"))?;
    std::fs::write(vault.path().join("readme.md"), "read me")?;
    let (filesystem, roots) = filesystem(vault.path().to_path_buf(), true)?;

    let result = filesystem.list_directory(&ListDirectoryRequest {
        path: path(&roots, ".")?,
    })?;

    assert_eq!(result.entries.len(), 2);
    assert_eq!(result.entries[0].name, "notes");
    assert_eq!(result.entries[0].kind, EntryKind::Directory);
    assert_eq!(result.entries[1].name, "readme.md");
    assert_eq!(result.rendered, "[DIR] notes\n[FILE] readme.md");
    Ok(())
}

#[test]
fn lists_sizes_and_sorts_by_size() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("large.md"), "123456789")?;
    std::fs::write(vault.path().join("small.md"), "1")?;
    let (filesystem, roots) = filesystem(vault.path().to_path_buf(), true)?;

    let result = filesystem.list_directory_with_sizes(&ListDirectoryWithSizesRequest {
        path: path(&roots, ".")?,
        sort_by: SortBy::Size,
    })?;

    assert_eq!(result.entries[0].name, "small.md");
    assert_eq!(result.entries[0].size, Some(1));
    assert_eq!(result.entries[1].name, "large.md");
    assert_eq!(result.entries[1].size, Some(9));
    assert!(result.rendered.contains("[FILE] small.md (1 B)"));
    Ok(())
}

#[test]
fn builds_bounded_tree_and_honours_excludes() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::create_dir_all(vault.path().join("notes/deep"))?;
    std::fs::create_dir_all(vault.path().join("private/nested"))?;
    std::fs::create_dir(vault.path().join(".contextos"))?;
    std::fs::write(vault.path().join("notes/deep/hidden-by-depth.md"), "deep")?;
    std::fs::write(vault.path().join("notes/visible.md"), "visible")?;
    std::fs::write(vault.path().join("private/nested/secret.md"), "secret")?;
    let (filesystem, roots) = filesystem(vault.path().to_path_buf(), true)?;

    let tree = filesystem.directory_tree(&DirectoryTreeRequest {
        path: path(&roots, ".")?,
        exclude_patterns: vec!["private/**".to_owned()],
        max_depth: 2,
    })?;

    let serialised = serde_json::to_value(&tree)?;
    assert_eq!(serialised["type"], "dir");
    assert!(serialised.to_string().contains("visible.md"));
    assert!(!serialised.to_string().contains("hidden-by-depth.md"));
    assert!(!serialised.to_string().contains("private"));
    assert!(!serialised.to_string().contains(".contextos"));
    Ok(())
}

#[test]
fn searches_case_insensitive_globs_with_excludes_and_limit() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::create_dir_all(vault.path().join("notes/private"))?;
    std::fs::write(vault.path().join("notes/Alpha.MD"), "alpha")?;
    std::fs::write(vault.path().join("notes/beta.md"), "beta")?;
    std::fs::write(vault.path().join("notes/private/secret.md"), "secret")?;
    let (filesystem, roots) = filesystem(vault.path().to_path_buf(), true)?;

    let results = filesystem.search_files(&SearchFilesRequest {
        path: path(&roots, ".")?,
        pattern: "**/*.md".to_owned(),
        exclude_patterns: vec!["**/private/**".to_owned()],
        max_results: 1,
    })?;

    assert_eq!(results, vec!["notes/Alpha.MD"]);
    Ok(())
}

#[test]
fn reports_metadata_permissions_and_bounded_hash() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let note = vault.path().join("note.md");
    std::fs::write(&note, "hello")?;
    let mut permissions = note.metadata()?.permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&note, permissions)?;
    let (filesystem, roots) = filesystem(vault.path().to_path_buf(), true)?;

    let info = filesystem.file_info(&FileInfoRequest {
        path: path(&roots, "note.md")?,
    })?;

    assert_eq!(info.path, "note.md");
    assert_eq!(info.kind, EntryKind::File);
    assert_eq!(info.size, 5);
    assert!(info.readonly);
    assert_eq!(
        info.content_hash.as_ref().map(<&str>::from),
        Some("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824")
    );
    assert!(info.modified.is_some());
    Ok(())
}

#[test]
fn lists_resolved_roots_and_managed_flags() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let (filesystem, _) = filesystem(vault.path().to_path_buf(), false)?;

    let roots = filesystem.list_allowed_directories();

    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0].path, dunce::canonicalize(vault.path())?.to_string_lossy());
    assert!(!roots[0].managed);
    Ok(())
}

#[test]
fn list_directory_omits_a_path_matching_hidden() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::create_dir(vault.path().join("secret"))?;
    std::fs::write(vault.path().join("visible.md"), "visible")?;
    let (filesystem, roots) = filesystem_with_hidden(vault.path().to_path_buf(), true, vec!["secret/**".to_owned()])?;

    let result = filesystem.list_directory(&ListDirectoryRequest {
        path: path(&roots, ".")?,
    })?;

    assert_eq!(result.entries.len(), 1);
    assert_eq!(result.entries[0].name, "visible.md");
    Ok(())
}

#[test]
fn list_directory_with_sizes_omits_a_path_matching_hidden() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("secret.md"), "secret")?;
    std::fs::write(vault.path().join("visible.md"), "visible")?;
    let (filesystem, roots) = filesystem_with_hidden(vault.path().to_path_buf(), true, vec!["secret.md".to_owned()])?;

    let result = filesystem.list_directory_with_sizes(&ListDirectoryWithSizesRequest {
        path: path(&roots, ".")?,
        sort_by: SortBy::Name,
    })?;

    assert_eq!(result.entries.len(), 1);
    assert_eq!(result.entries[0].name, "visible.md");
    Ok(())
}

#[test]
fn directory_tree_omits_a_subtree_matching_hidden() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::create_dir_all(vault.path().join("secret/nested"))?;
    std::fs::write(vault.path().join("secret/nested/private.md"), "private")?;
    std::fs::write(vault.path().join("visible.md"), "visible")?;
    let (filesystem, roots) = filesystem_with_hidden(vault.path().to_path_buf(), true, vec!["secret/**".to_owned()])?;

    let tree = filesystem.directory_tree(&DirectoryTreeRequest {
        path: path(&roots, ".")?,
        exclude_patterns: vec![],
        max_depth: 3,
    })?;

    let serialised = serde_json::to_value(&tree)?.to_string();
    assert!(serialised.contains("visible.md"));
    assert!(!serialised.contains("secret"));
    assert!(!serialised.contains("private.md"));
    Ok(())
}

#[test]
fn search_files_omits_a_path_matching_hidden() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::create_dir(vault.path().join("secret"))?;
    std::fs::write(vault.path().join("secret/note.md"), "secret")?;
    std::fs::write(vault.path().join("visible.md"), "visible")?;
    let (filesystem, roots) = filesystem_with_hidden(vault.path().to_path_buf(), true, vec!["secret/**".to_owned()])?;

    let results = filesystem.search_files(&SearchFilesRequest {
        path: path(&roots, ".")?,
        pattern: "**/*.md".to_owned(),
        exclude_patterns: vec![],
        max_results: usize::MAX,
    })?;

    assert_eq!(results, vec!["visible.md"]);
    Ok(())
}

#[test]
fn direct_read_of_a_hidden_path_is_unaffected() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::create_dir(vault.path().join("secret"))?;
    std::fs::write(vault.path().join("secret/note.md"), "still readable")?;
    let (filesystem, roots) = filesystem_with_hidden(vault.path().to_path_buf(), true, vec!["secret/**".to_owned()])?;

    let result = filesystem.read_text(&ReadTextRequest {
        path: path(&roots, "secret/note.md")?,
        limit: None,
    })?;

    assert_eq!(result.content, "still readable");
    Ok(())
}

#[test]
fn hidden_count_mismatch_is_rejected_at_construction() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let vault_root = VaultRoot::try_from(VaultRootInput {
        path: vault.path().to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?;
    let roots = VaultSet::try_from(vec![vault_root])?;

    let result = Filesystem::try_from(FilesystemConfig {
        roots,
        limits: vec![FsLimits {
            max_read_bytes: 1024,
            max_batch_files: 50,
        }],
        hidden: vec![],
        atomic_write_guard: None,
    });

    assert!(matches!(result, Err(FsError::HiddenCountMismatch { .. })));
    Ok(())
}

#[test]
fn require_directory_accepts_a_folder_and_refuses_a_file_or_a_missing_path_with_stable_codes()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::create_dir(vault.path().join("wiki"))?;
    std::fs::write(vault.path().join("wiki/note.md"), "note")?;
    let (filesystem, roots) = filesystem(vault.path().to_path_buf(), true)?;

    filesystem.require_directory(&path(&roots, "wiki")?)?;
    filesystem.require_directory(&path(&roots, ".")?)?;
    let file = filesystem.require_directory(&path(&roots, "wiki/note.md")?);
    let missing = filesystem.require_directory(&path(&roots, "wikis")?);

    assert_eq!(file.map_err(|error| error.code()), Err("path/not-a-directory"));
    assert_eq!(missing.map_err(|error| error.code()), Err("path/not-found"));
    Ok(())
}

#[test]
fn is_file_is_true_only_for_an_existing_regular_file() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::create_dir(vault.path().join("wiki"))?;
    std::fs::write(vault.path().join("wiki/note.md"), "note")?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(vault.path().join("wiki/note.md"), vault.path().join("alias.md"))?;
    let (filesystem, roots) = filesystem(vault.path().to_path_buf(), true)?;

    assert!(filesystem.is_file(&path(&roots, "wiki/note.md")?)?);
    assert!(
        !filesystem.is_file(&path(&roots, "wiki")?)?,
        "a directory is not a file"
    );
    assert!(!filesystem.is_file(&path(&roots, "wiki/missing.md")?)?);
    #[cfg(unix)]
    assert!(
        filesystem.is_file(&path(&roots, "alias.md")?)?,
        "a link that stays inside the vault"
    );
    Ok(())
}

#[test]
fn list_files_returns_every_file_but_no_directory_hidden_path_or_symlink() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::create_dir_all(vault.path().join("wiki/deep"))?;
    std::fs::create_dir(vault.path().join("secret"))?;
    std::fs::create_dir(vault.path().join(".git"))?;
    std::fs::write(vault.path().join("b.md"), "b")?;
    std::fs::write(vault.path().join("wiki/deep/A.png"), "a")?;
    std::fs::write(vault.path().join("secret/note.md"), "hidden")?;
    std::fs::write(vault.path().join(".git/config"), "hidden")?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(vault.path().join("b.md"), vault.path().join("link.md"))?;
    let mut hidden = default_hidden_patterns();
    hidden.push("secret/**".to_owned());
    let (filesystem, roots) = filesystem_with_hidden(vault.path().to_path_buf(), true, hidden)?;

    let files = filesystem.list_files(&ListFilesRequest {
        path: path(&roots, ".")?,
        max_files: 100,
    })?;

    assert_eq!(files, vec!["b.md", "wiki/deep/A.png"]);
    Ok(())
}

#[test]
fn list_files_fails_rather_than_truncating_when_the_vault_holds_more_files_than_the_cap()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("a.md"), "a")?;
    std::fs::write(vault.path().join("b.md"), "b")?;
    let (filesystem, roots) = filesystem(vault.path().to_path_buf(), true)?;

    let error = filesystem
        .list_files(&ListFilesRequest {
            path: path(&roots, ".")?,
            max_files: 1,
        })
        .err()
        .ok_or("listing above the cap must fail")?;

    assert!(matches!(error, FsError::TooManyFiles { maximum: 1, .. }), "{error:?}");
    assert_eq!(error.code(), "io/too-many-files");
    Ok(())
}

#[cfg(unix)]
#[test]
fn list_files_skips_a_directory_it_may_not_read_instead_of_failing() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;

    let vault = tempdir()?;
    std::fs::create_dir(vault.path().join("locked"))?;
    std::fs::write(vault.path().join("locked/inside.md"), "x")?;
    std::fs::write(vault.path().join("open.md"), "x")?;
    std::fs::set_permissions(vault.path().join("locked"), std::fs::Permissions::from_mode(0o000))?;
    let restricted = std::fs::read_dir(vault.path().join("locked")).is_err();
    let (filesystem, roots) = filesystem(vault.path().to_path_buf(), true)?;

    let outcome = filesystem.list_files(&ListFilesRequest {
        path: path(&roots, ".")?,
        max_files: 100,
    });
    std::fs::set_permissions(vault.path().join("locked"), std::fs::Permissions::from_mode(0o755))?;

    if restricted {
        assert_eq!(outcome?, vec!["open.md"]);
    }
    Ok(())
}

#[test]
fn hidden_paths_hide_what_the_listing_tools_hide_and_nothing_else() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;
    let hidden = HiddenPaths::try_from(HiddenPathsInput {
        root: vault.path(),
        patterns: &["secret/**".to_owned(), "*.tmp".to_owned(), ".git/".to_owned()],
    })?;

    assert!(hidden.hides("secret/plan.md", false));
    assert!(hidden.hides("secret", true));
    assert!(hidden.hides("notes/scratch.tmp", false));
    assert!(hidden.hides(".git/config", false));
    assert!(!hidden.hides("notes/plan.md", false));
    assert!(!hidden.hides("secretive.md", false));
    Ok(())
}

#[test]
fn hidden_paths_reject_a_pattern_the_listing_tools_would_reject() -> Result<(), Box<dyn std::error::Error>> {
    let vault = tempdir()?;

    let result = HiddenPaths::try_from(HiddenPathsInput {
        root: vault.path(),
        patterns: &["[z-a]".to_owned()],
    });

    assert!(matches!(result, Err(FsError::InvalidExclude { .. })));
    Ok(())
}
