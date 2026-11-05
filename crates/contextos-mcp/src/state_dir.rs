//! Resolves each vault's derived-state directory: the text index, its lock
//! files, the link-graph cache, and the vector store.
//!
//! Default: a hidden directory under the operator's home directory
//! (`~/.contextos`), keyed to the vault's resolved root, deliberately not a
//! platform app-data directory (`directories::ProjectDirs`) and not inside
//! the vault. Both alternatives were tried and rejected: in-vault storage
//! sits inside a directory a third-party sync tool (for example Obsidian
//! Sync) can observe or replicate, and a platform app-data directory is
//! silently diverged by Windows' per-package file virtualisation for any
//! process that inherits a host's MSIX package identity (for example
//! Claude Desktop's own stdio server), which redirects the very same
//! nominal `%LOCALAPPDATA%\contextos\...` path to a private copy under
//! `%LOCALAPPDATA%\Packages\<package>\LocalCache\Local\...` invisibly, so a
//! packaged host and an unpackaged one (the CLI, or any other MCP host)
//! end up reading and writing two unrelated stores while believing they
//! share one. The profile root itself is not subject to that
//! virtualisation on Windows, and `~/.contextos` mirrors
//! `default_config_path` (`main.rs`)'s own already-established, deliberate
//! choice to resolve under the home directory identically on every
//! platform rather than each platform's native convention. `[[vault]]
//! state_directory` overrides this per vault: a relative path resolves
//! against the vault root (opting back into in-vault storage, an
//! operator's explicit choice to accept that trade-off), an absolute path
//! is used exactly as given.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use contextos_host::HostFolders;
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Typed failure resolving a vault's derived-state directory.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum StateDirError {
    #[error(
        "could not resolve the operator's home directory for derived vault state; set [[vault]] state_directory explicitly"
    )]
    HomeDirectoryUnavailable,
}

/// Resolves one vault's derived-state directory per the precedence
/// described in the module documentation.
///
/// # Errors
///
/// Returns [`StateDirError::HomeDirectoryUnavailable`] when no override is
/// configured and the platform cannot resolve a home directory to root the
/// default under.
pub(crate) fn resolve_state_directory(
    override_directory: Option<&Path>,
    vault_root: &Path,
) -> Result<PathBuf, StateDirError> {
    match override_directory {
        Some(path) if path.is_absolute() => Ok(path.to_path_buf()),
        Some(path) => Ok(vault_root.join(path)),
        None => default_state_directory(vault_root),
    }
}

fn default_state_directory(vault_root: &Path) -> Result<PathBuf, StateDirError> {
    let folders = HostFolders::detect().ok_or(StateDirError::HomeDirectoryUnavailable)?;
    Ok(vault_state_directory(&default_state_root(folders.home()), vault_root))
}

/// Composes the home-directory-relative default state root; split out from
/// [`default_state_directory`] so the choice of root is testable without
/// depending on the operator's actual home directory (`AGENTS.md` test
/// standards), the same rationale [`vault_state_directory`] itself already
/// documents.
pub(crate) fn default_state_root(home_dir: &Path) -> PathBuf {
    home_dir.join(".contextos")
}

/// Pure composition of an app-data root and a vault's state-directory key;
/// split out from [`default_state_directory`] so it is testable without
/// depending on the operator's actual home directory (`AGENTS.md` test
/// standards).
fn vault_state_directory(app_data_root: &Path, vault_root: &Path) -> PathBuf {
    app_data_root.join("vaults").join(vault_state_key(vault_root))
}

/// Deterministic, collision-resistant key for a vault's resolved root
/// path: a lowercase SHA-256 hex digest, so two different vaults never
/// share a state directory and the same vault always resolves to the same
/// one.
fn vault_state_key(vault_root: &Path) -> String {
    let digest = Sha256::digest(vault_root.to_string_lossy().as_bytes());
    digest.iter().fold(String::with_capacity(64), |mut key, byte| {
        let _ = write!(key, "{byte:02x}");
        key
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_vault_root_always_resolves_to_the_same_key() {
        let first = vault_state_key(Path::new("vaults/vault"));
        let second = vault_state_key(Path::new("vaults/vault"));
        assert_eq!(first, second);
    }

    #[test]
    fn different_vault_roots_resolve_to_different_keys() {
        let vault = vault_state_key(Path::new("vaults/vault"));
        let other = vault_state_key(Path::new("vaults/other"));
        assert_ne!(vault, other);
    }

    #[test]
    fn key_is_a_lowercase_sha256_hex_digest() {
        let key = vault_state_key(Path::new("vaults/vault"));
        assert_eq!(key.len(), 64);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn default_directory_is_scoped_under_the_state_root_and_a_vaults_segment() {
        let state_root = Path::new("state-root");
        let resolved = vault_state_directory(state_root, Path::new("vaults/vault"));
        assert!(resolved.starts_with(state_root.join("vaults")));
    }

    #[test]
    fn default_state_root_is_a_hidden_directory_under_the_home_directory_not_a_platform_app_data_directory() {
        let home_dir = Path::new("/home/example");
        let resolved = default_state_root(home_dir);
        assert_eq!(resolved, home_dir.join(".contextos"));
    }

    #[test]
    fn override_uses_an_absolute_path_exactly() -> Result<(), Box<dyn std::error::Error>> {
        let vault_root = tempfile::tempdir()?;
        let custom_state = tempfile::tempdir()?;

        let resolved = resolve_state_directory(Some(custom_state.path()), vault_root.path())?;

        assert_eq!(resolved, custom_state.path());
        Ok(())
    }

    #[test]
    fn override_resolves_a_relative_path_against_the_vault_root() -> Result<(), Box<dyn std::error::Error>> {
        let vault_root = tempfile::tempdir()?;

        let resolved = resolve_state_directory(Some(Path::new(".contextos")), vault_root.path())?;

        assert_eq!(resolved, vault_root.path().join(".contextos"));
        Ok(())
    }
}
