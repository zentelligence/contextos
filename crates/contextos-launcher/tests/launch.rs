//! The real launcher, copied under a public command's name, starting a program from an active release set. The
//! programs are shell scripts, so these run where a script is an executable.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::{TempDir, tempdir};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const RELEASE: &str = r#"{
  "schema_version": 1,
  "product": "contextos",
  "version": "1.0.0",
  "target": "x86_64-unknown-linux-gnu",
  "published_at": "2026-01-01T00:00:00Z",
  "minimum_launcher_version": "0.1.0",
  "commands": [{ "name": "contextos", "path": "bin/contextos" }]
}"#;

/// An installation whose active release's `contextos` is `script`, and the launcher copied to `path/contextos`.
fn installed(script: &str) -> Result<(TempDir, PathBuf), BoxError> {
    let directory = tempdir()?;
    let release = directory.path().join("root").join("versions").join("1.0.0");
    fs::create_dir_all(release.join("bin"))?;
    fs::write(release.join("release.json"), RELEASE)?;
    let program = release.join("bin").join("contextos");
    fs::write(&program, format!("#!/bin/sh\n{script}\n"))?;
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755))?;
    symlink("versions/1.0.0", directory.path().join("root").join("current"))?;

    let launcher = directory.path().join("path").join("contextos");
    fs::create_dir_all(directory.path().join("path"))?;
    fs::copy(env!("CARGO_BIN_EXE_contextos-launcher"), &launcher)?;
    Ok((directory, launcher))
}

fn launch(root: &Path, launcher: &Path) -> Result<(u32, Output), BoxError> {
    let child = Command::new(launcher)
        .env("CONTEXTOS_INSTALL_ROOT", root.join("root"))
        .env_remove("CONTEXTOS_LAUNCHER")
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    let id = child.id();
    Ok((id, child.wait_with_output()?))
}

// A service or MCP host registration records this path, so it follows every later update.
#[test]
fn the_started_program_is_told_where_its_launcher_is() -> Result<(), BoxError> {
    let (directory, launcher) = installed(r#"printf '%s' "$CONTEXTOS_LAUNCHER""#)?;

    let (_, output) = launch(directory.path(), &launcher)?;

    assert!(output.status.success());
    assert_eq!(Path::new(&String::from_utf8(output.stdout)?), launcher);
    Ok(())
}

// A service manager stops a service by signalling the process it started. If that were a launcher waiting on the
// server, the launcher would die at once and the server would be killed with no chance to shut down cleanly.
#[test]
fn the_started_program_takes_over_the_launchers_process() -> Result<(), BoxError> {
    let (directory, launcher) = installed(r#"printf '%s' "$$""#)?;

    let (id, output) = launch(directory.path(), &launcher)?;

    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout)?, id.to_string());
    Ok(())
}

#[test]
fn the_started_programs_exit_status_is_the_launchers() -> Result<(), BoxError> {
    let (directory, launcher) = installed("exit 7")?;

    let (_, output) = launch(directory.path(), &launcher)?;

    assert_eq!(output.status.code(), Some(7));
    Ok(())
}
