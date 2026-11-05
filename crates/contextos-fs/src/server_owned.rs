//! Folders only the server writes in.
//!
//! A tool call may not change anything below such a folder, except for the few paths the folder leaves open. The
//! guard sits in the filesystem adapter's mutation methods, the one place every tool write, edit, move, and delete
//! passes through, so no tool can reach the folder by another route. A write the server makes on its own account
//! (`Origin::Internal`) is never refused.

use contextos_core::{Origin, VaultPath};

use crate::FsError;

/// A vault-relative folder that only the server writes below.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerOwnedFolder {
    /// The folder, vault-relative with `/` separators and no leading or trailing `/`.
    pub folder: String,
    /// Paths below the folder that tools may still change, vault-relative.
    pub open: Vec<String>,
    /// The stable error code a refusal carries.
    pub code: &'static str,
    /// What the caller should do instead, carried by every refusal for this folder.
    pub remediation: &'static str,
}

/// The server-owned folders of a filesystem adapter.
#[derive(Clone, Debug, Default)]
pub(crate) struct ServerOwnedFolders {
    folders: Vec<ServerOwnedFolder>,
}

impl From<Vec<ServerOwnedFolder>> for ServerOwnedFolders {
    fn from(folders: Vec<ServerOwnedFolder>) -> Self {
        Self { folders }
    }
}

/// Compares ASCII case-insensitively, because a case-insensitive file system reaches the same folder under any case.
fn same_text(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

impl ServerOwnedFolder {
    /// Whether `relative` is the folder itself or lies below it.
    fn covers(&self, relative: &str) -> bool {
        relative.len() >= self.folder.len()
            && relative.is_char_boundary(self.folder.len())
            && same_text(&relative[..self.folder.len()], &self.folder)
            && (relative.len() == self.folder.len() || relative.as_bytes().get(self.folder.len()) == Some(&b'/'))
    }

    /// Whether `relative` is a folder that contains this folder, so changing it as a whole changes this folder too.
    fn lies_within(&self, relative: &str) -> bool {
        self.folder.len() > relative.len()
            && self.folder.is_char_boundary(relative.len())
            && same_text(&self.folder[..relative.len()], relative)
            && self.folder.as_bytes().get(relative.len()) == Some(&b'/')
    }

    fn leaves_open(&self, relative: &str) -> bool {
        self.open.iter().any(|open| same_text(open, relative))
    }
}

impl ServerOwnedFolders {
    /// Refuses a tool-origin change to `path` that falls in a server-owned folder.
    ///
    /// # Errors
    ///
    /// Returns [`FsError::ServerOwned`] naming the vault-relative path and carrying the folder's own code.
    pub(crate) fn check(&self, path: &VaultPath, origin: &Origin) -> Result<(), FsError> {
        self.refuse(path, origin, false)
    }

    /// Refuses a tool-origin change that takes the whole subtree at `path` with it (a move or a delete) when the
    /// subtree is, contains, or lies below a server-owned folder. Moving or deleting a parent would otherwise let a
    /// tool change the folder's contents out of the guard's sight and put them back.
    ///
    /// # Errors
    ///
    /// Returns [`FsError::ServerOwned`] naming the vault-relative path and carrying the folder's own code.
    pub(crate) fn check_subtree(&self, path: &VaultPath, origin: &Origin) -> Result<(), FsError> {
        self.refuse(path, origin, true)
    }

    fn refuse(&self, path: &VaultPath, origin: &Origin, whole_subtree: bool) -> Result<(), FsError> {
        if self.folders.is_empty() || !matches!(origin, Origin::Tool(_)) {
            return Ok(());
        }
        let relative = path.portable_relative();
        match self.folders.iter().find(|folder| {
            (folder.covers(&relative) && !folder.leaves_open(&relative))
                || (whole_subtree && folder.lies_within(&relative))
        }) {
            Some(folder) => Err(FsError::ServerOwned {
                path: std::path::PathBuf::from(relative),
                folder: folder.folder.clone(),
                code: folder.code,
                remediation: folder.remediation,
            }),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
#[path = "server_owned_test.rs"]
mod tests;
