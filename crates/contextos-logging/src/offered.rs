//! The log file each binary's `config` interview offers while its configuration sets none: one rule, so `contextos`
//! and `contextos-web` offer files side by side and never drift apart.

use std::path::{Path, PathBuf};

/// The size the offered log file is rotated at, in megabytes.
pub const OFFERED_MAX_SIZE_MB: u32 = 10;

/// How many days the offered log file's rotated files are kept.
pub const OFFERED_RETENTION_DAYS: u32 = 7;

/// The log file named `file_name` that this host's `config` interview offers, because a background service's
/// standard error would otherwise be lost:
///
/// - Windows: `contextos\logs\{file_name}` under `%LOCALAPPDATA%`, since the service runs with no console.
/// - macOS: `~/Library/Logs/contextos/{file_name}`, since the `launchd` agent sends standard error nowhere.
/// - Elsewhere none: `systemd --user` keeps standard error in its journal.
#[must_use]
pub fn offered_log_file(file_name: &str) -> Option<PathBuf> {
    let folders = contextos_host::HostFolders::detect();
    offered_log_file_on(
        std::env::consts::OS,
        folders.as_ref().map(contextos_host::HostFolders::home),
        folders.as_ref().map(contextos_host::HostFolders::data_local),
        file_name,
    )
}

/// [`offered_log_file`] for the host `os` (an [`std::env::consts::OS`] value) with these home and local data
/// folders, taken as values so every host's rule can be tested on any host.
#[must_use]
pub fn offered_log_file_on(
    os: &str,
    home: Option<&Path>,
    local_data: Option<&Path>,
    file_name: &str,
) -> Option<PathBuf> {
    match os {
        "windows" => local_data.map(|folder| folder.join("contextos").join("logs").join(file_name)),
        "macos" => home.map(|folder| folder.join("Library").join("Logs").join("contextos").join(file_name)),
        _ => None,
    }
}

#[cfg(test)]
#[path = "offered_test.rs"]
mod tests;
