use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn a_regular_file_is_read_whole() -> TestResult {
    let directory = tempfile::tempdir()?;
    let file = directory.path().join("note.md");
    std::fs::write(&file, "text")?;

    assert_eq!(read_regular_file(&file)?, b"text");
    assert_eq!(read_regular_text(&file)?, "text");
    Ok(())
}

#[test]
fn a_size_no_buffer_can_hold_is_an_out_of_memory_error_rather_than_a_panic() {
    // A sparse file can claim any size without using disk, so the size must never be trusted to allocate.
    let error = buffer_for(u64::MAX).err();

    assert_eq!(error.map(|error| error.kind()), Some(io::ErrorKind::OutOfMemory));
}

#[test]
fn a_missing_file_keeps_the_not_found_kind() -> TestResult {
    let directory = tempfile::tempdir()?;

    let error = read_regular_text(&directory.path().join("missing.md"))
        .err()
        .ok_or("must fail")?;

    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert!(!is_not_regular_file(&error));
    Ok(())
}

#[test]
fn text_that_is_not_utf8_is_invalid_data() -> TestResult {
    let directory = tempfile::tempdir()?;
    let file = directory.path().join("note.md");
    std::fs::write(&file, [0xFF, 0xFE])?;

    let error = read_regular_text(&file).err().ok_or("must fail")?;

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_directory_is_not_a_regular_file() -> TestResult {
    let directory = tempfile::tempdir()?;

    let error = read_regular_file(directory.path()).err().ok_or("must fail")?;

    assert!(is_not_regular_file(&error), "{error:?}");
    Ok(())
}

#[cfg(unix)]
mod named_pipes {
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::Duration;

    use super::*;

    fn make_pipe(path: &Path) -> TestResult {
        if !std::process::Command::new("mkfifo").arg(path).status()?.success() {
            return Err("mkfifo must succeed".into());
        }
        Ok(())
    }

    /// Runs `open` on its own thread and fails rather than hangs when it does not finish. On a timeout it opens the
    /// pipe from both ends on threads of their own, which releases a reader or writer blocked on it.
    fn finishes_within<T: Send + 'static>(
        pipe: PathBuf,
        open: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, Box<dyn std::error::Error>> {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(open());
        });
        match receiver.recv_timeout(Duration::from_secs(10)) {
            Ok(value) => Ok(value),
            Err(RecvTimeoutError::Timeout) => {
                let writer = pipe.clone();
                std::thread::spawn(move || OpenOptions::new().write(true).open(writer));
                std::thread::spawn(move || File::open(pipe));
                Err("the open blocked on a named pipe instead of refusing it".into())
            }
            Err(RecvTimeoutError::Disconnected) => Err("the open thread panicked".into()),
        }
    }

    #[test]
    fn reading_a_named_pipe_is_refused_without_blocking() -> TestResult {
        let directory = tempfile::tempdir()?;
        let pipe = directory.path().join("note.md");
        make_pipe(&pipe)?;
        let read = pipe.clone();

        let result = finishes_within(pipe, move || read_regular_text(&read))?;

        assert!(result.as_ref().is_err_and(is_not_regular_file), "{result:?}");
        Ok(())
    }

    #[test]
    fn opening_a_named_pipe_with_no_reader_for_writing_is_refused_without_blocking() -> TestResult {
        let directory = tempfile::tempdir()?;
        let pipe = directory.path().join("log.md");
        make_pipe(&pipe)?;
        let opened = pipe.clone();

        let result = finishes_within(pipe, move || {
            open_regular_file(&opened, OpenOptions::new().append(true)).map(|_| ())
        })?;

        assert!(result.as_ref().is_err_and(is_not_regular_file), "{result:?}");
        Ok(())
    }

    #[test]
    fn opening_a_named_pipe_that_has_a_reader_for_writing_is_refused() -> TestResult {
        let directory = tempfile::tempdir()?;
        let pipe = directory.path().join("log.md");
        make_pipe(&pipe)?;
        let _reader = open_without_waiting(&pipe, OpenOptions::new().read(true))?;
        let opened = pipe.clone();

        let result = finishes_within(pipe, move || {
            open_regular_file(&opened, OpenOptions::new().append(true)).map(|_| ())
        })?;

        assert!(result.as_ref().is_err_and(is_not_regular_file), "{result:?}");
        Ok(())
    }
}
