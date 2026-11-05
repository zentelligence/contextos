use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn rejection(result: Result<InstructionsFile, ProjectionError>) -> Result<TargetRootRejection, String> {
    match result {
        Ok(file) => Err(format!("expected a rejection, got {file:?}")),
        Err(ProjectionError::TargetRootRejected { reason }) => Ok(reason),
        Err(other) => Err(format!("expected a target rejection, got {other}")),
    }
}

#[test]
fn a_relative_instructions_path_is_rejected() -> TestResult {
    assert_eq!(
        rejection(InstructionsFile::try_from(Path::new(".claude/CLAUDE.md")))?,
        TargetRootRejection::NotAbsolute
    );
    Ok(())
}

#[test]
fn an_instructions_path_without_a_file_name_is_rejected() -> TestResult {
    let temporary = std::env::temp_dir();
    let drive_root = temporary.ancestors().last().ok_or("a path has an ancestor")?;
    assert_eq!(
        rejection(InstructionsFile::try_from(drive_root))?,
        TargetRootRejection::NoFileName
    );
    Ok(())
}

#[test]
fn an_instructions_path_naming_a_folder_is_rejected() -> TestResult {
    let dir = tempfile::tempdir()?;
    assert_eq!(
        rejection(InstructionsFile::try_from(dir.path()))?,
        TargetRootRejection::NotFile
    );
    Ok(())
}

#[test]
fn a_missing_file_in_an_existing_folder_is_accepted_and_resolves_to_itself() -> TestResult {
    let dir = tempfile::tempdir()?;
    let file = InstructionsFile::try_from(dir.path().join("CLAUDE.md").as_path())?;
    assert_eq!(file.resolved(), dunce::canonicalize(dir.path())?.join("CLAUDE.md"));
    Ok(())
}

#[test]
fn a_missing_folder_is_accepted_without_being_created() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join(".codex/AGENTS.md");
    let file = InstructionsFile::try_from(path.as_path())?;
    assert_eq!(
        file.resolved(),
        dunce::canonicalize(dir.path())?.join(".codex").join("AGENTS.md")
    );
    assert!(!dir.path().join(".codex").exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn an_operator_made_link_is_resolved_once_to_the_real_file_and_the_configured_path_is_kept() -> TestResult {
    let dir = tempfile::tempdir()?;
    let dotfiles = tempfile::tempdir()?;
    let real = dotfiles.path().join("claude.md");
    std::fs::write(&real, "x")?;
    let link = dir.path().join("CLAUDE.md");
    std::os::unix::fs::symlink(&real, &link)?;
    let file = InstructionsFile::try_from(link.as_path())?;
    assert_eq!(file.resolved(), real.canonicalize()?);
    assert_eq!(<&Path>::from(&file), link.as_path());
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_link_to_nowhere_is_refused_rather_than_replaced() -> TestResult {
    let dir = tempfile::tempdir()?;
    let link = dir.path().join("CLAUDE.md");
    std::os::unix::fs::symlink(dir.path().join("missing.md"), &link)?;
    assert!(matches!(
        InstructionsFile::try_from(link.as_path()),
        Err(ProjectionError::TargetRootUnresolvable { .. })
    ));
    Ok(())
}

#[cfg(unix)]
#[test]
fn an_instructions_file_re_linked_to_another_file_after_start_is_refused_at_the_next_write() -> TestResult {
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("CLAUDE.md");
    std::fs::write(&file, "mine\n")?;
    let instructions = InstructionsFile::try_from(file.as_path())?;
    let elsewhere = dir.path().join("vault-file.md");
    std::fs::write(&elsewhere, "vault content\n")?;
    std::fs::remove_file(&file)?;
    std::os::unix::fs::symlink(&elsewhere, &file)?;

    let error = instructions
        .write(b"orientation\n")
        .err()
        .ok_or("the write was not refused")?;

    assert_eq!(error.code(), "projection/path-rejected", "{error:?}");
    assert_eq!(std::fs::read(&elsewhere)?, b"vault content\n");
    assert!(
        std::fs::symlink_metadata(&file)?.file_type().is_symlink(),
        "the re-pointed link was replaced"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn an_operator_made_link_to_the_instructions_file_is_written_through_but_refused_once_re_linked() -> TestResult {
    let dir = tempfile::tempdir()?;
    let dotfiles = dir.path().join("dotfiles-CLAUDE.md");
    std::fs::write(&dotfiles, "mine\n")?;
    let link = dir.path().join("CLAUDE.md");
    std::os::unix::fs::symlink(&dotfiles, &link)?;
    let instructions = InstructionsFile::try_from(link.as_path())?;
    instructions.write(b"first\n")?;
    assert_eq!(std::fs::read(&dotfiles)?, b"first\n");
    assert!(std::fs::symlink_metadata(&link)?.file_type().is_symlink());

    let elsewhere = dir.path().join("vault-file.md");
    std::fs::write(&elsewhere, "vault content\n")?;
    std::fs::remove_file(&link)?;
    std::os::unix::fs::symlink(&elsewhere, &link)?;
    let error = instructions
        .write(b"second\n")
        .err()
        .ok_or("the write was not refused")?;

    assert_eq!(error.code(), "projection/path-rejected", "{error:?}");
    assert_eq!(std::fs::read(&elsewhere)?, b"vault content\n");
    assert_eq!(std::fs::read(&dotfiles)?, b"first\n");
    Ok(())
}

/// Runs `body` on its own thread and fails, rather than hangs, when it does not finish in time.
#[cfg(unix)]
fn within_a_bounded_wait<T: Send + 'static>(
    body: impl FnOnce() -> T + Send + 'static,
    release: impl FnOnce(),
) -> Result<T, String> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(body());
    });
    receiver.recv_timeout(std::time::Duration::from_secs(5)).map_err(|_| {
        release();
        "the instructions file was opened and blocked".to_owned()
    })
}

