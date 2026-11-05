//! What `contextos-web --version` prints, which people read and the updater's install step parses:
//! the version must be the last word, with nothing attached to it.

use std::process::Command;

#[test]
fn the_version_flag_prints_the_product_name_and_the_bare_version() -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new(env!("CARGO_BIN_EXE_contextos-web"))
        .arg("--version")
        .output()?;

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout)?,
        format!("ContextOS Web {}\n", env!("CARGO_PKG_VERSION"))
    );
    Ok(())
}
