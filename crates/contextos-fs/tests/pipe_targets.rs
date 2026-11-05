//! A mutation whose target is a named pipe is refused as not a file and returns at once, leaving the pipe in place:
//! opening a pipe the usual way waits for a reader or writer, and a mutation does that while holding the vault's
//! write lock, so every later mutation would wait too.
#![cfg(unix)]

use std::path::{Path, PathBuf};

use contextos_core::{
    AppendMutation, Clock, ContentHash, Origin, VaultPath, VaultPathInput, VaultRoot, VaultRootInput, VaultSet,
    WriteMutation,
};
use contextos_fs::{
    Filesystem, FilesystemConfig, FilesystemService, FilesystemServiceConfig, FsError, FsLimits,
    default_hidden_patterns, open_without_waiting,
};
use time::OffsetDateTime;
use time::macros::datetime;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Clone, Copy, Debug)]
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        datetime!(2026-10-23 10:11:12 +11:00)
    }
}

struct Vault {
    directory: tempfile::TempDir,
    roots: VaultSet,
    service: FilesystemService<FixedClock>,
}

impl Vault {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let roots = VaultSet::try_from(vec![VaultRoot::try_from(VaultRootInput {
            path: directory.path().to_path_buf(),
            managed: true,
            name: Some("vault".to_owned()),
        })?])?;
        let filesystem = Filesystem::try_from(FilesystemConfig {
            roots: roots.clone(),
            limits: vec![FsLimits {
                max_read_bytes: 1024,
                max_batch_files: 50,
            }],
            hidden: vec![default_hidden_patterns()],
            atomic_write_guard: None,
        })?;
        let service = FilesystemService::from(FilesystemServiceConfig {
            filesystem,
            clock: FixedClock,
        });
        Ok(Self {
            directory,
            roots,
            service,
        })
    }

    /// Makes a named pipe with no reader or writer at `name` and returns its vault path and host path.
    fn pipe(&self, name: &str) -> Result<(VaultPath, PathBuf), Box<dyn std::error::Error>> {
        let host = self.directory.path().join(name);
        assert!(
            std::process::Command::new("mkfifo").arg(&host).status()?.success(),
            "mkfifo must succeed"
        );
        let path = VaultPath::try_from(VaultPathInput {
            roots: &self.roots,
            raw: name,
        })?;
        Ok((path, host))
    }
}

/// Runs `mutate` on its own thread and fails rather than hangs when it does not finish. On a timeout it opens the
/// pipe at `pipe` from both ends, without waiting, which releases a mutation blocked on it.
fn finishes_within<T: Send + 'static>(
    pipe: &Path,
    mutate: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Box<dyn std::error::Error>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(mutate());
    });
    match receiver.recv_timeout(std::time::Duration::from_secs(10)) {
        Ok(value) => Ok(value),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            let reader = open_without_waiting(pipe, std::fs::OpenOptions::new().read(true));
            let writer = open_without_waiting(pipe, std::fs::OpenOptions::new().write(true));
            drop((reader, writer));
            Err("the mutation blocked on a named pipe instead of refusing it".into())
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err("the mutation thread panicked".into()),
    }
}

fn is_pipe(host: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::symlink_metadata(host).is_ok_and(|metadata| metadata.file_type().is_fifo())
}

#[test]
fn appending_to_a_named_pipe_is_refused_as_not_a_file_without_blocking() -> TestResult {
    let vault = Vault::new()?;
    let (path, host) = vault.pipe("log.md")?;
    let service = vault.service.clone();

    let result = finishes_within(&host, move || {
        service.append_file(&AppendMutation {
            path,
            preamble: "# Log\n".to_owned(),
            content: "- entry\n".to_owned(),
            origin: Origin::Tool("vault_log_append".to_owned()),
        })
    })?;

    assert!(matches!(result, Err(FsError::NotFile { .. })), "{result:?}");
    assert!(is_pipe(&host), "the pipe must be left in place");
    Ok(())
}

#[test]
fn appending_to_a_named_pipe_that_has_a_reader_is_refused_and_sends_it_nothing() -> TestResult {
    use std::io::Read;
    let vault = Vault::new()?;
    let (path, host) = vault.pipe("log.md")?;
    let mut reader = open_without_waiting(&host, std::fs::OpenOptions::new().read(true))?;
    let service = vault.service.clone();

    let result = finishes_within(&host, move || {
        service.append_file(&AppendMutation {
            path,
            preamble: String::new(),
            content: "- entry\n".to_owned(),
            origin: Origin::Tool("vault_log_append".to_owned()),
        })
    })?;

    assert!(matches!(result, Err(FsError::NotFile { .. })), "{result:?}");
    let mut received = Vec::new();
    // With no writer left, a non-blocking read returns what was sent and then the end of the stream.
    let _ = reader.read_to_end(&mut received);
    assert!(received.is_empty(), "the reader received {received:?}");
    Ok(())
}

#[test]
fn writing_over_a_named_pipe_with_an_expected_hash_is_refused_as_not_a_file_without_blocking() -> TestResult {
    let vault = Vault::new()?;
    let (path, host) = vault.pipe("note.md")?;
    let service = vault.service.clone();

    let result = finishes_within(&host, move || {
        service.write_file(&WriteMutation {
            path,
            content: "text".to_owned().into(),
            expected_hash: Some(ContentHash::from([0_u8; 32])),
            force: false,
            origin: Origin::Tool("fs_write_file".to_owned()),
        })
    })?;

    assert!(matches!(result, Err(FsError::NotFile { .. })), "{result:?}");
    assert!(is_pipe(&host), "the pipe must be left in place");
    Ok(())
}

#[test]
fn writing_over_a_named_pipe_without_an_expected_hash_is_refused_as_not_a_file_without_blocking() -> TestResult {
    let vault = Vault::new()?;
    let (path, host) = vault.pipe("note.md")?;
    let service = vault.service.clone();

    let result = finishes_within(&host, move || {
        service.write_file(&WriteMutation {
            path,
            content: "text".to_owned().into(),
            expected_hash: None,
            force: false,
            origin: Origin::Tool("fs_write_file".to_owned()),
        })
    })?;

    assert!(matches!(result, Err(FsError::NotFile { .. })), "{result:?}");
    assert!(is_pipe(&host), "the pipe must be left in place");
    Ok(())
}
