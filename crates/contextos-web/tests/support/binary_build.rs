//! Building the real `contextos` binary the integration tests start, once per test process, and saying why when it fails.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// What building the binary came to, kept for the whole process: every test of a process shares one build and one answer.
pub type Built = OnceLock<Result<PathBuf, String>>;

/// How cargo is started: the program, and any arguments that come before the ones that ask for the build.
#[derive(Clone, Copy)]
pub struct Cargo<'a> {
    pub program: &'a str,
    pub leading: &'a [&'a str],
}

/// The binary `cargo` builds for `contextos-mcp`, in the profile `release` names, under `target`.
pub fn build(cargo: Cargo<'_>, release: bool, target: &Path) -> Result<PathBuf, String> {
    let mut build = std::process::Command::new(cargo.program);
    build.args(cargo.leading);
    build.args(["build", "-p", "contextos-mcp", "--bin", "contextos"]);
    if release {
        build.arg("--release");
    }
    let output = build.output().map_err(|error| {
        format!(
            "building the contextos-mcp binary failed: {} could not be started: {error}",
            cargo.program
        )
    })?;
    if !output.status.success() {
        // What cargo said last is what explains a failure: the lock it waited for, the crate that did not compile.
        let said = String::from_utf8_lossy(&output.stderr);
        let tail: Vec<&str> = said.lines().rev().take(20).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        return Err(format!(
            "building the contextos-mcp binary failed ({}): {}",
            output.status,
            tail.join("\n")
        ));
    }
    let profile = if release { "release" } else { "debug" };
    let path = target
        .join(profile)
        .join(format!("contextos{}", std::env::consts::EXE_SUFFIX));
    if !path.exists() {
        return Err(format!("expected the contextos binary at {}", path.display()));
    }
    Ok(path)
}

/// [`build`], run the first time it is asked for in `built` and remembered after, so tests running side by side start one
/// build between them and not one each.
pub fn built_once(built: &Built, cargo: Cargo<'_>, release: bool, target: &Path) -> Result<PathBuf, String> {
    built.get_or_init(|| build(cargo, release, target)).clone()
}
