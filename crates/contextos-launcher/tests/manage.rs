//! `contextos-launcher` under its own name: the program that installs, updates, and removes the suite. A real install
//! needs the real release server, so these stop at the manifest address or start from an installed fixture.

use std::process::Command;

use tempfile::tempdir;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Serialises these tests. They write programs and then run them, and a process started by another test at that moment
/// inherits the open file, which makes the kernel refuse to run it with "Text file busy".
#[cfg(unix)]
static EXECUTABLES: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(unix)]
fn exclusive() -> std::sync::MutexGuard<'static, ()> {
    EXECUTABLES.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn launcher() -> Command {
    Command::new(env!("CARGO_BIN_EXE_contextos-launcher"))
}

#[test]
fn the_version_flag_prints_the_launchers_name_and_the_bare_version() -> Result<(), BoxError> {
    let output = launcher().arg("--version").output()?;

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout)?,
        format!("ContextOS Launcher {}\n", env!("CARGO_PKG_VERSION"))
    );
    Ok(())
}

// People installing ContextOS should not have to find and type a version number.
// A browser saves a second download as `contextos-launcher (1)`, and someone may rename the file for themselves. Only
// the programs' names forward, so such a copy still installs rather than looking for a program of its own name.
#[test]
fn a_renamed_launcher_still_manages_the_installation() -> Result<(), BoxError> {
    #[cfg(unix)]
    let _exclusive = exclusive();
    let directory = tempdir()?;
    for name in ["contextos-launcher (1)", "contextos-launcher-linux", "launcher"] {
        let renamed = directory.path().join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        std::fs::copy(env!("CARGO_BIN_EXE_contextos-launcher"), &renamed)?;

        let output = Command::new(&renamed)
            .arg("--version")
            .env("HOME", directory.path())
            .env("CONTEXTOS_INSTALL_ROOT", directory.path().join("root"))
            .output()?;

        assert!(output.status.success(), "{name}: {}", String::from_utf8_lossy(&output.stderr));
        let expected = format!("ContextOS Launcher {}\n", env!("CARGO_PKG_VERSION"));
        assert_eq!(String::from_utf8(output.stdout)?, expected, "{name}");
    }
    Ok(())
}

#[test]
fn install_needs_no_version() -> Result<(), BoxError> {
    let directory = tempdir()?;

    let output = launcher()
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

#[test]
fn update_check_says_nothing_is_changed_when_the_manifest_cannot_be_reached() -> Result<(), BoxError> {
    let directory = tempdir()?;

    let output = launcher()
        .args(["update", "check"])
        .env("HOME", directory.path())
        .env("USERPROFILE", directory.path())
        .env("CONTEXTOS_INSTALL_ROOT", directory.path().join("root"))
        .env("CONTEXTOS_UPDATE_URL", "http://127.0.0.1:9/latest.json")
        .output()?;

    assert_eq!(output.status.code(), Some(1));
    assert!(!directory.path().join("root").join("current").exists());
    Ok(())
}

#[test]
fn uninstall_with_nothing_installed_says_so() -> Result<(), BoxError> {
    let directory = tempdir()?;

    let output = launcher()
        .args(["uninstall", "--yes"])
        .env("HOME", directory.path())
        .env("USERPROFILE", directory.path())
        .env("CONTEXTOS_INSTALL_ROOT", directory.path().join("root"))
        .output()?;

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8(output.stdout)?.contains("not installed"));
    Ok(())
}

#[cfg(unix)]
mod installed {
    use std::fmt::Write;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    use sha2::{Digest, Sha256};
    use tempfile::{TempDir, tempdir};

    use super::{BoxError, exclusive, launcher};

