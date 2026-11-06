//! `contextos-launcher uninstall`: removes what an install added, and nothing else. Vaults and settings are the
//! operator's, so they stay, and a file is only removed if it is still the one that was placed.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use contextos_host::HostFolders;
use updater_engine::UpdateLock;
use updater_launcher::{ProcessEnvironment, resolve_command, resolve_installation_root};

use crate::commands::{Ledger, PlacedLauncher, digest, remove_from_profile};
use crate::update_cli::{CliResult, ENV_PREFIX, PRODUCT, confirm};

/// `contextos-launcher uninstall`.
///
/// # Errors
///
/// Returns an error if the installation root cannot be resolved, or its ledger cannot be read. Whatever cannot be
/// removed after that is reported rather than stopping the rest.
pub(crate) fn run_uninstall(yes: bool) -> CliResult<String> {
    let root = resolve_installation_root(&ProcessEnvironment, ENV_PREFIX, PRODUCT)?;
    let root = root.as_path();
    if !root.exists() {
        return Ok("ContextOS is not installed.".to_owned());
    }
    if !yes && !confirm("Remove ContextOS from this computer? Your vaults and settings are kept.") {
        return Ok("Uninstall cancelled.".to_owned());
    }

    let ledger = Ledger::load(root)?;
    let mut report = Vec::new();
    {
        let _lock = UpdateLock::acquire(root)?;
        retire_programs(root, &mut report);
        for placed in &ledger.launchers {
            remove_launcher(placed, root, &mut report);
        }
        for profile in &ledger.profiles {
            if let Err(error) = remove_from_profile(profile) {
                report.push(format!("{} could not be changed: {error}", profile.display()));
            }
        }
        #[cfg(windows)]
        if let Some(folder) = &ledger.path_folder
            && let Err(error) = crate::commands::remove_folder_from_path(folder)
        {
            report.push(error.to_string());
        }
    }
    if let Err(error) = fs::remove_dir_all(root) {
        report.push(format!(
            "{} could not be fully removed, perhaps because a program there is still running: {error}",
            root.display()
        ));
    }

    report.push("ContextOS has been removed.".to_owned());
    report.push(match HostFolders::detect() {
        Some(folders) => format!(
            "Your vaults and your settings in {} have been kept.",
            folders.config().join(PRODUCT).display()
        ),
        None => "Your vaults and settings have been kept.".to_owned(),
    });
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

fn remove_launcher(placed: &PlacedLauncher, root: &Path, report: &mut Vec<String>) {
    if !placed.path.exists() {
        return;
    }
    if digest(&placed.path).ok().as_deref() != Some(placed.sha256.as_str()) {
        report.push(format!(
            "{} has changed since it was installed, so it was left as it is.",
            placed.path.display()
        ));
        return;
    }
    if fs::remove_file(&placed.path).is_ok() {
        return;
    }
    let mut folders = vec![std::env::temp_dir()];
    folders.extend(root.parent().map(Path::to_path_buf));
    if !move_out_of_the_way(&placed.path, &folders) {
        report.push(format!("{} could not be removed.", placed.path.display()));
    }
}

/// Windows will not delete a running program, which this launcher may be, but lets it be renamed: moved out of the
/// folder it was in, it no longer holds up that folder's removal. A rename cannot cross drives, so each of `folders` is
/// tried in turn, returning whether one worked.
fn move_out_of_the_way(path: &Path, folders: &[PathBuf]) -> bool {
    let name = format!("contextos-launcher-{}.old", std::process::id());
    folders
        .iter()
        .any(|folder| fs::rename(path, folder.join(&name)).is_ok())
}

#[cfg(test)]
#[path = "uninstall_test.rs"]
mod tests;
