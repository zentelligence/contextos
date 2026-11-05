//! A generated commit message names the tool that made the change and how many files it touched, so history reads as
//! a list of operations; the body stays bounded however large the operation was.

use contextos_core::{Clock, OpKind, OperationEvent, Origin, VaultRoot, VaultRootInput, VaultSet, VersionsVault};
use contextos_git::{Git2Vault, Git2VaultConfig};
use tempfile::tempdir;

mod support;

type BoxError = Box<dyn std::error::Error>;

struct Fixture {
    vault: tempfile::TempDir,
    root: VaultRoot,
    git: Git2Vault<support::FixedClock>,
}

fn fixture() -> Result<Fixture, BoxError> {
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
    std::fs::write(vault.path().join("seed.md"), "seed\n")?;
    git.initialise(&support::writer(&root)?)?;
    Ok(Fixture { vault, root, git })
}

impl Fixture {
    fn stage(&self, file: &str, origin: Origin) -> Result<(), BoxError> {
        std::fs::write(self.vault.path().join(file), "content\n")?;
        self.git
            .stage(&OperationEvent {
                kind: OpKind::Create,
                paths: vec![support::vault_path(&self.root, file)?],
                origin,
                summary: format!("Created {file}"),
                at: support::FixedClock.now(),
            })
            .map_err(|warning| warning.message)?;
        Ok(())
    }

    fn commit_message(&self) -> Result<String, BoxError> {
        Ok(self.git.commit(None)?.message.ok_or("a commit message")?)
    }
}

fn subject(message: &str) -> &str {
    message.lines().next().unwrap_or_default()
}

#[test]
fn a_single_tool_change_names_the_tool_and_one_file() -> Result<(), BoxError> {
    let fixture = fixture()?;
    fixture.stage("a.md", Origin::Tool("fs_write_file".to_owned()))?;

    assert_eq!(subject(&fixture.commit_message()?), "mcp: fs_write_file (1 file)");
    Ok(())
}

#[test]
fn a_tool_and_the_internal_writes_it_caused_are_named_by_the_tool_alone() -> Result<(), BoxError> {
    let fixture = fixture()?;
    fixture.stage("a.md", Origin::Tool("fs_write_file".to_owned()))?;
    fixture.stage("index.md", Origin::Internal("index".to_owned()))?;
    fixture.stage("log.md", Origin::Internal("oplog".to_owned()))?;

    assert_eq!(subject(&fixture.commit_message()?), "mcp: fs_write_file (3 files)");
    Ok(())
}

#[test]
fn work_done_on_a_tools_behalf_by_an_internal_service_is_named_for_that_tool() -> Result<(), BoxError> {
    let fixture = fixture()?;
    for file in ["a.md", "b.md", "c.md"] {
        fixture.stage(file, Origin::Internal("vault_index_rebuild".to_owned()))?;
    }

    assert_eq!(
        subject(&fixture.commit_message()?),
        "mcp: vault_index_rebuild (3 files)"
    );
    Ok(())
}

#[test]
fn several_tools_are_all_named_in_the_order_they_first_appeared() -> Result<(), BoxError> {
    let fixture = fixture()?;
    fixture.stage("a.md", Origin::Tool("fs_write_file".to_owned()))?;
    fixture.stage("b.md", Origin::Tool("note_create".to_owned()))?;
    fixture.stage("c.md", Origin::Tool("fs_write_file".to_owned()))?;

    assert_eq!(
        subject(&fixture.commit_message()?),
        "mcp: fs_write_file, note_create (3 files)"
    );
    Ok(())
}

#[test]
fn a_file_touched_twice_counts_once() -> Result<(), BoxError> {
    let fixture = fixture()?;
    fixture.stage("a.md", Origin::Tool("fs_write_file".to_owned()))?;
    fixture.stage("a.md", Origin::Tool("fs_edit_file".to_owned()))?;

    assert_eq!(
        subject(&fixture.commit_message()?),
        "mcp: fs_write_file, fs_edit_file (1 file)"
    );
    Ok(())
}

#[test]
fn the_body_lists_what_was_done_but_stays_bounded_for_a_large_operation() -> Result<(), BoxError> {
    let fixture = fixture()?;
    for number in 0..75 {
        fixture.stage(
            &format!("note-{number:02}.md"),
            Origin::Internal("vault_index_rebuild".to_owned()),
        )?;
    }

    let message = fixture.commit_message()?;

    assert_eq!(subject(&message), "mcp: vault_index_rebuild (75 files)");
    assert!(message.contains("Created note-00.md"));
    assert!(!message.contains("Created note-74.md"), "the body is capped");
    assert!(message.trim_end().ends_with("and 25 more operations"), "{message}");
    assert!(message.lines().count() <= 55, "{}", message.lines().count());
    Ok(())
}

#[test]
fn an_explicit_message_is_used_as_given() -> Result<(), BoxError> {
    let fixture = fixture()?;
    fixture.stage("a.md", Origin::Tool("fs_write_file".to_owned()))?;

    let message = fixture.git.commit(Some("operator says so"))?.message;

    assert_eq!(message.as_deref(), Some("operator says so"));
    Ok(())
}
