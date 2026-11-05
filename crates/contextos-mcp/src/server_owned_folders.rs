//! Every folder only the server writes in, gathered for the filesystem guard and for the protected restore paths.
//!
//! Each folder's own rule lives in its own module ([`crate::events_folder`], [`crate::packages_folder`]); this module
//! is the one list the composition root and `doctor` build from, so they cannot disagree about which folders are
//! protected.

use contextos_fs::ServerOwnedFolder;

/// The folders only the server writes in.
pub(crate) fn server_owned_folders() -> Vec<ServerOwnedFolder> {
    vec![
        crate::events_folder::server_owned_folder(),
        crate::packages_folder::server_owned_folder(),
    ]
}

/// Adds the server-owned folders to a vault's protected restore paths, whatever the configuration lists, so a
/// restore never reaches into a folder the server alone writes in (and then fails part-way on the guard).
pub(crate) fn with_server_owned_restore_paths(mut paths: Vec<std::path::PathBuf>) -> Vec<std::path::PathBuf> {
    for folder in server_owned_folders() {
        let folder = std::path::PathBuf::from(folder.folder);
        if !paths.iter().any(|path| folder.starts_with(path)) {
            paths.push(folder);
        }
    }
    paths
}

#[cfg(test)]
#[path = "server_owned_folders_test.rs"]
mod tests;
