//! A temporary one-root vault for the unit tests of the read paths.

use contextos_core::{VaultPath, VaultPathInput, VaultRoot, VaultRootInput, VaultSet};
use tempfile::TempDir;

use crate::{Filesystem, FilesystemConfig, FsLimits, default_hidden_patterns};

pub(crate) struct Fixture {
    pub(crate) directory: TempDir,
    pub(crate) filesystem: Filesystem,
    roots: VaultSet,
}

impl Fixture {
    pub(crate) fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let roots = VaultSet::try_from(vec![VaultRoot::try_from(VaultRootInput {
            path: directory.path().to_path_buf(),
            managed: true,
            name: Some("vault".to_owned()),
        })?])?;
        let filesystem = Filesystem::try_from(FilesystemConfig {
            roots: roots.clone(),
            limits: vec![FsLimits {
                max_read_bytes: 1024 * 1024,
                max_batch_files: 50,
            }],
            hidden: vec![default_hidden_patterns()],
            atomic_write_guard: None,
        })?;
        Ok(Self {
            directory,
            filesystem,
            roots,
        })
    }

    /// A vault path for `name`, whether or not the file exists.
    pub(crate) fn write_path(&self, name: &str) -> Result<VaultPath, Box<dyn std::error::Error>> {
        Ok(VaultPath::try_from(VaultPathInput {
            roots: &self.roots,
            raw: name,
        })?)
    }

    pub(crate) fn write(&self, name: &str, bytes: &[u8]) -> Result<VaultPath, Box<dyn std::error::Error>> {
        std::fs::write(self.directory.path().join(name), bytes)?;
        Ok(VaultPath::try_from(VaultPathInput {
            roots: &self.roots,
            raw: name,
        })?)
    }
}

/// Replaces the file at `file` with a named pipe that has no writer, so opening it for reading the usual way would
/// wait for ever.
#[cfg(unix)]
pub(crate) fn replace_with_pipe(file: &std::path::Path) {
    let _ = std::fs::remove_file(file);
    let made = std::process::Command::new("mkfifo").arg(file).status();
    assert!(made.is_ok_and(|status| status.success()), "mkfifo must succeed");
}

/// Runs `read` on its own thread and fails rather than hangs when it does not finish. On a timeout it opens the
/// pipe at `pipe` for writing, which releases a reader blocked on it so the thread does not outlive the test.
#[cfg(unix)]
pub(crate) fn finishes_within<T: Send + 'static>(
    pipe: &std::path::Path,
    read: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Box<dyn std::error::Error>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(read());
    });
    match receiver.recv_timeout(std::time::Duration::from_secs(10)) {
        Ok(value) => Ok(value),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            // Without waiting itself, in case nothing is reading the pipe at this moment.
            let _ = crate::regular_file::open_without_waiting(pipe, std::fs::OpenOptions::new().write(true));
            Err("the read blocked on a named pipe instead of refusing it".into())
        }
        // The read or the hook panicked, for example because `mkfifo` is not installed; nothing blocked.
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err("the read thread panicked".into()),
    }
}
