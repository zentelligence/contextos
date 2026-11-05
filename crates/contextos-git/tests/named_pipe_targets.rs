//! A vault file that is a named pipe is refused by Git initialisation and restore rather than waited on: opening a
//! pipe the usual way waits for a writer, which would hold the thread, and the vault's write lock, for ever.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use contextos_core::{OpKind, OperationEvent, Origin, VaultRoot, VaultRootInput, VersionsVault};
use contextos_fs::{FilesystemService, FilesystemServiceConfig};
use contextos_git::{Git2Vault, Git2VaultConfig, GitError, GitRestoreRequest, GitWriteError};
use tempfile::tempdir;

mod support;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn make_pipe(path: &Path) -> TestResult {
    if !std::process::Command::new("mkfifo").arg(path).status()?.success() {
        return Err("mkfifo must succeed".into());
    }
    Ok(())
}

fn is_pipe(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_fifo())
}

/// Runs `call` on its own thread and fails rather than hangs when it does not finish. On a timeout it opens the pipe
/// from both ends on threads of their own, which releases a reader or writer blocked on it.
fn finishes_within<T: Send + 'static>(
    pipe: PathBuf,
    call: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Box<dyn std::error::Error>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(call());
    });
    match receiver.recv_timeout(Duration::from_secs(10)) {
        Ok(value) => Ok(value),
        Err(RecvTimeoutError::Timeout) => {
            let writer = pipe.clone();
            std::thread::spawn(move || std::fs::OpenOptions::new().write(true).open(writer));
            std::thread::spawn(move || std::fs::File::open(pipe));
            Err("the call blocked on a named pipe instead of refusing it".into())
        }
        Err(RecvTimeoutError::Disconnected) => Err("the call thread panicked".into()),
    }
}

fn git(root: &VaultRoot) -> Result<Git2Vault<support::FixedClock>, Box<dyn std::error::Error>> {
    Ok(Git2Vault::try_from(Git2VaultConfig {
        root: root.clone(),
        roots: contextos_core::VaultSet::try_from(vec![root.clone()])?,
        clock: support::FixedClock,
        author_name: "Context OS MCP".to_owned(),
        author_email: "mcp@contextos.local".to_owned(),
        allow_destructive_restore: false,
        protected_restore_paths: Vec::new(),
    })?)
}

fn root(path: &Path) -> Result<VaultRoot, Box<dyn std::error::Error>> {
    Ok(VaultRoot::try_from(VaultRootInput {
        path: path.to_path_buf(),
        managed: true,
        name: Some("vault".to_owned()),
    })?)
}

#[test]
fn initialising_a_vault_whose_ignore_file_is_a_named_pipe_is_refused_without_blocking() -> TestResult {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("note.md"), "tracked\n")?;
    let pipe = vault.path().join(".gitignore");
    make_pipe(&pipe)?;
    let root = root(vault.path())?;
    let writer = FilesystemService::from(FilesystemServiceConfig {
        filesystem: support::filesystem(&root)?,
        clock: support::FixedClock,
    });
    let git = git(&root)?;

    let result = finishes_within(pipe.clone(), move || git.initialise(&writer).map(|_| ()))?;

    assert!(
        matches!(result, Err(GitWriteError::Git(GitError::ReadIgnore { .. }))),
        "{result:?}"
    );
    assert!(is_pipe(&pipe), "the pipe must be left in place");
    assert!(!vault.path().join(".git").exists(), "no repository may be created");
    Ok(())
}

#[test]
fn restoring_over_a_named_pipe_is_refused_without_blocking() -> TestResult {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("note.md"), "baseline\n")?;
    let root = root(vault.path())?;
    let writer = FilesystemService::from(FilesystemServiceConfig {
        filesystem: support::filesystem(&root)?,
        clock: support::FixedClock,
    });
    let git = git(&root)?;
    let baseline = git.initialise(&writer)?.commit_id.ok_or("initial commit missing")?;
    let pipe = vault.path().join("note.md");
    std::fs::remove_file(&pipe)?;
    make_pipe(&pipe)?;
    let request = GitRestoreRequest {
        path: support::vault_path(&root, "note.md")?,
        reference: baseline,
        dry_run: false,
    };

    let result = finishes_within(pipe.clone(), move || git.restore(&request, &writer).map(|_| ()))?;

    assert!(
        matches!(result, Err(GitWriteError::Git(GitError::ReadRestoreTarget { .. }))),
        "{result:?}"
    );
    assert!(is_pipe(&pipe), "the pipe must be left in place");
    Ok(())
}

fn modified(root: &VaultRoot, raw: &str) -> Result<OperationEvent, Box<dyn std::error::Error>> {
    Ok(OperationEvent {
        kind: OpKind::Modify,
        paths: vec![support::vault_path(root, raw)?],
        origin: Origin::Tool("fs_write_file".to_owned()),
        summary: format!("Overwrote {raw}"),
        at: time::macros::datetime!(2026-10-23 10:00:00 +11:00),
    })
}

fn committed_paths(vault: &Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let repository = git2::Repository::open(vault)?;
    let tree = repository.head()?.peel_to_tree()?;
    Ok(tree
        .iter()
        .filter_map(|entry| entry.name().ok().map(str::to_owned))
        .collect())
}

#[test]
fn staging_a_note_swapped_for_a_named_pipe_stages_it_as_gone_without_blocking() -> TestResult {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("note.md"), "baseline\n")?;
    let root = root(vault.path())?;
    let writer = FilesystemService::from(FilesystemServiceConfig {
        filesystem: support::filesystem(&root)?,
        clock: support::FixedClock,
    });
    let git = git(&root)?;
    git.initialise(&writer)?;
    let pipe = vault.path().join("note.md");
    std::fs::remove_file(&pipe)?;
    make_pipe(&pipe)?;
    let event = modified(&root, "note.md")?;

    let staged = finishes_within(pipe.clone(), move || {
        git.stage(&event).map_err(|warning| warning.message)?;
        git.commit(None).map_err(|error| error.to_string())
    })?;

    let result = staged.map_err(|message| -> Box<dyn std::error::Error> { message.into() })?;
    assert_eq!(result.committed_paths, vec![PathBuf::from("note.md")]);
    assert!(!committed_paths(vault.path())?.contains(&"note.md".to_owned()));
    assert!(is_pipe(&pipe), "the pipe must be left in place");
    Ok(())
}

#[test]
fn recovering_a_pending_note_swapped_for_a_named_pipe_does_not_block_start_up() -> TestResult {
    let vault = tempdir()?;
    std::fs::write(vault.path().join("note.md"), "baseline\n")?;
    let root = root(vault.path())?;
    let writer = FilesystemService::from(FilesystemServiceConfig {
        filesystem: support::filesystem(&root)?,
        clock: support::FixedClock,
    });
    let interrupted = git(&root)?;
    interrupted.initialise(&writer)?;
    std::fs::write(vault.path().join("note.md"), "changed\n")?;
    interrupted
        .stage(&modified(&root, "note.md")?)
        .map_err(|warning| warning.message)?;
    drop(interrupted);
    let pipe = vault.path().join("note.md");
    std::fs::remove_file(&pipe)?;
    make_pipe(&pipe)?;
    let restarted = git(&root)?;

    let result = finishes_within(pipe.clone(), move || restarted.recover_staged())??;

    assert_eq!(result.committed_paths, vec![PathBuf::from("note.md")]);
    assert!(!committed_paths(vault.path())?.contains(&"note.md".to_owned()));
    assert!(is_pipe(&pipe), "the pipe must be left in place");
    Ok(())
}
