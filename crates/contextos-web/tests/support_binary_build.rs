//! How the integration tests get the real `contextos` binary: one build per test process, and a failure that says why.
//! A shell stands in for `cargo` here and its exit is read in Unix terms, so these run on Unix hosts only.

#![cfg(unix)]

mod support;

use std::path::Path;

use support::BoxError;
use support::binary_build::{Built, Cargo, built_once};

/// Asks for a build with the shell standing in for `cargo`, running `body` and given cargo's arguments as its own.
fn built_by_script(built: &Built, body: &str) -> Result<std::path::PathBuf, String> {
    let cargo = Cargo {
        program: "sh",
        leading: &["-c", body, "cargo"],
    };
    built_once(built, cargo, false, Path::new("/nonexistent"))
}

#[test]
fn a_build_that_fails_says_how_it_ended_and_what_cargo_said() -> Result<(), BoxError> {
    let built = built_by_script(&Built::new(), "echo 'error: a lock could not be taken' >&2; exit 3");

    let reason = built.err().ok_or("a failed build was taken for a success")?;
    assert!(reason.contains("exit status: 3"), "{reason}");
    assert!(reason.contains("a lock could not be taken"), "{reason}");
    Ok(())
}

#[test]
fn a_build_killed_by_a_signal_says_so() -> Result<(), BoxError> {
    let built = built_by_script(&Built::new(), "kill -9 $$");

    let reason = built.err().ok_or("a killed build was taken for a success")?;
    assert!(reason.contains("signal: 9"), "{reason}");
    Ok(())
}

#[test]
fn tests_running_side_by_side_start_one_build_between_them() -> Result<(), BoxError> {
    let log = tempfile::NamedTempFile::new()?;
    let body = format!("echo built >> {}; exit 0", log.path().display());
    let built = Built::new();

    let results: Vec<_> = (0..4).map(|_| built_by_script(&built, &body)).collect();

    let runs = std::fs::read_to_string(log.path())?.lines().count();
    assert_eq!(runs, 1, "cargo was started {runs} times for {} tests", results.len());
    Ok(())
}
