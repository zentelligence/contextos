//! A pending batch remembers when it began, so a caller can cap how long sustained activity may keep deferring the
//! commit: the debounce only ever resets on new changes, and a busy vault would otherwise never commit.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use contextos_core::{Clock, OpKind, OperationEvent, Origin, VaultRoot, VaultRootInput, VaultSet, VersionsVault};
use contextos_git::{Git2Vault, Git2VaultConfig};
use tempfile::tempdir;
use time::OffsetDateTime;

mod support;

type BoxError = Box<dyn std::error::Error>;

/// A clock the test moves by hand.
#[derive(Clone, Debug)]
struct ManualClock(Arc<AtomicI64>);

impl Clock for ManualClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(self.0.load(Ordering::SeqCst))
    }
}

struct Fixture {
    vault: tempfile::TempDir,
    root: VaultRoot,
    seconds: Arc<AtomicI64>,
    git: Git2Vault<ManualClock>,
}

fn fixture() -> Result<Fixture, BoxError> {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("note.md"), "initial\n")?;
    let root = VaultRoot::try_from(VaultRootInput {
        path: vault.path().to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?;
    let seconds = Arc::new(AtomicI64::new(1_000_000));
    let git = Git2Vault::try_from(Git2VaultConfig {
        root: root.clone(),
        roots: VaultSet::try_from(vec![root.clone()])?,
        clock: ManualClock(Arc::clone(&seconds)),
        author_name: "Context OS MCP".to_owned(),
        author_email: "mcp@contextos.local".to_owned(),
        allow_destructive_restore: false,
        protected_restore_paths: Vec::new(),
    })?;
    git.initialise(&support::writer(&root)?)?;
    Ok(Fixture {
        vault,
        root,
        seconds,
        git,
    })
}

impl Fixture {
    fn stage(&self, content: &str) -> Result<(), BoxError> {
        std::fs::write(self.vault.path().join("note.md"), content)?;
        self.git
            .stage(&OperationEvent {
                kind: OpKind::Modify,
                paths: vec![support::vault_path(&self.root, "note.md")?],
                origin: Origin::Tool("fs_write_file".to_owned()),
                summary: "Changed note.md".to_owned(),
                at: ManualClock(Arc::clone(&self.seconds)).now(),
            })
            .map_err(|warning| warning.message)?;
        Ok(())
    }

    fn advance(&self, seconds: i64) {
        self.seconds.fetch_add(seconds, Ordering::SeqCst);
    }
}

const CAP: Duration = Duration::from_secs(300);

#[test]
fn an_empty_batch_has_no_age() -> Result<(), BoxError> {
    let fixture = fixture()?;

    assert!(!fixture.git.batch_older_than(CAP)?);
    Ok(())
}

#[test]
fn a_batch_ages_from_its_first_change_not_its_latest() -> Result<(), BoxError> {
    let fixture = fixture()?;
    fixture.stage("one\n")?;
    fixture.advance(200);
    fixture.stage("two\n")?;
    fixture.advance(90);
    assert!(
        !fixture.git.batch_older_than(CAP)?,
        "290 seconds since the first change"
    );

    fixture.advance(20);
    fixture.stage("three\n")?;

    assert!(
        fixture.git.batch_older_than(CAP)?,
        "310 seconds since the first change, though the last was just now"
    );
    Ok(())
}

#[test]
fn committing_starts_a_new_batch() -> Result<(), BoxError> {
    let fixture = fixture()?;
    fixture.stage("one\n")?;
    fixture.advance(400);
    assert!(fixture.git.batch_older_than(CAP)?);

    fixture.git.commit(None)?;

    assert!(!fixture.git.batch_older_than(CAP)?);
    fixture.stage("two\n")?;
    assert!(!fixture.git.batch_older_than(CAP)?, "the new batch began just now");
    Ok(())
}
