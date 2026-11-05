//! `contextos-web packages install` given a package folder rather than a bundle, as a real process: the folder is
//! installed as a bundle of the same files would be, a folder holding a link is refused, and the folder itself is
//! left as it was. The host's data directory is redirected to a temporary directory, so nothing here reads or writes
//! the operator's own.
#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::{Command, Output};

#[allow(dead_code)]
#[path = "../src/release_fixture.rs"]
mod release_fixture;

type BoxError = Box<dyn std::error::Error>;

const SCRIPT: &[u8] = b"log_info(\"a\");";

/// A package folder named `acme` 1.0.0 whose entry script and one template sit in sub-folders.
fn write_package(folder: &Path) -> Result<(), BoxError> {
    let template: &[u8] = b"# Shipped\n";
    let (manifest, files) = release_fixture::sketch_release(
        "name = \"acme\"\nversion = \"1.0.0\"\ndigest = \"\"\ntemplates = [\"templates\"]\n\n[[workflow]]\nslug = \"sync\"\nentry = \"scripts/sync.rhai\"\n",
        &[("scripts/sync.rhai", SCRIPT), ("templates/shipped/index.md", template)],
    )?;
    for (path, bytes) in &files {
        let target = folder.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(target, bytes)?;
    }
    std::fs::write(folder.join("manifest.toml"), manifest)?;
    Ok(())
}

fn contextos_web(data: &Path, arguments: &[&str]) -> Result<Output, BoxError> {
    Ok(Command::new(env!("CARGO_BIN_EXE_contextos-web"))
        .args(arguments)
        .env("XDG_DATA_HOME", data)
        .env("HOME", data)
        .env_remove("CONTEXTOS_WEB_CONFIG")
        .output()?)
}

#[test]
fn a_package_folder_installs_with_its_sub_folders_and_is_left_unchanged() -> Result<(), BoxError> {
    let root = tempfile::tempdir()?;
    let data = root.path().join("data");
    let folder = root.path().join("acme");
    write_package(&folder)?;

    let output = contextos_web(&data, &["packages", "install", "--shared", &folder.to_string_lossy()])?;

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8(output.stdout)?, "Installed acme 1.0.0.\n");
    let installed = data.join("contextos/packages/acme/1.0.0");
    assert_eq!(std::fs::read(installed.join("sync/scripts/sync.rhai"))?, SCRIPT);
    assert_eq!(
        std::fs::read(installed.join("templates/shipped/index.md"))?,
        b"# Shipped\n"
    );
    assert_eq!(std::fs::read(folder.join("sync/scripts/sync.rhai"))?, SCRIPT);

    let listed = contextos_web(&data, &["packages", "list", "--shared"])?;
    assert!(
        String::from_utf8(listed.stdout)?
            .lines()
            .any(|line| line == "acme 1.0.0")
    );
    Ok(())
}

#[test]
fn a_package_folder_holding_a_link_is_refused_and_nothing_is_installed() -> Result<(), BoxError> {
    let root = tempfile::tempdir()?;
    let data = root.path().join("data");
    let folder = root.path().join("acme");
    write_package(&folder)?;
    std::os::unix::fs::symlink(
        folder.join("sync/scripts/sync.rhai"),
        folder.join("templates/shipped/linked.md"),
    )?;

    let output = contextos_web(&data, &["packages", "install", "--shared", &folder.to_string_lossy()])?;

    assert!(!output.status.success());
    let refusal = String::from_utf8(output.stdout)?;
    assert!(refusal.starts_with("refused packages/"), "{refusal}");
    assert!(refusal.contains("templates/shipped/linked.md"), "{refusal}");
    let listed = contextos_web(&data, &["packages", "list", "--shared"])?;
    assert!(String::from_utf8(listed.stdout)?.ends_with("No packages installed.\n"));
    Ok(())
}

#[test]
fn removing_or_inspecting_a_name_that_could_leave_the_store_is_refused_and_nothing_outside_it_is_touched()
-> Result<(), BoxError> {
    let root = tempfile::tempdir()?;
    let data = root.path().join("data");
    let victim = data.join("contextos/victim/1.0.0");
    std::fs::create_dir_all(data.join("contextos/packages"))?;
    std::fs::create_dir_all(&victim)?;
    std::fs::write(victim.join("manifest.toml"), "not a package")?;

    for action in ["remove", "inspect"] {
        let output = contextos_web(&data, &["packages", action, "--shared", "../victim", "1.0.0"])?;

        assert!(!output.status.success(), "{action} accepted the name");
        assert!(
            String::from_utf8_lossy(&output.stdout).starts_with("refused packages/invalid-name: "),
            "{action}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(
        victim.join("manifest.toml").exists(),
        "a folder outside the store was removed"
    );
    Ok(())
}
