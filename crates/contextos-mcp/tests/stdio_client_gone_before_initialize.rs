//! A stdio client that goes away before it initialises the session has ended the conversation, the same as one that
//! closes its input later on: the server stops cleanly, with a success status and no error report.

use std::process::{Command, Stdio};

type BoxError = Box<dyn std::error::Error>;

#[test]
fn a_client_that_closes_stdio_before_initialising_stops_the_server_cleanly() -> Result<(), BoxError> {
    let home = tempfile::tempdir()?;
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    let output = Command::new(env!("CARGO_BIN_EXE_contextos"))
        .args(["--vault", vault.path().to_str().ok_or("non-UTF-8 vault")?])
        .stdin(Stdio::null())
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("CONTEXTOS_MCP_CONFIG")
        .output()?;
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!stderr.contains("Error:"), "the server reported an error: {stderr}");
    assert!(
        output.status.success(),
        "the server exited with {}: {stderr}",
        output.status
    );
    Ok(())
}