/// Reads the instructions file at `link`, returning the stable code of the refusal, or a description of what happened.
#[cfg(unix)]
fn refusal_code(link: &Path, release: impl FnOnce()) -> Result<&'static str, String> {
    let link = link.to_path_buf();
    within_a_bounded_wait(
        move || match InstructionsFile::try_from(link.as_path()) {
            Err(error) => Ok(error.code()),
            Ok(file) => match file.read() {
                Err(error) => Ok(error.code()),
                Ok(contents) => Err(format!("the read was allowed and returned {contents:?}")),
            },
        },
        release,
    )?
}

#[cfg(unix)]
#[test]
fn an_instructions_link_to_a_fifo_is_refused_and_never_opened() -> TestResult {
    let dir = tempfile::tempdir()?;
    let fifo = dir.path().join("pipe");
    let made = std::process::Command::new("mkfifo").arg(&fifo).status()?;
    assert!(made.success(), "mkfifo failed");
    let link = dir.path().join("CLAUDE.md");
    std::os::unix::fs::symlink(&fifo, &link)?;
    let unblock = fifo.clone();

    // A regression blocks in `open`; opening the write end afterwards releases that thread so the test can end.
    let code = refusal_code(&link, move || {
        let _ = std::fs::OpenOptions::new().write(true).open(unblock);
    })?;

    assert_eq!(code, "projection/path-rejected");
    Ok(())
}

#[cfg(unix)]
#[test]
fn an_instructions_link_to_a_socket_is_refused() -> TestResult {
    let dir = tempfile::tempdir()?;
    let socket = dir.path().join("sock");
    let _listener = std::os::unix::net::UnixListener::bind(&socket)?;
    let link = dir.path().join("CLAUDE.md");
    std::os::unix::fs::symlink(&socket, &link)?;

    let code = refusal_code(&link, || {})?;

    assert_eq!(code, "projection/path-rejected");
    Ok(())
}

#[cfg(unix)]
#[test]
fn an_instructions_link_to_a_device_is_refused() -> TestResult {
    let dir = tempfile::tempdir()?;
    let link = dir.path().join("CLAUDE.md");
    std::os::unix::fs::symlink("/dev/null", &link)?;

    let code = refusal_code(&link, || {})?;

    assert_eq!(code, "projection/path-rejected");
    Ok(())
}

#[cfg(unix)]
#[test]
fn an_instructions_link_to_a_regular_file_is_still_read_through() -> TestResult {
    let dir = tempfile::tempdir()?;
    let real = dir.path().join("dotfiles-claude.md");
    std::fs::write(&real, "my rules\n")?;
    let link = dir.path().join("CLAUDE.md");
    std::os::unix::fs::symlink(&real, &link)?;

    let file = InstructionsFile::try_from(link.as_path())?;

    assert_eq!(file.read()?, Some(b"my rules\n".to_vec()));
    Ok(())
}

#[cfg(unix)]
#[test]
fn an_instructions_link_swapped_to_a_fifo_after_start_is_refused_and_never_opened() -> TestResult {
    let dir = tempfile::tempdir()?;
    let real = dir.path().join("dotfiles-claude.md");
    std::fs::write(&real, "my rules\n")?;
    let link = dir.path().join("CLAUDE.md");
    std::os::unix::fs::symlink(&real, &link)?;
    let file = InstructionsFile::try_from(link.as_path())?;
    let fifo = dir.path().join("pipe");
    assert!(std::process::Command::new("mkfifo").arg(&fifo).status()?.success());
    std::fs::remove_file(&link)?;
    std::os::unix::fs::symlink(&fifo, &link)?;
    let unblock = fifo.clone();

    let outcome = within_a_bounded_wait(
        move || file.read().map(|_| ()).map_err(|error| error.code()),
        move || {
            let _ = std::fs::OpenOptions::new().write(true).open(unblock);
        },
    )?;

    assert!(outcome.is_err(), "the swapped link was read");
    Ok(())
}
