//! Uncommitted changes made by MCP tools (paths recorded as pending, still differing from the last commit after the
//! debounce window) are told apart from edits an operator made outside MCP, which are only ever counted.

use std::time::Duration;

use contextos_core::{Clock, OpKind, OperationEvent, Origin, VaultRoot, VaultRootInput, VaultSet, VersionsVault};
use contextos_git::{Git2Vault, Git2VaultConfig};
use tempfile::tempdir;

mod support;

type BoxError = Box<dyn std::error::Error>;

const WINDOW: Duration = Duration::from_secs(30);

struct Fixture {
    vault: tempfile::TempDir,
    root: VaultRoot,
    git: Git2Vault<support::FixedClock>,
}

fn fixture() -> Result<Fixture, BoxError> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("note.md"), "initial\n")?;
    std::fs::write(vault.path().join("operator.md"), "initial\n")?;
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
    Ok(Fixture { vault, root, git })
}

impl Fixture {
    /// Changes `note.md` and stages it as an MCP tool would.
    fn mcp_change(&self) -> Result<(), BoxError> {
        std::fs::write(self.vault.path().join("note.md"), "changed by a tool\n")?;
        self.git
            .stage(&OperationEvent {
                kind: OpKind::Modify,
                paths: vec![support::vault_path(&self.root, "note.md")?],
                origin: Origin::Tool("fs_write_file".to_owned()),
                summary: "Changed note.md".to_owned(),
                at: support::FixedClock.now(),
            })
            .map_err(|warning| warning.message)?;
        Ok(())
    }

    /// Backdates every pending marker to `seconds` before the fixed clock's instant.
    fn age_markers(&self, seconds: u64) -> Result<(), BoxError> {
        let now = std::time::SystemTime::from(support::FixedClock.now());
        let stamped = now - Duration::from_secs(seconds);
        for entry in std::fs::read_dir(self.vault.path().join(".git/contextos-mcp/pending"))? {
            let path = entry?.path();
            if path.extension().and_then(std::ffi::OsStr::to_str) == Some("pending") {
                std::fs::File::options().write(true).open(path)?.set_modified(stamped)?;
            }
        }
        Ok(())
    }
}

#[test]
fn a_tool_change_within_the_window_is_not_yet_overdue() -> Result<(), BoxError> {
    let fixture = fixture()?;
    fixture.mcp_change()?;
    fixture.age_markers(5)?;

    let report = fixture.git.uncommitted(WINDOW)?;

    assert!(report.overdue_mcp_paths.is_empty(), "{report:?}");
    Ok(())
}

#[test]
fn a_tool_change_still_uncommitted_after_the_window_is_overdue() -> Result<(), BoxError> {
    let fixture = fixture()?;
    fixture.mcp_change()?;
    fixture.age_markers(31)?;

    let report = fixture.git.uncommitted(WINDOW)?;

    assert_eq!(report.overdue_mcp_paths, vec!["note.md"]);
    assert_eq!(report.operator_changes, 0);
    Ok(())
}

#[test]
fn a_recorded_path_that_no_longer_differs_from_the_last_commit_is_not_overdue() -> Result<(), BoxError> {
    let fixture = fixture()?;
    fixture.mcp_change()?;
    fixture.age_markers(120)?;
    std::fs::write(fixture.vault.path().join("note.md"), "initial\n")?;
    let repository = git2::Repository::open(fixture.vault.path())?;
    let mut index = repository.index()?;
    index.add_path(std::path::Path::new("note.md"))?;
    index.write()?;

    let report = fixture.git.uncommitted(WINDOW)?;

    assert!(report.overdue_mcp_paths.is_empty(), "{report:?}");
    Ok(())
}

#[test]
fn an_edit_made_outside_mcp_is_counted_but_never_overdue() -> Result<(), BoxError> {
    let fixture = fixture()?;
    std::fs::write(fixture.vault.path().join("operator.md"), "edited in Obsidian\n")?;
    std::fs::write(fixture.vault.path().join("new-by-operator.md"), "new\n")?;

    let report = fixture.git.uncommitted(WINDOW)?;

    assert!(
        report.overdue_mcp_paths.is_empty(),
        "`report.overdue_mcp_paths` should be empty, got {:?}",
        report.overdue_mcp_paths
    );
    assert_eq!(report.operator_changes, 2);
    Ok(())
}

#[test]
fn committing_the_recorded_paths_clears_the_overdue_report() -> Result<(), BoxError> {
    let fixture = fixture()?;
    fixture.mcp_change()?;
    fixture.age_markers(31)?;
    assert_eq!(fixture.git.uncommitted(WINDOW)?.overdue_mcp_paths.len(), 1);

    fixture.git.recover_staged()?;

    let report = fixture.git.uncommitted(WINDOW)?;
    assert!(
        report.overdue_mcp_paths.is_empty(),
        "`report.overdue_mcp_paths` should be empty, got {:?}",
        report.overdue_mcp_paths
    );
    Ok(())
}

#[test]
fn staging_a_path_again_refreshes_its_marker_so_the_batch_is_not_overdue() -> Result<(), BoxError> {
    let fixture = fixture()?;
    fixture.mcp_change()?;
    fixture.age_markers(120)?;
    assert_eq!(fixture.git.uncommitted(WINDOW)?.overdue_mcp_paths.len(), 1);

    fixture.mcp_change()?;

    let report = fixture.git.uncommitted(WINDOW)?;
    assert!(
        report.overdue_mcp_paths.is_empty(),
        "the path was touched just now: {report:?}"
    );
    Ok(())
}

#[test]
fn a_batch_is_overdue_only_when_all_of_it_has_been_quiet_for_the_window() -> Result<(), BoxError> {
    let fixture = fixture()?;
    fixture.mcp_change()?;
    fixture.age_markers(120)?;
    std::fs::write(fixture.vault.path().join("operator.md"), "touched by a later tool\n")?;
    fixture
        .git
        .stage(&OperationEvent {
            kind: OpKind::Modify,
            paths: vec![support::vault_path(&fixture.root, "operator.md")?],
            origin: Origin::Tool("fs_write_file".to_owned()),
            summary: "Changed operator.md".to_owned(),
            at: support::FixedClock.now(),
        })
        .map_err(|warning| warning.message)?;

    let report = fixture.git.uncommitted(WINDOW)?;

    assert!(
        report.overdue_mcp_paths.is_empty(),
        "an older path in a batch that is still receiving changes is inside its rolling window: {report:?}"
    );
    Ok(())
}
