//! This product's stable launchers: `updater-engine` does the work, and this names the product and puts the outcome
//! into words for the person installing.

use std::path::Path;

use contextos_host::HostFolders;
use updater_core::ProductVersion;
use updater_engine::{PathChange, StableLaunchers};

use crate::LAUNCHER_NAME;
use crate::update_cli::CliResult;

/// The name shown to people, which also marks what is added to a shell profile.
const PRODUCT_LABEL: &str = "ContextOS";

pub(crate) fn host_folders() -> CliResult<HostFolders> {
    Ok(HostFolders::detect().ok_or("no home folder could be found for the ContextOS commands")?)
}

pub(crate) fn stable_launchers<'a>(root: &'a Path, folders: &'a HostFolders) -> StableLaunchers<'a> {
    StableLaunchers {
        installation_root: root,
        home: folders.home(),
        launcher_name: LAUNCHER_NAME,
        product_label: PRODUCT_LABEL,
    }
}

/// Sets up the commands for the release set `version` and puts them on `PATH`, returning what happened in words.
pub(crate) fn set_up_commands(root: &Path, version: &ProductVersion) -> CliResult<String> {
    let mut report = refresh_launchers(root, version)?;
    let folders = host_folders()?;
    let launchers = stable_launchers(root, &folders);
    let change = launchers.add_to_path(
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("SHELL").as_deref(),
    )?;
    report.push(path_report(&launchers.folder(), &change));
    Ok(report.join("\n"))
}

/// Brings every command's launcher up to the release set `version`, returning anything the operator should know.
pub(crate) fn refresh_launchers(root: &Path, version: &ProductVersion) -> CliResult<Vec<String>> {
    let folders = host_folders()?;
    let left_alone = stable_launchers(root, &folders).refresh(version)?;
    Ok(left_alone.iter().map(|path| left_alone_report(path)).collect())
}

fn left_alone_report(path: &Path) -> String {
    format!(
        "{} already exists and is not a ContextOS launcher, so it was left as it is.",
        path.display()
    )
}

fn path_report(folder: &Path, change: &PathChange) -> String {
    let folder = folder.display();
    match change {
        PathChange::AlreadyOnPath => format!("The ContextOS commands are in {folder}."),
        PathChange::AddedToProfiles(profiles) => {
            let names: Vec<String> = profiles.iter().map(|profile| profile.display().to_string()).collect();
            format!(
                "The ContextOS commands are in {folder}, which was added to your PATH in {}.",
                names.join(" and ")
            )
        }
        PathChange::AddedToUserPath => format!("The ContextOS commands are in {folder}, which is on your Path."),
        PathChange::NeedsAddingByHand => {
            format!("The ContextOS commands are in {folder}. Add that folder to your PATH to use them.")
        }
    }
}
