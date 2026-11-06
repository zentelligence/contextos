//! `contextos-launcher uninstall`: removes what an install added, and nothing else. Vaults and settings are the
//! operator's, so they stay, and `updater-engine` only removes a launcher that is still the one it placed.

use std::fs;
use std::path::Path;
use std::process::Command;

use updater_engine::UpdateLock;
use updater_launcher::{ProcessEnvironment, resolve_command, resolve_installation_root};

use crate::commands::{host_folders, stable_launchers};
use crate::update_cli::{CliResult, ENV_PREFIX, PRODUCT, confirm};

/// `contextos-launcher uninstall`.
///
/// # Errors
///
/// Returns an error if the installation root or home folder cannot be resolved, or the record of what was placed cannot
/// be read. Whatever cannot be removed after that is reported rather than stopping the rest.
pub(crate) fn run_uninstall(yes: bool) -> CliResult<String> {
    let root = resolve_installation_root(&ProcessEnvironment, ENV_PREFIX, PRODUCT)?;
    let root = root.as_path();
    if !root.exists() {
        return Ok("ContextOS is not installed.".to_owned());
    }
    if !yes && !confirm("Remove ContextOS from this computer? Your vaults and settings are kept.") {
        return Ok("Uninstall cancelled.".to_owned());
    }

    let folders = host_folders()?;
    let mut report = Vec::new();
    {
        let _lock = UpdateLock::acquire(root)?;
        retire_programs(root, &mut report);
        let removal = stable_launchers(root, &folders).remove()?;
        for path in removal.changed {
            report.push(format!(
                "{} has changed since it was installed, so it was left as it is.",
                path.display()
            ));
        }
        for path in removal.failed {
            report.push(format!("{} could not be removed or changed.", path.display()));
        }
    }
    if let Err(error) = fs::remove_dir_all(root) {
        report.push(format!(
            "{} could not be fully removed, perhaps because a program there is still running: {error}",
            root.display()
        ));
    }

    report.push("ContextOS has been removed.".to_owned());
    report.push(format!(
        "Your vaults and your settings in {} have been kept.",
        folders.config().join(PRODUCT).display()
    ));
    Ok(report.join("\n"))
}

/// Asks the installed programs to remove what they registered, while they are still there to ask: a service left
/// behind would fail at every login, and a host would keep offering a server that has gone.
fn retire_programs(root: &Path, report: &mut Vec<String>) {
    let Ok(installation) = updater_core::InstallationRoot::try_from(root.to_path_buf()) else {
        return;
    };
    for program in ["contextos", "contextos-web"] {
        let removed = run_installed(&installation, program, &["service", "uninstall"]);
        if !removed {
            report.push(format!("The {program} background service could not be removed."));
        }
    }
    let deregister = ["config", "mcp", "deregister", "--host", "claude-desktop"];
    if !run_installed(&installation, "contextos", &deregister) {
        report.push("If Claude Desktop still lists ContextOS, remove it there.".to_owned());
    }
}

/// Runs an installed program, returning whether it succeeded.
fn run_installed(root: &updater_core::InstallationRoot, program: &str, arguments: &[&str]) -> bool {
    resolve_command(root, program).is_ok_and(|resolved| {
        Command::new(resolved.executable_path)
            .args(arguments)
            .output()
            .is_ok_and(|output| output.status.success())
    })
}
