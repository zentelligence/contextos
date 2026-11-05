//! A file's creation date is its first commit in the vault's history, not the latest change to it, and a file
//! without history has none.

use contextos_core::{ReportsCreation, VaultRoot, VaultRootInput, VaultSet};
use contextos_git::{Git2Vault, Git2VaultConfig};
use git2::{Repository, Signature, Time};
use tempfile::tempdir;
use time::macros::datetime;

mod support;

type BoxError = Box<dyn std::error::Error>;

fn commit_all(repository: &Repository, message: &str, unix_seconds: i64, offset_minutes: i32) -> Result<(), BoxError> {
    let mut index = repository.index()?;
    index.add_all(["*"], git2::IndexAddOption::DEFAULT, None)?;
    index.write()?;
    let tree = repository.find_tree(index.write_tree()?)?;
    let signature = Signature::new(
        "Fixture",
        "fixture@example.invalid",
        &Time::new(unix_seconds, offset_minutes),
    )?;
    let parents = repository
        .head()
        .ok()
        .and_then(|head| head.peel_to_commit().ok())
        .into_iter()
        .collect::<Vec<_>>();
    let parent_refs = parents.iter().collect::<Vec<_>>();
    repository.commit(Some("HEAD"), &signature, &signature, message, &tree, &parent_refs)?;
    Ok(())
}

fn vault_with_history() -> Result<(tempfile::TempDir, VaultRoot, Git2Vault<support::FixedClock>), BoxError> {
    let vault = tempdir()?;
    let root = VaultRoot::try_from(VaultRootInput {
        path: vault.path().to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?;
    let git = Git2Vault::try_from(Git2VaultConfig {
        root: root.clone(),
        roots: VaultSet::try_from(vec![root.clone()])?,
        clock: support::FixedClock,
        author_name: "Context OS MCP".to_owned(),
        author_email: "mcp@contextos.local".to_owned(),
        allow_destructive_restore: false,
        protected_restore_paths: Vec::new(),
    })?;
    Ok((vault, root, git))
}

#[test]
fn a_tracked_file_reports_the_commit_that_first_added_it() -> Result<(), BoxError> {
    let (vault, root, git) = vault_with_history()?;
    let repository = Repository::init(vault.path())?;
    std::fs::create_dir_all(vault.path().join("memory/wiki"))?;
    std::fs::write(vault.path().join("memory/wiki/index.md"), "first\n")?;
    commit_all(&repository, "add", 1_700_000_000, 600)?;
    std::fs::write(vault.path().join("memory/wiki/index.md"), "second\n")?;
    std::fs::write(vault.path().join("later.md"), "later\n")?;
    commit_all(&repository, "change", 1_800_000_000, 600)?;

    let created = git.created_at(&support::vault_path(&root, "memory/wiki/index.md")?);
    let later = git.created_at(&support::vault_path(&root, "later.md")?);

    assert_eq!(created, Some(datetime!(2023-11-15 08:13:20 +10:00)));
    assert_eq!(later, Some(datetime!(2027-01-15 18:00:00 +10:00)));
    Ok(())
}

#[test]
fn an_untracked_file_has_no_creation_date() -> Result<(), BoxError> {
    let (vault, root, git) = vault_with_history()?;
    let repository = Repository::init(vault.path())?;
    std::fs::write(vault.path().join("tracked.md"), "tracked\n")?;
    commit_all(&repository, "add", 1_700_000_000, 0)?;
    std::fs::write(vault.path().join("untracked.md"), "untracked\n")?;

    assert_eq!(git.created_at(&support::vault_path(&root, "untracked.md")?), None);
    Ok(())
}

#[test]
fn a_vault_that_is_not_a_repository_has_no_creation_dates() -> Result<(), BoxError> {
    let (vault, root, git) = vault_with_history()?;
    std::fs::write(vault.path().join("note.md"), "note\n")?;

    assert_eq!(git.created_at(&support::vault_path(&root, "note.md")?), None);
    Ok(())
}

#[test]
fn a_repository_without_commits_has_no_creation_dates() -> Result<(), BoxError> {
    let (vault, root, git) = vault_with_history()?;
    Repository::init(vault.path())?;
    std::fs::write(vault.path().join("note.md"), "note\n")?;

    assert_eq!(git.created_at(&support::vault_path(&root, "note.md")?), None);
    Ok(())
}

#[test]
fn a_file_removed_and_later_added_back_is_dated_from_when_it_came_back() -> Result<(), BoxError> {
    let (vault, root, git) = vault_with_history()?;
    let repository = Repository::init(vault.path())?;
    std::fs::write(vault.path().join("anchor.md"), "anchor\n")?;
    std::fs::write(vault.path().join("note.md"), "first life\n")?;
    commit_all(&repository, "add", 1_700_000_000, 0)?;
    std::fs::remove_file(vault.path().join("note.md"))?;
    commit_all(&repository, "remove", 1_750_000_000, 0)?;
    std::fs::write(vault.path().join("note.md"), "second life\n")?;
    commit_all(&repository, "add back", 1_800_000_000, 0)?;

    let created = git.created_at(&support::vault_path(&root, "note.md")?);
    let anchor = git.created_at(&support::vault_path(&root, "anchor.md")?);

    assert_eq!(created, Some(datetime!(2027-01-15 8:00:00 UTC)));
    assert_eq!(anchor, Some(datetime!(2023-11-14 22:13:20 UTC)));
    Ok(())
}
