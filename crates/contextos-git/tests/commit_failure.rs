//! A commit that fails after the write succeeded must not lose or hide anything: the paths stay pending, the
//! failure is reported by `status` with how many times it has been tried, and a later successful commit clears it.

use contextos_core::{OpKind, OperationEvent, Origin, VaultRoot, VaultRootInput, VaultSet, VersionsVault};
use contextos_git::{Git2Vault, Git2VaultConfig};
use tempfile::tempdir;
use time::macros::datetime;

mod support;

type BoxError = Box<dyn std::error::Error>;

struct Fixture {
    vault: tempfile::TempDir,
    root: VaultRoot,
    git: Git2Vault<support::FixedClock>,
}

fn staged_change() -> Result<Fixture, BoxError> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("note.md"), "initial\n")?;
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
    git.initialise(&support::writer(&root)?)?;
    std::fs::write(vault.path().join("note.md"), "changed\n")?;
    git.stage(&OperationEvent {
        kind: OpKind::Modify,
        paths: vec![support::vault_path(&root, "note.md")?],
        origin: Origin::Tool("fs_write_file".to_owned()),
        summary: "Changed note.md".to_owned(),
        at: datetime!(2026-07-18 18:30:00 +10:00),
    })
    .map_err(|warning| warning.message)?;
    Ok(Fixture { vault, root, git })
}

/// A lock on the branch reference makes the commit fail deterministically, and removing it lets a retry succeed.
fn lock_branch(vault: &std::path::Path) -> std::path::PathBuf {
    vault.join(".git/refs/heads/main.lock")
}

#[test]
fn a_failed_commit_keeps_its_paths_pending_and_is_reported_with_its_attempts() -> Result<(), BoxError> {
    let fixture = staged_change()?;
    std::fs::write(lock_branch(fixture.vault.path()), "")?;

    assert!(fixture.git.commit(None).is_err());
    let first = fixture.git.status()?;
    assert!(fixture.git.commit(None).is_err());
    let second = fixture.git.status()?;

    assert_eq!(first.pending_paths, vec!["note.md"]);
    let failure = first.commit_failure.ok_or("the failure should be reported")?;
    assert_eq!(failure.attempts, 1);
    assert_eq!(failure.code, "git/commit");
    assert_eq!(second.commit_failure.ok_or("still reported")?.attempts, 2);
    assert_eq!(second.pending_paths, vec!["note.md"]);
    Ok(())
}

#[test]
fn a_later_successful_commit_clears_the_reported_failure() -> Result<(), BoxError> {
    let fixture = staged_change()?;
    std::fs::write(lock_branch(fixture.vault.path()), "")?;
    assert!(fixture.git.commit(None).is_err());
    std::fs::remove_file(lock_branch(fixture.vault.path()))?;

    let committed = fixture.git.commit(None)?;

    let status = fixture.git.status()?;
    assert_eq!(committed.committed_paths.len(), 1);
    assert!(status.commit_failure.is_none());
    assert!(
        status.pending_paths.is_empty(),
        "`status.pending_paths` should be empty, got {:?}",
        status.pending_paths
    );
    let _root = &fixture.root;
    Ok(())
}

#[test]
fn a_debounced_commit_that_fails_is_recorded_the_same_way() -> Result<(), BoxError> {
    let fixture = staged_change()?;
    let generation = fixture.git.pending_generation()?;
    std::fs::write(lock_branch(fixture.vault.path()), "")?;

    assert!(fixture.git.commit_if_generation(generation).is_err());

    let failure = fixture
        .git
        .status()?
        .commit_failure
        .ok_or("the failure should be reported")?;
    assert_eq!(failure.attempts, 1);
    Ok(())
}

#[test]
fn a_superseded_generation_is_neither_a_commit_nor_a_failure() -> Result<(), BoxError> {
    let fixture = staged_change()?;
    let stale = fixture.git.pending_generation()?;
    std::fs::write(lock_branch(fixture.vault.path()), "")?;
    fixture
        .git
        .stage(&OperationEvent {
            kind: OpKind::Modify,
            paths: vec![support::vault_path(&fixture.root, "note.md")?],
            origin: Origin::Tool("fs_write_file".to_owned()),
            summary: "Changed note.md again".to_owned(),
            at: datetime!(2026-07-18 18:31:00 +10:00),
        })
        .map_err(|warning| warning.message)?;

    let outcome = fixture.git.commit_if_generation(stale)?;

    assert!(outcome.commit_id.is_none());
    assert!(fixture.git.status()?.commit_failure.is_none());
    Ok(())
}

#[test]
fn a_commit_that_fails_before_reaching_the_reference_still_reports_a_commit_failure() -> Result<(), BoxError> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("note.md"), "initial\n")?;
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
    // A repository with no commit yet: there is no parent to commit onto.
    git2::Repository::init(vault.path())?;
    git.stage(&OperationEvent {
        kind: OpKind::Create,
        paths: vec![support::vault_path(&root, "note.md")?],
        origin: Origin::Tool("fs_write_file".to_owned()),
        summary: "Created note.md".to_owned(),
        at: datetime!(2026-07-18 18:30:00 +10:00),
    })
    .map_err(|warning| warning.message)?;

    let error = git
        .commit(None)
        .err()
        .ok_or("committing without a parent should fail")?;

    assert_eq!(error.code(), "git/commit");
    Ok(())
}
