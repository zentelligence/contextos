//! What `contextos --version` prints, which people read and the updater's install step parses.

use std::path::Path;
use std::process::Command;

use updater_engine::{ExecutableVersionProbe, SpawnVersionProbe};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[test]
fn the_version_flag_prints_the_product_name_and_the_bare_version() -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new(env!("CARGO_BIN_EXE_contextos"))
        .arg("--version")
        .output()?;

    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout)?, format!("ContextOS {VERSION}\n"));
    Ok(())
}

// An install refuses a release set whose executables do not report its version, so a version the
// updater cannot read makes every release uninstallable, whatever its signature and checksums say.
#[test]
fn the_updater_reads_the_version_the_binary_reports() -> Result<(), Box<dyn std::error::Error>> {
    let reported = SpawnVersionProbe.probe_version("contextos", Path::new(env!("CARGO_BIN_EXE_contextos")))?;

    assert_eq!(reported.to_string(), VERSION);
    Ok(())
}

// Installing, updating, and removing ContextOS belong to `contextos-launcher`.
#[test]
fn install_and_update_are_not_commands_of_this_program() -> Result<(), Box<dyn std::error::Error>> {
    for command in ["install", "update", "rollback"] {
        let output = Command::new(env!("CARGO_BIN_EXE_contextos")).arg(command).output()?;

        assert_eq!(output.status.code(), Some(2), "{command}");
    }
    Ok(())
}
