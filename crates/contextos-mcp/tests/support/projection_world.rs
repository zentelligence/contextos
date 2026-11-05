//! Temporary vaults, home directory, and state root for tests of agent host projection. Nothing here reads or writes
//! the operator's real home directory.
//!
//! Shared through `tests/support/mod.rs`; a test binary that uses only part of it would otherwise warn, which is
//! expected rather than genuine dead code.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use contextos_mcp::{Config, ConfigLocation, ConfigProvenance, ContextOsServer, ProjectionConfig};

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A valid agent definition at the `balanced` tier with every tool.
pub fn agent(body: &str) -> String {
    format!("---\ndescription: Reviews code\ntier: balanced\naccess: all\n---\n\n{body}\n")
}

/// A valid skill definition.
pub fn skill(name: &str) -> String {
    format!("---\nname: {name}\ndescription: Teaches {name}\n---\n\nHow to {name}.\n")
}

pub struct ProjectionWorld {
    pub home: tempfile::TempDir,
    pub vaults: Vec<tempfile::TempDir>,
}

impl ProjectionWorld {
    pub fn new(vault_count: usize) -> Result<Self, BoxError> {
        let home = tempfile::Builder::new().prefix("home").tempdir()?;
        let vaults = (0..vault_count)
            .map(|index| {
                let vault = tempfile::Builder::new().prefix(&format!("vault{index}")).tempdir()?;
                std::fs::create_dir_all(vault.path().join("registry/agents"))?;
                std::fs::create_dir_all(vault.path().join("registry/skills"))?;
                Ok(vault)
            })
            .collect::<Result<Vec<_>, BoxError>>()?;
        Ok(Self { home, vaults })
    }

    /// Makes the home folders of Claude Code and Codex, as installing both hosts would. A host whose home folder is
    /// absent is not projected into.
    pub fn install_hosts(&self) -> Result<(), BoxError> {
        std::fs::create_dir_all(self.home().join(".claude"))?;
        std::fs::create_dir_all(self.home().join(".codex"))?;
        Ok(())
    }

    pub fn home(&self) -> &Path {
        self.home.path()
    }

    pub fn vault(&self, index: usize) -> &Path {
        self.vaults[index].path()
    }

    /// Writes `contents` at `relative` inside vault `index`, outside the server, as an edit made in Obsidian would.
    pub fn write_vault(&self, index: usize, relative: &str, contents: &str) -> Result<(), BoxError> {
        let path = self.vault(index).join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, contents)?;
        Ok(())
    }

    /// A configuration over every vault, as if loaded from `location`, with `projection` as its `[projection]`.
    pub fn config(&self, location: ConfigLocation, projection: Option<ProjectionConfig>) -> Result<Config, BoxError> {
        self.config_from_file(location, self.home().join(".config/contextos/config.toml"), projection)
    }

    /// As [`Self::config`], as if loaded from the configuration file at `path`, whose identity owns what it projects.
    pub fn config_from_file(
        &self,
        location: ConfigLocation,
        path: PathBuf,
        projection: Option<ProjectionConfig>,
    ) -> Result<Config, BoxError> {
        let mut config = super::scoped_config(self.vaults.iter().map(|vault| vault.path().to_path_buf()).collect())?;
        for vault in &mut config.vaults {
            // Hard deletes, so a test never sends a file to the platform's real trash.
            vault.git.destructive_delete = true;
        }
        config.projection = projection;
        config.provenance = ConfigProvenance {
            location,
            path: Some(path),
            home: Some(self.home().to_path_buf()),
        };
        Ok(config)
    }

    /// A server whose configuration is the platform default, so projection is on.
    pub fn projecting_server(&self) -> Result<ContextOsServer, BoxError> {
        Ok(ContextOsServer::try_from(
            self.config(ConfigLocation::PlatformDefault, None)?,
        )?)
    }

    /// A projecting server whose configuration is the platform-default file `config_file`, a different configuration
    /// from [`Self::projecting_server`]'s when the names differ, so what one projected is foreign to the other.
    pub fn projecting_server_for(&self, config_file: &str) -> Result<ContextOsServer, BoxError> {
        let path = self.home().join(".config/contextos").join(config_file);
        Ok(ContextOsServer::try_from(self.config_from_file(
            ConfigLocation::PlatformDefault,
            path,
            None,
        )?)?)
    }

    pub fn claude_agent(&self, slug: &str) -> PathBuf {
        self.home().join(".claude/agents").join(format!("{slug}.md"))
    }

    pub fn codex_agent(&self, slug: &str) -> PathBuf {
        self.home().join(".codex/agents").join(format!("{slug}.toml"))
    }

    pub fn claude_skill(&self, slug: &str) -> PathBuf {
        self.home().join(".claude/skills").join(slug)
    }

    pub fn codex_skill(&self, slug: &str) -> PathBuf {
        self.home().join(".agents/skills").join(slug)
    }

    /// Whether anything at all exists in the temporary home.
    pub fn home_is_empty(&self) -> Result<bool, BoxError> {
        Ok(std::fs::read_dir(self.home())?.next().is_none())
    }
}
