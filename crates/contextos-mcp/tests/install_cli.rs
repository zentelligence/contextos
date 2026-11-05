//! `contextos install` through the real binary. An install needs the real release server, so these stop at the
//! manifest address, which is refused before anything is fetched or written.

use std::process::Command;

use tempfile::tempdir;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

// People installing ContextOS should not have to find and type a version number.
#[test]
fn install_needs_no_version() -> Result<(), BoxError> {
    let directory = tempdir()?;

    let output = Command::new(env!("CARGO_BIN_EXE_contextos"))
        .arg("install")
        .env("HOME", directory.path())
        .env("USERPROFILE", directory.path())
        .env("CONTEXTOS_INSTALL_ROOT", directory.path().join("root"))
        .env("CONTEXTOS_UPDATE_URL", "http://127.0.0.1:9/latest.json")
        .output()?;

    let stderr = String::from_utf8(output.stderr)?;
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("is not an HTTPS url"), "{stderr}");
    Ok(())
}
