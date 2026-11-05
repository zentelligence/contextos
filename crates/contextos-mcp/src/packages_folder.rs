//! Where a vault keeps its installed workflow packages and libraries, and the one rule about who may write there.
//!
//! [`crate::server_owned_folders`] gathers this folder's rule with the other server-owned folders'.

use contextos_fs::ServerOwnedFolder;

/// The vault-relative folder every installed release of a vault's package store lives under.
pub(crate) const PACKAGES_FOLDER: &str = ".contextos/packages";

/// The stable code of a tool call refused for writing in the package store.
pub(crate) const SERVER_OWNED_CODE: &str = "packages/server-owned";

/// The package store as a server-owned folder: nothing below it is open to tools, since `contextos-web` installs and
/// removes releases there and a tool edit would change what a job runs.
pub(crate) fn server_owned_folder() -> ServerOwnedFolder {
    ServerOwnedFolder {
        folder: PACKAGES_FOLDER.to_owned(),
        open: Vec::new(),
        code: SERVER_OWNED_CODE,
        remediation: "Only contextos-web writes the vault's package store. Install or remove releases with contextos-web \
                      workflows install, libraries install, or packages install/remove.",
    }
}
