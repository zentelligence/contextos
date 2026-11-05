//! The `contextos service` command through the real binary, on a home and configuration directory that are empty
//! temporary folders, and the process record the process serving HTTP keeps beside its configuration.

#[cfg(unix)]
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};

use tempfile::tempdir;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A `contextos` command whose home, configuration, and data directories are `directory`, so nothing of the
/// operator's is read or registered.
fn contextos(directory: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_contextos"));
    command
        .env("HOME", directory)
        .env("USERPROFILE", directory)
        .env("XDG_CONFIG_HOME", directory.join("xdg-config"))
        .env("XDG_DATA_HOME", directory.join("xdg-data"))
        .env_remove("CONTEXTOS_MCP_CONFIG")
        .env_remove("CONTEXTOS_MCP_TOKEN")
        .env_remove("CONTEXTOS_MCP_LOG_LEVEL");
    command
}

/// Whether anything was written under `directory` other than the files the test put there.
fn written_nothing(directory: &Path, expected: &[&str]) -> Result<bool, BoxError> {
    let mut names = std::fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<Result<Vec<_>, _>>()?;
    names.sort();
    Ok(names == expected)
}

// Windows `status` reads the real registry through PowerShell, which a test must not do.
#[cfg(not(windows))]
#[test]
fn status_with_nothing_installed_says_so_and_registers_nothing() -> Result<(), BoxError> {
    let directory = tempdir()?;

    let output = contextos(directory.path())
        .args(["service", "status"])
        .stdin(Stdio::null())
        .output()?;

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(
        String::from_utf8(output.stdout)?.trim(),
        "The contextos service is not installed."
    );
    assert!(written_nothing(directory.path(), &[])?);
    Ok(())
}

#[test]
fn install_refuses_a_configuration_that_cannot_load_and_registers_nothing() -> Result<(), BoxError> {
    let directory = tempdir()?;
    std::fs::write(directory.path().join("config.toml"), "this is not = [valid toml")?;

    let output = contextos(directory.path())
        .arg("--config")
        .arg(directory.path().join("config.toml"))
        .args(["service", "install"])
        .stdin(Stdio::null())
        .output()?;
    let stderr = String::from_utf8(output.stderr)?;

    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("was not installed"), "{stderr}");
    assert!(written_nothing(directory.path(), &["config.toml"])?);
    Ok(())
}

#[test]
fn install_refuses_an_address_that_needs_a_token_and_registers_nothing() -> Result<(), BoxError> {
    let directory = tempdir()?;
    std::fs::write(directory.path().join("config.toml"), "")?;

    let output = contextos(directory.path())
        .arg("--config")
        .arg(directory.path().join("config.toml"))
        .args(["--http", "0.0.0.0:3737", "service", "install"])
        .stdin(Stdio::null())
        .output()?;
    let stderr = String::from_utf8(output.stderr)?;

    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("0.0.0.0:3737"), "{stderr}");
    assert!(written_nothing(directory.path(), &["config.toml"])?);
    Ok(())
}

#[cfg(unix)]
#[test]
fn the_process_serving_http_keeps_a_record_beside_its_configuration_until_it_stops_cleanly() -> Result<(), BoxError> {
    let directory = tempdir()?;
    let record = directory.path().join("config.toml.process.json");
    let mut child = contextos(directory.path())
        .arg("--config")
        .arg(directory.path().join("config.toml"))
        .args(["--http", "127.0.0.1:0", "--no-stdio"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let stderr = child.stderr.take().ok_or("standard error was not captured")?;
    let mut lines = BufReader::new(stderr).lines();
    let mut seen = String::new();
    let serving = lines.by_ref().any(|line| {
        let line = line.unwrap_or_default();
        seen.push_str(&line);
        line.contains("starting ContextOS MCP HTTP server")
    });
    std::thread::spawn(move || lines.for_each(drop));
    assert!(serving, "contextos ended without serving HTTP; it logged:\n{seen}");

    let written = std::fs::read_to_string(&record);
    let terminated = Command::new("kill").arg(child.id().to_string()).status()?;
    let exit = child.wait()?;

    let written = written?;
    assert!(written.contains(&format!("\"pid\":{}", child.id())), "{written}");
    assert!(terminated.success());
    assert!(exit.success(), "{exit:?}");
    assert!(!record.exists(), "a clean stop leaves no record");
    Ok(())
}

#[test]
fn a_stdio_only_process_never_writes_a_record() -> Result<(), BoxError> {
    let directory = tempdir()?;

    let status = contextos(directory.path())
        .arg("--config")
        .arg(directory.path().join("config.toml"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;

    assert!(
        !directory.path().join("config.toml.process.json").exists(),
        "{status:?}"
    );
    Ok(())
}
