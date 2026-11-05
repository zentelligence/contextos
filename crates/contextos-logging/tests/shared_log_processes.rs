//! Real processes appending to one rotating log: the test binary re-invokes
//! itself with an environment variable naming the log, so each child owns its
//! own handles exactly as separate `contextos` processes do.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use contextos_logging::{LogRotationPolicy, RotatingLog, SystemClock};

const LOG_ENV: &str = "CONTEXTOS_LOGGING_TEST_SHARED_LOG";
const WRITER_ENV: &str = "CONTEXTOS_LOGGING_TEST_WRITER";
const WRITERS: usize = 4;
const LINES_PER_WRITER: usize = 400;
const MAX_SIZE: u64 = 4096;
/// Every line is exactly this many bytes, newline included.
const LINE_LEN: u64 = 32;

fn line(writer: usize, number: usize) -> String {
    format!("w{writer:02}-n{number:05}-{}\n", "x".repeat(32 - 11 - 1))
}

/// Child role: write this process's lines through its own `RotatingLog`.
/// A no-op in the parent test run, where the environment variable is unset.
#[test]
fn child_writer() -> Result<(), Box<dyn std::error::Error>> {
    let (Ok(log), Ok(writer)) = (std::env::var(LOG_ENV), std::env::var(WRITER_ENV)) else {
        return Ok(());
    };
    let writer: usize = writer.parse()?;
    let policy = LogRotationPolicy {
        max_size_bytes: Some(MAX_SIZE),
        ..LogRotationPolicy::default()
    };
    let mut rotating = RotatingLog::open(PathBuf::from(log), policy, SystemClock)?;
    for number in 0..LINES_PER_WRITER {
        rotating.write_all(line(writer, number).as_bytes())?;
    }
    Ok(())
}

fn log_files(directory: &Path) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.starts_with("contextos.log") {
            files.push(path);
        }
    }
    Ok(files)
}

#[test]
fn several_processes_share_one_log_without_losing_lines_or_overshooting_the_size_limit()
-> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(line(0, 0).len() as u64, LINE_LEN);
    let directory = tempfile::tempdir()?;
    let log = directory.path().join("contextos.log");
    let executable = std::env::current_exe()?;

    let mut children = Vec::new();
    for writer in 0..WRITERS {
        children.push(
            Command::new(&executable)
                .args(["--exact", "child_writer", "--test-threads=1"])
                .env(LOG_ENV, &log)
                .env(WRITER_ENV, writer.to_string())
                .spawn()?,
        );
    }
    for mut child in children {
        assert!(child.wait()?.success(), "a writer process failed");
    }

    let files = log_files(directory.path())?;
    let mut lines = Vec::new();
    for file in &files {
        let content = std::fs::read_to_string(file)?;
        // Concurrent writers may each pass one size check before writing, so a
        // file can overshoot by up to one line per writer, never more.
        let ceiling = MAX_SIZE + LINE_LEN * WRITERS as u64;
        assert!(
            content.len() as u64 <= ceiling,
            "{} is {} bytes, over the {ceiling} byte ceiling",
            file.display(),
            content.len()
        );
        lines.extend(content.lines().map(|line| format!("{line}\n")));
    }
    lines.sort();
    let mut expected: Vec<String> = (0..WRITERS)
        .flat_map(|writer| (0..LINES_PER_WRITER).map(move |number| line(writer, number)))
        .collect();
    expected.sort();
    assert_eq!(lines, expected, "every line appears exactly once across all files");

    // One rotation per limit crossing: a duplicate rotation would leave a
    // rotated file far below the limit. Only the live file may be small.
    let rotated: Vec<&PathBuf> = files.iter().filter(|file| **file != log).collect();
    assert!(
        !rotated.is_empty(),
        "the volume written must have rotated at least once"
    );
    for file in rotated {
        let len = std::fs::metadata(file)?.len();
        assert!(
            len > MAX_SIZE - LINE_LEN,
            "{} holds only {len} bytes: it was rotated before reaching the limit",
            file.display()
        );
    }
    Ok(())
}