    const RELEASE: &str = r#"{
      "schema_version": 1,
      "product": "contextos",
      "version": "1.0.0",
      "target": "x86_64-unknown-linux-gnu",
      "published_at": "2026-01-01T00:00:00Z",
      "minimum_launcher_version": "0.1.0",
      "commands": [
        { "name": "contextos", "path": "bin/contextos" },
        { "name": "contextos-web", "path": "bin/contextos-web" }
      ]
    }"#;
    const PROFILE_BLOCK: &str = "\n# Added by ContextOS\nexport PATH=\"$HOME/.local/bin:$PATH\"\n";

    struct Fixture {
        directory: TempDir,
    }

    impl Fixture {
        fn home(&self) -> PathBuf {
            self.directory.path().join("home")
        }

        fn root(&self) -> PathBuf {
            self.directory.path().join("root")
        }

        fn commands(&self) -> PathBuf {
            self.home().join(".local").join("bin")
        }

        fn calls(&self) -> String {
            fs::read_to_string(self.directory.path().join("calls")).unwrap_or_default()
        }

        fn run(&self, command: &mut Command) -> Result<Output, BoxError> {
            Ok(command
                .env("HOME", self.home())
                .env("SHELL", "/bin/zsh")
                .env("CONTEXTOS_INSTALL_ROOT", self.root())
                .output()?)
        }
    }

    fn digest(bytes: &[u8]) -> String {
        Sha256::digest(bytes).iter().fold(String::new(), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        })
    }

    /// A complete installation: a release whose programs log their arguments, the launcher's copies, the profile
    /// line, and the record of what the installer placed.
    fn fixture() -> Result<Fixture, BoxError> {
        let directory = tempdir()?;
        let fixture = Fixture { directory };
        let release = fixture.root().join("versions").join("1.0.0");
        fs::create_dir_all(release.join("bin"))?;
        fs::write(release.join("release.json"), RELEASE)?;
        let calls = fixture.directory.path().join("calls");
        for name in ["contextos", "contextos-web"] {
            let program = release.join("bin").join(name);
            fs::write(
                &program,
                format!("#!/bin/sh\necho \"{name} $*\" >> '{}'\n", calls.display()),
            )?;
            fs::set_permissions(&program, fs::Permissions::from_mode(0o755))?;
        }
        symlink("versions/1.0.0", fixture.root().join("current"))?;

        fs::create_dir_all(fixture.commands())?;
        let mut launchers = Vec::new();
        for name in ["contextos", "contextos-web", "contextos-launcher"] {
            let path = fixture.commands().join(name);
            fs::write(&path, b"a launcher")?;
            launchers.push(serde_json::json!({ "path": path, "sha256": digest(b"a launcher") }));
        }
        let profile = fixture.home().join(".zshrc");
        fs::write(&profile, format!("alias ll='ls -l'\n{PROFILE_BLOCK}"))?;
        fs::write(
            fixture.root().join("launchers.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "launchers": launchers,
                "profiles": [profile],
                "path_folder": null
            }))?,
        )?;
        Ok(fixture)
    }

    fn exists(path: &Path) -> bool {
        path.symlink_metadata().is_ok()
    }

    #[test]
    fn uninstall_removes_the_commands_the_path_line_and_the_releases() -> Result<(), BoxError> {
        let _exclusive = exclusive();
        let fixture = fixture()?;

        let output = fixture.run(launcher().args(["uninstall", "--yes"]))?;

        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        for name in ["contextos", "contextos-web", "contextos-launcher"] {
            assert!(!exists(&fixture.commands().join(name)), "{name}");
        }
        assert!(!exists(&fixture.root()));
        assert_eq!(fs::read_to_string(fixture.home().join(".zshrc"))?, "alias ll='ls -l'\n");
        Ok(())
    }

    // A service left behind would keep failing at every login, and a host would keep offering a server that is gone.
    #[test]
    fn uninstall_removes_services_and_host_registrations_first() -> Result<(), BoxError> {
        let _exclusive = exclusive();
        let fixture = fixture()?;

        fixture.run(launcher().args(["uninstall", "--yes"]))?;

        let calls = fixture.calls();
        assert!(calls.contains("contextos service uninstall"), "{calls}");
        assert!(calls.contains("contextos-web service uninstall"), "{calls}");
        assert!(
            calls.contains("contextos config mcp deregister --host claude-desktop"),
            "{calls}"
        );
        Ok(())
    }

    #[test]
    fn uninstall_keeps_vaults_and_settings() -> Result<(), BoxError> {
        let _exclusive = exclusive();
        let fixture = fixture()?;
        let settings = fixture.home().join(".config").join("contextos").join("config.toml");
        fs::create_dir_all(settings.parent().ok_or("no parent")?)?;
        fs::write(&settings, "settings")?;
        let note = fixture.home().join("vault").join("note.md");
        fs::create_dir_all(note.parent().ok_or("no parent")?)?;
        fs::write(&note, "my note")?;

        let output = fixture.run(launcher().args(["uninstall", "--yes"]))?;

        assert!(output.status.success());
        assert_eq!(fs::read_to_string(settings)?, "settings");
        assert_eq!(fs::read_to_string(note)?, "my note");
        Ok(())
    }

    // A file changed since install is no longer the launcher that was placed there, whatever its name.
    #[test]
    fn uninstall_leaves_a_command_that_is_no_longer_the_one_it_placed() -> Result<(), BoxError> {
        let _exclusive = exclusive();
        let fixture = fixture()?;
        let replaced = fixture.commands().join("contextos");
        fs::write(&replaced, b"something of the operator's")?;
        let unrelated = fixture.commands().join("another-tool");
        fs::write(&unrelated, b"another tool")?;

        let output = fixture.run(launcher().args(["uninstall", "--yes"]))?;

        assert!(output.status.success());
        assert_eq!(fs::read(replaced)?, b"something of the operator's");
        assert_eq!(fs::read(unrelated)?, b"another tool");
        assert!(String::from_utf8(output.stdout)?.contains("left as it is"));
        Ok(())
    }

    #[test]
    fn uninstall_asks_first_and_declining_changes_nothing() -> Result<(), BoxError> {
        let _exclusive = exclusive();
        let fixture = fixture()?;

        let mut command = launcher();
        command.arg("uninstall").stdin(std::process::Stdio::null());
        let output = fixture.run(&mut command)?;

        assert!(output.status.success());
        assert!(exists(&fixture.commands().join("contextos")));
        assert!(exists(&fixture.root()));
        assert_eq!(fixture.calls(), "");
        Ok(())
    }

    // `update` belongs to the launcher under its own name only: under a program's name, every argument is the
    // program's, so a program's own subcommand can never be taken over.
    #[test]
    fn under_a_programs_name_every_argument_goes_to_the_program() -> Result<(), BoxError> {
        let _exclusive = exclusive();
        let fixture = fixture()?;
        let named = fixture.directory.path().join("contextos");
        fs::copy(env!("CARGO_BIN_EXE_contextos-launcher"), &named)?;

        let output = fixture.run(Command::new(&named).args(["update", "--yes"]))?;

        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(fixture.calls(), "contextos update --yes\n");
        Ok(())
    }

    #[test]
    fn a_command_with_nothing_installed_says_how_to_install() -> Result<(), BoxError> {
        let _exclusive = exclusive();
        let directory = tempdir()?;
        let named = directory.path().join("contextos");
        fs::copy(env!("CARGO_BIN_EXE_contextos-launcher"), &named)?;

        let output = Command::new(&named)
            .env("HOME", directory.path())
            .env("CONTEXTOS_INSTALL_ROOT", directory.path().join("root"))
            .output()?;

        assert!(!output.status.success());
        assert!(String::from_utf8(output.stderr)?.contains("contextos-launcher install"));
        Ok(())
    }
}
