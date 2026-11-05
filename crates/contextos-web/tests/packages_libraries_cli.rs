//! `contextos-web packages` with library packages, as a real process: install resolves libraries as a run does, `list`
//! shows each package's exports, the libraries it requires, and the workflow releases that hold a library release, and
//! `remove` refuses a library release a workflow release would choose and names that release unless `--force` is given.
//! The host's data directory is redirected to a temporary directory, so nothing here reads or writes the operator's own.
#![cfg(target_os = "linux")]

use std::fmt::Write as _;
use std::path::Path;
use std::process::{Command, Output};

#[allow(dead_code)]
#[path = "../src/release_fixture.rs"]
mod release_fixture;

type BoxError = Box<dyn std::error::Error>;

/// A package folder. A library exports its one module `m` and has no workflow; a package with `workflow` declares one.
fn write_package(
    folder: &Path,
    name: &str,
    version: &str,
    dependencies: &[(&str, &str)],
    workflow: bool,
) -> Result<(), BoxError> {
    std::fs::create_dir_all(folder)?;
    let (file, text): (&str, &[u8]) = if workflow {
        ("sync.rhai", b"()")
    } else {
        ("m.rhai", b"fn m() { 1 }")
    };
    let mut sketch = format!("name = \"{name}\"\nversion = \"{version}\"\ndigest = \"\"\n");
    if !workflow {
        sketch.push_str("modules = [\"m.rhai\"]\nexports = [\"m.rhai\"]\n");
    }
    for (dependency, requirement) in dependencies {
        let _ = write!(
            sketch,
            "\n[[dependencies]]\nname = \"{dependency}\"\nrequirement = \"{requirement}\"\n"
        );
    }
    if workflow {
        sketch.push_str("\n[[workflow]]\nslug = \"sync\"\nentry = \"sync.rhai\"\n");
    }
    let (manifest, files) = release_fixture::sketch_release(&sketch, &[(file, text)])?;
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

struct Host {
    root: tempfile::TempDir,
}

impl Host {
    fn new() -> Result<Self, BoxError> {
        Ok(Self {
            root: tempfile::tempdir()?,
        })
    }

    fn data(&self) -> std::path::PathBuf {
        self.root.path().join("data")
    }

    fn run(&self, arguments: &[&str]) -> Result<Output, BoxError> {
        contextos_web(&self.data(), arguments)
    }

    fn install(
        &self,
        name: &str,
        version: &str,
        dependencies: &[(&str, &str)],
        workflow: bool,
    ) -> Result<Output, BoxError> {
        let folder = self.root.path().join(format!("{name}-{version}"));
        write_package(&folder, name, version, dependencies, workflow)?;
        self.run(&["packages", "install", "--shared", &folder.to_string_lossy()])
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn installing_a_package_whose_library_is_not_installed_is_refused_naming_it() -> Result<(), BoxError> {
    let host = Host::new()?;

    let output = host.install("acme", "1.0.0", &[("acme-text", "^1")], true)?;

    assert!(!output.status.success());
    assert!(stdout(&output).starts_with("refused packages/"), "{}", stdout(&output));
    assert!(stdout(&output).contains("acme-text"), "{}", stdout(&output));
    assert_eq!(
        stdout(&host.run(&["packages", "list", "--shared"])?),
        "Package store shared\nLedger: none yet\nNo packages installed.\n"
    );
    Ok(())
}

#[test]
fn list_shows_each_packages_exports_the_libraries_it_requires_and_who_holds_a_library_release() -> Result<(), BoxError>
{
    let host = Host::new()?;
    assert!(host.install("acme-text", "1.2.0", &[], false)?.status.success());
    assert!(
        host.install("acme", "1.0.0", &[("acme-text", "^1")], true)?
            .status
            .success()
    );

    let listed = host.run(&["packages", "list", "--shared"])?;

    let text = stdout(&listed);
    for line in [
        "acme 1.0.0",
        "    sync: future, no lock yet",
        "    requires acme-text ^1",
        "acme-text 1.2.0",
        "    exports m.rhai",
        "    kept: the first validation of acme 1.0.0 (sync) would choose it",
    ] {
        assert!(text.lines().any(|found| found == line), "missing {line:?} in\n{text}");
    }
    Ok(())
}

#[test]
fn list_reports_a_release_that_cannot_be_read_for_itself_and_still_lists_the_others() -> Result<(), BoxError> {
    let host = Host::new()?;
    assert!(host.install("acme-text", "1.2.0", &[], false)?.status.success());
    assert!(host.install("broken", "1.0.0", &[], false)?.status.success());
    let broken = host.data().join("contextos/packages/broken/1.0.0");
    std::fs::write(broken.join("manifest.toml"), "this is not a manifest")?;

    let listed = host.run(&["packages", "list", "--shared"])?;

    assert!(listed.status.success(), "{}", stderr(&listed));
    let text = stdout(&listed);
    assert!(text.lines().any(|line| line == "acme-text 1.2.0"), "{text}");
    assert!(text.lines().any(|line| line == "broken 1.0.0"), "{text}");
    assert!(text.lines().any(|line| line.starts_with("    damaged: ")), "{text}");
    Ok(())
}

#[test]
fn removing_a_library_release_a_workflow_release_would_choose_is_refused_naming_it_and_force_removes_it()
-> Result<(), BoxError> {
    let host = Host::new()?;
    assert!(host.install("acme-text", "1.2.0", &[], false)?.status.success());
    assert!(
        host.install("acme", "1.0.0", &[("acme-text", "^1")], true)?
            .status
            .success()
    );

    let refused = host.run(&["packages", "remove", "--shared", "acme-text", "1.2.0"])?;

    assert_eq!(refused.status.code(), Some(1));
    let message = stdout(&refused);
    assert!(message.starts_with("refused packages/in-use: "), "{message}");
    assert!(message.contains("acme 1.0.0 (sync)"), "{message}");
    assert!(host.data().join("contextos/packages/acme-text/1.2.0").is_dir());

    let forced = host.run(&["packages", "remove", "--shared", "acme-text", "1.2.0", "--force"])?;

    assert!(forced.status.success(), "{}", stderr(&forced));
    assert_eq!(
        stdout(&forced),
        "Removed acme-text 1.2.0 from shared, forced.\nOverridden: the first validation of acme 1.0.0 (sync) would choose it\n"
    );
    assert!(!host.data().join("contextos/packages/acme-text/1.2.0").exists());
    Ok(())
}

#[test]
fn removing_a_release_nothing_keeps_says_only_that_it_was_removed() -> Result<(), BoxError> {
    let host = Host::new()?;
    assert!(host.install("acme-text", "1.2.0", &[], false)?.status.success());

    let removed = host.run(&["packages", "remove", "--shared", "acme-text", "1.2.0"])?;

    assert_eq!(stdout(&removed), "Removed acme-text 1.2.0 from shared.\n");
    Ok(())
}

#[test]
fn the_remove_help_explains_the_guard_and_force() -> Result<(), BoxError> {
    let host = Host::new()?;

    let help = host.run(&["packages", "remove", "--help"])?;

    let text = stdout(&help);
    assert!(text.contains("--force"), "{text}");
    assert!(text.contains("packages/in-use"), "{text}");
    Ok(())
}
