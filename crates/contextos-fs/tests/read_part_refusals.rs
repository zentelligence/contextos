//! The refusals of a part read that do not concern its offset, length, or version: a path that names nothing or
//! names something that is not a file is refused with the same codes a whole read gives.

mod read_support;

use contextos_fs::FsError;
use read_support::{TestResult, Vault};

#[test]
fn a_part_of_a_missing_file_is_refused_as_not_found() -> TestResult {
    let vault = Vault::new()?;
    let path = vault.path("missing.md")?;

    let result = vault.part(&path, 0, 4);

    assert!(matches!(result, Err(FsError::NotFound { .. })), "{result:?}");
    Ok(())
}

#[test]
fn a_part_of_a_directory_is_refused_as_not_a_file() -> TestResult {
    let vault = Vault::new()?;
    std::fs::create_dir(vault.root.join("folder"))?;
    let path = vault.path("folder")?;

    let result = vault.part(&path, 0, 4);

    assert!(matches!(result, Err(FsError::NotFile { .. })), "{result:?}");
    Ok(())
}

/// Runs `read` on its own thread and fails rather than hangs when it does not finish, as opening a pipe with no
/// writer would block for ever.
#[cfg(unix)]
fn finishes_within<T: Send + 'static>(read: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(read());
    });
    receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .map_err(|_| "the read blocked instead of refusing the pipe".to_owned())
}

#[cfg(unix)]
#[test]
fn a_part_and_a_whole_read_of_a_named_pipe_are_refused_without_blocking() -> TestResult {
    let vault = Vault::new()?;
    let status = std::process::Command::new("mkfifo")
        .arg(vault.root.join("pipe.md"))
        .status()?;
    assert!(status.success(), "mkfifo must succeed");
    let path = vault.path("pipe.md")?;

    let (part, whole) = {
        let filesystem = vault.filesystem.clone();
        let part_path = path.clone();
        let part = finishes_within(move || {
            filesystem.read_part(&contextos_fs::ReadPartRequest {
                path: part_path,
                part: contextos_fs::BytePart::from((0, 4)),
                expected_version: None,
            })
        })?;
        let filesystem = vault.filesystem.clone();
        let whole =
            finishes_within(move || filesystem.read_text(&contextos_fs::ReadTextRequest { path, limit: None }))?;
        (part, whole)
    };

    assert!(matches!(part, Err(FsError::NotFile { .. })), "{part:?}");
    assert!(matches!(whole, Err(FsError::NotFile { .. })), "{whole:?}");
    Ok(())
}
