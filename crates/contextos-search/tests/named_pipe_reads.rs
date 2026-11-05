//! A note path that names a named pipe is never waited on by the embedding worker's source: opening a pipe the usual
//! way waits for a writer, which would hold the worker thread for ever. Like a missing file, it has no content to
//! embed.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use contextos_search::{FilesystemChunkSource, ReadsChunkSource};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn make_pipe(path: &Path) -> TestResult {
    if !std::process::Command::new("mkfifo").arg(path).status()?.success() {
        return Err("mkfifo must succeed".into());
    }
    Ok(())
}

/// Runs `read` on its own thread and fails rather than hangs when it does not finish. On a timeout it opens the pipe
/// from both ends on threads of their own, which releases a reader or writer blocked on it.
fn finishes_within<T: Send + 'static>(
    pipe: PathBuf,
    read: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Box<dyn std::error::Error>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(read());
    });
    match receiver.recv_timeout(Duration::from_secs(10)) {
        Ok(value) => Ok(value),
        Err(RecvTimeoutError::Timeout) => {
            let writer = pipe.clone();
            std::thread::spawn(move || std::fs::OpenOptions::new().write(true).open(writer));
            std::thread::spawn(move || std::fs::File::open(pipe));
            Err("the read blocked on a named pipe instead of refusing it".into())
        }
        Err(RecvTimeoutError::Disconnected) => Err("the read thread panicked".into()),
    }
}

#[test]
fn the_embedding_source_treats_a_named_pipe_as_having_no_content_without_blocking() -> TestResult {
    let vault = tempfile::tempdir()?;
    let pipe = vault.path().join("note.md");
    make_pipe(&pipe)?;
    let source = FilesystemChunkSource::new(vault.path().to_path_buf());

    let result = finishes_within(pipe, move || source.read("note.md"))?;

    assert!(matches!(result, Ok(None)), "{result:?}");
    Ok(())
}
